//! 双入口：stdio（逐行 JSON-RPC）与 streamable-http（POST /mcp），共用同一条 async 调用链。
//! 为什么 axum 仅在 http 模式：stdio 不需要 HTTP 栈（依赖整洁）。

use jumcp_core::audit::AuditLog;
use jumcp_core::config::Settings;
use jumcp_core::registry::CapabilityStore;
use jumcp_core::template::TemplateRegistry;
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
/// 不是兜底降级。HTTP 面没有这条路径：/mcp 只认数据库 access token 派生的身份。
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
    // 首次启动必须能从环境变量播种初始管理员：账号表为空时没有任何登录途径，
    // 管理面就此死锁。已有账号时这两个变量不参与（重启不该再要求一次口令）。
    let seeded = store
        .seed_initial_admin(
            &std::env::var("JUMCP_ADMIN_USERNAME").unwrap_or_default(),
            &std::env::var("JUMCP_ADMIN_PASSWORD").unwrap_or_default(),
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    if let Some(user) = seeded {
        tracing::info!(username = %user.username, "已创建初始管理员账号");
    }
    // 治理面不可静默失效：`Service::audit_event` 在无审计时是 no-op，
    // 维护通道的写工具会"写成功但无痕迹"。
    if settings.audit_path.is_empty() {
        anyhow::bail!(
            "HTTP 模式下 JUMCP_MCP_AUDIT_PATH 未配置：维护通道不允许无审计运行（审计不可静默失效）"
        );
    }
    // 两个面必须不同端口：同一端口下"分面"只剩一层路径判断，任何回退（SPA、兜底）都会
    // 把管理台页面或管理 API 暴露给 MCP 面。Settings::validate 已拦，这里再拦一次是为了
    // 把"直接构造 Settings 起服务"的路径也堵死。
    if settings.admin_port == settings.port {
        anyhow::bail!(
            "JUMCP_ADMIN_PORT 与 JUMCP_MCP_PORT 相同（{}）：管理面与 MCP 面必须使用不同端口",
            settings.port
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
    let mcp_app = jumcp_server::app::mcp_surface(
        server,
        service.clone(),
        store.clone(),
        settings.secret_key.clone(),
        builtin_endpoint.as_ref(),
    );
    let admin_app = jumcp_server::app::admin_surface(
        jumcp_server::admin::AdminState {
            service,
            store: store.clone(),
            transport_key: settings.transport_key.clone(),
        },
        &settings.web_dir,
    )
    .map_err(|e| anyhow::anyhow!("启动配置非法: {e}"))?;
    if settings.web_dir.is_empty() {
        tracing::warn!(
            "JUMCP_MCP_WEB_DIR 为空：管理面只提供 /admin/* API，不托管管理台静态站（浏览器访问会 404）"
        );
    }
    if !settings.admin_cookie_secure {
        tracing::warn!(
            "管理台会话 cookie 未启用 Secure：请仅在内网使用或启用 TLS（JUMCP_ADMIN_COOKIE_SECURE=1 要求 HTTPS，否则浏览器不回传 cookie）"
        );
    }
    // 先都绑定成功再 serve：端口占用要在启动期就失败，而不是演进成"管理面起来了、MCP 面没起"。
    let mcp_listener =
        bind_listener(&settings.host, settings.port, "JUMCP_MCP_PORT", "MCP 面").await?;
    let admin_listener = bind_listener(
        &settings.host,
        settings.admin_port,
        "JUMCP_ADMIN_PORT",
        "管理面",
    )
    .await?;
    tracing::info!(
        host = %settings.host,
        mcp_port = settings.port,
        admin_port = settings.admin_port,
        tokens = token_total,
        "HTTP 双监听已启动：MCP 面只挂 /mcp、/internal/mcp/*、/health；管理面只挂 /admin/*、/health 与静态站"
    );
    // 任一监听退出即整体退出：只有一个面活着的进程比死了更危险（探针打到哪半都可能误判健康）。
    tokio::try_join!(
        axum::serve(mcp_listener, mcp_app).into_future(),
        axum::serve(admin_listener, admin_app).into_future()
    )?;
    Ok(())
}

/// 绑定一个监听端口，失败时给出含变量名与地址的中文错误。
async fn bind_listener(
    host: &str,
    port: u16,
    var: &str,
    surface: &str,
) -> anyhow::Result<tokio::net::TcpListener> {
    let addr = format!("{host}:{port}");
    tokio::net::TcpListener::bind(&addr).await.map_err(|e| {
        anyhow::anyhow!(
            "{surface}监听失败：无法绑定 {addr}（{var}={port}）：{e}。端口可能已被占用、权限不足，或 JUMCP_MCP_HOST 指向了本机不存在的地址"
        )
    })
}
