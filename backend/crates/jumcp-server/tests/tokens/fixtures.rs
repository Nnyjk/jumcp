use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use jumcp_core::audit::AuditLog;
use jumcp_core::config::{AdminTokens, DatasourceRef, GroupDef, RoleDef, Settings, TenantPolicy};
use jumcp_core::entry::EffectiveScope;
use jumcp_core::registry::CapabilityStore;
use jumcp_core::template::TemplateRegistry;
use jumcp_server::admin::AdminState;
use jumcp_server::executor::{ExecFuture, QueryExecutor, QueryParams};
use jumcp_server::mcp::{McpServer, http_router};
use jumcp_server::service::Service;
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

pub(crate) const ALICE_TOKEN: &str = "tok_alice_000000000";
pub(crate) const BOB_TOKEN: &str = "tok_bob_00000000000";

struct FakeExecutor;

impl QueryExecutor for FakeExecutor {
    fn execute<'a>(&'a self, _scope: &'a EffectiveScope, _q: &'a QueryParams) -> ExecFuture<'a> {
        Box::pin(async {
            Ok(json!({
                "rows": [["2026-11-05", 3]],
                "row_count": 1,
                "truncated": false,
                "effective_max_rows": 100,
                "elapsed_ms": 1,
                "tokens": 3
            }))
        })
    }
}

fn settings() -> Settings {
    let mut roles = std::collections::HashMap::new();
    roles.insert(
        "analyst".into(),
        RoleDef {
            name: "数据分析".into(),
            default_groups: vec!["db.read.core".into(), "db.read.analytics".into()],
            default_budget: 2000,
        },
    );
    roles.insert(
        "admin".into(),
        RoleDef {
            name: "管理员".into(),
            default_groups: vec!["admin".into()],
            default_budget: 2500,
        },
    );
    Settings {
        admin_tokens: AdminTokens::parse(&format!("alice:{ALICE_TOKEN},bob:{BOB_TOKEN}"))
            .expect("admin tokens"),
        policy: TenantPolicy {
            groups: vec![
                GroupDef {
                    code: "db.read.core".into(),
                    name: "核心数据只读".into(),
                    priority: 10,
                    min_budget: 800,
                },
                GroupDef {
                    code: "db.read.analytics".into(),
                    name: "分析数据只读".into(),
                    priority: 8,
                    min_budget: 800,
                },
                GroupDef {
                    code: "admin".into(),
                    name: "管理员通道".into(),
                    priority: 1,
                    min_budget: 800,
                },
            ],
            roles,
            tenant_budget_cap: 2000,
            allow_intersect: false,
        },
        budget_overflow: "fail".into(),
        datasources: vec![DatasourceRef {
            name: "main".into(),
            dsn_env: "JUMCP_TEST_MAIN_DSN".into(),
            engine: "clickhouse".into(),
            readonly: true,
            allowed_schemas: vec![],
            statement_timeout_ms: 5000,
        }],
        ..Default::default()
    }
}

pub(crate) struct Harness {
    pub(crate) app: Router,
    pub(crate) store: Arc<CapabilityStore>,
    pub(crate) audit: Arc<AuditLog>,
}

pub(crate) fn harness() -> Harness {
    let audit = Arc::new(AuditLog::open(":memory:").expect("audit"));
    let store = Arc::new(CapabilityStore::open(":memory:", Some(audit.clone())).expect("store"));
    let registry = Arc::new(TemplateRegistry::new(store.clone()));
    let service = Arc::new(
        Service::new(settings())
            .with_executor(Arc::new(FakeExecutor))
            .with_audit(audit.clone())
            .with_templates(registry)
            .with_scope(EffectiveScope {
                groups: vec!["db.read.core".into()],
                budget: 2000,
                degraded: false,
                source: "default".into(),
                mode: "strict".into(),
                allowed_builtin_tools: None,
            }),
    );
    let admin = AdminState {
        service: service.clone(),
        store: store.clone(),
        tokens: settings().admin_tokens,
        transport_key: Some(crate::support::transport_key()),
    };
    let app = jumcp_server::admin::router(admin)
        .merge(http_router(Arc::new(McpServer::from_arc(service))));
    Harness { app, store, audit }
}

/// 发布一条可见可调的能力（store 直调，管理面发布路径已有独立测试）。
pub(crate) fn publish_capability(h: &Harness, id: &str, group: &str) {
    h.store
        .register(id, "tool", group, "read", json!({"description": id}))
        .expect("register");
    h.store.submit_for_review(id, "alice").expect("submit");
    h.store
        .promote_to_published(id, "ap-pub", "bob")
        .expect("publish");
    h.store
        .set_visibility(id, Some(true), Some(true), "alice")
        .expect("visibility");
}

pub(crate) async fn call(
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

pub(crate) fn rpc(method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","id":1,"method":method,"params":params})
}

pub(crate) async fn mcp(
    app: &Router,
    token: Option<&str>,
    body: Value,
    headers: &[(&str, &str)],
) -> Value {
    let (status, value) = call(app, "POST", "/mcp", token, headers, Some(body)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "MCP 入口应回 200 + JSON-RPC: {value}"
    );
    value
}

pub(crate) async fn create_token(app: &Router, body: Value) -> (StatusCode, Value) {
    call(
        app,
        "POST",
        "/admin/tokens",
        Some(ALICE_TOKEN),
        &[("x-admin-actor", "alice")],
        Some(body),
    )
    .await
}

pub(crate) fn base_token_body() -> Value {
    json!({
        "name": "分析型只读",
        "role": "analyst",
        "groups": ["db.read.core"],
        "builtin_tools": ["search_capabilities", "describe_capability"],
        "source": "agent",
        "caller": "agent-analyst",
        "expires_at": null
    })
}

pub(crate) async fn tool_names(app: &Router, token: &str) -> Vec<String> {
    let resp = mcp(app, Some(token), rpc("tools/list", json!({})), &[]).await;
    resp["result"]["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|t| t["name"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) async fn tool_names_with_groups(app: &Router, token: &str, groups: &str) -> Vec<String> {
    let resp = mcp(
        app,
        Some(token),
        rpc("tools/list", json!({})),
        &[("x-mcp-tool-groups", groups)],
    )
    .await;
    resp["result"]["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|t| t["name"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn err_code(resp: &Value) -> String {
    resp["result"]["structuredContent"]["error"]["code"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}
