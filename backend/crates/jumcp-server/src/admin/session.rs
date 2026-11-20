//! 管理台登录页后端契约：账号口令换 cookie 会话，这是管理面唯一的身份通道。
//!
//! 设计取舍：
//! - cookie `HttpOnly`（JS 读不到）+ `SameSite=Strict`：XSS 偷不走，跨站也带不过去；
//! - 会话无效即 401，没有任何回落凭据——管理面不认 Bearer，绕不过吊销；
//! - cookie 身份的非 GET 请求必须同源（`Origin`/`Referer` 的 authority 与 `Host` 一致），
//!   因为 `SameSite=Strict` 只能挡住跨站发起，挡不住同站兄弟子域与老浏览器的缺口；
//! - 登录失败按 username 计数（内存，单副本前提）：5 次/60s 后 429。按账号而不是按 IP，
//!   是因为部署常在反向代理后面（所有浏览器看起来同一个 IP），按 IP 会让一个人锁住所有人。

use super::*;
use axum::http::header::{COOKIE, HOST, ORIGIN, REFERER, SET_COOKIE};
use axum::http::{HeaderValue, Method};
use jumcp_core::registry::ADMIN_SESSION_ID_BYTES;
use jumcp_core::secret::random_hex;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 会话 cookie 名。
pub const SESSION_COOKIE: &str = "jumcp_session";
/// 登录失败限流：窗口内允许的失败次数，超过即 429。
pub const LOGIN_MAX_FAILURES: u32 = 5;
/// 登录失败限流窗口。
pub const LOGIN_WINDOW: Duration = Duration::from_secs(60);
/// 限流表容量上限：随机用户名失败每次都会插一个新键，没有上限就是可控的内存增长。
pub const MAX_TRACKED_LOGINS: usize = 1024;
const MAX_LOGIN_BODY: usize = 8 * 1024;

#[derive(Debug)]
struct LoginAttempt {
    failures: u32,
    window_start: Instant,
}

/// 登录失败计数（内存）。多副本部署时各副本独立计数，真正的强约束仍是口令强度与 TLS。
///
/// 为什么不用库：这是进程级防爆破护栏，重启清零可接受；落库会引入"限流表被审计/备份带出"
/// 的新面，且失败尝试本身不应写审计（否则就是给攻击者一个放大审计写入的入口）。
#[derive(Clone, Debug, Default)]
pub struct LoginLimiter {
    inner: Arc<Mutex<HashMap<String, LoginAttempt>>>,
}

impl LoginLimiter {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, LoginAttempt>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 是否处于封锁窗口内（窗口已过则顺手删除该键）。
    pub fn is_blocked(&self, key: &str) -> bool {
        let mut guard = self.lock();
        let expired = guard
            .get(key)
            .is_some_and(|attempt| attempt.window_start.elapsed() >= LOGIN_WINDOW);
        if expired {
            guard.remove(key);
        }
        match guard.get(key) {
            Some(attempt) => attempt.failures >= LOGIN_MAX_FAILURES,
            None => false,
        }
    }

    pub fn record_failure(&self, key: &str) {
        let mut guard = self.lock();
        // 写入前先按当前时刻清掉过期项：窗口外的计数没有任何语义，留着只是内存泄漏入口。
        guard.retain(|_, attempt| attempt.window_start.elapsed() < LOGIN_WINDOW);
        if !guard.contains_key(key) && guard.len() >= MAX_TRACKED_LOGINS {
            // 容量护栏：淘汰窗口起点最旧的一条（被淘汰者只是重获尝试次数，不影响正确性）。
            if let Some(oldest) = guard
                .iter()
                .min_by_key(|(_, attempt)| attempt.window_start)
                .map(|(key, _)| key.clone())
            {
                guard.remove(&oldest);
            }
        }
        let entry = guard
            .entry(key.to_string())
            .or_insert_with(|| LoginAttempt {
                failures: 0,
                window_start: Instant::now(),
            });
        entry.failures = entry.failures.saturating_add(1);
    }

    /// 成功后清零：只惩罚连续失败，不把正常用户的一次手抖锁成 60 秒。
    pub fn clear(&self, key: &str) {
        self.lock().remove(key);
    }
}

#[derive(Deserialize)]
struct LoginBody {
    username: String,
    password: String,
}

/// 从 `Cookie` 头取指定 cookie（多行 Cookie / 多余空格 / 无值都安全处理）。
pub(crate) fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    for raw in headers.get_all(COOKIE) {
        let Ok(raw) = raw.to_str() else { continue };
        for pair in raw.split(';') {
            if let Some((key, value)) = pair.split_once('=')
                && key.trim() == name
            {
                return Some(value.trim().to_string());
            }
        }
    }
    None
}

/// 是否为安全方法（幂等 GET/HEAD：CSRF 只针对改变状态的方法）。
fn is_safe_method(method: &Method) -> bool {
    method == Method::GET || method == Method::HEAD
}

/// 同源判定：`Origin`（没有则 `Referer`）的 authority 必须与 `Host` 一致。
///
/// 两端都按默认端口归一（`http://h:80` ≡ `http://h`），因此 `Host` 省略端口时不会误判。
/// 缺 `Host` 或缺 `Origin`/`Referer` 一律视为不同源（fail-closed）：浏览器对非 GET 请求
/// 一定会带 `Origin`，缺失只可能是非浏览器客户端或代理削掉了头。
pub(crate) fn same_origin(headers: &HeaderMap) -> bool {
    let Some(host) = headers.get(HOST).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let source = headers
        .get(ORIGIN)
        .or_else(|| headers.get(REFERER))
        .and_then(|v| v.to_str().ok());
    let Some(source) = source else { return false };
    let Some((scheme, rest)) = source.split_once("://") else {
        return false;
    };
    let scheme = scheme.to_ascii_lowercase();
    let default_port = match scheme.as_str() {
        "http" => ":80",
        "https" => ":443",
        _ => return false,
    };
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if authority.is_empty() {
        return false;
    }
    let normalize = |value: &str| -> String {
        let lower = value.to_ascii_lowercase();
        lower
            .strip_suffix(default_port)
            .map(str::to_string)
            .unwrap_or(lower)
    };
    normalize(&authority) == normalize(host.trim())
}

/// 浏览器跨站登录判定：只在 `Origin`/`Referer` **存在**且与 `Host` 不同源时成立。
///
/// 与 [`csrf_violation`] 的差别是"缺头不算跨站"：登录口必须能被 curl、冒烟脚本与
/// README 示例直接调用，它们不发 `Origin`/`Referer`；而浏览器发起跨站 POST 时一定会带
/// `Origin`，所以"声明了来源且不同源"就足以挡下浏览器侧的跨站登录。
pub(crate) fn cross_site(headers: &HeaderMap) -> bool {
    let declared = headers.get(ORIGIN).or_else(|| headers.get(REFERER));
    declared.is_some() && !same_origin(headers)
}

/// 登录请求体不可用（超限 / 不是 JSON）：400，且**不计入**限流。
///
/// 限流惩罚的是"口令试错"，畸形 body 不是口令尝试；把它算进失败次数等于给攻击者
/// 一个用错格式把任意账号锁死的开关。
fn bad_body(state: &AdminState, actor: &Actor, hint: &str) -> Response {
    ApiError::from_mcp_via_policy(
        state,
        actor,
        McpError::new("MCP_BAD_REQUEST", json!({"field": "body", "hint": hint})),
    )
    .into_response()
}

/// 登录：`{"username":"...","password":"..."}` → 204 + `Set-Cookie`。
///
/// 失败一律 401 且提示逐字相同：账号不存在、口令错误、账号已禁用对外不可区分，
/// 否则登录口就成了账号枚举器。
pub(super) async fn login(
    State(state): State<AdminState>,
    Extension(limiter): Extension<LoginLimiter>,
    req: Request,
) -> Response {
    let actor = admin_actor("");
    // 跨站登录在解析 body 之前拒绝：浏览器发起的跨站 POST 一定带 Origin，先挡在限流计数之外。
    if cross_site(req.headers()) {
        let e = McpError::new(
            "MCP_FORBIDDEN",
            json!({"hint": "浏览器跨站登录被拒绝：Origin/Referer 的 host 需与 Host 一致"}),
        );
        return ApiError::from_mcp_via_policy(&state, &actor, e).into_response();
    }
    let body = match axum::body::to_bytes(req.into_body(), MAX_LOGIN_BODY).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return bad_body(&state, &actor, "登录请求体必须是 JSON，且不超过 8 KiB");
        }
    };
    let parsed = match serde_json::from_slice::<LoginBody>(&body) {
        Ok(parsed) => parsed,
        Err(_) => {
            return bad_body(
                &state,
                &actor,
                "登录请求体必须是 JSON：{\"username\":\"...\",\"password\":\"...\"}",
            );
        }
    };
    // 用户名统一小写后计数：大小写变体不该是绕过限流的办法。
    let username = parsed.username.trim().to_ascii_lowercase();
    if limiter.is_blocked(&username) {
        let e = McpError::new(
            "MCP_RATE_LIMITED",
            json!({
                "hint": format!("登录失败次数过多：{} 秒后重试", LOGIN_WINDOW.as_secs()),
                "window_secs": LOGIN_WINDOW.as_secs(),
                "max_failures": LOGIN_MAX_FAILURES
            }),
        );
        return ApiError::from_mcp_via_policy(&state, &actor, e).into_response();
    }
    let failed = |limiter: &LoginLimiter, username: &str| {
        limiter.record_failure(username);
        ApiError::from_mcp_via_policy(
            &state,
            &actor,
            McpError::with_message(
                "MCP_UNAUTHORIZED",
                "用户名或密码错误".to_string(),
                json!({"hint": "账号不存在、口令错误与账号已停用不做区分"}),
            ),
        )
        .into_response()
    };
    let user = match state.store.admin_user_by_username(&parsed.username) {
        Ok(user) => user,
        Err(e) => return ApiError::from_mcp_via_policy(&state, &actor, e).into_response(),
    };
    // `authenticate` 内含 state 判定：禁用账号即使用对口令也登不进来。
    let Some(user) = user.filter(|user| user.authenticate(&parsed.password)) else {
        return failed(&limiter, &username);
    };
    limiter.clear(&username);
    let session_id =
        match random_hex(ADMIN_SESSION_ID_BYTES).map_err(|e| McpError::store(e.to_string())) {
            Ok(id) => id,
            Err(e) => return ApiError::from_mcp_via_policy(&state, &actor, e).into_response(),
        };
    let session = match state.store.create_admin_session(
        &session_id,
        &user.id,
        state.service.settings.admin_session_ttl_secs,
    ) {
        Ok(session) => session,
        Err(e) => return ApiError(e).into_response(),
    };
    if let Err(e) = state.store.mark_admin_login(&user.id) {
        let _ = state.store.delete_admin_session(&session.id);
        return ApiError(e).into_response();
    }
    // 审计 `admin.login`：object_id 用账号 id，副作用只留 username——**绝不**记录会话 id，
    // 它是凭据本身，写进审计等于把它交给所有能读审计的人。
    let mut payload = audit_base("session", &user.id, "admin.login", "ok");
    payload.insert(
        "data".into(),
        json!({"username": user.username, "expires_at": session.expires_at}),
    );
    if let Err(e) = state.service.audit_event(
        "admin_change",
        Value::Object(payload),
        &admin_actor(&user.username),
    ) {
        // 审计写不进去就不能放行会话（治理面不可静默失效）。
        let _ = state.store.delete_admin_session(&session.id);
        return ApiError(e).into_response();
    }
    let cookie = cookie_header(
        &session.id,
        state.service.settings.admin_session_ttl_secs,
        state.service.settings.admin_cookie_secure,
    );
    let Ok(cookie) = HeaderValue::from_str(&cookie) else {
        let _ = state.store.delete_admin_session(&session.id);
        return ApiError(McpError::new(
            "MCP_UNAUTHORIZED",
            json!({"hint": "会话 cookie 编码失败"}),
        ))
        .into_response();
    };
    let mut resp = StatusCode::NO_CONTENT.into_response();
    resp.headers_mut().insert(SET_COOKIE, cookie);
    resp.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    resp
}

/// 注销：吊销当前 cookie 会话（幂等）并让浏览器丢弃 cookie。
///
/// 幂等性要求它**不能**挂在鉴权层后面：没有 cookie、cookie 已过期、会话已被删，都应当 204。
/// 但仍然拒绝跨站注销（cookie 身份的非 GET 必须同源），否则第三方页面能把管理员悄悄踢下线。
pub(super) async fn logout(State(state): State<AdminState>, req: Request) -> Response {
    let headers = req.headers();
    let has_cookie = cookie_value(headers, SESSION_COOKIE).is_some();
    if has_cookie && csrf_violation(req.method(), headers) {
        let e = McpError::new(
            "MCP_FORBIDDEN",
            json!({"hint": "cookie 身份的非 GET 请求必须同源：Origin/Referer 的 host 需与 Host 一致"}),
        );
        return ApiError::from_mcp_via_policy(&state, &admin_actor(""), e).into_response();
    }
    if let Some(raw) = cookie_value(headers, SESSION_COOKIE) {
        let session = match state.store.admin_session(&raw) {
            Ok(session) => session,
            Err(e) => return ApiError(e).into_response(),
        };
        if let Some(session) = session {
            // 审计里的 actor 是账号 username（账号改名后仍以当前名为准）；账号已被删除时退化为 id。
            let username = state
                .store
                .admin_user(&session.admin_user_id)
                .ok()
                .flatten()
                .map(|user| user.username);
            if let Err(e) = state.store.delete_admin_session(&session.id) {
                return ApiError(e).into_response();
            }
            let mut payload = audit_base("session", &session.admin_user_id, "admin.logout", "ok");
            payload.insert("data".into(), json!({"username": username}));
            if let Err(e) = state.service.audit_event(
                "admin_change",
                Value::Object(payload),
                &admin_actor(username.as_deref().unwrap_or(&session.admin_user_id)),
            ) {
                return ApiError(e).into_response();
            }
        }
    }
    // 首部过期与是否命中会话无关：浏览器侧必须无条件丢弃，否则"注销后仍带着旧 cookie"。
    let mut resp = StatusCode::NO_CONTENT.into_response();
    resp.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&expired_cookie(state.service.settings.admin_cookie_secure))
            .unwrap_or_else(|_| HeaderValue::from_static("jumcp_session=")),
    );
    resp
}

/// 当前身份：登录页用它判断"要不要跳登录"。
///
/// `id` 是 username（审计与展示用的可读身份），`user_id` 是账号主键（32 位 hex）：
/// 前端按主键改自己的账号，不能拿 username 当外键（改名即失联）。
pub(super) async fn session_info(Extension(auth): Extension<AuthenticatedAdmin>) -> Json<Value> {
    Json(
        json!({"actor": {"id": auth.id, "user_id": auth.user_id, "role": "admin", "type": "admin"}}),
    )
}

/// `Set-Cookie` 文本。`Secure` 只在显式开启时加（纯 HTTP 内网部署带 Secure 会让 cookie 永不回传）。
pub(crate) fn cookie_header(id: &str, ttl_secs: i64, secure: bool) -> String {
    let ttl = ttl_secs.max(1);
    let mut header =
        format!("{SESSION_COOKIE}={id}; HttpOnly; SameSite=Strict; Path=/; Max-Age={ttl}");
    if secure {
        header.push_str("; Secure");
    }
    header
}

fn expired_cookie(secure: bool) -> String {
    let mut header = format!("{SESSION_COOKIE}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0");
    if secure {
        header.push_str("; Secure");
    }
    header
}

/// 供中间件复用：cookie 身份的非安全方法必须同源。
pub(crate) fn csrf_violation(method: &Method, headers: &HeaderMap) -> bool {
    !is_safe_method(method) && !same_origin(headers)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (k, v) in pairs {
            map.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        map
    }

    #[test]
    fn extracts_named_cookie_among_many() {
        let map = headers(&[("cookie", "a=1; jumcp_session=abc; b=2")]);
        assert_eq!(cookie_value(&map, SESSION_COOKIE).as_deref(), Some("abc"));
        assert_eq!(cookie_value(&map, "missing"), None);
        let empty = headers(&[]);
        assert_eq!(cookie_value(&empty, SESSION_COOKIE), None);
    }

    #[test]
    fn same_origin_compares_authority_and_normalizes_default_ports() {
        assert!(same_origin(&headers(&[
            ("host", "127.0.0.1:8080"),
            ("origin", "http://127.0.0.1:8080")
        ])));
        assert!(same_origin(&headers(&[
            ("host", "admin.example.com"),
            ("origin", "https://admin.example.com")
        ])));
        assert!(same_origin(&headers(&[
            ("host", "admin.example.com:443"),
            ("origin", "https://admin.example.com")
        ])));
        assert!(same_origin(&headers(&[
            ("host", "admin.example.com"),
            ("referer", "https://admin.example.com/admin/login?x=1")
        ])));
        // 不同源 / 端口不同 / 缺头 → 拒绝
        assert!(!same_origin(&headers(&[
            ("host", "admin.example.com"),
            ("origin", "https://evil.example.com")
        ])));
        assert!(!same_origin(&headers(&[
            ("host", "admin.example.com:8080"),
            ("origin", "http://admin.example.com:8000")
        ])));
        assert!(!same_origin(&headers(&[(
            "origin",
            "http://admin.example.com"
        )])));
        assert!(!same_origin(&headers(&[
            ("host", "admin.example.com"),
            ("origin", "null")
        ])));
    }

    #[test]
    fn csrf_only_applies_to_unsafe_cookie_methods() {
        let no_origin = headers(&[("host", "h")]);
        assert!(!csrf_violation(&Method::GET, &no_origin));
        assert!(csrf_violation(&Method::POST, &no_origin));
        let good = headers(&[("host", "h"), ("origin", "http://h")]);
        assert!(!csrf_violation(&Method::POST, &good));
        // Bearer 不走 cookie 分支，因此这里只测函数本身；同源判定与身份来源无关。
    }

    #[test]
    fn cookie_header_flags() {
        let plain = cookie_header("abc", 100, false);
        assert!(plain.contains("HttpOnly") && plain.contains("SameSite=Strict"));
        assert!(!plain.contains("Secure"));
        assert!(cookie_header("abc", 100, true).contains("; Secure"));
        assert!(expired_cookie(false).contains("Max-Age=0"));
    }

    #[test]
    fn limiter_blocks_after_max_failures() {
        let limiter = LoginLimiter::default();
        for _ in 0..LOGIN_MAX_FAILURES {
            assert!(!limiter.is_blocked("1.2.3.4"));
            limiter.record_failure("1.2.3.4");
        }
        assert!(limiter.is_blocked("1.2.3.4"));
        assert!(!limiter.is_blocked("5.6.7.8"), "按 IP 隔离");
        limiter.clear("1.2.3.4");
        assert!(!limiter.is_blocked("1.2.3.4"));
    }

    /// 攻击者用随机用户名轰炸时，表不能无限长：超限前先清过期、再淘汰最旧一条。
    #[test]
    fn limiter_is_bounded_by_max_tracked_logins() {
        let limiter = LoginLimiter::default();
        for i in 0..2000 {
            limiter.record_failure(&format!("user-{i}"));
        }
        assert!(
            limiter.lock().len() <= MAX_TRACKED_LOGINS,
            "限流表必须封顶在 {MAX_TRACKED_LOGINS} 条"
        );
        // 淘汰是"重获尝试次数"，不是放行：被追踪的键仍然按阈值封锁。
        limiter.clear("user-1999");
        for _ in 0..LOGIN_MAX_FAILURES {
            limiter.record_failure("user-1999");
        }
        assert!(limiter.is_blocked("user-1999"));
    }

    /// 窗口过期后同一用户名应当重新拿到全部尝试次数（过期即逐条删除，不靠容量淘汰）。
    #[test]
    fn limiter_window_expiry_restores_attempts() {
        let limiter = LoginLimiter::default();
        for _ in 0..LOGIN_MAX_FAILURES {
            limiter.record_failure("1.2.3.4");
        }
        assert!(limiter.is_blocked("1.2.3.4"));
        {
            let mut guard = limiter.lock();
            let attempt = guard.get_mut("1.2.3.4").expect("键仍在表内");
            attempt.window_start =
                Instant::now() - LOGIN_WINDOW - std::time::Duration::from_secs(1);
        }
        assert!(!limiter.is_blocked("1.2.3.4"), "窗口已过即解锁");
        assert!(
            !limiter.lock().contains_key("1.2.3.4"),
            "过期键必须被删除，而不是留到容量压力时才清"
        );
        for _ in 0..LOGIN_MAX_FAILURES - 1 {
            assert!(!limiter.is_blocked("1.2.3.4"));
            limiter.record_failure("1.2.3.4");
        }
        assert!(!limiter.is_blocked("1.2.3.4"), "少一次失败不应封锁");
        limiter.record_failure("1.2.3.4");
        assert!(limiter.is_blocked("1.2.3.4"), "重新攒满阈值应封锁");
    }

    /// 登录的同源判定必须放行非浏览器客户端：只看"声明了且跨站"。
    #[test]
    fn cross_site_requires_a_declared_source() {
        assert!(cross_site(&headers(&[
            ("host", "admin.example.com"),
            ("origin", "https://evil.example.com")
        ])));
        assert!(cross_site(&headers(&[
            ("host", "admin.example.com"),
            ("referer", "https://evil.example.com/phish")
        ])));
        assert!(!cross_site(&headers(&[
            ("host", "admin.example.com"),
            ("origin", "https://admin.example.com")
        ])));
        assert!(!cross_site(&headers(&[
            ("host", "admin.example.com"),
            ("referer", "https://admin.example.com/admin/login")
        ])));
        // curl / 冒烟脚本不带 Origin：不属于"跨站"，交给口令与限流。
        assert!(!cross_site(&headers(&[("host", "admin.example.com")])));
        assert!(!cross_site(&headers(&[])));
    }
}
