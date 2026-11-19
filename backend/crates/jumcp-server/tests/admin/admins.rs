//! 管理员账号管理契约：清单、新建、改用户名/口令/状态、删除与账号级护栏。
//!
//! 关键不变量：
//! - 任何时刻至少一名启用账号（否则平台再也没有人能登录）；
//! - 改口令或停用会立刻吊销该账号的会话（不是等会话自然过期）；
//! - 审计记动作与字段名，`password`/`password_hash` 永不出现在载荷里。

use crate::support::*;
use axum::http::StatusCode;
use serde_json::json;

fn item<'a>(body: &'a serde_json::Value, username: &str) -> &'a serde_json::Value {
    body["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|item| item["username"] == json!(username))
        .unwrap_or_else(|| panic!("no admin {username}: {body}"))
}

#[tokio::test]
async fn list_admins_shows_state_and_never_leaks_hashes() {
    let h = harness();
    let (status, body) = call(&h.app, Req::get("/admin/admins")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total"], json!(2));
    let alice = item(&body, "alice");
    assert_eq!(alice["state"], json!("active"));
    assert!(alice["id"].as_str().is_some_and(|id| !id.is_empty()));
    assert!(alice["created_at"].as_str().is_some_and(|v| !v.is_empty()));
    assert_eq!(alice["last_login_at"], json!(null));
    let dumped = body.to_string();
    assert!(!dumped.contains("$argon2id$"), "{dumped}");
    assert!(!dumped.contains(ALICE_PASSWORD), "{dumped}");
}

#[tokio::test]
async fn login_is_required_for_account_management() {
    let h = harness();
    let (status, body) = call(&h.app, Req::get("/admin/admins").without_token()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], json!("MCP_UNAUTHORIZED"));
}

#[tokio::test]
async fn create_admin_hashes_password_and_audits_action() {
    let h = harness();
    let (status, body) = call(
        &h.app,
        Req::post("/admin/admins")
            .body(json!({"username": "Carol", "password": "carol-password-1"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let created = &body["admin"];
    assert_eq!(created["username"], json!("carol"), "用户名统一小写");
    assert_eq!(created["state"], json!("active"));
    assert!(created.get("password_hash").is_none(), "{created}");

    let stored = h
        .store
        .admin_user(created["id"].as_str().expect("id"))
        .expect("lookup")
        .expect("carol");
    assert!(stored.password_hash.starts_with("$argon2id$"));
    assert!(!stored.password_hash.contains("carol-password-1"));

    let events = h
        .audit
        .query(Some(&json!({"event_type": "admin_change"})))
        .expect("query");
    let created_events: Vec<_> = events
        .iter()
        .filter(|e| e.payload["action"] == json!("admin.create"))
        .collect();
    assert_eq!(created_events.len(), 1);
    let dumped = created_events[0].payload.to_string();
    assert!(!dumped.contains("carol-password-1"), "{dumped}");
    assert!(!dumped.contains("$argon2id$"), "{dumped}");
    assert_eq!(events[0].surface, "admin");
}

#[tokio::test]
async fn duplicate_username_is_409_case_insensitively() {
    let h = harness();
    let (status, body) = call(
        &h.app,
        Req::post("/admin/admins")
            .body(json!({"username": "ALICE", "password": "another-password-1"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], json!("MCP_ADMIN_USER_EXISTS"));
}

#[tokio::test]
async fn invalid_username_and_weak_password_are_400() {
    let h = harness();
    for body in [
        json!({"username": "ab", "password": "long-enough-password"}),
        json!({"username": "has space", "password": "long-enough-password"}),
        json!({"username": "carol", "password": "short"}),
    ] {
        let (status, resp) = call(&h.app, Req::post("/admin/admins").body(body.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body} → {resp}");
        assert_eq!(resp["error"]["code"], json!("MCP_VALIDATION_ERROR"));
    }
}

#[tokio::test]
async fn password_change_revokes_existing_sessions() {
    let h = harness();
    let (status, body) = call(
        &h.app,
        Req::patch(&format!("/admin/admins/{}", bob_id(&h)))
            .body(json!({"password": "bob-brand-new-pass"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["admin"]["username"], json!("bob"));
    // bob 的会话被吊销，alice 的不受影响。
    let (status, _) = call(&h.app, Req::get("/admin/session").with_token(BOB_TOKEN)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = call(&h.app, Req::get("/admin/session").with_token(ALICE_TOKEN)).await;
    assert_eq!(status, StatusCode::OK);
    // 新口令可用，旧口令不行。
    let (status, _) = login(&h.app, "bob", "bob-brand-new-pass").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = login(&h.app, "bob", BOB_PASSWORD).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn rename_conflict_is_409_and_self_rename_is_allowed() {
    let h = harness();
    let bob = bob_id(&h);
    let (status, body) = call(
        &h.app,
        Req::patch(&format!("/admin/admins/{bob}")).body(json!({"username": "alice"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], json!("MCP_ADMIN_USER_EXISTS"));

    let (status, body) = call(
        &h.app,
        Req::patch(&format!("/admin/admins/{bob}")).body(json!({"username": "robert"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["admin"]["username"], json!("robert"));
    // 改名不动会话：username 只是审计身份，账号主键才是会话来源。
    let (status, body) = call(&h.app, Req::get("/admin/session").with_token(BOB_TOKEN)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["actor"]["id"], json!("robert"));
}

#[tokio::test]
async fn empty_patch_is_400() {
    let h = harness();
    let (status, body) = call(
        &h.app,
        Req::patch(&format!("/admin/admins/{}", bob_id(&h))).body(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], json!("MCP_VALIDATION_ERROR"));
}

#[tokio::test]
async fn last_active_admin_cannot_be_disabled() {
    let h = harness();
    let (status, body) = call(
        &h.app,
        Req::patch(&format!("/admin/admins/{}", bob_id(&h))).body(json!({"state": "disabled"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // 现在只剩 alice 一名启用账号：再禁用/删除都必须 409。
    let alice = alice_id(&h);
    let (status, body) = call(
        &h.app,
        Req::patch(&format!("/admin/admins/{alice}")).body(json!({"state": "disabled"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], json!("MCP_ADMIN_LAST_ACTIVE"));
    // 删自己先撞 SELF_DELETE（更具体的错误优先于"会没有管理员"）。
    let (status, body) = call(&h.app, Req::delete(&format!("/admin/admins/{alice}"))).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], json!("MCP_ADMIN_SELF_DELETE"));
}

#[tokio::test]
async fn disabled_admin_cannot_log_in_and_loses_sessions() {
    let h = harness();
    let (status, _) = call(
        &h.app,
        Req::patch(&format!("/admin/admins/{}", bob_id(&h))).body(json!({"state": "disabled"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = login(&h.app, "bob", BOB_PASSWORD).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = call(&h.app, Req::get("/admin/session").with_token(BOB_TOKEN)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn delete_self_is_409_and_delete_other_revokes_sessions() {
    let h = harness();
    let alice = alice_id(&h);
    let (status, body) = call(&h.app, Req::delete(&format!("/admin/admins/{alice}"))).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], json!("MCP_ADMIN_SELF_DELETE"));

    let bob = bob_id(&h);
    let (status, _) = call(&h.app, Req::delete(&format!("/admin/admins/{bob}"))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(&h.app, Req::get("/admin/session").with_token(BOB_TOKEN)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "删账号必须连带吊销会话");

    let events = h
        .audit
        .query(Some(&json!({"event_type": "admin_change"})))
        .expect("query");
    assert!(
        events
            .iter()
            .any(|e| e.payload["action"] == json!("admin.delete")),
        "删除必须留痕"
    );
}

#[tokio::test]
async fn unknown_admin_is_404() {
    let h = harness();
    let (status, body) = call(
        &h.app,
        Req::patch("/admin/admins/nope").body(json!({"state": "disabled"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], json!("MCP_ADMIN_USER_NOT_FOUND"));
    let (status, body) = call(&h.app, Req::delete("/admin/admins/nope")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], json!("MCP_ADMIN_USER_NOT_FOUND"));
}

/// 真实口令握手：与 [`support::seed_admin_accounts`] 的固定会话不同，这里走完整登录链路。
async fn login(app: &axum::Router, username: &str, password: &str) -> (StatusCode, String) {
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
    let cookie = resp
        .headers()
        .get(axum::http::header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .split(';')
        .next()
        .unwrap_or_default()
        .to_string();
    (status, cookie)
}

fn alice_id(h: &Harness) -> String {
    h.store
        .admin_user_by_username("alice")
        .expect("lookup")
        .expect("alice")
        .id
}

fn bob_id(h: &Harness) -> String {
    h.store
        .admin_user_by_username("bob")
        .expect("lookup")
        .expect("bob")
        .id
}
