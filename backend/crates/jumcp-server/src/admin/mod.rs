//! Admin HTTP API：账号管理 / 审核队列 / 状态流转 / 可见性 / 审计查询 + 健康检查。
//!
//! 边界：
//! - 只在 streamable-http 模式挂载（stdio 无管理面）；账号表为空且未提供初始账号时进程拒绝启动。
//! - 管理面与 MCP 面是**两个监听**（`JUMCP_ADMIN_PORT` / `JUMCP_MCP_PORT`），本文件只服务管理面。
//! - 身份只来自管理员账号的登录会话（`POST /admin/login` 下发 `jumcp_session` cookie），
//!   不接受 Bearer：管理面没有共享令牌，也就没有"脚本通道"这类旁路；
//!   `X-Admin-Actor` 仅用于一致性校验，永远以会话所属账号的 username 为准。
//! - 全部拒绝写 `policy_decision`，成功写 `admin_change`（submit 在此层写，publish/visibility 由
//!   CapabilityStore 写，避免同一变更两条记录）。
//! - 审计查询只读：不提供任何 UPDATE/DELETE 入口，DB 层另有 append-only trigger 兜底。
//!
//! 本文件放状态、路由表与鉴权/分页共享件；handler 按资源分到 `surface`（自身与审计读）、
//! `capabilities`（能力读）、`writes`（状态流转与人工维护写）、`stats`、`tokens`、`groups`、
//! `upstreams`、`packages`（内置包目录、安装与配置编辑）。

use crate::service::Service;
use axum::extract::{Extension, Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post, put};
use axum::{Json, Router};
use chrono::Utc;
use jumcp_core::access::{AccessTokenRecord, NewAccessToken};
use jumcp_core::audit::{AuditLog, Envelope, Order};
use jumcp_core::config::{
    MAX_GROUPS, RESERVED_GROUP_NAMES, is_principal_id, is_reserved_group_name,
    validate_display_name, validate_group_name,
};
use jumcp_core::entry::Actor;
use jumcp_core::errors::McpError;
use jumcp_core::registry::{
    ADMIN_CAPABILITY_IDS, BUILTIN_CAPABILITY_IDS, CapabilityRecord, CapabilityStore, ManagedGroup,
    McpUpstream, RESIDENT_CAPABILITY_IDS, STATUSES, UPSTREAM_TOOL_GROUP_MAX, UpstreamToolGroup,
    is_builtin_id,
};
use jumcp_core::secret::SecretKey;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

mod admins;
mod capabilities;
mod groups;
mod packages;
mod scoring;
mod session;
mod stats;
mod surface;
mod tokens;
mod transport;
mod upstreams;
mod writes;

use self::admins::{create_admin, delete_admin, list_admins, update_admin};
use self::capabilities::{capabilities, capability_detail, capability_versions};
use self::groups::{create_group, delete_group, list_groups, list_roles, update_group};
use self::packages::{install_package, list_catalog, uninstall_package, update_package_config};
use self::session::{login, logout, session_info};
use self::stats::{tool_calls, tool_candidates, tool_stats};
pub(crate) use self::stats::{STATS_DEFAULT_WINDOW_DAYS, tool_candidates_report};
pub(crate) use self::surface::queue_item;
use self::surface::{audit_events, health, me, review_queue};
use self::tokens::{
    create_token, delete_token, list_tokens, reissue_token, revoke_token, token_options,
};
use self::transport::get_transport_key;
use self::upstreams::{
    create_upstream, delete_upstream, get_upstream, list_upstreams, refresh_upstream,
    set_upstream_basic_tools, update_upstream, upstream_tools,
};
use self::writes::{
    create_capability, deprecate_capability, publish, reject_capability, submit, update_definition,
    visibility,
};

const ACTOR_HEADER: &str = "x-admin-actor";
/// 审计查询单页上限：HTTP 管理面与 MCP `admin_audit_queries` 共用（夹紧而不是报错）。
pub(crate) const MAX_LIMIT: i64 = 200;
const DEFAULT_LIMIT: i64 = 50;
/// 审计事件的来源面：`admin`（管理面写入）/ `mcp`（MCP 面调用与 agent 发起的治理写）/ `all`。
const SURFACES: &[&str] = &["admin", "mcp", "all"];
const FILTER_KEYS: &[&str] = &[
    "tenant",
    "event_type",
    "event_id",
    "object_type",
    "object_id",
    "actor_id",
    "actor_role",
    "token_id",
    "ts_after",
    "ts_before",
];

#[derive(Clone)]
pub struct AdminState {
    pub service: Arc<Service>,
    pub store: Arc<CapabilityStore>,
    /// 传输密钥（`JUMCP_TRANSPORT_KEY`）：安装/PATCH 解密 secret 字段用，
    /// 也经 `GET /admin/transport-key` 下发给管理台。
    pub transport_key: Option<SecretKey>,
}

/// 鉴权中间件派生出的管理主体：后续 handler 只用它，不读 `X-Admin-Actor` 做身份。
#[derive(Clone, Debug)]
pub struct AuthenticatedAdmin {
    /// 审计身份：账号 username（会话换了浏览器也不变）。
    pub id: String,
    /// 账号主键：判断"删除/禁用自己"只能靠它，username 可被改名。
    pub user_id: String,
}

/// 挂在管理面监听上的完整路由：`/health` 免鉴权，`/admin/login`、`/admin/logout` 自校验，
/// 其余 `/admin/*` 必须带管理员会话 cookie。
pub fn router(state: AdminState) -> Router {
    let protected = Router::new()
        .route("/session", get(session_info))
        .route("/me", get(me))
        .route("/admins", get(list_admins).post(create_admin))
        .route("/admins/{id}", patch(update_admin).delete(delete_admin))
        .route("/audit-events", get(audit_events))
        .route("/capabilities", get(capabilities).post(create_capability))
        .route("/capabilities/{id}", get(capability_detail))
        .route("/capabilities/{id}/versions", get(capability_versions))
        .route("/capabilities/{id}/definition", put(update_definition))
        .route("/roles", get(list_roles))
        .route("/groups", get(list_groups).post(create_group))
        .route("/groups/{code}", put(update_group).delete(delete_group))
        .route("/tokens", get(list_tokens).post(create_token))
        .route("/tokens/options", get(token_options))
        .route("/tokens/{id}/revoke", post(revoke_token))
        .route("/tokens/{id}/reissue", post(reissue_token))
        .route("/tokens/{id}", delete(delete_token))
        .route("/review-queue", get(review_queue))
        .route("/tools/stats", get(tool_stats))
        .route("/tools/candidates", get(tool_candidates))
        .route("/tools/{id}/calls", get(tool_calls))
        .route("/upstreams", get(list_upstreams).post(create_upstream))
        .route(
            "/upstreams/{id}",
            get(get_upstream)
                .put(update_upstream)
                .delete(delete_upstream),
        )
        .route("/upstreams/{id}/refresh", post(refresh_upstream))
        .route("/upstreams/{id}/tools", get(upstream_tools))
        .route("/upstreams/{id}/tools/basic", put(set_upstream_basic_tools))
        .route("/mcp/catalog", get(list_catalog))
        .route("/transport-key", get(get_transport_key))
        .route("/mcp/catalog/{id}/install", post(install_package))
        .route(
            "/mcp/catalog/{id}/install/{upstream_id}",
            delete(uninstall_package),
        )
        .route(
            "/mcp/catalog/{id}/install/{upstream_id}/config",
            patch(update_package_config),
        )
        .route("/capabilities/{id}/submit", post(submit))
        .route("/capabilities/{id}/publish", post(publish))
        .route("/capabilities/{id}/reject", post(reject_capability))
        .route("/capabilities/{id}/visibility", post(visibility))
        .route("/capabilities/{id}/deprecate", post(deprecate_capability))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_admin_identity,
        ));
    // 登录/注销**不**挂在鉴权层后面：登录当然不能要求已登录，注销必须幂等（重复注销仍 204）。
    // 限流器按 router 实例创建：它只该保护这台进程的登录口，且并行测试之间不能互相 429。
    let admin = Router::new()
        .route(
            "/login",
            post(login).layer(Extension(session::LoginLimiter::default())),
        )
        .route("/logout", post(logout))
        .merge(protected);
    Router::new()
        .route("/health", get(health))
        .nest("/admin", admin)
        .with_state(state)
}

// ---------------------------- 错误与鉴权 ----------------------------

pub struct ApiError(McpError);

impl From<McpError> for ApiError {
    fn from(e: McpError) -> Self {
        ApiError(e)
    }
}

fn status_for(code: &str) -> StatusCode {
    match code {
        "MCP_UNAUTHORIZED" => StatusCode::UNAUTHORIZED,
        // 同源校验失败是"已认证但不许这么做"，必须与 401 区分：前端据此提示而不是跳登录页。
        "MCP_FORBIDDEN" => StatusCode::FORBIDDEN,
        "MCP_RATE_LIMITED" => StatusCode::TOO_MANY_REQUESTS,
        "MCP_APPROVAL_REQUIRED"
        | "MCP_VALIDATION_ERROR"
        | "MCP_BAD_REQUEST"
        | "MCP_FILTER_UNSUPPORTED"
        | "MCP_TEMPLATE_INVALID"
        | "MCP_PREFLIGHT_REQUIRED"
        | "MCP_SCHEMA_OUT_OF_SCOPE"
        | "MCP_UPSTREAM_INVALID"
        | "MCP_UPSTREAM_WRITE_FORBIDDEN"
        | "MCP_UPSTREAM_STDIO_DISABLED"
        | "MCP_TRANSPORT_CIPHERTEXT_REQUIRED"
        | "MCP_TRANSPORT_DECRYPT_FAILED" => StatusCode::BAD_REQUEST,
        // 传输密钥/内置端点缺失是部署状态问题（HTTP 模式启动期已 fail-closed）。
        "MCP_TRANSPORT_KEY_MISSING" | "MCP_BUILTIN_UNAVAILABLE" => StatusCode::SERVICE_UNAVAILABLE,
        "MCP_STATE_TRANSITION_INVALID" | "MCP_VERSION_CONFLICT" | "MCP_BUILTIN_IMMUTABLE" => {
            StatusCode::CONFLICT
        }
        "MCP_GROUP_CONFLICT" | "MCP_GROUP_RESERVED" | "MCP_GROUP_IN_USE" => StatusCode::CONFLICT,
        "MCP_UPSTREAM_CONFLICT"
        | "MCP_UPSTREAM_PACKAGE_MANAGED"
        | "MCP_UPSTREAM_TOOL_UNKNOWN"
        | "MCP_UPSTREAM_TOOL_NOT_READ_ONLY"
        | "MCP_UPSTREAM_TOOL_NOT_ELIGIBLE" => StatusCode::CONFLICT,
        "MCP_TOKEN_ALREADY_REVOKED" => StatusCode::CONFLICT,
        // 账号操作的冲突：重名、会留下零启用账号、删除自己；都是"请求合法但不许这么做"。
        "MCP_ADMIN_USER_EXISTS" | "MCP_ADMIN_LAST_ACTIVE" | "MCP_ADMIN_SELF_DELETE" => {
            StatusCode::CONFLICT
        }
        "MCP_CAPABILITY_NOT_FOUND"
        | "MCP_GROUP_NOT_FOUND"
        | "MCP_TOKEN_NOT_FOUND"
        | "MCP_ADMIN_USER_NOT_FOUND"
        | "MCP_CATALOG_NOT_FOUND"
        | "MCP_UPSTREAM_NOT_FOUND" => StatusCode::NOT_FOUND,
        // 审计未启用 / 审计写失败：写端点必须整体失败，不能"写成功但无痕迹"。
        "MCP_AUDIT_ERROR" => StatusCode::INTERNAL_SERVER_ERROR,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = status_for(&self.0.code);
        let body = json!({"error": {
            "code": self.0.code,
            "message": self.0.message,
            "data": self.0.data
        }});
        (status, Json(body)).into_response()
    }
}

/// `Authorization: Bearer <token>` 的 token 部分；只有 MCP 面用它（管理面只认会话 cookie）。
pub(crate) fn bearer(headers: &HeaderMap) -> Option<&str> {
    let raw = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let (scheme, token) = raw.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
}

/// 管理面主体：会话所属账号的 username，来源固定 `admin-api`（审计可区分管理面与 MCP 调用方）。
///
/// `id` 为空只出现在"尚未认证"的拒绝路径：不得借审计链伪造一个管理员人格。
fn admin_actor(id: &str) -> Actor {
    let mut actor = Actor::new("default", "admin");
    actor.actor_type = "admin".to_string();
    actor.actor_id = id.to_string();
    actor.source = "admin-api".to_string();
    actor
}

/// 审计主体恒用派生 id；`X-Admin-Actor` 若存在则必须与之一致（防前端把身份写死在请求头里）。
fn actor_of(auth: &AuthenticatedAdmin, headers: &HeaderMap) -> Result<Actor, McpError> {
    let declared = headers
        .get(ACTOR_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(declared) = declared
        && declared != auth.id
    {
        return Err(McpError::new(
            "MCP_BAD_REQUEST",
            json!({"derived": auth.id, "declared": declared, "hint": "X-Admin-Actor 与 token 派生身份不一致"}),
        ));
    }
    Ok(admin_actor(&auth.id))
}

/// 管理面写路径的审计载荷骨架：与 MCP 侧共用 `jumcp_core::audit::payload`。
fn audit_base(
    object_type: &str,
    object_id: &str,
    action: &str,
    outcome: &str,
) -> Map<String, Value> {
    jumcp_core::audit::payload(object_type, object_id, action, outcome)
}

/// 拒绝路径统一审计：写失败则把审计错误上抛（治理面不可静默失效）。
fn reject(state: &AdminState, actor: &Actor, e: McpError, detail: Value) -> ApiError {
    match state.service.policy_decision(actor, &e, detail) {
        Ok(_) => ApiError(e),
        Err(audit_error) => ApiError(audit_error),
    }
}

/// 管理面鉴权：只认管理员会话 cookie（`jumcp_session`）。
///
/// - 无 cookie、会话过期、会话所属账号被禁用/删除都是 401，不提供任何回落凭据：
///   管理面没有共享令牌，唯一的身份通道就是账号密码登录换来的会话；
/// - 非 GET 请求必须同源（403 `MCP_FORBIDDEN`，不是 401）：cookie 会被浏览器自动带上，
///   不校验来源就等于允许任何站点借用户的会话改数据；
/// - 成功后注入 `AuthenticatedAdmin{ id: username, user_id }`：审计里的 actor 是账号 username，
///   而"删除/禁用自己"这类判断必须用不可改名的账号主键。
async fn require_admin_identity(
    State(state): State<AdminState>,
    mut req: Request,
    next: Next,
) -> Response {
    let actor = admin_actor("");
    let Some(presented) = session::cookie_value(req.headers(), session::SESSION_COOKIE) else {
        let e = McpError::new(
            "MCP_UNAUTHORIZED",
            json!({"hint": "管理面需要管理员会话：POST /admin/login 登录后再访问"}),
        );
        return ApiError::from_mcp_via_policy(&state, &actor, e).into_response();
    };
    let session = match state.store.admin_session(&presented) {
        Ok(session) => session,
        Err(e) => return ApiError::from_mcp_via_policy(&state, &actor, e).into_response(),
    };
    let Some(session) = session else {
        let e = McpError::new(
            "MCP_UNAUTHORIZED",
            json!({"hint": "管理台会话无效或已过期：请重新登录"}),
        );
        return ApiError::from_mcp_via_policy(&state, &actor, e).into_response();
    };
    // 会话吊销是改密/禁用的正常路径；这里再查一次账号状态，是为了让"漏吊销"退化成 401 而不是越权。
    let user = match state.store.admin_user(&session.admin_user_id) {
        Ok(Some(user)) if user.is_active() => user,
        Ok(_) => {
            let e = McpError::new(
                "MCP_UNAUTHORIZED",
                json!({"hint": "会话所属账号已被禁用或删除：请重新登录"}),
            );
            return ApiError::from_mcp_via_policy(&state, &actor, e).into_response();
        }
        Err(e) => return ApiError::from_mcp_via_policy(&state, &actor, e).into_response(),
    };
    if session::csrf_violation(req.method(), req.headers()) {
        let e = McpError::new(
            "MCP_FORBIDDEN",
            json!({
                "hint": "cookie 身份的非 GET 请求必须同源：Origin/Referer 的 host 需与 Host 一致",
            }),
        );
        return ApiError::from_mcp_via_policy(&state, &actor, e).into_response();
    }
    // last_seen 只是"最近活跃"信号，写失败不该把已认证的浏览器挡在外面。
    if let Err(e) = state.store.touch_admin_session(&session.id) {
        tracing::warn!(error = %e.message, "管理台会话 last_seen_at 更新失败");
    }
    req.extensions_mut().insert(AuthenticatedAdmin {
        id: user.username,
        user_id: user.id,
    });
    next.run(req).await
}

impl ApiError {
    fn from_mcp_via_policy(state: &AdminState, actor: &Actor, e: McpError) -> Self {
        reject(state, actor, e, json!({"surface": "admin"}))
    }
}

fn parse_page_param(
    state: &AdminState,
    actor: &Actor,
    surface: &str,
    field: &str,
    value: &str,
) -> Result<i64, ApiError> {
    value.parse::<i64>().map_err(|_| {
        reject(
            state,
            actor,
            McpError::new("MCP_BAD_REQUEST", json!({"field": field, "value": value})),
            json!({"surface": surface, "field": field}),
        )
    })
}

fn non_empty(value: &str) -> Value {
    if value.is_empty() {
        Value::Null
    } else {
        json!(value)
    }
}

/// 写端点的 fail-closed 前置：审计未启用就不写（与 MCP `require_audit` 同一口径）。
fn require_audit(state: &AdminState, actor: &Actor, surface: &str) -> Result<(), ApiError> {
    if state.service.audit.is_some() {
        return Ok(());
    }
    Err(reject(
        state,
        actor,
        McpError::new(
            "MCP_AUDIT_ERROR",
            json!({"hint": "审计未启用，维护通道拒绝写入"}),
        ),
        json!({"surface": surface}),
    ))
}

/// 供测试与文档引用的路由清单（避免文档与实现漂移）。
pub fn routes() -> Vec<(&'static str, &'static str)> {
    vec![
        ("GET", "/health"),
        ("POST", "/admin/login"),
        ("POST", "/admin/logout"),
        ("GET", "/admin/session"),
        ("GET", "/admin/me"),
        ("GET", "/admin/admins"),
        ("POST", "/admin/admins"),
        ("PATCH", "/admin/admins/{id}"),
        ("DELETE", "/admin/admins/{id}"),
        ("GET", "/admin/audit-events"),
        ("GET", "/admin/capabilities"),
        ("POST", "/admin/capabilities"),
        ("GET", "/admin/capabilities/{id}"),
        ("GET", "/admin/capabilities/{id}/versions"),
        ("PUT", "/admin/capabilities/{id}/definition"),
        ("GET", "/admin/roles"),
        ("GET", "/admin/groups"),
        ("POST", "/admin/groups"),
        ("PUT", "/admin/groups/{code}"),
        ("DELETE", "/admin/groups/{code}"),
        ("GET", "/admin/tokens"),
        ("POST", "/admin/tokens"),
        ("GET", "/admin/tokens/options"),
        ("POST", "/admin/tokens/{id}/revoke"),
        ("POST", "/admin/tokens/{id}/reissue"),
        ("DELETE", "/admin/tokens/{id}"),
        ("GET", "/admin/review-queue"),
        ("GET", "/admin/tools/stats"),
        ("GET", "/admin/tools/candidates"),
        ("GET", "/admin/tools/{id}/calls"),
        ("POST", "/admin/capabilities/{id}/submit"),
        ("POST", "/admin/capabilities/{id}/publish"),
        ("POST", "/admin/capabilities/{id}/reject"),
        ("POST", "/admin/capabilities/{id}/visibility"),
        ("POST", "/admin/capabilities/{id}/deprecate"),
        ("GET", "/admin/upstreams"),
        ("POST", "/admin/upstreams"),
        ("GET", "/admin/upstreams/{id}"),
        ("PUT", "/admin/upstreams/{id}"),
        ("DELETE", "/admin/upstreams/{id}"),
        ("POST", "/admin/upstreams/{id}/refresh"),
        ("GET", "/admin/upstreams/{id}/tools"),
        ("PUT", "/admin/upstreams/{id}/tools/basic"),
        ("GET", "/admin/mcp/catalog"),
        ("GET", "/admin/transport-key"),
        ("POST", "/admin/mcp/catalog/{id}/install"),
        ("DELETE", "/admin/mcp/catalog/{id}/install/{upstream_id}"),
        (
            "PATCH",
            "/admin/mcp/catalog/{id}/install/{upstream_id}/config",
        ),
    ]
}
