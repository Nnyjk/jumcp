use crate::support::*;
use axum::http::StatusCode;
use jumcp_core::registry::BUILTIN_CAPABILITY_IDS;
use serde_json::{Value, json};

#[tokio::test]
async fn capabilities_list_requires_token() {
    let h = harness();
    let (status, body) = call(&h.app, Req::get("/admin/capabilities").without_token()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], json!("MCP_UNAUTHORIZED"));
}

#[tokio::test]
async fn capabilities_list_defaults_to_fifty_and_hides_definition() {
    let h = harness();
    seed_with(&h.store, "cap.one", "db.read.core", "第一个能力");
    publish_capability(&h.store, "cap.two");
    let (status, body) = call(&h.app, Req::get("/admin/capabilities")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["limit"], json!(50));
    assert_eq!(body["offset"], json!(0));
    assert_eq!(body["total"], json!(2));
    let item = &body["items"][0];
    assert!(
        item.get("definition").is_none(),
        "列表页不回传完整 definition"
    );
    for key in [
        "id",
        "kind",
        "group",
        "risk",
        "status",
        "discoverable",
        "callable",
        "version",
        "definition_hash",
        "successor_id",
        "last_changed_at",
        "submitted_by",
        "submitted_at",
    ] {
        assert!(item.get(key).is_some(), "缺少字段 {key}");
    }
    assert!(item.get("updated").is_none(), "旧字段 updated 必须删除");
}

#[tokio::test]
async fn capabilities_list_filters_by_status_list() {
    let h = harness();
    seed_with(&h.store, "cap.draft", "db.read.core", "草稿");
    publish_capability(&h.store, "cap.pub");
    publish_capability(&h.store, "cap.dep");
    h.store.deprecate("cap.dep", "cap.pub").expect("deprecate");

    let (status, body) = call(
        &h.app,
        Req::get("/admin/capabilities?status=published,deprecated"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let ids: Vec<&str> = body["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|i| i["id"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&"cap.pub"));
    assert!(ids.contains(&"cap.dep"));
    assert!(!ids.contains(&"cap.draft"));
}

#[tokio::test]
async fn capabilities_list_filters_by_group_and_query() {
    let h = harness();
    seed_with(&h.store, "order.daily", "db.read.core", "订单日汇总");
    seed_with(
        &h.store,
        "Order.Realtime",
        "db.read.analytics",
        "real-time orders",
    );
    seed_with(&h.store, "kb.search", "kb.search", "知识库检索");

    let (status, body) = call(&h.app, Req::get("/admin/capabilities?group=db.read.core")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(1));
    assert_eq!(body["items"][0]["id"], json!("order.daily"));

    // q 大小写不敏感，匹配 id 或 description。
    let (status, body) = call(&h.app, Req::get("/admin/capabilities?q=order")).await;
    assert_eq!(status, StatusCode::OK);
    let ids: Vec<&str> = body["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|i| i["id"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&"order.daily"));
    assert!(ids.contains(&"Order.Realtime"));

    let (status, body) = call(
        &h.app,
        Req::get("/admin/capabilities?q=%E7%9F%A5%E8%AF%86%E5%BA%93"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(1));
}

#[tokio::test]
async fn capabilities_list_clamps_limit_and_rejects_bad_status() {
    let h = harness();
    seed_with(&h.store, "cap.a", "db.read.core", "a");
    let (status, body) = call(&h.app, Req::get("/admin/capabilities?limit=9999")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["limit"], json!(200));

    let (status, body) = call(&h.app, Req::get("/admin/capabilities?status=bogus")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], json!("MCP_BAD_REQUEST"));
    let decisions = h
        .audit
        .query(Some(&json!({"event_type": "policy_decision"})))
        .expect("query");
    assert!(
        decisions
            .iter()
            .any(|e| e.payload["error_code"] == json!("MCP_BAD_REQUEST"))
    );
}

/// 有效的模板定义（Postgres 方言由 `TemplateRegistry::new` 决定）。
fn daily_template_definition() -> Value {
    json!({
        "template_id": "order.daily_summary",
        "group": "db.read.core",
        "risk": "read",
        "description": "按日汇总订单",
        "sql": "SELECT day FROM public.orders WHERE day >= :day",
        "target": {"upstream": "ch", "tool": "query"},
        "params": [{"name":"day","type":"date","required":true,"nullable":false}],
        "constraints": {"allowed_schemas": ["public"]}
    })
}

#[tokio::test]
async fn capability_detail_returns_definition_and_is_read_only() {
    let h = harness();
    seed_capability(&h.store, "cap.detail");
    // 先产生一条审计，用于验证详情接口不新增记录。
    call(&h.app, Req::post("/admin/capabilities/cap.detail/submit")).await;
    let before = h.audit.query(None).expect("query").len();

    let (status, body) = call(&h.app, Req::get("/admin/capabilities/cap.detail")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], json!("cap.detail"));
    assert_eq!(body["status"], json!("review"));
    assert_eq!(body["version"], json!(1));
    assert_eq!(body["definition"]["description"], json!("cap.detail"));
    for key in [
        "kind",
        "group",
        "risk",
        "discoverable",
        "callable",
        "definition_hash",
        "successor_id",
        "last_changed_at",
        "submitted_by",
        "submitted_at",
    ] {
        assert!(body.get(key).is_some(), "详情缺字段 {key}");
    }
    assert!(body.get("definition_updated_at").is_some());
    assert!(body.get("updated").is_none(), "旧字段 updated 必须删除");
    assert_eq!(
        h.audit.query(None).expect("query").len(),
        before,
        "只读详情不得写审计"
    );

    let (status, body) = call(&h.app, Req::get("/admin/capabilities/cap.missing")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], json!("MCP_CAPABILITY_NOT_FOUND"));
}

#[tokio::test]
async fn draft_definition_edit_bumps_version_and_audits() {
    let h = harness();
    seed_capability(&h.store, "cap.draft");
    let (status, body) = call(
        &h.app,
        Req::put("/admin/capabilities/cap.draft/definition").body(json!({
            "definition": {"description": "v2", "params_schema": {"type":"object","properties":{}}}
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], json!("draft"));
    assert_eq!(body["version"], json!(2));
    assert!(body["definition_hash"].as_str().is_some());
    assert!(body["last_changed_at"].as_str().is_some());
    assert_eq!(body["discoverable"], json!(false));
    assert_eq!(body["callable"], json!(false));
    assert_eq!(
        h.store
            .get_version("cap.draft", 1)
            .expect("version")
            .expect("row")
            .definition["description"],
        json!("cap.draft"),
        "旧版本必须保留"
    );

    let changes = h
        .audit
        .query(Some(&json!({"event_type": "admin_change"})))
        .expect("query");
    let last = changes.last().expect("admin_change");
    assert_eq!(last.payload["object_type"], json!("capability"));
    assert_eq!(last.payload["object_id"], json!("cap.draft"));
    assert_eq!(last.payload["action"], json!("update_definition"));
    assert_eq!(last.payload["from_status"], json!("draft"));
    assert_eq!(last.payload["to_status"], json!("draft"));
    assert_eq!(last.payload["approval"], Value::Null);
    assert_ne!(
        last.payload["data"]["definition_hash_before"],
        last.payload["data"]["definition_hash_after"]
    );
    assert!(
        last.payload["changes"]
            .as_array()
            .is_some_and(|c| c.iter().any(|x| x["field"] == json!("description")))
    );

    // definition 不是对象 → 400。
    let (status, body) = call(
        &h.app,
        Req::put("/admin/capabilities/cap.draft/definition").body(json!({"definition": "x"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], json!("MCP_VALIDATION_ERROR"));

    // 显式 approval_id 作为外部票号写进版本行；票号不由服务生成。
    let (status, _) = call(
        &h.app,
        Req::put("/admin/capabilities/cap.draft/definition").body(json!({
            "definition": {"description": "v3"},
            "approval_id": "TICKET-42"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let versions = h.store.versions("cap.draft").expect("versions");
    assert_eq!(versions.len(), 3);
    assert_eq!(versions.last().unwrap().approval_id, "TICKET-42");
    assert_eq!(versions.last().unwrap().status, "draft");
    assert!(!versions.last().unwrap().created_at.is_empty());
}

#[tokio::test]
async fn published_definition_edit_allows_self_approval() {
    let h = harness();
    publish_capability(&h.store, "cap.pub");
    // publish_capability: submit alice → publish bob。

    let body = json!({"definition": {"description": "v2"}});
    let (status, resp) = call(
        &h.app,
        Req::put("/admin/capabilities/cap.pub/definition").body(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(resp["error"]["code"], json!("MCP_APPROVAL_REQUIRED"));

    // 提审人 alice 自己批准：允许，版本 +1，状态保持 published。
    let (status, resp) = call(
        &h.app,
        Req::put("/admin/capabilities/cap.pub/definition").body(json!({
            "definition": {"description": "v2"},
            "approval_id": "ap-2"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(resp["status"], json!("published"));
    assert_eq!(resp["version"], json!(2));
    assert_eq!(resp["discoverable"], json!(false));
    assert_eq!(resp["callable"], json!(false));
    assert!(resp["last_changed_at"].as_str().is_some());
    let rec = h.store.get("cap.pub").expect("get").expect("row");
    assert_eq!(rec.status, "published");
    assert_eq!(rec.definition["description"], json!("v2"));
    // 版本行盖章：提审人 / 批准人 / 外部票号均为事实记录。
    let versions = h.store.versions("cap.pub").expect("versions");
    let v2 = versions.last().expect("v2");
    assert_eq!(v2.version, 2);
    assert_eq!(v2.status, "published");
    assert_eq!(v2.submitted_by, "alice");
    assert_eq!(v2.approved_by, "alice");
    assert_eq!(v2.approval_id, "ap-2");
    assert!(!v2.approved_at.is_empty());
    // 审批审计对象是外部票号 + token 派生批准人。
    let changes = h
        .audit
        .query(Some(&json!({"event_type": "admin_change"})))
        .expect("query");
    let last = changes.last().expect("admin_change");
    assert_eq!(last.payload["action"], json!("update_definition"));
    assert_eq!(last.payload["approval"]["id"], json!("ap-2"));
    assert_eq!(last.payload["approval"]["submitted_by"], json!("alice"));
    assert_eq!(last.payload["approval"]["approved_by"], json!("alice"));
}
#[tokio::test]
async fn definition_edit_rejects_review_deprecated_and_builtin() {
    let h = harness();
    // review：必须先退回 draft。
    seed_capability(&h.store, "cap.review");
    h.store
        .submit_for_review("cap.review", "alice")
        .expect("submit");
    let (status, body) = call(
        &h.app,
        Req::put("/admin/capabilities/cap.review/definition")
            .body(json!({"definition": {"description": "x"}})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], json!("MCP_STATE_TRANSITION_INVALID"));

    // deprecated：拒绝。
    publish_capability(&h.store, "cap.old");
    seed_capability(&h.store, "cap.new");
    h.store.deprecate("cap.old", "cap.new").expect("deprecate");
    let (status, body) = call(
        &h.app,
        Req::put("/admin/capabilities/cap.old/definition")
            .body(json!({"definition": {"description": "x"}})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], json!("MCP_STATE_TRANSITION_INVALID"));

    // builtin：一律不可写（含不存在于注册表的行）。
    for id in BUILTIN_CAPABILITY_IDS {
        let (status, body) = call(
            &h.app,
            Req::put(&format!("/admin/capabilities/{id}/definition"))
                .body(json!({"definition": {"description": "影子"}})),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{id}");
        assert_eq!(
            body["error"]["code"],
            json!("MCP_BUILTIN_IMMUTABLE"),
            "{id}"
        );
    }
}

#[tokio::test]
async fn template_definition_edit_goes_through_template_gate() {
    let h = harness();
    let (status, _) = call(
        &h.app,
        Req::post("/admin/capabilities").body(json!({"definition": daily_template_definition()})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // 写语句在模板门禁被拒（不能借管理面绕过 sqlguard）。
    let mut bad = daily_template_definition();
    bad["sql"] = json!("DELETE FROM public.orders WHERE 1=1");
    let (status, body) = call(
        &h.app,
        Req::put("/admin/capabilities/order.daily_summary/definition")
            .body(json!({"definition": bad})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], json!("MCP_TEMPLATE_INVALID"));
    assert_eq!(
        h.store
            .get("order.daily_summary")
            .expect("get")
            .expect("row")
            .version,
        1
    );

    // template_id 必须与路径 id 一致。
    let mut mismatched = daily_template_definition();
    mismatched["template_id"] = json!("order.other");
    let (status, body) = call(
        &h.app,
        Req::put("/admin/capabilities/order.daily_summary/definition")
            .body(json!({"definition": mismatched})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], json!("MCP_TEMPLATE_INVALID"));

    // 合法修订：版本 +1，仍为 draft。
    let mut revised = daily_template_definition();
    revised["description"] = json!("按日汇总订单 v2");
    let (status, body) = call(
        &h.app,
        Req::put("/admin/capabilities/order.daily_summary/definition")
            .body(json!({"definition": revised})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["version"], json!(2));
    assert_eq!(body["status"], json!("draft"));

    // 详情端点返回的是存储形状（{"template": {...}, "base_sql": ...}）：前端原样回写必须可用。
    let (_, detail) = call(&h.app, Req::get("/admin/capabilities/order.daily_summary")).await;
    let mut stored = detail["definition"].clone();
    assert!(
        stored.get("template").is_some(),
        "详情定义应为存储形状: {stored}"
    );
    stored["template"]["description"] = json!("存储形状回写");
    let (status, body) = call(
        &h.app,
        Req::put("/admin/capabilities/order.daily_summary/definition")
            .body(json!({"definition": stored})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "存储形状定义必须可回写: {body}");
    assert_eq!(body["version"], json!(3));
    assert_eq!(
        h.store
            .get("order.daily_summary")
            .expect("get")
            .expect("row")
            .definition["template"]["description"],
        json!("存储形状回写")
    );
}
