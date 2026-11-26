use axum::http::StatusCode;
use jumcp_core::audit::{ActorRef, AuditEvent};
use serde_json::json;

use crate::fixtures::*;

/// 调用明细与审计过滤都要带出 token 来源：`actor.source/token_id` 可选，没有 token 归属的调用不匹配。
#[tokio::test]
async fn call_details_and_audit_filter_carry_token_origin() {
    let h = harness();
    // 先写一条没有 token_id/source 的 tool_call（非 token 发起）。
    let legacy = h
        .audit
        .append(AuditEvent {
            tenant: "default".into(),
            actor: ActorRef {
                actor_type: "agent".into(),
                id: "legacy-agent".into(),
                role: "analyst".into(),
                token_id: None,
                source: None,
            },
            event_type: "tool_call".into(),
            payload: json!({
                "object_type": "tool",
                "object_id": "admin_review_queue",
                "action": "invoke",
                "outcome": "ok",
                "data": {"arguments": {}}
            }),
            session_id: "default".into(),
            ts: String::new(),
            event_id: String::new(),
        })
        .expect("legacy event");

    let mut body = base_token_body();
    body["role"] = json!("admin");
    body["groups"] = json!(["admin"]);
    let (status, created) = create_token(&h.app, body).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let plaintext = created["token"].as_str().unwrap().to_string();
    let token_id = created["item"]["id"].as_str().unwrap().to_string();

    let call_resp = mcp(
        &h.app,
        Some(&plaintext),
        rpc(
            "tools/call",
            json!({"name": "admin_review_queue", "arguments": {}}),
        ),
        &[],
    )
    .await;
    assert_eq!(call_resp["result"]["isError"], json!(false), "{call_resp}");

    // token_id 过滤仍精确匹配该 token 的事件，没有 token 归属的事件不出现
    // （审计查询缺省全量，这里直接查审计存储，验证的是过滤语义）。
    let filtered = h
        .audit
        .query(Some(&json!({"token_id": token_id})))
        .expect("query");
    assert!(!filtered.is_empty(), "token 调用必须能被 token_id 过滤到");
    assert!(
        filtered
            .iter()
            .all(|e| e.actor["token_id"] == json!(token_id) && e.actor["source"] == json!("agent")),
        "{filtered:?}"
    );
    assert!(
        filtered.iter().all(|e| e.event_id != legacy.event_id),
        "旧事件（无 token_id）不得被 token_id 过滤命中"
    );
    let none = h
        .audit
        .query(Some(&json!({"token_id": "tok_does_not_exist"})))
        .expect("query");
    assert!(none.is_empty());

    // 这次工具调用属 MCP 面：显式 surface=admin 收窄后，它不该出现在平台操作列表里。
    let (status, platform) = call(
        &h.app,
        "GET",
        "/admin/audit-events?limit=200&surface=admin",
        Some(ALICE_TOKEN),
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{platform}");
    assert!(
        platform["events"]
            .as_array()
            .expect("events")
            .iter()
            .all(|e| e["event_type"] != json!("tool_call")),
        "{platform}"
    );

    // 工具调用明细：新调用的 actor 带 source/token_id，旧调用只有 id/role。
    let (status, calls) = call(
        &h.app,
        "GET",
        "/admin/tools/admin_review_queue/calls?window_days=1&limit=50",
        Some(ALICE_TOKEN),
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{calls}");
    let rows = calls["calls"].as_array().expect("calls");
    let new_call = rows
        .iter()
        .find(|c| c["actor"]["token_id"] == json!(token_id))
        .expect("新调用应带 token_id");
    assert_eq!(new_call["actor"]["source"], json!("agent"));
    assert_eq!(new_call["actor"]["id"], json!("agent-analyst"));
    let old_call = rows
        .iter()
        .find(|c| c["event_id"] == json!(legacy.event_id))
        .expect("旧调用仍在明细里");
    assert!(
        old_call["actor"].get("token_id").is_none(),
        "旧事件不得凭空出现 token_id: {old_call}"
    );
    assert!(old_call["actor"].get("source").is_none());
}
