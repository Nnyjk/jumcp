//! 管理台登录页后端契约：cookie 会话 + 与 Bearer 并存的身份判定。
//!
//! 设计取舍：
//! - 浏览器只认 cookie（`HttpOnly`，JS 读不到），脚本仍用 `Authorization: Bearer`；
//! - cookie **优先**：带了 cookie 就按 cookie 判定，无效即 401，不回落 Bearer。
//!   回落会让"会话已被吊销"看起来仍然登录，也会让同源检查（CSRF）可被 Bearer 顺手绕过；
//! - cookie 身份的非 GET 请求必须同源（`Origin`/`Referer` 的 authority 与 `Host` 一致），
//!   因为 `SameSite=Strict` 只能挡住跨站发起，挡不住同站兄弟子域与老浏览器的缺口；
//! - 登录失败按来源 IP 计数（内存，单副本前提）：5 次/60s 后 429。

use super::*;
use axum::body::Bytes;
use axum::extract::ConnectInfo;
use axum::http::header::{COOKIE, HOST, ORIGIN, REFERER, SET_COOKIE};
use axum::http::{HeaderValue, Method};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 会话 cookie 名。
pub const SESSION_COOKIE: &str = "jumcp_session";
/// 登录失败限流：窗口内允许的失败次数，超过即 429。
pub const LOGIN_MAX_FAILURES: u32 = 5;
/// 登录失败限流窗口。
pub const LOGIN_WINDOW: Duration = Duration::from_secs(60);
const MAX_LOGIN_BODY: usize = 8 * 1024;

#[derive(Debug)]
struct LoginAttempt {
    failures: u32,
    window_start: Instant,
}

/// 登录失败计数（内存）。多副本部署时各副本独立计数，真正的强约束仍是 token 长度与 TLS。
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

    /// 是否处于封锁窗口内（窗口已过则顺手清零）。
    pub fn is_blocked(&self, ip: &str) -> bool {
        let mut guard = self.lock();
        match guard.get_mut(ip) {
            Some(attempt) if attempt.window_start.elapsed() >= LOGIN_WINDOW => {
                attempt.failures = 0;
                attempt.window_start = Instant::now();
                false
            }
            Some(attempt) => attempt.failures >= LOGIN_MAX_FAILURES,
            None => false,
        }
    }

    pub fn record_failure(&self, ip: &str) {
        let mut guard = self.lock();
        let entry = guard.entry(ip.to_string()).or_insert(LoginAttempt {
            failures: 0,
            window_start: Instant::now(),
        });
        if entry.window_start.elapsed() >= LOGIN_WINDOW {
            entry.failures = 0;
            entry.window_start = Instant::now();
        }
        entry.failures = entry.failures.saturating_add(1);
    }

    /// 成功后清零：只惩罚连续失败，不把正常用户的一次手抖锁成 60 秒。
    pub fn clear(&self, ip: &str) {
        self.lock().remove(ip);
    }
}

#[derive(Deserialize)]
struct LoginBody {
    token: String,
}

/// 客户端地址：只信任 TCP 对端（不读 `X-Forwarded-For`——那由客户端随意伪造，
/// 让限流变成"改个 header 就重置"）。测试里没有 ConnectInfo 时归入 `unknown` 桶。
fn client_ip(req: &Request) -> String {
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string())
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

/// 登录：`{"token":"<admin token>"}` → 204 + `Set-Cookie`。
pub(super) async fn login(
    State(state): State<AdminState>,
    Extension(limiter): Extension<LoginLimiter>,
    req: Request,
) -> Response {
    let ip = client_ip(&req);
    if limiter.is_blocked(&ip) {
        let e = McpError::new(
            "MCP_RATE_LIMITED",
            json!({
                "hint": format!("登录失败次数过多：{} 秒后重试", LOGIN_WINDOW.as_secs()),
                "window_secs": LOGIN_WINDOW.as_secs(),
                "max_failures": LOGIN_MAX_FAILURES
            }),
        );
        return ApiError::from_mcp_via_policy(&state, &admin_actor(""), e).into_response();
    }
    let body = axum::body::to_bytes(req.into_body(), MAX_LOGIN_BODY)
        .await
        .unwrap_or_else(|_| Bytes::new());
    let token = serde_json::from_slice::<LoginBody>(&body)
        .map(|b| b.token)
        .unwrap_or_default();
    // 复用 admin token 表语义：`id:token` 列表、常量时间比较；失败即 401，不回显"哪一位错"。
    let Some(token_id) = state.tokens.authenticate(&token).map(str::to_string) else {
        limiter.record_failure(&ip);
        let e = McpError::new(
            "MCP_UNAUTHORIZED",
            json!({"hint": "管理面需要 JUMCP_ADMIN_TOKENS 中的 token"}),
        );
        return ApiError::from_mcp_via_policy(&state, &admin_actor(""), e).into_response();
    };
    limiter.clear(&ip);
    let session = match state
        .store
        .create_admin_session(&token_id, state.service.settings.admin_session_ttl_secs)
    {
        Ok(session) => session,
        Err(e) => return ApiError(e).into_response(),
    };
    // 审计 `admin.login`：object_id 用 token id（actor 标识），**绝不**记录会话 id——
    // 它是凭据本身，写进审计等于把它交给所有能读审计的人。
    let mut payload = audit_base("session", &token_id, "admin.login", "ok");
    payload.insert(
        "data".into(),
        json!({"token_id": token_id, "expires_at": session.expires_at}),
    );
    if let Err(e) = state.service.audit_event(
        "admin_change",
        Value::Object(payload),
        &admin_actor(&token_id),
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
            if let Err(e) = state.store.delete_admin_session(&session.id) {
                return ApiError(e).into_response();
            }
            let mut payload = audit_base("session", &session.token_id, "admin.logout", "ok");
            payload.insert("data".into(), json!({"token_id": session.token_id}));
            if let Err(e) = state.service.audit_event(
                "admin_change",
                Value::Object(payload),
                &admin_actor(&session.token_id),
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

/// 当前身份（cookie 或 Bearer 都算）：登录页用它判断"要不要跳登录"。
pub(super) async fn session_info(Extension(auth): Extension<AuthenticatedAdmin>) -> Json<Value> {
    Json(json!({"actor": {"id": auth.id, "role": "admin", "type": "admin"}}))
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
}
