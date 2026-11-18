//! 双入口：stdio（逐行 JSON-RPC）与 streamable-http（POST /mcp），共用同一条 async 调用链。
//! 为什么 axum 仅在 http 模式：stdio 不需要 HTTP 栈（依赖整洁）。

use jumcp_core::audit::AuditLog;
use jumcp_core::config::Settings;
use jumcp_core::registry::CapabilityStore;
use jumcp_core::template::TemplateRegistry;
use jumcp_server::connector::{ConnectorConfig, ConnectorExecutor};
use jumcp_server::mcp::McpServer;
use jumcp_server::service::{Service, health};
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
    // 注册表常驻：即使没有模板，模板/上游能力也应给出"不存在"而不是"未实现"。
    let store = Arc::new(
        CapabilityStore::open(&settings.registry_path, audit.clone())
            .map_err(|e| anyhow::anyhow!("打开注册表失败: {e}"))?,
    );
    // 引导导入：表为空且配置了 JUMCP_AGENT_TOKENS 时，把静态 token 一次性导入为
    // active 记录（明文值的 sha256），之后 /mcp 只认数据库。已有记录则绝不重导。
    if store
        .access_token_total()
        .map_err(|e| anyhow::anyhow!("{e}"))?
        == 0
        && !settings.agent_tokens.is_empty()
    {
        let imported = store
            .import_bootstrap_tokens(settings.agent_tokens.principals(), &settings.policy)
            .map_err(|e| anyhow::anyhow!("导入引导令牌失败: {e}"))?;
        tracing::info!(
            imported,
            "已导入 {} 个引导令牌（JUMCP_AGENT_TOKENS → access_tokens，source=bootstrap）",
            imported
        );
    }
    let registry = Arc::new(TemplateRegistry::with_dialect(
        store.clone(),
        jumcp_sqlguard::Dialect::ClickHouse,
    ));
    let readonly_datasources: Vec<_> = settings
        .datasources
        .iter()
        .filter(|ds| ds.readonly)
        .collect();
    if readonly_datasources.len() > 1 {
        let names: Vec<&str> = readonly_datasources
            .iter()
            .map(|ds| ds.name.as_str())
            .collect();
        anyhow::bail!(
            "只支持单一主数据源，但声明了多个 readonly 数据源: {}",
            names.join(",")
        );
    }
    let primary_datasource = settings.primary_datasource();
    match primary_datasource {
        None => {}
        Some(ds) if ds.engine == "clickhouse" => {
            let dsn = ds.dsn().map_err(|e| anyhow::anyhow!("{e}"))?;
            // 拉起 agent + `initialize` 握手都失败即拒绝启动（fail-closed）。
            let executor = ConnectorExecutor::connect(ConnectorConfig {
                datasource_name: ds.name.clone(),
                dsn,
                statement_timeout_ms: ds.statement_timeout_ms,
                allowed_schemas: ds.allowed_schemas.clone(),
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
            service = service.with_executor(Arc::new(executor));
        }
        Some(ds) => {
            anyhow::bail!(
                "数据源 {} 的引擎 {} 不受支持：当前只接受 engine=clickhouse（connector agent）",
                ds.name,
                ds.engine
            );
        }
    }
    if !settings.templates_file.is_empty() {
        let seeds = jumcp_server::templates_boot::load_template_seeds(&settings.templates_file)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        for seed in &seeds {
            // 注册门禁（sqlguard ClickHouse 方言）即发布前校验；ClickHouse 没有 PG 的 EXPLAIN 预检。
            let compiled = jumcp_server::templates_boot::register_seed(&registry, seed)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            jumcp_server::templates_boot::publish_seed(&registry, seed, &compiled)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
    }
    service = service.with_templates(registry);
    // 内置 MCP 端点只在 HTTP 模式存在：stdio 模式下没有本进程 HTTP 监听可指。
    let builtin_endpoint = if settings.transport == "stdio" {
        None
    } else {
        Some(
            jumcp_server::builtin::BuiltinEndpoint::local(&settings.host, settings.port)
                .map_err(|e| anyhow::anyhow!("内置 MCP 端点不可用: {e}"))?,
        )
    };
    let mut upstreams = jumcp_server::UpstreamManager::new(store.clone(), settings.upstream_write);
    if let Some(endpoint) = &builtin_endpoint {
        // 迁移必须在管理器建连之前：否则第一次 tools/list 会先按 stdio 旧行拉起子进程并失败。
        let key = settings
            .secret_key
            .clone()
            .expect("HTTP 模式的 JUMCP_SECRET_KEY 已在 Settings::validate 校验");
        let migrated = jumcp_server::builtin::migrate_legacy_packages(&store, &key, endpoint)
            .map_err(|e| anyhow::anyhow!("迁移内置 MCP 安装失败: {e}"))?;
        if migrated > 0 {
            tracing::warn!(
                migrated,
                "已把 clickhouse-basic 的 stdio 安装迁移为 HTTP 内嵌端点；密码已重新加密"
            );
        }
        upstreams = upstreams.with_builtin(endpoint.clone());
    }
    // router 上游：管理面写成功即生效，因此管理器在服务启动时固定挂上（只读配置来自 SQLite）。
    service = service.with_upstreams(Arc::new(upstreams));

    if let Some(log) = audit {
        service = service.with_audit(log);
    }

    let service = Arc::new(service);
    let server = Arc::new(McpServer::from_arc(service.clone()));
    if settings.transport == "stdio" {
        run_stdio(server).await
    } else {
        run_http(
            server,
            service.clone(),
            store.clone(),
            settings,
            builtin_endpoint,
        )
        .await
    }
}

/// stdio 传输：逐行 JSON-RPC，**没有 HTTP 头可派生 per-request 身份**。
///
/// 因此 stdio 下所有请求共用进程默认身份（`Service::process_actor`：优先 `analyst` 角色，
/// 否则取名字最前的**非 admin** 角色；全部角色都持 `admin` 组时角色为空、`require_scope()`
/// 失败即 fail-closed），审计里 actor.id 为空——这是传输本身的限制，
/// 不是兜底降级。HTTP 面没有这条路径：/mcp 只认 `JUMCP_AGENT_TOKENS` 派生的身份。
async fn run_stdio(server: Arc<McpServer>) -> anyhow::Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let actor = server.service().process_actor();
    tracing::info!(
        role = %actor.role,
        "stdio 传输：无 per-request 身份，全部请求使用进程默认身份（HTTP 面身份来自 JUMCP_AGENT_TOKENS）"
    );
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
    builtin_endpoint: Option<jumcp_server::builtin::BuiltinEndpoint>,
) -> anyhow::Result<()> {
    // fail-closed：没有 token 就不开管理面，也不提供"半开放"降级。
    if settings.admin_tokens.is_empty() {
        anyhow::bail!(
            "HTTP 模式下 JUMCP_ADMIN_TOKENS 未配置：管理面不允许无鉴权启动（格式 id:token,id:token）"
        );
    }
    // 治理面不可静默失效：`Service::audit_event` 在无审计时是 no-op，
    // 维护通道（admin_register_template / admin_set_visibility）会"写成功但无痕迹"。
    if settings.audit_path.is_empty() {
        anyhow::bail!(
            "HTTP 模式下 JUMCP_MCP_AUDIT_PATH 未配置：维护通道不允许无审计运行（审计不可静默失效）"
        );
    }
    let token_total = store
        .access_token_total()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    if token_total == 0 {
        tracing::warn!(
            "access_tokens 为空：/mcp 将拒绝所有请求，请先在管理面 POST /admin/tokens 生成令牌"
        );
    }
    tracing::info!(
        host = %settings.host,
        port = settings.port,
        tokens = token_total,
        "HTTP /mcp 身份来自数据库 access_tokens（Bearer token → Actor；不再读取 JUMCP_AGENT_TOKENS）"
    );
    let listener =
        tokio::net::TcpListener::bind(format!("{}:{}", settings.host, settings.port)).await?;
    let admin = jumcp_server::admin::AdminState {
        service,
        store: store.clone(),
        tokens: settings.admin_tokens.clone(),
        transport_key: settings.transport_key.clone(),
    };
    let admin_app = jumcp_server::admin::router(admin);
    let mut app = admin_app.merge(jumcp_server::mcp::http_router(server));
    if let Some(endpoint) = &builtin_endpoint {
        let key = Arc::new(
            settings
                .secret_key
                .clone()
                .expect("HTTP 模式的 JUMCP_SECRET_KEY 已在 Settings::validate 校验"),
        );
        app = app.merge(jumcp_server::builtin::router(
            jumcp_server::builtin::BuiltinState::new(store.clone(), key, endpoint),
        ));
    }
    // 管理台静态托管：`JUMCP_MCP_WEB_DIR` 为空即不挂；
    // 非空时目录/产物不合法直接拒绝启动，绝不静默降级成 404。
    let app = jumcp_server::web::attach(app, &settings.web_dir)
        .map_err(|e| anyhow::anyhow!("启动配置非法: {e}"))?;
    axum::serve(listener, app).await?;
    Ok(())
}
