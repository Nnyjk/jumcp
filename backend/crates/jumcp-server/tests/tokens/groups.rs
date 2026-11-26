use axum::http::StatusCode;
use serde_json::json;

use crate::fixtures::*;

/// admin_* 的组判定：admin 组放行全部维护工具；header 收窄掉 admin 组后不可见不可调。
#[tokio::test]
async fn admin_tools_require_the_admin_group() {
    let h = harness();
    let mut admin_body = base_token_body();
    admin_body["name"] = json!("维护通道");
    admin_body["role"] = json!("admin");
    admin_body["groups"] = json!(["admin", "db.read.core"]);
    let (status, created) = create_token(&h.app, admin_body).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let plaintext = created["token"].as_str().unwrap().to_string();
    let names = tool_names(&h.app, &plaintext).await;
    assert!(
        names.contains(&"admin_review_queue".to_string()),
        "{names:?}"
    );
    assert!(
        names.contains(&"admin_audit_queries".to_string()),
        "admin 组放行全部维护工具: {names:?}"
    );
    assert!(
        !names.contains(&"list_groups".to_string()),
        "没给 basic 组就没有常驻元工具: {names:?}"
    );

    // admin 组在 scope 内 → 可调。
    let ok = mcp(
        &h.app,
        Some(&plaintext),
        rpc(
            "tools/call",
            json!({"name": "admin_review_queue", "arguments": {}}),
        ),
        &[],
    )
    .await;
    assert_eq!(ok["result"]["isError"], json!(false), "{ok}");

    // header 收窄掉 admin 组：即使 allowlist 命中也不可见、不可调。
    let narrowed = tool_names_with_groups(&h.app, &plaintext, "db.read.core").await;
    assert!(
        !narrowed.contains(&"admin_review_queue".to_string()),
        "{narrowed:?}"
    );
    let denied = mcp(
        &h.app,
        Some(&plaintext),
        rpc(
            "tools/call",
            json!({"name": "admin_review_queue", "arguments": {}}),
        ),
        &[("x-mcp-tool-groups", "db.read.core")],
    )
    .await;
    assert_eq!(err_code(&denied), "MCP_CAPABILITY_NOT_FOUND", "{denied}");

    // header 不能扩大 token 组：声明 analytics 越界。
    let escalated = mcp(
        &h.app,
        Some(&plaintext),
        rpc("tools/list", json!({})),
        &[("x-mcp-tool-groups", "db.read.analytics")],
    )
    .await;
    assert_eq!(
        escalated["error"]["data"]["mcp_code"],
        json!("MCP_GROUP_FORBIDDEN"),
        "{escalated}"
    );
}

/// 组闭环：GET /admin/groups 带 token_count；被 token 引用的组删除返回 409 且带两个计数。
#[tokio::test]
async fn group_token_count_and_delete_guard() {
    let h = harness();
    let (status, group) = call(
        &h.app,
        "POST",
        "/admin/groups",
        Some(ALICE_TOKEN),
        &[("x-admin-actor", "alice")],
        Some(
            json!({"code": "db.read.tmp", "name": "临时只读组", "priority": 5, "min_budget": 800, "roles": ["analyst"]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{group}");

    let mut body = base_token_body();
    body["groups"] = json!(["db.read.tmp"]);
    let (status, created) = create_token(&h.app, body).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");

    let (status, listed) = call(&h.app, "GET", "/admin/groups", Some(ALICE_TOKEN), &[], None).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let item = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["code"] == json!("db.read.tmp"))
        .expect("组必须出现");
    assert_eq!(item["token_count"], json!(1));

    let (status, blocked) = call(
        &h.app,
        "DELETE",
        "/admin/groups/db.read.tmp",
        Some(ALICE_TOKEN),
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{blocked}");
    assert_eq!(blocked["error"]["code"], json!("MCP_GROUP_IN_USE"));
    assert_eq!(blocked["error"]["data"]["capability_count"], json!(0));
    assert_eq!(blocked["error"]["data"]["token_count"], json!(1));

    // 撤销后引用归零，删除恢复可行。
    let id = created["item"]["id"].as_str().unwrap().to_string();
    let (status, _) = call(
        &h.app,
        "POST",
        &format!("/admin/tokens/{id}/revoke"),
        Some(ALICE_TOKEN),
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, deleted) = call(
        &h.app,
        "DELETE",
        "/admin/groups/db.read.tmp",
        Some(ALICE_TOKEN),
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{deleted}");
}

/// `initialize._meta.mcp.groups` 对 token scope 同样只能收窄，不能扩到 token 未授予的组。
#[tokio::test]
async fn meta_groups_can_only_narrow_token_scope() {
    let h = harness();
    publish_capability(&h, "cap.core", "db.read.core");
    publish_capability(&h, "cap.analytics", "db.read.analytics");
    let mut body = base_token_body();
    body["groups"] = json!(["db.read.core", "db.read.analytics"]);
    let (status, created) = create_token(&h.app, body).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let plaintext = created["token"].as_str().unwrap().to_string();

    let init = mcp(
        &h.app,
        Some(&plaintext),
        rpc(
            "initialize",
            json!({"_meta": {"mcp": {"groups": ["db.read.core"]}}}),
        ),
        &[],
    )
    .await;
    assert_eq!(init["error"], serde_json::Value::Null, "{init}");
    let names = tool_names(&h.app, &plaintext).await;
    assert!(names.contains(&"cap.core".to_string()), "{names:?}");
    assert!(
        !names.contains(&"cap.analytics".to_string()),
        "_meta 未声明的组不得出现: {names:?}"
    );

    // 越界 _meta：strict 直接拒绝（不会裁成 token 组）。
    let escalated = mcp(
        &h.app,
        Some(&plaintext),
        rpc(
            "initialize",
            json!({"_meta": {"mcp": {"groups": ["db.read.sensitive"]}}}),
        ),
        &[],
    )
    .await;
    assert_eq!(
        escalated["error"]["data"]["mcp_code"],
        json!("MCP_GROUP_FORBIDDEN"),
        "{escalated}"
    );
}
