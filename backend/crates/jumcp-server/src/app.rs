//! 两个监听面的装配。
//!
//! 为什么分成两个 Router 而不是一个 Router 两倍监听：分面是**部署事实**（网关要按端口转发、
//! 内网只暴露管理面、MCP 面给 agent），路由表分开才能保证"一个面上不存在另一个面的端点"——
//! 靠鉴权中间件区分做不到，`/mcp` 的 Bearer token 与管理面 token 是两套身份。
//!
//! 面与路径：
//! - MCP 面（`JUMCP_MCP_PORT`）：`/mcp`、`/internal/mcp/{package}/{upstream}`、`/health`；其余 404。
//! - 管理面（`JUMCP_ADMIN_PORT`）：`/admin/*`、`/health` + 管理台静态站（`JUMCP_MCP_WEB_DIR`）；其余 404。
//!
//! 两面都保留 `/health`：k8s/swarm 的 healthcheck 不该因为选错端口而失败。

use crate::admin::AdminState;
use crate::builtin::{BuiltinEndpoint, BuiltinState};
use crate::mcp::McpServer;
use crate::service::{Service, health_report};
use crate::web;
use axum::Router;
use axum::routing::get;
use jumcp_core::registry::CapabilityStore;
use jumcp_core::secret::SecretKey;
use std::sync::Arc;

/// `/health`：无鉴权、两个面各挂一次。handler 自己持有 `Arc<Service>`，因此能 merge 进任一面。
pub fn health_router(service: Arc<Service>) -> Router {
    Router::new().route(
        "/health",
        get(move || {
            let service = service.clone();
            async move { axum::Json(health_report(&service)) }
        }),
    )
}

/// MCP 面：给 agent 用的 JSON-RPC 入口与进程内内置端点的回环口，**不托管任何页面**。
pub fn mcp_surface(
    server: Arc<McpServer>,
    service: Arc<Service>,
    store: Arc<CapabilityStore>,
    secret_key: Option<SecretKey>,
    builtin: Option<&BuiltinEndpoint>,
) -> Router {
    let app = crate::mcp::http_router(server).merge(health_router(service));
    let app = match (secret_key, builtin) {
        // 内置端点要求传输密钥（它用 Bearer 回环、且要解密密文 secret）：两者缺一即不挂。
        (Some(key), Some(endpoint)) => app.merge(crate::builtin::router(BuiltinState::new(
            store,
            Arc::new(key),
            endpoint,
        ))),
        _ => app,
    };
    web::strict_fallback(app)
}

/// 管理面：管理 API + 管理台静态站。`web_dir` 为空则只提供 API（不挂静态，行为与历史上一致）。
pub fn admin_surface(state: AdminState, web_dir: &str) -> Result<Router, String> {
    web::attach(crate::admin::router(state), web_dir)
}
