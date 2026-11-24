use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use jumcp_core::audit::AuditLog;
use jumcp_core::config::{GroupDef, RoleDef, Settings, TenantPolicy};
use jumcp_core::entry::EffectiveScope;
use jumcp_core::registry::CapabilityStore;
use jumcp_core::template::TemplateRegistry;
use jumcp_server::admin::AdminState;
use jumcp_server::mcp::{McpServer, http_router};
use jumcp_server::service::Service;
use jumcp_server::upstream::UpstreamManager;
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

// 管理面只认会话 cookie：这就是 `seed_admin_accounts` 建好的 alice 会话 id。
pub(crate) const ALICE_TOKEN: &str = crate::support::ALICE_TOKEN;

pub(crate) fn settings(stdio: bool) -> Settings {
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
        "core-only".into(),
        RoleDef {
            name: "只看核心".into(),
            default_groups: vec!["db.read.core".into()],
            default_budget: 2000,
        },
    );
    roles.insert(
        "admin".into(),
        RoleDef {
            name: "管理员".into(),
            default_groups: vec![
                "db.read.core".into(),
                "db.read.analytics".into(),
                "admin".into(),
            ],
            default_budget: 2500,
        },
    );
    Settings {
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
        upstream_stdio: stdio,
        ..Default::default()
    }
}

pub(crate) struct Harness {
    pub(crate) app: Router,
    pub(crate) store: Arc<CapabilityStore>,
    pub(crate) audit: Arc<AuditLog>,
}

pub(crate) fn harness() -> Harness {
    harness_with(false)
}

pub(crate) fn harness_with(stdio: bool) -> Harness {
    assemble(stdio, None)
}

/// 与 `harness_with(false, false)` 相同，但换成显式的前置拦截配置：
/// 白名单与上限必须能按用例切换，而 `from_env` 是进程全局的。
pub(crate) fn harness_with_preflight(guard: jumcp_server::preflight::PreflightConfig) -> Harness {
    assemble(false, Some(Arc::new(guard)))
}

fn assemble(stdio: bool, guard: Option<Arc<jumcp_server::preflight::PreflightConfig>>) -> Harness {
    let audit = Arc::new(AuditLog::open(":memory:").expect("audit"));
    let store = Arc::new(CapabilityStore::open(":memory:", Some(audit.clone())).expect("store"));
    let boot = settings(stdio);
    let registry = Arc::new(TemplateRegistry::new(store.clone()));
    let manager = Arc::new(UpstreamManager::new(store.clone()));
    let ttl = boot.admin_session_ttl_secs;
    let service = Arc::new(
        Service::new(boot)
            .with_audit(audit.clone())
            .with_templates(registry)
            .with_upstreams(manager)
            .with_scope(scope(&["db.read.core", "db.read.analytics"])),
    );
    crate::support::seed_admin_accounts(&store, ttl);
    let admin = AdminState {
        service: service.clone(),
        store: store.clone(),
        transport_key: Some(crate::support::transport_key()),
    };
    let mut server = McpServer::from_arc(service);
    if let Some(guard) = guard {
        server = server.with_preflight(Some(guard));
    }
    let app = jumcp_server::admin::router(admin).merge(http_router(Arc::new(server)));
    Harness { app, store, audit }
}

pub(crate) fn scope(groups: &[&str]) -> EffectiveScope {
    EffectiveScope {
        groups: groups.iter().map(|g| (*g).to_string()).collect(),
        budget: 2000,
        degraded: false,
        source: "default".into(),
        mode: "strict".into(),
        allowed_builtin_tools: None,
    }
}

pub(crate) async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let builder =
        crate::support::attach_identity(Request::builder().method(method).uri(uri), token);
    let request = builder
        .header(header::CONTENT_TYPE, "application/json")
        .body(body.map_or_else(Body::empty, |v| Body::from(v.to_string())))
        .expect("request");
    let response = app.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("json body")
    };
    (status, value)
}

/// 现场签发一个 agent token：allowlist 显式列出内置 id 与上游暴露名。
pub(crate) async fn issue_token(
    h: &Harness,
    caller: &str,
    role: &str,
    groups: &[&str],
    tools: &[&str],
) -> String {
    let (status, body) = call(
        &h.app,
        "POST",
        "/admin/tokens",
        Some(ALICE_TOKEN),
        Some(json!({
            "name": format!("token {caller}"),
            "role": role,
            "groups": groups,
            "builtin_tools": tools,
            "source": "agent",
            "caller": caller
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "签发令牌失败: {body}");
    body["token"].as_str().expect("token").to_string()
}

pub(crate) async fn mcp(app: &Router, token: &str, method: &str, params: Value) -> Value {
    let (status, body) = call(
        app,
        "POST",
        "/mcp",
        Some(token),
        Some(json!({"jsonrpc":"2.0","id":1,"method":method,"params":params})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "MCP 入口应回 200: {body}");
    body
}

pub(crate) async fn tool_names(app: &Router, token: &str) -> Vec<String> {
    let resp = mcp(app, token, "tools/list", json!({})).await;
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

pub(crate) async fn call_tool(app: &Router, token: &str, name: &str, arguments: Value) -> Value {
    mcp(
        app,
        token,
        "tools/call",
        json!({"name": name, "arguments": arguments}),
    )
    .await
}

pub(crate) fn upstream_body(id: &str, url: &str) -> Value {
    json!({
        "id": id,
        "name": format!("上游 {id}"),
        "transport": "http",
        "url": url,
        "args": [],
        "env": {},
        "headers": {},
        "group": "db.read.analytics",
        "category": "custom",
        "enabled": true,
        "timeout_ms": 5000
    })
}

pub(crate) async fn create_upstream(h: &Harness, body: Value) -> (StatusCode, Value) {
    call(
        &h.app,
        "POST",
        "/admin/upstreams",
        Some(ALICE_TOKEN),
        Some(body),
    )
    .await
}
