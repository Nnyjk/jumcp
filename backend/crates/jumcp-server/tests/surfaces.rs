//! 双面隔离契约：一个面上不存在另一个面的端点。
//!
//! 为什么需要独立用例：`/admin/*` 与 `/mcp` 曾经共用一个监听口，靠鉴权中间件区分身份。
//! 分面后安全边界变成"端口 + 路由表"，因此必须钉住两点：
//! - MCP 面看不到 `/admin/*`，也不能通过 SPA 回退泄漏管理台页面；
//! - 管理面看不到 `/mcp` 与 `/internal/mcp/*`，也不能把它们当深链交给前端路由。
//!
//! `/health` 两面都留：healthcheck 不该因为选错端口而失败。

#[path = "support/mod.rs"]
mod support;

use axum::Router;
use axum::http::StatusCode;
use jumcp_core::audit::AuditLog;
use jumcp_core::entry::EffectiveScope;
use jumcp_core::registry::CapabilityStore;
use jumcp_core::secret::SecretKey;
use jumcp_core::template::TemplateRegistry;
use jumcp_server::admin::AdminState;
use jumcp_server::app::{admin_surface, mcp_surface};
use jumcp_server::builtin::BuiltinEndpoint;
use jumcp_server::mcp::McpServer;
use jumcp_server::service::Service;
use serde_json::{Value, json};
use std::io::Write;
use std::sync::Arc;
use support::{ALICE_TOKEN, send_raw, settings};

struct Surfaces {
    mcp: Router,
    admin: Router,
}

fn build(web_dir: &str) -> Surfaces {
    let settings = settings();
    let audit = Arc::new(AuditLog::open(":memory:").expect("audit"));
    let store = Arc::new(CapabilityStore::open(":memory:", Some(audit.clone())).expect("store"));
    let service = Arc::new(
        Service::new(settings.clone())
            .with_audit(audit)
            .with_templates(Arc::new(TemplateRegistry::new(store.clone())))
            .with_scope(EffectiveScope {
                groups: vec!["db.read.core".into()],
                budget: 2000,
                degraded: false,
                source: "header".into(),
                mode: "strict".into(),
                allowed_builtin_tools: None,
            }),
    );
    let endpoint = BuiltinEndpoint::local(&settings.host, settings.port).expect("builtin endpoint");
    let mcp = mcp_surface(
        Arc::new(McpServer::from_arc(service.clone())),
        service.clone(),
        store.clone(),
        Some(SecretKey::parse(support::TEST_SECRET_KEY).expect("secret key")),
        Some(&endpoint),
    );
    support::seed_admin_accounts(&store, settings.admin_session_ttl_secs);
    let admin = admin_surface(
        AdminState {
            service,
            store,
            transport_key: None,
        },
        web_dir,
    )
    .expect("admin surface");
    Surfaces { mcp, admin }
}

/// 带内置静态站的管理面：index.html + 一个 `/assets/` 资源，用于区分"文件命中"与"SPA 回退"。
fn static_site() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut index = std::fs::File::create(dir.path().join("index.html")).expect("index.html");
    index
        .write_all(b"<!doctype html><title>jumcp admin</title>")
        .expect("write index");
    std::fs::create_dir(dir.path().join("assets")).expect("assets dir");
    std::fs::write(dir.path().join("assets").join("app.js"), b"console.log(1)").expect("app.js");
    dir
}

async fn request(
    app: &Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
) -> (StatusCode, String) {
    let response = send_raw(app, method, uri, token, &[], None).await;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    (status, String::from_utf8_lossy(&bytes).to_string())
}

fn code(body: &str) -> Value {
    serde_json::from_str::<Value>(body)
        .expect("json body")
        .get("error")
        .and_then(|e| e.get("code"))
        .cloned()
        .unwrap_or(Value::Null)
}

#[tokio::test]
async fn mcp_surface_hides_admin_namespace_and_static_site() {
    let site = static_site();
    let s = build(site.path().to_str().expect("web dir"));

    // /health 保留：运维探针不知道（也不该知道）自己打的是哪个面。
    let (status, body) = request(&s.mcp, "GET", "/health", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("audit_chain"),
        "health 载荷走同一实现: {body}"
    );

    // /mcp 存在：口味由 JSON-RPC 错误决定，不是路由 404。
    let (status, _) = request(&s.mcp, "POST", "/mcp", None).await;
    assert_ne!(status, StatusCode::NOT_FOUND, "/mcp 必须挂在 MCP 面");

    // 内置端点回环口在 MCP 面（token 由 UpstreamManager 注入，裸访问只应被拒，不能 404）。
    let (status, _) = request(&s.mcp, "POST", "/internal/mcp/pkg/up", None).await;
    assert_ne!(
        status,
        StatusCode::NOT_FOUND,
        "/internal/mcp 必须挂在 MCP 面"
    );

    // 管理面命名空间整个不存在：连"带正确 admin token"也必须 404（不是 401）。
    let (status, body) = request(&s.mcp, "GET", "/admin/me", Some(ALICE_TOKEN)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(code(&body), json!("MCP_NOT_FOUND"));
    let (status, body) = request(&s.mcp, "GET", "/admin", Some(ALICE_TOKEN)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(code(&body), json!("MCP_NOT_FOUND"));

    // 静态站不挂在 MCP 面：根路径是 JSON 404，绝不能吐 index.html。
    let (status, body) = request(&s.mcp, "GET", "/", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(code(&body), json!("MCP_NOT_FOUND"));
    assert!(!body.contains("<!doctype html"), "MCP 面不得托管页面");
}

#[tokio::test]
async fn admin_surface_hides_mcp_namespace() {
    let site = static_site();
    let s = build(site.path().to_str().expect("web dir"));

    let (status, _) = request(&s.admin, "GET", "/health", None).await;
    assert_eq!(status, StatusCode::OK);

    // 管理台 API 在位但需要身份：401 而非 404，证明路由挂载与鉴权各自生效。
    let (status, body) = request(&s.admin, "GET", "/admin/me", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(code(&body), json!("MCP_UNAUTHORIZED"));

    // MCP 面与回环口在管理面不存在，且不能被 SPA 回退吞掉。
    for uri in ["/mcp", "/internal/mcp/clickhouse/http"] {
        let (status, body) = request(&s.admin, "POST", uri, Some(ALICE_TOKEN)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri} 不应出现在管理面");
        assert_eq!(code(&body), json!("MCP_NOT_FOUND"), "{uri} 必须 JSON 404");
    }

    // 静态站仍然完整：根 → index，资源 → 文件，深链 → SPA 回退。
    let (status, body) = request(&s.admin, "GET", "/", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("jumcp admin"), "index.html: {body}");
    let (status, body) = request(&s.admin, "GET", "/review/queue", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("jumcp admin"), "深链回退到 SPA: {body}");
    let (status, body) = request(&s.admin, "GET", "/assets/app.js", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "console.log(1)");
}

/// 内置端点的回环地址必须指向 MCP 面端口。
///
/// 分面之后这条不再是"显然"：`local(host, port)` 的 port 若误传管理面端口，
/// 安装出来的上游会把自己的管理 API 当 MCP 服务器，症状是 404 而不是鉴权失败。
#[test]
fn builtin_endpoint_binds_to_mcp_port() {
    let endpoint = BuiltinEndpoint::local("0.0.0.0", 8123).expect("endpoint");
    assert_eq!(endpoint.base, "http://127.0.0.1:8123/internal/mcp");
    assert_eq!(
        endpoint.url_for("jumcp-clickhouse", "http"),
        "http://127.0.0.1:8123/internal/mcp/jumcp-clickhouse/http"
    );
    assert!(endpoint.matches(&endpoint.url_for("pkg", "up")));
    assert!(!endpoint.matches("http://127.0.0.1:8080/internal/mcp/pkg/up"));
}
