//! 令牌授予面：`/admin/tokens/options` 只回角色与组。
//!
//! 工具粒度不再挂在令牌上：可见性由「组内两层暴露」决定（见 `exposure.rs`），
//! 令牌的 `groups` 是唯一硬上限。

use crate::fixtures::*;
use axum::http::StatusCode;
use serde_json::{Value, json};

#[tokio::test]
async fn options_returns_roles_and_groups_only() {
    let h = harness();
    let (status, body) = call(
        &h.app,
        "GET",
        "/admin/tokens/options",
        Some(ALICE_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let mut keys: Vec<&str> = body
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort();
    assert_eq!(keys, vec!["groups", "roles"], "{body}");

    let group_of = |code: &str| -> Value {
        body["groups"]
            .as_array()
            .expect("groups")
            .iter()
            .find(|g| g["code"] == json!(code))
            .cloned()
            .unwrap_or_else(|| panic!("缺少组 {code}: {body}"))
    };
    let basic = group_of("basic");
    assert_eq!(basic["source"], json!("builtin"));
    assert_eq!(basic["name"], json!("基础工具"));
    let core = group_of("db.read.core");
    assert_eq!(core["name"], json!("核心数据只读"));
    assert_eq!(core["min_budget"], json!(800));
    assert_eq!(core["token_count"], json!(0));

    // 角色仍然带默认组与预算，签发时可以直接照抄。
    let analyst = body["roles"]
        .as_array()
        .expect("roles")
        .iter()
        .find(|r| r["code"] == json!("analyst"))
        .expect("analyst");
    assert_eq!(
        analyst["default_groups"],
        json!(["db.read.core", "db.read.analytics"])
    );
}

#[tokio::test]
async fn token_groups_must_come_from_the_policy() {
    let h = harness();
    let (status, body) = call(
        &h.app,
        "POST",
        "/admin/tokens",
        Some(ALICE_TOKEN),
        Some(json!({
            "name": "bad group",
            "role": "analyst",
            "groups": ["db.read.core", "no.such.group"],
            "source": "agent",
            "caller": "agent-bad-group"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], json!("MCP_VALIDATION_ERROR"));
    assert_eq!(body["error"]["data"]["unknown"], json!(["no.such.group"]));

    // basic 现在是普通组：可以直接写进令牌。
    let (status, body) = call(
        &h.app,
        "POST",
        "/admin/tokens",
        Some(ALICE_TOKEN),
        Some(json!({
            "name": "basic only",
            "role": "analyst",
            "groups": ["basic"],
            "source": "agent",
            "caller": "agent-basic-only"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let token = body["token"].as_str().expect("token");
    let names = tool_names(&h.app, token).await;
    assert!(
        names.contains(&"search_capabilities".to_string()),
        "{names:?}"
    );
    assert!(
        names.iter().all(|n| !n.contains("__")),
        "basic 组不带任何上游工具: {names:?}"
    );
}
