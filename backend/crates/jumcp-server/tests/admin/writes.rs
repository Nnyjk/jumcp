use crate::fixtures::template_definition;
use crate::support::*;
use axum::http::StatusCode;
use jumcp_core::entry::EffectiveScope;
use jumcp_core::registry::CapabilityStore;
use jumcp_core::template::TemplateRegistry;
use jumcp_server::admin::{AdminState, router};
use jumcp_server::service::Service;
use serde_json::{Value, json};
use std::sync::Arc;

#[tokio::test]
async fn create_capability_writes_draft_and_admin_change() {
    let h = harness();
    let (status, body) = call(
        &h.app,
        Req::post("/admin/capabilities")
            .body(json!({"definition": template_definition("order.new_template")})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["id"], json!("order.new_template"));
    assert_eq!(body["status"], json!("draft"));
    assert!(body["version"].as_i64().is_some());
    let rec = h
        .store
        .get("order.new_template")
        .expect("get")
        .expect("draft 已落库");
    assert_eq!(rec.status, "draft");
    assert_eq!(body["definition_hash"], json!(rec.definition_hash));

    let changes = h
        .audit
        .query(Some(&json!({"event_type": "admin_change"})))
        .expect("query");
    let created = changes
        .iter()
        .find(|e| e.payload["action"] == json!("create"))
        .expect("create 的 admin_change");
    assert_eq!(created.payload["object_type"], json!("capability"));
    assert_eq!(created.payload["object_id"], json!("order.new_template"));
    assert_eq!(created.payload["to_status"], json!("draft"));
    assert_eq!(
        created.payload["data"]["definition_hash_before"],
        Value::Null
    );
    assert_eq!(
        created.payload["data"]["definition_hash_after"],
        json!(rec.definition_hash)
    );
    assert!(
        created.payload["changes"]
            .as_array()
            .is_some_and(|c| !c.is_empty())
    );
    let created_fields: Vec<&str> = created.payload["changes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["field"].as_str())
        .collect();
    assert!(
        !created_fields.contains(&"template"),
        "create 也必须展开嵌套对象: {created_fields:?}"
    );
    assert!(
        created_fields.iter().any(|f| f.starts_with("template.")),
        "应出现 template.* 点路径: {created_fields:?}"
    );
    assert_eq!(created.actor["id"], json!("alice"));

    // builtin id：409，且不落库。
    let (status, body) = call(
        &h.app,
        Req::post("/admin/capabilities")
            .body(json!({"definition": template_definition("search_capabilities")})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], json!("MCP_BUILTIN_IMMUTABLE"));
    assert!(h.store.get("search_capabilities").expect("get").is_none());
}

#[tokio::test]
async fn deprecate_capability_updates_status_and_writes_admin_change() {
    let h = harness();
    publish_capability(&h.store, "cap.old");
    seed_capability(&h.store, "cap.new");
    let (status, body) = call(
        &h.app,
        Req::post("/admin/capabilities/cap.old/deprecate").body(json!({"successor_id": "cap.new"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], json!("deprecated"));
    assert_eq!(body["successor_id"], json!("cap.new"));
    let rec = h.store.get("cap.old").expect("get").expect("row");
    assert_eq!(rec.status, "deprecated");
    assert_eq!(rec.successor_id, "cap.new");
    assert!(!rec.discoverable && !rec.callable);

    let changes = h
        .audit
        .query(Some(&json!({"event_type": "admin_change"})))
        .expect("query");
    let dep = changes
        .iter()
        .find(|e| e.payload["action"] == json!("deprecate"))
        .expect("deprecate 的 admin_change");
    assert_eq!(dep.payload["object_id"], json!("cap.old"));
    assert_eq!(dep.payload["from_status"], json!("published"));
    assert_eq!(dep.payload["to_status"], json!("deprecated"));
    let fields: Vec<&str> = dep.payload["changes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["field"].as_str())
        .collect();
    assert!(fields.contains(&"status"));
    assert!(fields.contains(&"successor_id"));
    // 已发布能力默认不可见不可调：deprecate 不制造不存在的字段变更。
    assert!(!fields.contains(&"discoverable"));
    assert!(!fields.contains(&"callable"));
    assert_eq!(
        dep.payload["data"]["definition_hash_before"],
        dep.payload["data"]["definition_hash_after"]
    );

    // builtin → 409；空 successor → 400（store 是唯一判定点）。
    let (status, body) = call(
        &h.app,
        Req::post("/admin/capabilities/admin_set_visibility/deprecate")
            .body(json!({"successor_id": "cap.new"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], json!("MCP_BUILTIN_IMMUTABLE"));

    let (status, body) = call(
        &h.app,
        Req::post("/admin/capabilities/cap.new/deprecate").body(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], json!("MCP_VALIDATION_ERROR"));
    assert_eq!(
        h.store.get("cap.new").expect("get").expect("row").status,
        "draft"
    );
}

/// 下架的四条拒绝路径：successor 缺失/自指/重复下架/未知目标都必须显式失败且不改库。
#[tokio::test]
async fn deprecate_rejects_bad_successor_and_repeat() {
    let h = harness();
    publish_capability(&h.store, "cap.old");
    seed_capability(&h.store, "cap.new");

    // successor 不存在：404 + 把不存在的 id 指出来，目标保持 published。
    let (status, body) = call(
        &h.app,
        Req::post("/admin/capabilities/cap.old/deprecate")
            .body(json!({"successor_id": "cap.missing"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], json!("MCP_CAPABILITY_NOT_FOUND"));
    assert_eq!(body["error"]["data"]["capability_id"], json!("cap.missing"));
    let rec = h.store.get("cap.old").expect("get").expect("row");
    assert_eq!(rec.status, "published");
    assert_eq!(rec.successor_id, "");

    // successor 自指：400，迁移指引不能指向自己。
    let (status, body) = call(
        &h.app,
        Req::post("/admin/capabilities/cap.old/deprecate").body(json!({"successor_id": "cap.old"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], json!("MCP_VALIDATION_ERROR"));

    // 未知目标：404，不是静默成功。
    let (status, body) = call(
        &h.app,
        Req::post("/admin/capabilities/cap.ghost/deprecate")
            .body(json!({"successor_id": "cap.new"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], json!("MCP_CAPABILITY_NOT_FOUND"));
    assert_eq!(body["error"]["data"]["capability_id"], json!("cap.ghost"));

    // 正常下架。
    let (status, body) = call(
        &h.app,
        Req::post("/admin/capabilities/cap.old/deprecate").body(json!({"successor_id": "cap.new"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["successor_id"], json!("cap.new"));

    // 重复下架：409 且不覆盖既有 successor、不再写第二条 admin_change。
    let before = h
        .audit
        .query(Some(&json!({"event_type": "admin_change"})))
        .expect("query")
        .len();
    let (status, body) = call(
        &h.app,
        Req::post("/admin/capabilities/cap.old/deprecate")
            .body(json!({"successor_id": "cap.other"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], json!("MCP_STATE_TRANSITION_INVALID"));
    assert_eq!(body["error"]["data"]["from"], json!("deprecated"));
    assert_eq!(
        h.store
            .get("cap.old")
            .expect("get")
            .expect("row")
            .successor_id,
        "cap.new"
    );
    let after = h
        .audit
        .query(Some(&json!({"event_type": "admin_change"})))
        .expect("query")
        .len();
    assert_eq!(before, after, "被拒的下架不得写审计");
}

#[tokio::test]
async fn write_endpoints_refuse_when_audit_disabled() {
    let store = Arc::new(CapabilityStore::open(":memory:", None).expect("store"));
    store
        .register("cap.keep", "tool", "db.read.core", "read", json!({}))
        .expect("register");
    let registry = Arc::new(TemplateRegistry::new(store.clone()));
    let service = Arc::new(
        Service::new(settings())
            .with_templates(registry)
            .with_scope(EffectiveScope {
                groups: vec!["db.read.core".into()],
                budget: 2000,
                degraded: false,
                source: "header".into(),
                mode: "strict".into(),
                allowed_builtin_tools: None,
            }),
    );
    let app = router(AdminState {
        service,
        store: store.clone(),
        tokens: settings().admin_tokens,
        transport_key: Some(crate::support::transport_key()),
    });

    let (status, body) = call(
        &app,
        Req::post("/admin/capabilities")
            .body(json!({"definition": template_definition("order.blocked")})),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["error"]["code"], json!("MCP_AUDIT_ERROR"));
    assert!(
        store.get("order.blocked").expect("get").is_none(),
        "审计未启用不得落库"
    );

    let (status, body) = call(
        &app,
        Req::post("/admin/capabilities/cap.keep/deprecate")
            .body(json!({"successor_id": "cap.next"})),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["error"]["code"], json!("MCP_AUDIT_ERROR"));
    assert_eq!(
        store.get("cap.keep").expect("get").expect("row").status,
        "draft",
        "审计未启用不得改状态"
    );

    // submit：状态不得推进。
    store
        .register("cap.submit", "tool", "db.read.core", "read", json!({}))
        .expect("register");
    let (status, body) = call(&app, Req::post("/admin/capabilities/cap.submit/submit")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["error"]["code"], json!("MCP_AUDIT_ERROR"));
    assert_eq!(
        store.get("cap.submit").expect("get").expect("row").status,
        "draft"
    );

    // publish：review 也不得被推进（否则就是"无痕成功"）。
    store
        .register("cap.review", "tool", "db.read.core", "read", json!({}))
        .expect("register");
    store
        .submit_for_review("cap.review", "carol")
        .expect("submit");
    let (status, body) = call(
        &app,
        Req::post("/admin/capabilities/cap.review/publish").body(json!({"approval_id": "ap-1"})),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["error"]["code"], json!("MCP_AUDIT_ERROR"));
    assert_eq!(
        store.get("cap.review").expect("get").expect("row").status,
        "review"
    );

    // reject：review 不得被退回（否则就是"无痕成功"）。
    let (status, body) = call(
        &app,
        Req::post("/admin/capabilities/cap.review/reject").body(json!({"reason": "无审计"})),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["error"]["code"], json!("MCP_AUDIT_ERROR"));
    assert_eq!(
        store.get("cap.review").expect("get").expect("row").status,
        "review"
    );

    // visibility：双布尔保持原值。
    let (status, body) = call(
        &app,
        Req::post("/admin/capabilities/cap.keep/visibility")
            .body(json!({"discoverable": true, "callable": true})),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["error"]["code"], json!("MCP_AUDIT_ERROR"));
    let rec = store.get("cap.keep").expect("get").expect("row");
    assert!(!rec.discoverable && !rec.callable, "审计未启用不得改可见性");
}

#[tokio::test]
async fn visibility_writes_only_real_changes_without_approval() {
    let h = harness();
    publish_capability(&h.store, "cap.vis");
    let count_changes = || {
        h.audit
            .query(Some(&json!({"event_type": "admin_change"})))
            .expect("query")
            .len()
    };
    let before_count = count_changes();

    // 无实际变化：不落库、不写事件。
    let (status, body) = call(
        &h.app,
        Req::post("/admin/capabilities/cap.vis/visibility")
            .body(json!({"discoverable": false, "callable": false})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(count_changes(), before_count, "无变化不得写 admin_change");

    let (status, body) = call(
        &h.app,
        Req::post("/admin/capabilities/cap.vis/visibility")
            .body(json!({"discoverable": true, "callable": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["discoverable"], json!(true));
    assert_eq!(body["callable"], json!(true));
    let changes = h
        .audit
        .query(Some(&json!({"event_type": "admin_change"})))
        .expect("query");
    let vis = changes
        .iter()
        .find(|e| e.payload["action"] == json!("visibility"))
        .expect("真实变化必须记 admin_change");
    assert_eq!(vis.payload["object_id"], json!("cap.vis"));
    assert_eq!(vis.payload["approval"], Value::Null);
    assert!(
        vis.payload.get("approval_id").is_none(),
        "可见性不再生成 approval_id"
    );
    let fields: Vec<&str> = vis.payload["changes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["field"].as_str())
        .collect();
    assert_eq!(fields, vec!["discoverable", "callable"]);
}
