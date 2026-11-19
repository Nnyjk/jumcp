//! 管理台登录契约：账号口令换 cookie 会话、CSRF 同源、限流与过期/吊销。
//!
//! 断言口径与 `docs/mcp-router.md` 的「登录契约」一节逐条对应；任何一条改动都必须同时改文档，
//! 因为 `admin-web` 是照这份契约实现的（它不在本仓库的测试覆盖范围内）。

use crate::support::*;
use axum::Router;
use axum::http::StatusCode;
use axum::http::header::SET_COOKIE;
use jumcp_core::audit::AuditLog;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

const SESSION_COOKIE: &str = "jumcp_session";

/// 登录并返回 (状态, `Set-Cookie` 原值)。
async fn login(app: &Router, username: &str, password: &str) -> (StatusCode, String) {
    let resp = send_raw(
        app,
        "POST",
        "/admin/login",
        None,
        &[],
        Some(json!({"username": username, "password": password})),
    )
    .await;
    let status = resp.status();
    let set_cookie = resp
        .headers()
        .get(SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    (status, set_cookie)
}

/// `Set-Cookie` 里的 `name=value` 片段（浏览器回传时只带这一段）。
fn cookie_pair(set_cookie: &str) -> String {
    set_cookie
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_string()
}

#[tokio::test]
async fn login_issues_session_cookie_and_reports_actor() {
    let h = harness();
    let (status, set_cookie) = login(&h.app, "alice", ALICE_PASSWORD).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(
        set_cookie.starts_with(&format!("{SESSION_COOKIE}=")),
        "cookie 名必须是 {SESSION_COOKIE}：{set_cookie}"
    );
    assert!(set_cookie.contains("HttpOnly"), "JS 不可读");
    assert!(set_cookie.contains("SameSite=Strict"));
    assert!(set_cookie.contains("Path=/"));
    assert!(set_cookie.contains("Max-Age=43200"), "默认 TTL 12 小时");
    assert!(
        !set_cookie.contains("Secure"),
        "纯 HTTP 部署默认不加 Secure"
    );
    let value = cookie_pair(&set_cookie);
    let session_id = value
        .split_once('=')
        .map(|(_, id)| id)
        .expect("cookie pair");
    assert!(session_id.len() >= 32, "会话 id 必须是不可预测的长随机串");

    let (status, body) = send(
        &h.app,
        "GET",
        "/admin/session",
        None,
        &[("cookie", value.as_str())],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // 账号主键不硬编码：播种每次都会变，只要求与 /admin/admins 里同 username 行的 id 一致。
    let (status, admins) = send(
        &h.app,
        "GET",
        "/admin/admins",
        None,
        &[("cookie", value.as_str())],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let alice_id = admins["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|row| row["username"] == json!("alice"))
        .and_then(|row| row["id"].as_str())
        .expect("alice 的账号主键");
    assert_eq!(alice_id.len(), 32, "账号主键是 32 位十六进制");
    assert_eq!(
        body["actor"],
        json!({"id": "alice", "user_id": alice_id, "role": "admin", "type": "admin"})
    );
    // `/admin/me` 与 session 的 actor 字段集合必须完全一致（前者平铺，不套 actor）。
    let (status, me) = send(
        &h.app,
        "GET",
        "/admin/me",
        None,
        &[("cookie", value.as_str())],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        me,
        json!({"id": "alice", "user_id": alice_id, "role": "admin", "type": "admin"}),
        "/admin/me 与 /admin/session 的 actor 必须同形"
    );

    // 登录本身要留痕，且属于平台面（surface=admin），否则统计口径会把登录算进工具调用。
    let events = h
        .audit
        .query(Some(&json!({"event_type": "admin_change"})))
        .expect("query");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].payload["action"], json!("admin.login"));
    assert_eq!(events[0].actor["id"], json!("alice"));
    assert_eq!(events[0].surface, "admin");
    assert!(
        !events[0].payload.to_string().contains(session_id),
        "会话 id 是凭据，绝不能写进审计"
    );
    assert!(
        !events[0].payload.to_string().contains(ALICE_PASSWORD),
        "口令绝不能写进审计"
    );
    // 登录成功必须回写 last_login_at：账号列表靠它显示"最近登录"。
    let user = h
        .store
        .admin_user_by_username("alice")
        .expect("lookup")
        .expect("alice");
    assert!(user.last_login_at.is_some(), "{user:?}");
}

#[tokio::test]
async fn secure_flag_is_opt_in() {
    let mut settings = settings();
    settings.admin_cookie_secure = true;
    let h = harness_custom(
        Arc::new(AuditLog::open(":memory:").expect("audit")),
        settings,
    );
    let (status, set_cookie) = login(&h.app, "alice", ALICE_PASSWORD).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(set_cookie.contains("; Secure"), "显式开启时必须有 Secure");
}

#[tokio::test]
async fn wrong_password_is_401_and_sets_no_cookie() {
    let h = harness();
    let (status, set_cookie) = login(&h.app, "alice", "not-the-password").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(set_cookie.is_empty());
}

/// 超限 body 是客户端错误，不是"口令错误"：必须 400 且不消耗尝试次数。
#[tokio::test]
async fn oversized_login_body_is_400_and_does_not_count_against_limiter() {
    let h = harness();
    let huge = json!({"username": "alice", "password": "x".repeat(9 * 1024)});
    for _ in 0..10 {
        let (status, body) = send(
            &h.app,
            "POST",
            "/admin/login",
            None,
            &[],
            Some(huge.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], json!("MCP_BAD_REQUEST"));
        assert_eq!(body["error"]["data"]["field"], json!("body"));
    }
    // 重放 10 次都没有计入限流，因此正确口令仍然能登录（而不是先撞 429）。
    let (status, _) = login(&h.app, "alice", ALICE_PASSWORD).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

/// 浏览器跨站登录直接拒绝；不带 Origin（curl/冒烟脚本）照旧走口令校验。
#[tokio::test]
async fn cross_site_login_is_forbidden() {
    let h = harness();
    let body = json!({"username": "alice", "password": ALICE_PASSWORD});
    let (status, resp) = send(
        &h.app,
        "POST",
        "/admin/login",
        None,
        &[
            ("host", "localhost"),
            ("origin", "https://evil.example.com"),
        ],
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(resp["error"]["code"], json!("MCP_FORBIDDEN"));
    assert!(
        resp["error"]["data"]["hint"]
            .as_str()
            .unwrap_or_default()
            .contains("跨站"),
        "hint 必须说清是跨站登录: {resp}"
    );
    let (status, _) = send(
        &h.app,
        "POST",
        "/admin/login",
        None,
        &[("host", "localhost"), ("origin", "http://localhost")],
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "同源登录照常放行");
}

/// 失败提示对"账号不存在 / 口令错误 / 已禁用"必须逐字相同，否则登录口就是账号枚举器。
#[tokio::test]
async fn login_failure_message_is_uniform() {
    let h = harness();
    let mut messages = Vec::new();
    for (username, password) in [("nobody", "whatever-pass"), ("alice", "wrong-password-1")] {
        let (status, body) = send(
            &h.app,
            "POST",
            "/admin/login",
            None,
            &[],
            Some(json!({"username": username, "password": password})),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{username}");
        assert_eq!(body["error"]["code"], json!("MCP_UNAUTHORIZED"));
        messages.push(body["error"]["message"].clone());
    }
    // 第 3 条：已禁用账号用对口令也登不进来，且提示相同。
    let bob = h
        .store
        .admin_user_by_username("bob")
        .expect("lookup")
        .expect("bob");
    h.store
        .update_admin_user(&bob.id, None, None, Some("disabled"))
        .expect("disable bob");
    let (status, body) = send(
        &h.app,
        "POST",
        "/admin/login",
        None,
        &[],
        Some(json!({"username": "bob", "password": BOB_PASSWORD})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    messages.push(body["error"]["message"].clone());
    assert_eq!(
        messages,
        vec![json!("用户名或密码错误"); 3],
        "三种失败必须同一条文案"
    );
}

#[tokio::test]
async fn five_failures_then_429_even_for_valid_password() {
    let h = harness();
    for _ in 0..5 {
        let (status, _) = login(&h.app, "alice", "wrong-password").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let resp = send_raw(
        &h.app,
        "POST",
        "/admin/login",
        None,
        &[],
        Some(json!({"username": "alice", "password": ALICE_PASSWORD})),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .expect("body");
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(body["error"]["code"], json!("MCP_RATE_LIMITED"));
}

#[tokio::test]
async fn expired_session_is_401() {
    let mut settings = settings();
    settings.admin_session_ttl_secs = 1;
    let h = harness_custom(
        Arc::new(AuditLog::open(":memory:").expect("audit")),
        settings,
    );
    let (status, set_cookie) = login(&h.app, "alice", ALICE_PASSWORD).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let value = cookie_pair(&set_cookie);
    let (status, _) = send(
        &h.app,
        "GET",
        "/admin/session",
        None,
        &[("cookie", value.as_str())],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "刚登录必然有效");
    // 过期判定按秒级 unix 时间，1s TTL 需要真的跨过一秒（不 sleep 就是在测自己）。
    std::thread::sleep(Duration::from_millis(1_200));
    let (status, body) = send(
        &h.app,
        "GET",
        "/admin/session",
        None,
        &[("cookie", value.as_str())],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "过期必须 401 而不是 403");
    assert_eq!(body["error"]["code"], json!("MCP_UNAUTHORIZED"));
}

#[tokio::test]
async fn logout_revokes_session_and_is_idempotent() {
    let h = harness();
    let (_, set_cookie) = login(&h.app, "alice", ALICE_PASSWORD).await;
    let value = cookie_pair(&set_cookie);
    let headers = [
        ("cookie", value.as_str()),
        ("host", "localhost"),
        ("origin", "http://localhost"),
    ];
    let (status, _) = send(&h.app, "POST", "/admin/logout", None, &headers, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // 吊销后同一 cookie 一律 401：不能让"已注销"的浏览器继续读到管理数据。
    let (status, _) = send(
        &h.app,
        "GET",
        "/admin/session",
        None,
        &[("cookie", value.as_str())],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // 幂等：重复注销仍然 204（否则前端重试会看到 401 并误判为"未登录"）。
    let (status, _) = send(&h.app, "POST", "/admin/logout", None, &headers, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // 审计里注销事件带账号身份，但同样不含会话 id。
    let events = h
        .audit
        .query(Some(&json!({"event_type": "admin_change"})))
        .expect("query");
    assert!(
        events
            .iter()
            .any(|e| e.payload["action"] == json!("admin.logout"))
    );
}

#[tokio::test]
async fn cookie_writes_require_same_origin_and_bearer_is_not_admin_auth() {
    let h = harness();
    let (_, set_cookie) = login(&h.app, "alice", ALICE_PASSWORD).await;
    let value = cookie_pair(&set_cookie);
    let body = json!({"code": "ops", "name": "运维组"});

    // 1) cookie 身份 + 缺 Origin/Referer → 403（不是 401：身份是有效的，拒绝的是来源）。
    let (status, error) = send(
        &h.app,
        "POST",
        "/admin/groups",
        None,
        &[("cookie", value.as_str()), ("host", "localhost")],
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error["error"]["code"], json!("MCP_FORBIDDEN"));

    // 2) 跨源 Origin → 同样 403。
    let (status, _) = send(
        &h.app,
        "POST",
        "/admin/groups",
        None,
        &[
            ("cookie", value.as_str()),
            ("host", "localhost"),
            ("origin", "http://evil.example.com"),
        ],
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // 3) 同源 → 放行（这里会真的建组，因此必须用同一条链路验证"放行"而不只是"不 403"）。
    let (status, _) = send(
        &h.app,
        "POST",
        "/admin/groups",
        None,
        &[
            ("cookie", value.as_str()),
            ("host", "localhost"),
            ("origin", "http://localhost"),
        ],
        Some(body),
    )
    .await;
    assert!(status.is_success(), "同源 cookie 写请求必须放行：{status}");

    // 4) Bearer 不是管理面凭据：管理面只认会话 cookie，脚本也不能绕过同源检查。
    let (status, error) = send(
        &h.app,
        "POST",
        "/admin/groups",
        Some("tok_whatever_00000000"),
        &[("host", "localhost"), ("origin", "http://localhost")],
        Some(json!({"code": "ops2", "name": "运维二组"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{error}");
    assert_eq!(error["error"]["code"], json!("MCP_UNAUTHORIZED"));
}

#[tokio::test]
async fn invalid_cookie_does_not_fall_back_to_bearer() {
    let h = harness();
    let (status, body) = send(
        &h.app,
        "GET",
        "/admin/session",
        Some("tok_whatever_00000000"),
        &[("cookie", "jumcp_session=deadbeef")],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], json!("MCP_UNAUTHORIZED"));
}
