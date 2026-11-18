//! 管理台登录契约：cookie 会话、Bearer 并存、CSRF 同源、限流与过期/吊销。
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
async fn login(app: &Router, token: &str) -> (StatusCode, String) {
    let resp = send_raw(
        app,
        "POST",
        "/admin/login",
        None,
        &[],
        Some(json!({"token": token})),
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
    let (status, set_cookie) = login(&h.app, ALICE_TOKEN).await;
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
    assert!(
        value.len() > SESSION_COOKIE.len() + 1 + 32,
        "会话 id 必须是不可预测的长随机串"
    );

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
    assert_eq!(
        body["actor"],
        json!({"id": "alice", "role": "admin", "type": "admin"})
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
        !events[0]
            .payload
            .to_string()
            .contains(&value[SESSION_COOKIE.len() + 1..]),
        "会话 id 是凭据，绝不能写进审计"
    );
}

#[tokio::test]
async fn secure_flag_is_opt_in() {
    let mut settings = settings();
    settings.admin_cookie_secure = true;
    let h = harness_custom(
        Arc::new(AuditLog::open(":memory:").expect("audit")),
        settings,
    );
    let (status, set_cookie) = login(&h.app, ALICE_TOKEN).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(set_cookie.contains("; Secure"), "显式开启时必须有 Secure");
}

#[tokio::test]
async fn wrong_token_is_401_and_sets_no_cookie() {
    let h = harness();
    let (status, set_cookie) = login(&h.app, "not-an-admin-token").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(set_cookie.is_empty());
}

#[tokio::test]
async fn five_failures_then_429_even_for_valid_token() {
    let h = harness();
    for _ in 0..5 {
        let (status, _) = login(&h.app, "wrong").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let resp = send_raw(
        &h.app,
        "POST",
        "/admin/login",
        None,
        &[],
        Some(json!({"token": ALICE_TOKEN})),
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
    let (status, set_cookie) = login(&h.app, ALICE_TOKEN).await;
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
    let (_, set_cookie) = login(&h.app, ALICE_TOKEN).await;
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
    // 审计里注销事件带 token 身份，但同样不含会话 id。
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
async fn cookie_writes_require_same_origin_but_bearer_does_not() {
    let h = harness();
    let (_, set_cookie) = login(&h.app, ALICE_TOKEN).await;
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

    // 4) Bearer 不受同源限制（脚本/CI 没有浏览器，也不存在可被借用的 cookie）。
    let (status, error) = send(
        &h.app,
        "POST",
        "/admin/groups",
        Some(ALICE_TOKEN),
        &[],
        Some(json!({"code": "ops2", "name": "运维二组"})),
    )
    .await;
    assert!(
        status.is_success(),
        "Bearer 不应被 CSRF 拦截：{status} {error}"
    );
}

#[tokio::test]
async fn invalid_cookie_does_not_fall_back_to_bearer() {
    let h = harness();
    let (status, body) = send(
        &h.app,
        "GET",
        "/admin/session",
        Some(ALICE_TOKEN),
        &[("cookie", "jumcp_session=deadbeef")],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], json!("MCP_UNAUTHORIZED"));
}
