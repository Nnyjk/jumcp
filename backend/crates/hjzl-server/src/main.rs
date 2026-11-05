//! 双入口：stdio（逐行 JSON-RPC）与 streamable-http（POST /mcp），共用同一条 async 调用链。
//! 为什么 axum 仅在 http 模式：stdio 不需要 HTTP 栈（依赖整洁）。

use hjzl_core::audit::AuditLog;
use hjzl_core::config::Settings;
use hjzl_core::registry::CapabilityStore;
use hjzl_core::template::TemplateRegistry;
use hjzl_server::mcp::McpServer;
use hjzl_server::pg_executor::{PgExecutor, PgExecutorConfig};
use hjzl_server::service::{Service, health};
use serde_json::Value;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()))
        .init();
    let settings = Settings::from_env().map_err(|e| anyhow::anyhow!("启动配置非法: {e}"))?;
    tracing::info!(health = ?health(&settings), "health ok");

    let mut service = Service::new(settings.clone());
    let audit = if settings.audit_path.is_empty() {
        None
    } else {
        Some(Arc::new(
            AuditLog::open(&settings.audit_path).map_err(|e| anyhow::anyhow!("{e}"))?,
        ))
    };
    // 注册表常驻：即使没有模板，db_run_template 也应给出"不存在"而不是"未实现"。
    let store = Arc::new(
        CapabilityStore::open(&settings.registry_path, audit.clone())
            .map_err(|e| anyhow::anyhow!("打开注册表失败: {e}"))?,
    );
    let registry = Arc::new(TemplateRegistry::new(store.clone()));
    let mut pg: Option<Arc<PgExecutor>> = None;
    if let Some(ds) = settings.primary_datasource() {
        // 只读账号自检失败即拒绝启动（fail-closed）：任何降级都会把治理面暴露在可写连接上。
        let dsn = ds.dsn().map_err(|e| anyhow::anyhow!("{e}"))?;
        let executor = PgExecutor::connect(PgExecutorConfig {
            datasource_name: ds.name.clone(),
            dsn,
            statement_timeout_ms: ds.statement_timeout_ms,
            allowed_schemas: ds.allowed_schemas.clone(),
            max_connections: 5,
        })
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "数据源 {} 启动自检失败: [{}] {}",
                ds.name,
                e.code,
                e.message
            )
        })?;
        let executor = Arc::new(executor);
        pg = Some(executor.clone());
        service = service.with_executor(executor);
    }
    if !settings.templates_file.is_empty() {
        let seeds = hjzl_server::templates_boot::load_template_seeds(&settings.templates_file)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        for seed in &seeds {
            let compiled = hjzl_server::templates_boot::register_seed(&registry, seed)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            // 发布前预检：有真实数据源时必须 EXPLAIN 通过；失败即拒绝发布（fail-closed）。
            if let Some(executor) = &pg {
                executor
                    .explain(&compiled.base_sql, compiled.bind_order.len())
                    .await
                    .map_err(|e| {
                        anyhow::anyhow!(
                            "模板 {} 预检失败: [{}] {}",
                            compiled.def.template_id,
                            e.code,
                            e.message
                        )
                    })?;
            }
            hjzl_server::templates_boot::publish_seed(&registry, seed, &compiled)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
    }
    service = service.with_templates(registry);

    if let Some(log) = audit {
        service = service.with_audit(log);
    }

    let service = Arc::new(service);
    let server = Arc::new(McpServer::from_arc(service.clone()));
    if settings.transport == "stdio" {
        run_stdio(server).await
    } else {
        run_http(server, service.clone(), store.clone(), settings).await
    }
}

async fn run_stdio(server: Arc<McpServer>) -> anyhow::Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let mut reader = tokio::io::BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let req: Value = serde_json::from_str(trimmed)?;
        let resp = server.handle(&req).await;
        if !resp.is_null() {
            stdout
                .write_all(serde_json::to_string(&resp)?.as_bytes())
                .await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }
    }
    Ok(())
}

async fn run_http(
    server: Arc<McpServer>,
    service: Arc<Service>,
    store: Arc<CapabilityStore>,
    settings: Settings,
) -> anyhow::Result<()> {
    // fail-closed：没有 token 就不开管理面，也不提供"半开放"降级。
    if settings.admin_tokens.is_empty() {
        anyhow::bail!(
            "HTTP 模式下 HJZL_ADMIN_TOKENS 未配置：管理面不允许无鉴权启动（格式 id:token,id:token）"
        );
    }
    let listener =
        tokio::net::TcpListener::bind(format!("{}:{}", settings.host, settings.port)).await?;
    let admin = hjzl_server::admin::AdminState {
        service,
        store,
        tokens: settings.admin_tokens.clone(),
    };
    let admin_app = hjzl_server::admin::router(admin);
    let mcp_app = axum::Router::new()
        .route("/mcp", axum::routing::post(post_mcp))
        .with_state(server);
    let app = admin_app.merge(mcp_app);
    axum::serve(listener, app).await?;
    Ok(())
}

async fn post_mcp(
    axum::extract::State(server): axum::extract::State<Arc<McpServer>>,
    headers: axum::http::HeaderMap,
    body: String,
) -> axum::Json<Value> {
    // 请求头只做可见性收窄（§2.1）：判定失败即整体拒绝并写 policy_decision，不静默降级。
    let header_map: std::collections::HashMap<String, String> = headers
        .iter()
        .filter_map(|(k, v)| v.to_str().ok().map(|s| (k.to_string(), s.to_string())))
        .collect();
    let scope =
        match hjzl_server::service::scope_from_headers(&server.service().settings, &header_map) {
            Ok(s) => Some(s),
            Err(e) => {
                let actor = hjzl_core::entry::Actor::new("default", "analyst");
                let audit_error = server
                    .service()
                    .policy_decision(
                        &actor,
                        &e.code,
                        serde_json::json!({"transport": "http", "detail": e.data}),
                    )
                    .err();
                let err = audit_error.unwrap_or(e);
                return axum::Json(serde_json::json!({
                    "jsonrpc":"2.0","id":null,
                    "error":{"code":-32602,"message":err.message,
                              "data":{"mcp_code": err.code, "detail": err.data}}
                }));
            }
        };
    let req: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            return axum::Json(serde_json::json!({
                "jsonrpc":"2.0","id":null,
                "error":{"code":-32700,"message":"Parse error","data":{"detail": e.to_string()}}
            }));
        }
    };
    axum::Json(server.handle_scoped(&req, scope.as_ref()).await)
}
