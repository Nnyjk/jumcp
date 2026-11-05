//! Admin HTTP API 契约测试：axum oneshot，不需要真实 PG，也不起监听端口。
//!
//! 覆盖：鉴权（无/错 token）、审核队列状态面、非法状态转移 409、四眼 400、
//! 可见性成功写 admin_change、审计过滤与分页、limit 夹紧、health 的链状态。

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use hjzl_core::audit::{ActorRef, AuditEvent, AuditLog};
use hjzl_core::config::{AdminTokens, DatasourceRef, GroupDef, RoleDef, Settings, TenantPolicy};
use hjzl_core::entry::EffectiveScope;
use hjzl_core::registry::CapabilityStore;
use hjzl_server::admin::{AdminState, router};
use hjzl_server::service::Service;
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

const ALICE_TOKEN: &str = "tok_alice_000000000";
const BOB_TOKEN: &str = "tok_bob_00000000000";

fn settings() -> Settings {
    let mut roles = std::collections::HashMap::new();
    roles.insert(
        "analyst".into(),
        RoleDef {
            name: "analyst".into(),
            default_groups: vec!["db.read.core".into()],
            default_budget: 2000,
        },
    );
    Settings {
        admin_tokens: AdminTokens::parse(&format!("alice:{ALICE_TOKEN},bob:{BOB_TOKEN}"))
            .expect("admin tokens"),
        policy: TenantPolicy {
            groups: vec![GroupDef {
                name: "db.read.core".into(),
                priority: 10,
                min_budget: 800,
            }],
            roles,
            tenant_budget_cap: 2000,
            allow_intersect: false,
        },
        datasources: vec![DatasourceRef {
            name: "main-pg".into(),
            dsn_env: "HJZL_TEST_MAIN_DSN".into(),
            engine: "postgres".into(),
            readonly: true,
            allowed_schemas: vec!["public".into()],
            statement_timeout_ms: 5000,
        }],
        ..Default::default()
    }
}

struct Harness {
    app: Router,
    audit: Arc<AuditLog>,
    store: Arc<CapabilityStore>,
}

fn harness_with_audit(audit: Arc<AuditLog>) -> Harness {
    let store = Arc::new(CapabilityStore::open(":memory:", Some(audit.clone())).expect("store"));
    let service = Arc::new(
        Service::new(settings())
            .with_audit(audit.clone())
            .with_scope(EffectiveScope {
                groups: vec!["db.read.core".into()],
                budget: 2000,
                degraded: false,
                source: "header".into(),
                mode: "strict".into(),
            }),
    );
    let state = AdminState {
        service,
        store: store.clone(),
        tokens: settings().admin_tokens,
    };
    Harness {
        app: router(state),
        audit,
        store,
    }
}

fn harness() -> Harness {
    harness_with_audit(Arc::new(AuditLog::open(":memory:").expect("audit")))
}

struct Req {
    method: &'static str,
    uri: String,
    token: Option<String>,
    actor: Option<String>,
    body: Option<Value>,
}

impl Req {
    fn get(uri: &str) -> Self {
        Self {
            method: "GET",
            uri: uri.to_string(),
            token: Some(ALICE_TOKEN.into()),
            actor: None,
            body: None,
        }
    }
    fn post(uri: &str) -> Self {
        Self {
            method: "POST",
            uri: uri.to_string(),
            token: Some(ALICE_TOKEN.into()),
            actor: Some("alice".into()),
            body: None,
        }
    }
    fn body(mut self, body: Value) -> Self {
        self.body = Some(body);
        self
    }
    fn without_token(mut self) -> Self {
        self.token = None;
        self
    }
    fn with_token(mut self, token: &str) -> Self {
        self.token = Some(token.to_string());
        self
    }
    fn actor(mut self, actor: &str) -> Self {
        self.actor = Some(actor.to_string());
        self
    }
}

async fn call(app: &Router, req: Req) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(req.method).uri(req.uri);
    if let Some(token) = req.token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    if let Some(actor) = req.actor {
        builder = builder.header("x-admin-actor", actor);
    }
    let body = match req.body {
        Some(v) => Body::from(v.to_string()),
        None => Body::empty(),
    };
    let request = builder
        .header(header::CONTENT_TYPE, "application/json")
        .body(body)
        .expect("request");
    let response = app.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    let value: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("json body")
    };
    (status, value)
}

fn seed_capability(store: &CapabilityStore, id: &str) {
    store
        .register(
            id,
            "tool",
            "db.read.core",
            "read",
            json!({"description": id}),
        )
        .expect("register");
}

fn publish_capability(store: &CapabilityStore, id: &str) {
    seed_capability(store, id);
    store.submit_for_review(id, "alice").expect("submit");
    store
        .promote_to_published(id, "ap-1", "bob", "")
        .expect("publish");
}

fn audit_event(event_type: &str) -> AuditEvent {
    AuditEvent {
        tenant: "t1".into(),
        actor: ActorRef {
            actor_type: "agent".into(),
            id: "a-1".into(),
            role: "analyst".into(),
        },
        event_type: event_type.into(),
        payload: json!({"tool": "db_query_readonly"}),
        session_id: "s-1".into(),
        ts: String::new(),
        event_id: String::new(),
    }
}

// ---------------------------- 鉴权 ----------------------------

#[tokio::test]
async fn missing_token_is_401_and_audited() {
    let h = harness();
    let (status, body) = call(&h.app, Req::get("/admin/review-queue").without_token()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], json!("MCP_UNAUTHORIZED"));
    let events = h
        .audit
        .query(Some(&json!({"event_type": "policy_decision"})))
        .expect("query");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].payload["reason_code"], json!("MCP_UNAUTHORIZED"));
}

#[tokio::test]
async fn wrong_token_is_401() {
    let h = harness();
    let (status, body) = call(&h.app, Req::get("/admin/review-queue").with_token("nope")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], json!("MCP_UNAUTHORIZED"));
}

#[tokio::test]
async fn health_does_not_require_token() {
    let h = harness();
    let (status, body) = call(&h.app, Req::get("/health").without_token()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], json!("ok"));
    assert_eq!(body["audit_chain"], json!("valid"));
    assert_eq!(body["datasources"][0]["name"], json!("main-pg"));
    assert_eq!(body["datasources"][0]["readonly"], json!(true));
}

#[tokio::test]
async fn health_reports_tampered_chain_but_stays_200() {
    let path = std::env::temp_dir().join("hjzl-admin-tampered.sqlite3");
    let _ = std::fs::remove_file(&path);
    let log = Arc::new(AuditLog::open(path.to_str().expect("path")).expect("audit"));
    log.append(audit_event("tool_call")).expect("append");
    log.append(audit_event("tool_call")).expect("append");
    // 模拟字节级篡改：绕过 trigger 改一行 payload（真实攻击者同样只能这样改）。
    {
        let conn = rusqlite::Connection::open(&path).expect("raw");
        conn.execute("DROP TRIGGER trg_audit_no_update", [])
            .expect("drop");
        conn.execute(
            "UPDATE audit_events SET payload='{\"forged\":1}' WHERE rowid=1",
            [],
        )
        .expect("tamper");
    }
    let h = harness_with_audit(log);
    let (status, body) = call(&h.app, Req::get("/health").without_token()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], json!("ok"));
    assert_eq!(body["audit_chain"], json!("tampered"));
    assert!(body["audit_chain_detail"].as_str().is_some());
    let _ = std::fs::remove_file(&path);
}

// ---------------------------- 审核队列与流转 ----------------------------

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
    assert!(body["items"][0]["definition_hash"].as_str().is_some());
    assert!(body["items"][0]["updated"].as_str().is_some());
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
    assert_eq!(changes[0].payload["capability_id"], json!("cap.a"));
    assert_eq!(changes[0].payload["diff"], json!("draft->review"));
    assert!(changes[0].payload["approval_id"].as_str().is_some());
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
            .any(|e| e.payload["reason_code"] == json!("MCP_STATE_TRANSITION_INVALID"))
    );
}

#[tokio::test]
async fn publish_by_same_person_is_400() {
    let h = harness();
    seed_capability(&h.store, "cap.b");
    h.store.submit_for_review("cap.b", "alice").expect("submit");
    let (status, body) = call(
        &h.app,
        Req::post("/admin/capabilities/cap.b/publish")
            .actor("alice")
            .body(json!({"approval_id": "ap-9", "approved_by": "alice"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], json!("MCP_APPROVAL_REQUIRED"));
    // 失败不改变状态。
    assert_eq!(
        h.store.get("cap.b").expect("get").expect("row").status,
        "review"
    );
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
            .body(json!({"approval_id": "ap-9", "approved_by": "bob"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], json!("published"));
    let changes = h
        .audit
        .query(Some(&json!({"event_type": "admin_change"})))
        .expect("query");
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].payload["approval_id"], json!("ap-9"));
}

#[tokio::test]
async fn visibility_updates_flags_and_writes_admin_change() {
    let h = harness();
    publish_capability(&h.store, "cap.d");
    let (status, body) = call(
        &h.app,
        Req::post("/admin/capabilities/cap.d/visibility")
            .body(json!({"discoverable": true, "callable": true, "approval_id": "ap-vis"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["discoverable"], json!(true));
    assert_eq!(body["callable"], json!(true));
    let changes = h
        .audit
        .query(Some(&json!({"event_type": "admin_change"})))
        .expect("query");
    let vis = changes
        .iter()
        .find(|e| e.payload["approval_id"] == json!("ap-vis"))
        .expect("visibility admin_change");
    assert!(
        vis.payload["diff"]
            .as_str()
            .unwrap_or_default()
            .contains("callable")
    );
    assert!(vis.payload["before_hash"].as_str().is_some());
    assert!(vis.payload["after_hash"].as_str().is_some());
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

// ---------------------------- 审计查询 ----------------------------

#[tokio::test]
async fn audit_events_filter_and_pagination() {
    let h = harness();
    for _ in 0..3 {
        h.audit.append(audit_event("tool_call")).expect("append");
    }
    h.audit
        .append(audit_event("capability_search"))
        .expect("append");

    let (status, body) = call(
        &h.app,
        Req::get("/admin/audit-events?event_type=tool_call&limit=2&offset=1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(3));
    assert_eq!(body["limit"], json!(2));
    assert_eq!(body["offset"], json!(1));
    let events = body["events"].as_array().expect("events");
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|e| e["event_type"] == json!("tool_call")));
    assert!(events[0]["hash"].as_str().is_some());
    assert!(events[0]["prev_hash"].as_str().is_some());

    // 时间范围过滤可组合。
    let (status, body) = call(
        &h.app,
        Req::get("/admin/audit-events?ts_after=1970-01-01T00:00:00Z&tenant=t1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(4));
}

#[tokio::test]
async fn audit_events_limit_is_clamped() {
    let h = harness();
    h.audit.append(audit_event("tool_call")).expect("append");
    let (status, body) = call(&h.app, Req::get("/admin/audit-events?limit=9999")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["limit"], json!(200));
}

#[tokio::test]
async fn audit_events_unknown_filter_is_400() {
    let h = harness();
    let (status, body) = call(&h.app, Req::get("/admin/audit-events?bogus=1")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], json!("MCP_FILTER_UNSUPPORTED"));
    let decisions = h
        .audit
        .query(Some(&json!({"event_type": "policy_decision"})))
        .expect("query");
    assert_eq!(
        decisions[0].payload["reason_code"],
        json!("MCP_FILTER_UNSUPPORTED")
    );
}

#[tokio::test]
async fn routes_are_documented() {
    let table = hjzl_server::admin::routes();
    assert!(table.iter().any(|(_, path)| *path == "/admin/review-queue"));
    assert!(table.iter().any(|(_, path)| *path == "/health"));
    assert!(table.iter().any(|(_, path)| *path == "/admin/capabilities"));
    assert_eq!(table.len(), 7);
}

// ---------------------------- 能力列表 ----------------------------

fn seed_with(store: &CapabilityStore, id: &str, group: &str, description: &str) {
    store
        .register(
            id,
            "tool",
            group,
            "read",
            json!({"description": description}),
        )
        .expect("register");
}

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
        "updated",
    ] {
        assert!(item.get(key).is_some(), "缺少字段 {key}");
    }
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
            .any(|e| e.payload["reason_code"] == json!("MCP_BAD_REQUEST"))
    );
}
