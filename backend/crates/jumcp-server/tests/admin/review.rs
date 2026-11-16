use crate::support::*;
use axum::http::StatusCode;
use serde_json::{Value, json};

#[tokio::test]
async fn review_queue_lists_only_draft_and_review() {
    let h = harness();
    seed_capability(&h.store, "cap.draft");
    seed_capability(&h.store, "cap.review");
    h.store
        .submit_for_review("cap.review", "alice")
        .expect("submit");
    publish_capability(&h.store, "cap.published");
    h.store.deprecate("cap.published", "cap.next").expect("dep");

    let (status, body) = call(&h.app, Req::get("/admin/review-queue")).await;
    assert_eq!(status, StatusCode::OK);
    let ids: Vec<&str> = body["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|i| i["id"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&"cap.draft"));
    assert!(ids.contains(&"cap.review"));
    let statuses: Vec<&str> = body["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|i| i["status"].as_str().unwrap_or_default())
        .collect();
    assert!(statuses.contains(&"draft") && statuses.contains(&"review"));
    assert!(body["items"][0]["definition_hash"].as_str().is_some());
    assert!(body["items"][0]["last_changed_at"].as_str().is_some());
    assert!(body["items"][0].get("updated").is_none());
}

#[tokio::test]
async fn submit_moves_draft_to_review_and_audits() {
    let h = harness();
    seed_capability(&h.store, "cap.a");
    let (status, body) = call(
        &h.app,
        Req::post("/admin/capabilities/cap.a/submit").actor("alice"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], json!("review"));
    let changes = h
        .audit
        .query(Some(&json!({"event_type": "admin_change"})))
        .expect("query");
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].payload["object_type"], json!("capability"));
    assert_eq!(changes[0].payload["object_id"], json!("cap.a"));
    assert_eq!(changes[0].payload["action"], json!("submit"));
    assert_eq!(changes[0].payload["from_status"], json!("draft"));
    assert_eq!(changes[0].payload["to_status"], json!("review"));
    assert_eq!(changes[0].payload["approval"], Value::Null);
    assert_eq!(changes[0].payload["changes"][0]["field"], json!("status"));
    assert_eq!(changes[0].payload["data"]["submitted_by"], json!("alice"));
    assert_eq!(
        changes[0].payload["data"]["definition_hash_before"],
        changes[0].payload["data"]["definition_hash_after"],
        "状态变更前后定义哈希相等"
    );
}

#[tokio::test]
async fn submit_invalid_transition_is_409() {
    let h = harness();
    publish_capability(&h.store, "cap.done");
    let (status, body) = call(&h.app, Req::post("/admin/capabilities/cap.done/submit")).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], json!("MCP_STATE_TRANSITION_INVALID"));
    let decisions = h
        .audit
        .query(Some(&json!({"event_type": "policy_decision"})))
        .expect("query");
    assert!(
        decisions
            .iter()
            .any(|e| e.payload["error_code"] == json!("MCP_STATE_TRANSITION_INVALID"))
    );
}

#[tokio::test]
async fn reject_moves_review_to_draft_and_audits() {
    let h = harness();
    seed_capability(&h.store, "cap.reject");
    h.store
        .submit_for_review("cap.reject", "alice")
        .expect("submit");
    let (status, body) = call(
        &h.app,
        Req::post("/admin/capabilities/cap.reject/reject")
            .actor("alice")
            .body(json!({"reason": "  字段命名需统一  "})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], json!("cap.reject"));
    assert_eq!(body["status"], json!("draft"));
    assert_eq!(body["reason"], json!("字段命名需统一"));
    assert!(body["last_changed_at"].as_str().is_some());
    assert!(body.get("updated").is_none());

    let rec = h.store.get("cap.reject").expect("get").expect("row");
    assert_eq!(rec.status, "draft");
    assert_eq!(rec.submitted_by, "", "退回后重新提审时重新记录提审人");

    let changes = h
        .audit
        .query(Some(&json!({"event_type": "admin_change"})))
        .expect("query");
    let rejection = changes
        .iter()
        .find(|e| e.payload["action"] == json!("reject"))
        .expect("reject 的 admin_change");
    assert_eq!(rejection.payload["object_type"], json!("capability"));
    assert_eq!(rejection.payload["object_id"], json!("cap.reject"));
    assert_eq!(rejection.payload["from_status"], json!("review"));
    assert_eq!(rejection.payload["to_status"], json!("draft"));
    assert_eq!(rejection.payload["reason"], json!("字段命名需统一"));
    assert_eq!(rejection.payload["approval"], Value::Null);
    assert_eq!(rejection.actor["id"], json!("alice"));

    // 退回后可重新提审：draft → review 仍然成立。
    h.store
        .submit_for_review("cap.reject", "alice")
        .expect("re-submit");
}

#[tokio::test]
async fn reject_rejects_invalid_state_reason_and_unknown_id() {
    let h = harness();

    // draft → 409，状态不变。
    seed_capability(&h.store, "cap.draft");
    let (status, body) = call(
        &h.app,
        Req::post("/admin/capabilities/cap.draft/reject").body(json!({"reason": "原因"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], json!("MCP_STATE_TRANSITION_INVALID"));
    assert_eq!(body["error"]["data"]["from"], json!("draft"));
    assert_eq!(body["error"]["data"]["to"], json!("draft"));
    assert_eq!(
        h.store.get("cap.draft").expect("get").expect("row").status,
        "draft"
    );

    // published → 409，状态不变。
    publish_capability(&h.store, "cap.pub");
    let (status, body) = call(
        &h.app,
        Req::post("/admin/capabilities/cap.pub/reject").body(json!({"reason": "原因"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], json!("MCP_STATE_TRANSITION_INVALID"));
    assert_eq!(
        h.store.get("cap.pub").expect("get").expect("row").status,
        "published"
    );

    // builtin：ensure_mutable 先于取行判定，409 且不落库。
    let (status, body) = call(
        &h.app,
        Req::post("/admin/capabilities/search_capabilities/reject").body(json!({"reason": "原因"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], json!("MCP_BUILTIN_IMMUTABLE"));
    assert!(
        h.store.get("search_capabilities").expect("get").is_none(),
        "builtin 不得被写入注册表"
    );

    // 空/纯空格 reason → 400，且仍 review。
    seed_capability(&h.store, "cap.empty");
    h.store
        .submit_for_review("cap.empty", "alice")
        .expect("submit");
    for reason in [json!(""), json!("   ")] {
        let (status, body) = call(
            &h.app,
            Req::post("/admin/capabilities/cap.empty/reject").body(json!({"reason": reason})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], json!("MCP_VALIDATION_ERROR"));
        assert_eq!(
            h.store.get("cap.empty").expect("get").expect("row").status,
            "review",
            "校验失败不得改状态"
        );
    }
    // 缺 reason 字段同样 400。
    let (status, body) = call(
        &h.app,
        Req::post("/admin/capabilities/cap.empty/reject").body(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], json!("MCP_VALIDATION_ERROR"));

    // 超长 reason（>500 字符）→ 400，data 带 max。
    let long = "字".repeat(501);
    let (status, body) = call(
        &h.app,
        Req::post("/admin/capabilities/cap.empty/reject").body(json!({"reason": long})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], json!("MCP_VALIDATION_ERROR"));
    assert_eq!(body["error"]["data"]["max"], json!(500));
    assert_eq!(
        h.store.get("cap.empty").expect("get").expect("row").status,
        "review"
    );

    // 未知 id → 404。
    let (status, body) = call(
        &h.app,
        Req::post("/admin/capabilities/cap.nope/reject").body(json!({"reason": "原因"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], json!("MCP_CAPABILITY_NOT_FOUND"));
}

#[tokio::test]
async fn publish_by_same_person_succeeds() {
    let h = harness();
    seed_capability(&h.store, "cap.b");
    h.store.submit_for_review("cap.b", "alice").expect("submit");
    let (status, body) = call(
        &h.app,
        Req::post("/admin/capabilities/cap.b/publish")
            .actor("alice")
            .body(json!({"approval_id": "ap-9"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], json!("published"));
    let changes = h
        .audit
        .query(Some(&json!({"event_type": "admin_change"})))
        .expect("query");
    let publish = changes
        .iter()
        .find(|e| e.payload["action"] == json!("publish"))
        .expect("publish 事件");
    assert_eq!(publish.payload["approval"]["submitted_by"], json!("alice"));
    assert_eq!(publish.payload["approval"]["approved_by"], json!("alice"));
    assert_eq!(publish.actor["id"], json!("alice"));
}

#[tokio::test]
async fn publish_by_other_person_succeeds_with_admin_change() {
    let h = harness();
    seed_capability(&h.store, "cap.c");
    h.store.submit_for_review("cap.c", "alice").expect("submit");
    // 提审人 alice（记录在案），批准人 bob（用自己的 token 调用）。
    let (status, body) = call(
        &h.app,
        Req::post("/admin/capabilities/cap.c/publish")
            .with_token(BOB_TOKEN)
            .actor("bob")
            .body(json!({"approval_id": "ap-9"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], json!("published"));
    let changes = h
        .audit
        .query(Some(&json!({"event_type": "admin_change"})))
        .expect("query");
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].payload["action"], json!("publish"));
    assert_eq!(changes[0].payload["object_type"], json!("capability"));
    assert_eq!(changes[0].payload["object_id"], json!("cap.c"));
    assert_eq!(changes[0].payload["from_status"], json!("review"));
    assert_eq!(changes[0].payload["to_status"], json!("published"));
    assert_eq!(changes[0].payload["approval"]["id"], json!("ap-9"));
    assert_eq!(
        changes[0].payload["approval"]["submitted_by"],
        json!("alice")
    );
    assert_eq!(changes[0].payload["approval"]["approved_by"], json!("bob"));
    assert_eq!(changes[0].payload["approval"]["verified"], json!(false));
    assert_eq!(
        changes[0].payload["data"]["definition_hash_before"],
        changes[0].payload["data"]["definition_hash_after"],
        "发布不改变定义，前后哈希相等"
    );
}

#[tokio::test]
async fn visibility_updates_flags_and_writes_admin_change() {
    let h = harness();
    publish_capability(&h.store, "cap.d");
    let (status, body) = call(
        &h.app,
        Req::post("/admin/capabilities/cap.d/visibility")
            .body(json!({"discoverable": true, "callable": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["discoverable"], json!(true));
    assert_eq!(body["callable"], json!(true));
    assert!(body["last_changed_at"].as_str().is_some());
    let changes = h
        .audit
        .query(Some(&json!({"event_type": "admin_change"})))
        .expect("query");
    let vis = changes
        .iter()
        .find(|e| e.payload["action"] == json!("visibility"))
        .expect("visibility admin_change");
    assert_eq!(vis.payload["object_type"], json!("capability"));
    assert_eq!(vis.payload["object_id"], json!("cap.d"));
    assert_eq!(vis.payload["approval"], Value::Null);
    let fields: Vec<&str> = vis.payload["changes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["field"].as_str())
        .collect();
    assert!(fields.contains(&"discoverable") && fields.contains(&"callable"));
    assert_eq!(vis.payload["changes"][0]["before"], json!(false));
    assert_eq!(vis.payload["changes"][0]["after"], json!(true));
    assert_eq!(
        vis.payload["data"]["definition_hash_before"],
        vis.payload["data"]["definition_hash_after"]
    );
}

#[tokio::test]
async fn mismatched_actor_header_is_400_with_both_ids() {
    let h = harness();
    seed_capability(&h.store, "cap.e");
    let (status, body) = call(
        &h.app,
        Req::post("/admin/capabilities/cap.e/submit").actor("bob"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], json!("MCP_BAD_REQUEST"));
    assert_eq!(body["error"]["data"]["derived"], json!("alice"));
    assert_eq!(body["error"]["data"]["declared"], json!("bob"));
}

#[tokio::test]
async fn missing_actor_header_is_allowed_and_uses_derived_id() {
    let h = harness();
    seed_capability(&h.store, "cap.f");
    let mut req = Req::post("/admin/capabilities/cap.f/submit");
    req.actor = None;
    let (status, body) = call(&h.app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], json!("review"));
    // 审计主体来自 token 映射，而不是请求头（缺失时也不例外）。
    let changes = h
        .audit
        .query(Some(&json!({"event_type": "admin_change"})))
        .expect("query");
    assert_eq!(changes[0].actor["id"], json!("alice"));
}
