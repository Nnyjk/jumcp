//! Admin HTTP API 契约测试：axum oneshot，不需要真实 PG，也不起监听端口。
//!
//! 覆盖：鉴权（无/错 token）、审核队列状态面、非法状态转移 409、同人发布成功、
//! 可见性成功写 admin_change、审计过滤与分页、limit 夹紧、health 的链状态。

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use jumcp_core::audit::{ActorRef, AuditEvent, AuditLog};
use jumcp_core::config::{AdminTokens, DatasourceRef, GroupDef, RoleDef, Settings, TenantPolicy};
use jumcp_core::entry::EffectiveScope;
use jumcp_core::registry::{BUILTIN_CAPABILITY_IDS, CapabilityStore};
use jumcp_core::template::TemplateRegistry;
use jumcp_server::admin::{AdminState, router};
use jumcp_server::service::Service;
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
            name: "数据分析".into(),
            default_groups: vec!["db.read.core".into()],
            default_budget: 2000,
        },
    );
    Settings {
        admin_tokens: AdminTokens::parse(&format!("alice:{ALICE_TOKEN},bob:{BOB_TOKEN}"))
            .expect("admin tokens"),
        policy: TenantPolicy {
            groups: vec![GroupDef {
                code: "db.read.core".into(),
                name: "核心数据只读".into(),
                priority: 10,
                min_budget: 800,
            }],
            roles,
            tenant_budget_cap: 2000,
            allow_intersect: false,
        },
        datasources: vec![DatasourceRef {
            name: "main-pg".into(),
            dsn_env: "JUMCP_TEST_MAIN_DSN".into(),
            engine: "clickhouse".into(),
            readonly: true,
            allowed_schemas: vec![],
            statement_timeout_ms: 5000,
        }],
        ..Default::default()
    }
}

struct Harness {
    app: Router,
    audit: Arc<AuditLog>,
    store: Arc<CapabilityStore>,
    service: Arc<Service>,
}

fn harness_with_audit(audit: Arc<AuditLog>) -> Harness {
    let store = Arc::new(CapabilityStore::open(":memory:", Some(audit.clone())).expect("store"));
    let registry = Arc::new(TemplateRegistry::new(store.clone()));
    let service = Arc::new(
        Service::new(settings())
            .with_audit(audit.clone())
            // 模板注册表与 store 同源：人工登记走 `TemplateRegistry::register`。
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
    let state = AdminState {
        service: service.clone(),
        store: store.clone(),
        tokens: settings().admin_tokens,
    };
    Harness {
        app: router(state),
        audit,
        store,
        service,
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
    fn put(uri: &str) -> Self {
        Self {
            method: "PUT",
            uri: uri.to_string(),
            token: Some(ALICE_TOKEN.into()),
            actor: Some("alice".into()),
            body: None,
        }
    }
    fn delete(uri: &str) -> Self {
        Self {
            method: "DELETE",
            uri: uri.to_string(),
            token: Some(ALICE_TOKEN.into()),
            actor: None,
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
        .promote_to_published(id, "ap-1", "bob")
        .expect("publish");
}

fn audit_event(event_type: &str) -> AuditEvent {
    AuditEvent {
        tenant: "t1".into(),
        actor: ActorRef {
            actor_type: "agent".into(),
            id: "a-1".into(),
            role: "analyst".into(),
            token_id: None,
            source: None,
        },
        event_type: event_type.into(),
        payload: json!({
            "object_type": "tool",
            "object_id": "db_query_readonly",
            "action": "invoke",
            "outcome": "ok",
            "data": {}
        }),
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
    assert_eq!(events[0].payload["error_code"], json!("MCP_UNAUTHORIZED"));
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
    let path = std::env::temp_dir().join("jumcp-admin-tampered.sqlite3");
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
        Req::post("/admin/capabilities/db_query_readonly/reject").body(json!({"reason": "原因"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], json!("MCP_BUILTIN_IMMUTABLE"));
    assert!(
        h.store.get("db_query_readonly").expect("get").is_none(),
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
    assert_eq!(body["order"], json!("desc"), "默认最新优先");

    // 时间范围过滤可组合。
    let (status, body) = call(
        &h.app,
        Req::get("/admin/audit-events?ts_after=1970-01-01T00:00:00Z&tenant=t1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(4));

    // object_id / actor_id / event_id 过滤走 payload 与 actor_json。
    let (status, body) = call(
        &h.app,
        Req::get("/admin/audit-events?object_id=db_query_readonly"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(4));
    let (status, body) = call(&h.app, Req::get("/admin/audit-events?actor_id=a-1")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(4));
    let (status, body) = call(&h.app, Req::get("/admin/audit-events?order=asc")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["order"], json!("asc"));
    assert_eq!(body["events"][0]["event_type"], json!("tool_call"));
    let (status, body) = call(&h.app, Req::get("/admin/audit-events?order=bogus")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], json!("MCP_BAD_REQUEST"));
    // session_id 已从过滤契约删除：必须报不支持，而不是静默忽略。
    let (status, body) = call(&h.app, Req::get("/admin/audit-events?session_id=s-1")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], json!("MCP_FILTER_UNSUPPORTED"));
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
        decisions[0].payload["error_code"],
        json!("MCP_FILTER_UNSUPPORTED")
    );
}

#[tokio::test]
async fn routes_are_documented() {
    let table = jumcp_server::admin::routes();
    assert!(table.iter().any(|(_, path)| *path == "/admin/review-queue"));
    assert!(table.iter().any(|(_, path)| *path == "/health"));
    assert!(table.iter().any(|(_, path)| *path == "/admin/capabilities"));
    assert!(table.iter().any(|(_, path)| *path == "/admin/tools/stats"));
    assert!(
        table
            .iter()
            .any(|(_, path)| *path == "/admin/tools/{id}/calls")
    );
    assert!(
        table
            .iter()
            .any(|(_, path)| *path == "/admin/capabilities/{id}/deprecate")
    );
    assert!(
        table
            .iter()
            .any(|(_, path)| *path == "/admin/capabilities/{id}/reject")
    );
    assert!(
        table
            .iter()
            .any(|(_, path)| *path == "/admin/capabilities/{id}/definition")
    );
    assert!(
        table
            .iter()
            .any(|(_, path)| *path == "/admin/capabilities/{id}/versions")
    );
    assert!(table.iter().any(|(_, path)| *path == "/admin/me"));
    assert!(table.iter().any(|(_, path)| *path == "/admin/roles"));
    assert!(table.iter().any(|(_, path)| *path == "/admin/groups"));
    assert!(
        table
            .iter()
            .any(|(_, path)| *path == "/admin/groups/{code}")
    );
    assert!(
        table
            .iter()
            .any(|(_, path)| *path == "/admin/upstreams/{id}/tools")
    );
    assert!(
        table
            .iter()
            .any(|(_, path)| *path == "/admin/upstreams/{id}/tools/basic")
    );
    assert_eq!(table.len(), 35);
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

// ---------------------------- 工具统计与人工维护 ----------------------------

/// 相对当前时间的审计时间戳：固定日期会让"窗口内/窗口外"的断言随时间腐坏。
fn minutes_ago(minutes: i64) -> String {
    (chrono::Utc::now() - chrono::Duration::minutes(minutes)).to_rfc3339()
}

fn append_event(log: &AuditLog, event_type: &str, ts: String, session: &str, payload: Value) {
    log.append(AuditEvent {
        tenant: "t1".into(),
        actor: ActorRef {
            actor_type: "agent".into(),
            id: "a-1".into(),
            role: "analyst".into(),
            token_id: None,
            source: None,
        },
        event_type: event_type.into(),
        payload,
        session_id: session.into(),
        ts,
        event_id: String::new(),
    })
    .expect("append");
}

fn template_run(
    log: &AuditLog,
    id: &str,
    minutes: i64,
    session: &str,
    outcome: &str,
    cache: &str,
    tokens: Option<i64>,
) {
    let mut data = json!({
        "arguments": {"template_id": id, "params": {}},
        "executed_script": "SELECT 1",
        "bound_params": [],
        "cache": cache,
        "elapsed_ms": 5,
        "version": 1
    });
    if let Some(t) = tokens {
        data["tokens"] = json!(t);
    }
    append_event(
        log,
        "template_run",
        minutes_ago(minutes),
        session,
        json!({
            "object_type": "template",
            "object_id": id,
            "action": "invoke",
            "outcome": outcome,
            "data": data
        }),
    );
}

fn tool_call(log: &AuditLog, minutes: i64, session: &str, outcome: &str, tokens: Option<i64>) {
    let mut data = json!({
        "arguments": {"sql": "SELECT 1"},
        "executed_script": "SELECT 1",
        "bound_params": [],
        "elapsed_ms": 3
    });
    if let Some(t) = tokens {
        data["tokens"] = json!(t);
    }
    append_event(
        log,
        "tool_call",
        minutes_ago(minutes),
        session,
        json!({
            "object_type": "tool",
            "object_id": "db_query_readonly",
            "action": "invoke",
            "outcome": outcome,
            "data": data
        }),
    );
}

fn seed_template_capability(store: &CapabilityStore, id: &str) {
    store
        .register(
            id,
            "template",
            "db.read.core",
            "read",
            json!({"description": id}),
        )
        .expect("register template");
}

fn find_tool<'a>(body: &'a Value, id: &str) -> &'a Value {
    body["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .find(|t| t["id"] == json!(id))
        .unwrap_or_else(|| panic!("统计里缺少 {id}"))
}

fn template_definition(id: &str) -> Value {
    json!({
        "template_id": id,
        "group": "db.read.core",
        "risk": "read",
        "description": "人工登记的模板",
        "sql": "SELECT day FROM public.orders WHERE created_at >= :start_date",
        "params": [{"name":"start_date","type":"timestamp","required":true,"nullable":false}],
        "constraints": {
            "allowed_schemas": ["public"],
            "auto_limit": 100,
            "max_result_tokens": 1500,
            "statement_timeout_ms": 5000
        },
        "result_cache_ttl_s": 0
    })
}

#[tokio::test]
async fn stats_aggregates_calls_success_retries_and_token_samples() {
    let h = harness();
    seed_template_capability(&h.store, "order.daily_summary");
    // 同一 session：ok(miss) → ok(hit) → error → error(budget)。
    // error 之后同 session 还有一次调用，因此计 1 次重试（budget error 不是"重试源"）。
    template_run(
        &h.audit,
        "order.daily_summary",
        50,
        "s-1",
        "ok",
        "miss",
        Some(300),
    );
    template_run(
        &h.audit,
        "order.daily_summary",
        40,
        "s-1",
        "ok",
        "hit",
        Some(500),
    );
    template_run(
        &h.audit,
        "order.daily_summary",
        30,
        "s-1",
        "error",
        "miss",
        None,
    );
    template_run(
        &h.audit,
        "order.daily_summary",
        20,
        "s-1",
        "error",
        "miss",
        None,
    );
    tool_call(&h.audit, 10, "s-2", "ok", Some(100));

    let (status, body) = call(&h.app, Req::get("/admin/tools/stats")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["window"]["days"], json!(7), "window_days 默认 7");
    assert!(body["window"]["from"].as_str().is_some());
    assert!(body["window"]["to"].as_str().is_some());
    assert_eq!(body["truncated"], json!(false));

    let tpl = find_tool(&body, "order.daily_summary");
    assert_eq!(tpl["kind"], json!("template"));
    assert_eq!(tpl["group"], json!("db.read.core"));
    assert_eq!(tpl["lifecycle_status"], json!("draft"));
    assert_eq!(tpl["risk"], json!("read"));
    assert_eq!(tpl["discoverable"], json!(false));
    assert_eq!(tpl["callable"], json!(false));
    assert_eq!(tpl["version"], json!(1));
    assert!(tpl["last_changed_at"].as_str().is_some());
    assert_eq!(tpl["source"], json!("managed"));
    assert!(tpl.get("status").is_none(), "旧字段 status 必须删除");
    assert_eq!(tpl["calls"], json!(4));
    assert_eq!(tpl["success"], json!(2), "ok 与 summary 计入 success");
    assert_eq!(
        tpl["failure"],
        json!(2),
        "error 与 budget_rejected 计入 failure"
    );
    assert_eq!(tpl["success_rate"], json!(0.5));
    assert_eq!(tpl["cache_hits"], json!(1));
    assert_eq!(tpl["hit_rate"], json!(0.25));
    assert_eq!(
        tpl["retries"],
        json!(1),
        "error 后同 session 还有调用 → 1 次重试"
    );
    assert_eq!(tpl["avg_response_tokens"], json!(400.0), "(300+500)/2");
    assert_eq!(tpl["p95_response_tokens"], json!(500), "最近秩法 n=2");
    assert_eq!(tpl["max_response_tokens"], json!(500));
    assert_eq!(tpl["recent_tokens"], json!([300, 500]), "时间升序");
    assert_eq!(tpl["last_outcome"], json!("error"), "取时间最新一次");
    assert!(tpl["last_call_at"].as_str().is_some());
    assert_eq!(tpl["health"], json!("failing"), "success_rate 0.5 < 0.80");

    let builtin = find_tool(&body, "db_query_readonly");
    assert_eq!(builtin["kind"], json!("tool"));
    assert_eq!(builtin["lifecycle_status"], json!("published"));
    assert_eq!(builtin["risk"], json!("read"));
    assert_eq!(builtin["discoverable"], json!(true));
    assert_eq!(builtin["callable"], json!(true));
    assert_eq!(builtin["version"], Value::Null);
    assert_eq!(builtin["last_changed_at"], Value::Null);
    assert_eq!(builtin["source"], json!("builtin"));
    assert_eq!(builtin["calls"], json!(1));
    assert_eq!(builtin["success_rate"], json!(1.0));
    assert_eq!(builtin["hit_rate"], Value::Null, "非 template 不给命中率");
    assert_eq!(builtin["health"], json!("ok"));
}

#[tokio::test]
async fn stats_p95_sorts_samples_but_recent_tokens_keep_time_order() {
    let h = harness();
    seed_template_capability(&h.store, "cap.p95");
    // 时间升序采样，token 明显乱序：p95 必须先排序再取位，recent_tokens 仍按时间序。
    for (minutes, tokens) in [(40, 1), (30, 100), (20, 2), (10, 99)] {
        template_run(
            &h.audit,
            "cap.p95",
            minutes,
            "s-1",
            "ok",
            "miss",
            Some(tokens),
        );
    }
    let (status, body) = call(&h.app, Req::get("/admin/tools/stats")).await;
    assert_eq!(status, StatusCode::OK);
    let item = find_tool(&body, "cap.p95");
    assert_eq!(
        item["p95_response_tokens"],
        json!(100),
        "最近秩法 rank=ceil(0.95*4)=4，排序后第 4 小是 100"
    );
    assert_eq!(
        item["recent_tokens"],
        json!([1, 100, 2, 99]),
        "recent_tokens 保持时间升序，不因 p95 排序而改变"
    );
    assert_eq!(item["avg_response_tokens"], json!(50.5));
}

#[tokio::test]
async fn stats_lists_registered_capability_without_calls_as_unknown() {
    let h = harness();
    seed_capability(&h.store, "cap.idle");
    let (status, body) = call(&h.app, Req::get("/admin/tools/stats")).await;
    assert_eq!(status, StatusCode::OK);
    let idle = find_tool(&body, "cap.idle");
    assert_eq!(idle["calls"], json!(0));
    assert_eq!(idle["success"], json!(0));
    assert_eq!(idle["failure"], json!(0));
    assert_eq!(idle["success_rate"], Value::Null, "calls=0 时成功率是 null");
    assert_eq!(idle["hit_rate"], Value::Null);
    assert_eq!(idle["health"], json!("unknown"));
    assert_eq!(idle["retries"], json!(0));
    assert_eq!(idle["last_call_at"], Value::Null);
    assert_eq!(idle["last_outcome"], Value::Null);
    assert_eq!(idle["avg_response_tokens"], Value::Null);
    assert_eq!(idle["p95_response_tokens"], Value::Null);
    assert_eq!(idle["max_response_tokens"], Value::Null);
    assert_eq!(idle["recent_tokens"], json!([]));
}

#[tokio::test]
async fn stats_lists_all_nine_builtin_ids() {
    let h = harness();
    let (status, body) = call(&h.app, Req::get("/admin/tools/stats")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(BUILTIN_CAPABILITY_IDS.len(), 9);
    for id in BUILTIN_CAPABILITY_IDS {
        let cap = find_tool(&body, id);
        assert_eq!(
            cap["source"],
            json!("builtin"),
            "{id} 的 source 必须是 builtin"
        );
        assert_eq!(cap["lifecycle_status"], json!("published"));
        assert_eq!(cap["health"], json!("unknown"));
    }
    let admin_only = find_tool(&body, "admin_register_template");
    assert_eq!(admin_only["group"], json!("admin"));
    let resident = find_tool(&body, "db_run_template");
    assert_eq!(resident["group"], json!("basic"));

    // 三类职责的代表：basic 走查/取数、analysis 列表分析与变更提交、admin 仅审计查询。
    for (id, category) in [
        ("search_capabilities", "basic"),
        ("db_query_readonly", "basic"),
        ("admin_review_queue", "analysis"),
        ("admin_audit_queries", "admin"),
    ] {
        assert_eq!(
            find_tool(&body, id)["category"],
            json!(category),
            "{id} 的 category"
        );
    }
}

#[tokio::test]
async fn stats_clamps_window_and_marks_limit_truncation() {
    let h = harness();
    seed_with(&h.store, "aaa.first", "db.read.core", "a");
    seed_with(&h.store, "bbb.second", "db.read.core", "b");
    // 窗口外的调用不得计入：60 天前远超 30 天上限（`minutes_ago` 以分钟为单位）。
    tool_call(&h.audit, 60 * 24 * 60, "s-old", "ok", Some(10));

    let (status, body) = call(&h.app, Req::get("/admin/tools/stats?window_days=0")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["window"]["days"], json!(1), "下界夹紧到 1");

    let (status, body) = call(&h.app, Req::get("/admin/tools/stats?window_days=999")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["window"]["days"], json!(30), "上界夹紧到 30");
    assert_eq!(
        find_tool(&body, "db_query_readonly")["calls"],
        json!(0),
        "窗口外的调用不计入"
    );
    assert_eq!(body["truncated"], json!(false));

    let (status, body) = call(&h.app, Req::get("/admin/tools/stats?limit=1")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["tools"].as_array().expect("tools").len(), 1);
    assert_eq!(body["truncated"], json!(true), "超过 limit 必须标记截断");
    assert_eq!(
        body["tools"][0]["id"],
        json!("aaa.first"),
        "calls 相同（0）时按 id 升序"
    );

    let (status, body) = call(&h.app, Req::get("/admin/tools/stats?limit=9999")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["limit"],
        Value::Null,
        "响应不带 limit（只有 window/truncated/tools）"
    );
    assert_eq!(
        body["tools"].as_array().expect("tools").len(),
        11,
        "9 个 builtin + 2 个注册能力"
    );
    assert_eq!(body["truncated"], json!(false));
}

#[tokio::test]
async fn tool_calls_detail_is_scoped_ordered_and_nullable() {
    let h = harness();
    seed_template_capability(&h.store, "order.daily_summary");
    template_run(
        &h.audit,
        "order.daily_summary",
        30,
        "s-1",
        "ok",
        "miss",
        Some(120),
    );
    template_run(
        &h.audit,
        "order.daily_summary",
        20,
        "s-1",
        "error",
        "miss",
        None,
    );
    tool_call(&h.audit, 10, "s-2", "ok", Some(100));

    let (status, body) = call(&h.app, Req::get("/admin/tools/order.daily_summary/calls")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], json!("order.daily_summary"));
    assert_eq!(body["total"], json!(2), "只含该 id 的事件");
    assert_eq!(body["limit"], json!(50));
    assert_eq!(body["offset"], json!(0));
    let calls = body["calls"].as_array().expect("calls");
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["outcome"], json!("error"), "时间倒序");
    assert_eq!(calls[0]["tokens"], Value::Null, "缺失的 tokens 返回 null");
    assert_eq!(calls[0]["error_code"], Value::Null);
    assert_eq!(calls[0]["error_message"], Value::Null);
    assert_eq!(calls[0]["version"], json!(1));
    assert_eq!(calls[0]["cache"], json!("miss"));
    assert_eq!(calls[0]["elapsed_ms"], json!(5));
    assert_eq!(calls[0]["event_type"], json!("template_run"));
    assert_eq!(calls[0]["object_type"], json!("template"));
    assert_eq!(calls[0]["object_id"], json!("order.daily_summary"));
    assert_eq!(calls[0]["actor"]["id"], json!("a-1"));
    assert_eq!(calls[0]["actor"]["role"], json!("analyst"));
    assert_eq!(
        calls[0]["arguments"],
        json!({"template_id": "order.daily_summary", "params": {}})
    );
    assert_eq!(calls[0]["executed_script"], json!("SELECT 1"));
    assert_eq!(calls[0]["bound_params"], json!([]));
    assert!(calls[0]["event_id"].as_str().is_some());
    assert!(calls[0]["ts"].as_str().is_some());
    assert!(
        calls[0].get("session_id").is_none(),
        "session_id 不出现在明细"
    );
    assert!(calls[0].get("mcp_code").is_none(), "mcp_code 已删除");
    assert_eq!(calls[1]["tokens"], json!(120));

    // limit 夹紧 + offset 生效。
    let (status, body) = call(
        &h.app,
        Req::get("/admin/tools/order.daily_summary/calls?limit=9999&offset=1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["limit"], json!(200));
    assert_eq!(body["total"], json!(2));
    assert_eq!(body["calls"].as_array().expect("calls").len(), 1);
    assert_eq!(body["calls"][0]["tokens"], json!(120));

    // 未注册/无调用的 id：空列表而不是 404。
    let (status, body) = call(&h.app, Req::get("/admin/tools/nope/calls")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(0));
    assert_eq!(body["calls"], json!([]));
}

/// 明细只读新载荷：`data.arguments` / `data.executed_script` / `data.bound_params`。
/// 旧行的 `sql`/`params`/`mcp_code` 一律不回退，旧行在明细页不出现。
#[tokio::test]
async fn tool_calls_detail_reads_new_payload_only() {
    let h = harness();
    append_event(
        &h.audit,
        "tool_call",
        minutes_ago(20),
        "s-1",
        json!({
            "object_type": "tool",
            "object_id": "db_query_readonly",
            "action": "invoke",
            "outcome": "ok",
            "data": {
                "arguments": {"sql": "SELECT 2", "params": [2]},
                "executed_script": "SELECT 2",
                "bound_params": [2]
            }
        }),
    );
    // 旧载荷（只有 sql/params/mcp_code）：不参与明细。
    append_event(
        &h.audit,
        "tool_call",
        minutes_ago(15),
        "s-1",
        json!({
            "tool": "db_query_readonly",
            "outcome": "error",
            "sql": "SELECT 3",
            "params": [3],
            "mcp_code": "MCP_SQL_REJECTED"
        }),
    );
    // 新载荷的错误行：错误原因在顶层。
    append_event(
        &h.audit,
        "tool_call",
        minutes_ago(10),
        "s-1",
        json!({
            "object_type": "tool",
            "object_id": "db_query_readonly",
            "action": "invoke",
            "outcome": "error",
            "error_code": "MCP_SQL_REJECTED",
            "error_message": "只读查询被拒绝",
            "data": {
                "arguments": {"sql": "SELECT 4"},
                "bound_params": []
            }
        }),
    );

    let (status, body) = call(&h.app, Req::get("/admin/tools/db_query_readonly/calls")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(2), "旧载荷行不计入明细");
    let calls = body["calls"].as_array().expect("calls");
    assert_eq!(calls[0]["error_code"], json!("MCP_SQL_REJECTED"));
    assert_eq!(calls[0]["error_message"], json!("只读查询被拒绝"));
    assert_eq!(calls[0]["arguments"], json!({"sql": "SELECT 4"}));
    assert_eq!(calls[0]["executed_script"], Value::Null, "缺失时不回退 sql");
    assert_eq!(calls[0]["bound_params"], json!([]));
    assert_eq!(
        calls[1]["arguments"],
        json!({"sql": "SELECT 2", "params": [2]})
    );
    assert_eq!(calls[1]["executed_script"], json!("SELECT 2"));
    assert_eq!(calls[1]["bound_params"], json!([2]));
    assert_eq!(calls[1]["outcome"], json!("ok"));
}

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
            .body(json!({"definition": template_definition("db_query_readonly")})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], json!("MCP_BUILTIN_IMMUTABLE"));
    assert!(h.store.get("db_query_readonly").expect("get").is_none());
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
        Req::post("/admin/capabilities/db_query_readonly/deprecate")
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

#[tokio::test]
async fn stats_retries_are_counted_per_session_not_globally() {
    let h = harness();
    seed_template_capability(&h.store, "cap.interleaved");
    seed_template_capability(&h.store, "cap.consecutive");
    // A 失败 → B 调用 → A 重试：其它会话插在中间不得吞掉 A 的重试（全局 windows(2) 会漏计）。
    template_run(
        &h.audit,
        "cap.interleaved",
        50,
        "s-a",
        "error",
        "miss",
        None,
    );
    template_run(
        &h.audit,
        "cap.interleaved",
        45,
        "s-b",
        "ok",
        "miss",
        Some(10),
    );
    template_run(
        &h.audit,
        "cap.interleaved",
        40,
        "s-a",
        "ok",
        "hit",
        Some(20),
    );
    // 同一会话连续两次失败再成功：每次都紧跟下一次 → 2 次重试。
    template_run(
        &h.audit,
        "cap.consecutive",
        30,
        "s-a",
        "error",
        "miss",
        None,
    );
    template_run(
        &h.audit,
        "cap.consecutive",
        25,
        "s-a",
        "unknown",
        "miss",
        None,
    );
    template_run(
        &h.audit,
        "cap.consecutive",
        20,
        "s-a",
        "ok",
        "miss",
        Some(5),
    );

    let (status, body) = call(&h.app, Req::get("/admin/tools/stats")).await;
    assert_eq!(status, StatusCode::OK);
    let interleaved = find_tool(&body, "cap.interleaved");
    assert_eq!(interleaved["calls"], json!(3));
    assert_eq!(interleaved["success"], json!(2));
    assert_eq!(interleaved["failure"], json!(1));
    assert_eq!(
        interleaved["retries"],
        json!(1),
        "s-a 的失败后同一会话还有一次尝试 → 计 1 次重试"
    );

    let consecutive = find_tool(&body, "cap.consecutive");
    assert_eq!(consecutive["calls"], json!(3));
    assert_eq!(
        consecutive["retries"],
        json!(2),
        "两次失败后都跟了下一次调用"
    );
}

#[tokio::test]
async fn tool_calls_window_defaults_to_thirty_days_and_clamps() {
    let h = harness();
    seed_template_capability(&h.store, "order.history");
    template_run(
        &h.audit,
        "order.history",
        10,
        "s-new",
        "ok",
        "miss",
        Some(10),
    );
    // 60 天前：默认 30 天窗口之外、90 天窗口之内（审计 append 保留显式 ts）。
    template_run(
        &h.audit,
        "order.history",
        60 * 24 * 60,
        "s-old",
        "ok",
        "miss",
        Some(99),
    );

    let (status, body) = call(&h.app, Req::get("/admin/tools/order.history/calls")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(1), "默认 30 天窗口不含 60 天前的调用");
    assert_eq!(body["calls"][0]["object_id"], json!("order.history"));
    assert_eq!(body["calls"][0]["tokens"], json!(10));
    assert!(body["calls"][0].get("session_id").is_none());

    let (status, body) = call(
        &h.app,
        Req::get("/admin/tools/order.history/calls?window_days=90"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(2), "90 天窗口包含 60 天前的调用");
    assert_eq!(body["calls"][1]["tokens"], json!(99));

    // window_days 夹紧到 1：只保留最近的调用，分页字段照常返回。
    let (status, body) = call(
        &h.app,
        Req::get("/admin/tools/order.history/calls?window_days=0"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(1));
    assert_eq!(body["limit"], json!(50));
    assert_eq!(body["offset"], json!(0));

    // 上界夹紧到 90 天：9999 与 90 等价。
    let (status, body) = call(
        &h.app,
        Req::get("/admin/tools/order.history/calls?window_days=9999"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(2));

    // 未知查询键仍然 400。
    let (status, body) = call(&h.app, Req::get("/admin/tools/order.history/calls?bogus=1")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], json!("MCP_FILTER_UNSUPPORTED"));
}

// ---------------------------- 能力详情与非内建定义编辑 ----------------------------

/// 有效的模板定义（Postgres 方言由 `TemplateRegistry::new` 决定）。
fn daily_template_definition() -> Value {
    json!({
        "template_id": "order.daily_summary",
        "group": "db.read.core",
        "risk": "read",
        "description": "按日汇总订单",
        "sql": "SELECT day FROM public.orders WHERE day >= :day",
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

    // 显式 approval_id 作为外部票号写进版本行（不再生成 `draft-edit:*`）。
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

// ---------------------------- 组维护 ----------------------------

#[tokio::test]
async fn groups_crud_takes_effect_in_effective_policy() {
    let h = harness();
    let (status, body) = call(&h.app, Req::get("/admin/groups")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(2), "内置 basic + 一个 policy 组");
    assert_eq!(
        body["items"][0]["code"],
        json!("basic"),
        "内置组固定排在第一位"
    );
    assert_eq!(body["items"][0]["source"], json!("builtin"));
    assert_eq!(body["items"][0]["name"], json!("基础工具"));
    assert_eq!(body["items"][0]["priority"], json!(0));
    assert_eq!(body["items"][0]["min_budget"], json!(800));
    assert_eq!(body["items"][0]["roles"], json!([]));
    assert_eq!(body["items"][0]["grantable"], json!(false));
    assert_eq!(body["items"][0]["token_count"], json!(0));
    assert_eq!(
        body["items"][0]["capability_count"],
        json!(5),
        "没有上游覆盖时 basic 只含 5 个常驻内置工具"
    );
    let policy_item = body["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|i| i["code"] == json!("db.read.core"))
        .expect("policy group");
    assert_eq!(
        policy_item["name"],
        json!("核心数据只读"),
        "组条目必须同时给 code 与中文展示名"
    );
    assert_eq!(policy_item["source"], json!("policy"));
    assert_eq!(policy_item["grantable"], json!(true));
    assert_eq!(
        policy_item["roles"],
        json!(["analyst"]),
        "policy 组角色从 roles 反查（角色 code）"
    );

    let (status, body) = call(
        &h.app,
        Req::post("/admin/groups").body(json!({
            "code": "db.read.new",
            "name": "新只读组",
            "priority": 7,
            "min_budget": 900,
            "roles": ["analyst"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["code"], json!("db.read.new"));
    assert_eq!(body["name"], json!("新只读组"));
    assert_eq!(body["source"], json!("managed"));
    assert_eq!(body["roles"], json!(["analyst"]));
    assert_eq!(body["capability_count"], json!(0));

    // 重复（managed）→ 409。
    let (status, body) = call(
        &h.app,
        Req::post("/admin/groups").body(json!({
            "code": "db.read.new", "name": "重复组", "priority": 1, "min_budget": 800, "roles": []
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], json!("MCP_GROUP_CONFLICT"));

    // 未知角色 → 400。
    let (status, body) = call(
        &h.app,
        Req::post("/admin/groups").body(json!({
            "code": "db.read.ghost", "name": "幽灵组", "priority": 1, "min_budget": 800,
            "roles": ["ghost"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], json!("MCP_VALIDATION_ERROR"));
    assert_eq!(body["error"]["data"]["unknown_roles"], json!(["ghost"]));

    // 修改：name/priority/min_budget/roles 可改，code 不可改。
    let (status, body) = call(
        &h.app,
        Req::put("/admin/groups/db.read.new").body(json!({
            "name": "新只读组（改名）", "priority": 9, "min_budget": 1000, "roles": ["analyst"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["code"], json!("db.read.new"), "路径 code 保持不变");
    assert_eq!(body["name"], json!("新只读组（改名）"));
    assert_eq!(body["priority"], json!(9));
    assert_eq!(body["min_budget"], json!(1000));

    // body 里出现 code → 400（拒绝静默忽略改名）。
    let (status, body) = call(
        &h.app,
        Req::put("/admin/groups/db.read.new")
            .body(json!({"code": "db.read.other", "name": "换 code"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], json!("MCP_VALIDATION_ERROR"));
    assert_eq!(body["error"]["data"]["field"], json!("code"));

    // 有效策略立刻生效：新组按 code 进入 groups（展示名一并保留），并追加进 analyst 默认组。
    let policy = h.service.effective_policy().expect("effective policy");
    let merged = policy
        .groups
        .iter()
        .find(|g| g.code == "db.read.new")
        .expect("managed group merged by code");
    assert_eq!(merged.name, "新只读组（改名）");
    assert!(
        policy.roles["analyst"]
            .default_groups
            .contains(&"db.read.new".to_string())
    );

    // 列表合并 policy + managed（外加内置 basic）。
    let (status, body) = call(&h.app, Req::get("/admin/groups")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(3));
    let managed = body["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|i| i["code"] == json!("db.read.new"))
        .expect("managed group");
    assert_eq!(managed["name"], json!("新只读组（改名）"));
    assert_eq!(managed["source"], json!("managed"));
    assert_eq!(managed["priority"], json!(9));

    // 删除 → 下一次 effective policy 不再包含它；响应带 code（name 仅是展示）。
    let (status, body) = call(&h.app, Req::delete("/admin/groups/db.read.new")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["deleted"], json!(true));
    assert_eq!(body["code"], json!("db.read.new"));
    assert_eq!(body["name"], json!("新只读组（改名）"));
    let policy = h.service.effective_policy().expect("effective policy");
    assert!(!policy.groups.iter().any(|g| g.code == "db.read.new"));

    // 三次写操作都落 admin_change：object_type=group，object_id=code（无 `group:` 前缀）。
    let changes = h
        .audit
        .query(Some(&json!({"event_type": "admin_change"})))
        .expect("query");
    let group_changes: Vec<_> = changes
        .iter()
        .filter(|e| e.payload["object_type"] == json!("group"))
        .collect();
    assert_eq!(group_changes.len(), 3, "create/update/delete 各一条");
    assert_eq!(group_changes[0].payload["object_id"], json!("db.read.new"));
    assert_eq!(group_changes[0].payload["action"], json!("group_create"));
    assert_eq!(group_changes[1].payload["action"], json!("group_update"));
    assert_eq!(group_changes[2].payload["action"], json!("group_delete"));
    for change in &group_changes {
        assert_eq!(change.payload["approval"], Value::Null);
        assert!(
            change.payload["changes"]
                .as_array()
                .is_some_and(|c| !c.is_empty())
        );
        assert_eq!(
            change.payload["data"]["group"]["code"],
            json!("db.read.new")
        );
        assert!(
            change.payload["data"]["group"]["name"]
                .as_str()
                .is_some_and(|n| !n.is_empty()),
            "组审计 data 必须带展示名: {change:?}"
        );
    }
    // 改名落在 changes 的 name 字段上（before/after 都是展示名）。
    let renamed = group_changes[1].payload["changes"]
        .as_array()
        .expect("changes")
        .iter()
        .find(|c| c["field"] == json!("name"))
        .expect("update 审计含 name 变化");
    assert_eq!(renamed["before"], json!("新只读组"));
    assert_eq!(renamed["after"], json!("新只读组（改名）"));
}

#[tokio::test]
async fn groups_list_filters_by_q_source_and_role() {
    let h = harness();
    // policy 组 db.read.core（核心数据只读，analyst）+ 两个管理组。
    call(
        &h.app,
        Req::post("/admin/groups").body(json!({
            "code": "db.read.analytics2", "name": "分析快照组", "priority": 3,
            "min_budget": 800, "roles": ["analyst"]
        })),
    )
    .await;
    call(
        &h.app,
        Req::post("/admin/groups").body(json!({
            "code": "kb.extra", "name": "补充知识库", "priority": 2,
            "min_budget": 800, "roles": []
        })),
    )
    .await;

    // 不带过滤：全部 4 条（内置 basic + policy + 两个管理组）。
    let (_, body) = call(&h.app, Req::get("/admin/groups")).await;
    assert_eq!(body["total"], json!(4));

    // q 对 code 做小写包含。
    let (_, body) = call(&h.app, Req::get("/admin/groups?q=ANALYTICS")).await;
    assert_eq!(body["total"], json!(1));
    assert_eq!(body["items"][0]["code"], json!("db.read.analytics2"));

    // q 对中文展示名做包含。
    let (_, body) = call(&h.app, Req::get("/admin/groups?q=知识库")).await;
    assert_eq!(body["total"], json!(1));
    assert_eq!(body["items"][0]["code"], json!("kb.extra"));

    // source 过滤。
    let (_, body) = call(&h.app, Req::get("/admin/groups?source=policy")).await;
    assert_eq!(body["total"], json!(1));
    assert_eq!(body["items"][0]["code"], json!("db.read.core"));
    let (_, body) = call(&h.app, Req::get("/admin/groups?source=managed")).await;
    assert_eq!(body["total"], json!(2));
    let (_, body) = call(&h.app, Req::get("/admin/groups?source=builtin")).await;
    assert_eq!(body["total"], json!(1), "内置组只有一个 basic");
    assert_eq!(body["items"][0]["code"], json!("basic"));
    assert_eq!(body["items"][0]["grantable"], json!(false));

    // role 过滤：角色 code 精确匹配授权角色。
    let (_, body) = call(&h.app, Req::get("/admin/groups?role=analyst")).await;
    assert_eq!(body["total"], json!(2), "只返回授权给 analyst 的组");
    let codes: Vec<&str> = body["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter_map(|i| i["code"].as_str())
        .collect();
    assert_eq!(codes, vec!["db.read.core", "db.read.analytics2"]);

    // 组合过滤。
    let (_, body) = call(
        &h.app,
        Req::get("/admin/groups?source=managed&role=analyst&q=快照"),
    )
    .await;
    assert_eq!(body["total"], json!(1));
    assert_eq!(body["items"][0]["code"], json!("db.read.analytics2"));

    // 非法 source → 400。
    let (status, body) = call(&h.app, Req::get("/admin/groups?source=config")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], json!("MCP_VALIDATION_ERROR"));
}

#[tokio::test]
async fn roles_list_returns_code_and_chinese_name() {
    let h = harness();
    let (status, body) = call(&h.app, Req::get("/admin/roles")).await;
    assert_eq!(status, StatusCode::OK);
    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["code"], json!("analyst"));
    assert_eq!(items[0]["name"], json!("数据分析"));
    assert_eq!(items[0]["default_groups"], json!(["db.read.core"]));
    assert_eq!(items[0]["default_budget"], json!(2000));
}

#[tokio::test]
async fn groups_reject_reserved_policy_and_missing() {
    let h = harness();
    for code in ["basic", "builtin", "admin"] {
        let (status, body) = call(
            &h.app,
            Req::post("/admin/groups").body(json!({
                "code": code, "name": "保留组", "priority": 1, "min_budget": 800, "roles": []
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "创建保留组 {code}");
        assert_eq!(body["error"]["code"], json!("MCP_GROUP_RESERVED"));

        let (status, body) = call(
            &h.app,
            Req::put(&format!("/admin/groups/{code}"))
                .body(json!({"priority": 1, "min_budget": 800, "roles": []})),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "修改保留组 {code}");
        assert_eq!(body["error"]["code"], json!("MCP_GROUP_RESERVED"));

        let (status, body) = call(&h.app, Req::delete(&format!("/admin/groups/{code}"))).await;
        assert_eq!(status, StatusCode::CONFLICT, "删除保留组 {code}");
        assert_eq!(body["error"]["code"], json!("MCP_GROUP_RESERVED"));
    }

    // 配置组：创建/修改/删除一律 409（只能改配置文件）。
    let (status, body) = call(
        &h.app,
        Req::post("/admin/groups").body(json!({
            "code": "db.read.core", "name": "核心数据只读", "priority": 1,
            "min_budget": 800, "roles": ["analyst"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], json!("MCP_GROUP_CONFLICT"));
    let (status, body) = call(
        &h.app,
        Req::put("/admin/groups/db.read.core")
            .body(json!({"priority": 1, "min_budget": 800, "roles": ["analyst"]})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], json!("MCP_GROUP_CONFLICT"));
    let (status, body) = call(&h.app, Req::delete("/admin/groups/db.read.core")).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], json!("MCP_GROUP_CONFLICT"));

    // 不存在 → 404（改/删）。
    let (status, body) = call(
        &h.app,
        Req::put("/admin/groups/db.read.missing")
            .body(json!({"priority": 1, "min_budget": 800, "roles": []})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], json!("MCP_GROUP_NOT_FOUND"));
    let (status, body) = call(&h.app, Req::delete("/admin/groups/db.read.missing")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], json!("MCP_GROUP_NOT_FOUND"));

    // 非法组 code / 空展示名 / 非法数值 → 400。
    for body in [
        json!({"code": "Bad Name", "name": "大写组", "priority": 1, "min_budget": 800, "roles": []}),
        json!({"code": "db.read.bad", "name": "   ", "priority": 1, "min_budget": 800, "roles": []}),
        json!({"code": "db.read.bad", "name": "组", "priority": 1, "min_budget": 0, "roles": []}),
        json!({"code": "db.read.bad", "name": "组", "priority": -1, "min_budget": 800, "roles": []}),
    ] {
        let (status, resp) = call(&h.app, Req::post("/admin/groups").body(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(resp["error"]["code"], json!("MCP_VALIDATION_ERROR"));
    }
}

#[tokio::test]
async fn group_delete_refused_while_capability_uses_it() {
    let h = harness();
    call(
        &h.app,
        Req::post("/admin/groups").body(json!({
            "code": "db.read.used", "name": "被引用组", "priority": 1,
            "min_budget": 800, "roles": ["analyst"]
        })),
    )
    .await;
    h.store
        .register(
            "cap.used",
            "tool",
            "db.read.used",
            "read",
            json!({"description": "引用组"}),
        )
        .expect("register");

    let (status, body) = call(&h.app, Req::delete("/admin/groups/db.read.used")).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], json!("MCP_GROUP_IN_USE"));
    assert_eq!(body["error"]["data"]["capability_count"], json!(1));

    // 列表上 capability_count 同步。
    let (_, body) = call(&h.app, Req::get("/admin/groups")).await;
    let used = body["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|i| i["code"] == json!("db.read.used"))
        .expect("group");
    assert_eq!(used["capability_count"], json!(1));
}
