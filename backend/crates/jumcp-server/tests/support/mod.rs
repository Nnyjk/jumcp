//! 集成测试共享设施：真实 axum 链路、admin 鉴权请求、能力发布与审计样例。
//!
//! 各测试文件的 `call`/`mcp` 签名按自身语义保留，统一走 [`send`] 发送请求。

#![allow(dead_code)]

pub mod fake_upstream;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use jumcp_core::audit::{ActorRef, AuditEvent, AuditLog};
use jumcp_core::config::{AdminTokens, DatasourceRef, GroupDef, RoleDef, Settings, TenantPolicy};
use jumcp_core::entry::EffectiveScope;
use jumcp_core::registry::CapabilityStore;
use jumcp_core::template::TemplateRegistry;
use jumcp_server::admin::{AdminState, router};
use jumcp_server::service::Service;
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

pub const ALICE_TOKEN: &str = "tok_alice_000000000";
pub const BOB_TOKEN: &str = "tok_bob_00000000000";

pub fn settings() -> Settings {
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

pub struct Harness {
    pub app: Router,
    pub audit: Arc<AuditLog>,
    pub store: Arc<CapabilityStore>,
    pub service: Arc<Service>,
}

pub fn harness_with_audit(audit: Arc<AuditLog>) -> Harness {
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

pub fn harness() -> Harness {
    harness_with_audit(Arc::new(AuditLog::open(":memory:").expect("audit")))
}

pub struct Req {
    method: &'static str,
    uri: String,
    token: Option<String>,
    pub(crate) actor: Option<String>,
    body: Option<Value>,
}

impl Req {
    pub fn get(uri: &str) -> Self {
        Self {
            method: "GET",
            uri: uri.to_string(),
            token: Some(ALICE_TOKEN.into()),
            actor: None,
            body: None,
        }
    }
    pub fn post(uri: &str) -> Self {
        Self {
            method: "POST",
            uri: uri.to_string(),
            token: Some(ALICE_TOKEN.into()),
            actor: Some("alice".into()),
            body: None,
        }
    }
    pub fn put(uri: &str) -> Self {
        Self {
            method: "PUT",
            uri: uri.to_string(),
            token: Some(ALICE_TOKEN.into()),
            actor: Some("alice".into()),
            body: None,
        }
    }
    pub fn delete(uri: &str) -> Self {
        Self {
            method: "DELETE",
            uri: uri.to_string(),
            token: Some(ALICE_TOKEN.into()),
            actor: None,
            body: None,
        }
    }
    pub fn body(mut self, body: Value) -> Self {
        self.body = Some(body);
        self
    }
    pub fn without_token(mut self) -> Self {
        self.token = None;
        self
    }
    pub fn with_token(mut self, token: &str) -> Self {
        self.token = Some(token.to_string());
        self
    }
    pub fn actor(mut self, actor: &str) -> Self {
        self.actor = Some(actor.to_string());
        self
    }
}

/// 发送一条 HTTP 请求并解析 JSON 响应；空响应体返回 `Value::Null`。
pub async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    for (key, value) in headers {
        builder = builder.header(*key, *value);
    }
    let body = match body {
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

pub async fn call(app: &Router, req: Req) -> (StatusCode, Value) {
    let headers: Vec<(&str, &str)> = match &req.actor {
        Some(actor) => vec![("x-admin-actor", actor.as_str())],
        None => vec![],
    };
    send(
        app,
        req.method,
        &req.uri,
        req.token.as_deref(),
        &headers,
        req.body,
    )
    .await
}

pub fn rpc(method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","id":1,"method":method,"params":params})
}

pub fn err_code(resp: &Value) -> String {
    resp["result"]["content"]["error"]["code"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

pub fn seed_capability(store: &CapabilityStore, id: &str) {
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

/// 注册一个带描述能力的 tool，指定分组与描述。
pub fn seed_with(store: &CapabilityStore, id: &str, group: &str, description: &str) {
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

pub fn publish_capability(store: &CapabilityStore, id: &str) {
    seed_capability(store, id);
    store.submit_for_review(id, "alice").expect("submit");
    store
        .promote_to_published(id, "ap-1", "bob")
        .expect("publish");
}

pub fn audit_event(event_type: &str) -> AuditEvent {
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
