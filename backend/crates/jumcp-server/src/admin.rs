//! Admin HTTP API（W4.1 最小版）：审核队列 / 状态流转 / 可见性 / 审计查询 + 健康检查。
//!
//! 边界：
//! - 只在 streamable-http 模式挂载（stdio 无管理面）；未配置 `JUMCP_ADMIN_TOKENS` 时进程拒绝启动。
//! - 身份只能来自 token→actor 映射：`X-Admin-Actor` 仅用于一致性校验，永远以派生 id 为准。
//! - 全部拒绝写 `policy_decision`，成功写 `admin_change`（submit 在此层写，publish/visibility 由
//!   CapabilityStore 写，避免同一变更两条记录）。
//! - 审计查询只读：不提供任何 UPDATE/DELETE 入口，DB 层另有 append-only trigger 兜底。

use crate::service::Service;
use axum::extract::{Extension, Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use chrono::Utc;
use jumcp_core::access::{AccessTokenRecord, NewAccessToken};
use jumcp_core::audit::{AuditLog, ChainVerdict, Envelope, Order};
use jumcp_core::config::{
    AdminTokens, MAX_GROUPS, RESERVED_GROUP_NAMES, is_principal_id, is_reserved_group_name,
    validate_display_name, validate_group_name,
};
use jumcp_core::entry::Actor;
use jumcp_core::errors::McpError;
use jumcp_core::registry::{
    ADMIN_CAPABILITY_IDS, BUILTIN_CAPABILITY_IDS, CapabilityRecord, CapabilityStore, ManagedGroup,
    McpUpstream, STATUSES, is_builtin_id,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

const ACTOR_HEADER: &str = "x-admin-actor";
/// 审计查询单页上限：HTTP 管理面与 MCP `admin_audit_queries` 共用（夹紧而不是报错）。
pub(crate) const MAX_LIMIT: i64 = 200;
const DEFAULT_LIMIT: i64 = 50;
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
    pub tokens: AdminTokens,
}

/// 鉴权中间件派生出的管理主体：后续 handler 只用它，不再读 `X-Admin-Actor` 做身份。
#[derive(Clone, Debug)]
pub struct AuthenticatedAdmin {
    pub id: String,
}

/// 挂在 streamable-http 监听上的完整路由：`/health` 免鉴权，`/admin/*` 必须带 Bearer token。
pub fn router(state: AdminState) -> Router {
    let admin = Router::new()
        .route("/me", get(me))
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
        .route("/tools/{id}/calls", get(tool_calls))
        .route("/upstreams", get(list_upstreams).post(create_upstream))
        .route(
            "/upstreams/{id}",
            get(get_upstream)
                .put(update_upstream)
                .delete(delete_upstream),
        )
        .route("/upstreams/{id}/refresh", post(refresh_upstream))
        .route("/capabilities/{id}/submit", post(submit))
        .route("/capabilities/{id}/publish", post(publish))
        .route("/capabilities/{id}/reject", post(reject_capability))
        .route("/capabilities/{id}/visibility", post(visibility))
        .route("/capabilities/{id}/deprecate", post(deprecate_capability))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_token,
        ));
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
        "MCP_APPROVAL_REQUIRED"
        | "MCP_VALIDATION_ERROR"
        | "MCP_BAD_REQUEST"
        | "MCP_FILTER_UNSUPPORTED"
        | "MCP_TEMPLATE_INVALID"
        | "MCP_PREFLIGHT_REQUIRED"
        | "MCP_SCHEMA_OUT_OF_SCOPE"
        | "MCP_UPSTREAM_INVALID"
        | "MCP_UPSTREAM_WRITE_FORBIDDEN"
        | "MCP_UPSTREAM_STDIO_DISABLED" => StatusCode::BAD_REQUEST,
        "MCP_STATE_TRANSITION_INVALID" | "MCP_VERSION_CONFLICT" | "MCP_BUILTIN_IMMUTABLE" => {
            StatusCode::CONFLICT
        }
        "MCP_GROUP_CONFLICT" | "MCP_GROUP_RESERVED" | "MCP_GROUP_IN_USE" => StatusCode::CONFLICT,
        "MCP_UPSTREAM_CONFLICT" => StatusCode::CONFLICT,
        "MCP_TOKEN_ALREADY_REVOKED" => StatusCode::CONFLICT,
        "MCP_CAPABILITY_NOT_FOUND"
        | "MCP_GROUP_NOT_FOUND"
        | "MCP_TOKEN_NOT_FOUND"
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

/// `Authorization: Bearer <token>` 的 token 部分；MCP 入口复用同一解析，避免两套鉴权格式。
pub(crate) fn bearer(headers: &HeaderMap) -> Option<&str> {
    let raw = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let (scheme, token) = raw.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
}

/// 管理面主体：静态 admin token 派生，来源固定 `admin-api`（审计可区分管理面与 MCP token）。
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

/// 统一审计载荷骨架（单一来源在 `jumcp_core::audit::payload`）。
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

async fn require_token(State(state): State<AdminState>, mut req: Request, next: Next) -> Response {
    let derived = bearer(req.headers()).and_then(|token| state.tokens.authenticate(token));
    let Some(id) = derived else {
        let actor = admin_actor("");
        let e = McpError::new(
            "MCP_UNAUTHORIZED",
            json!({"hint": "管理面需要 Authorization: Bearer <JUMCP_ADMIN_TOKENS 中的 token>"}),
        );
        return ApiError::from_mcp_via_policy(&state, &actor, e).into_response();
    };
    req.extensions_mut()
        .insert(AuthenticatedAdmin { id: id.to_string() });
    next.run(req).await
}

impl ApiError {
    fn from_mcp_via_policy(state: &AdminState, actor: &Actor, e: McpError) -> Self {
        reject(state, actor, e, json!({"surface": "admin"}))
    }
}

// ---------------------------- 处理器 ----------------------------

/// `GET /admin/me`：当前 token 派生出的管理主体（前端据此显示操作者，不再自述身份）。
async fn me(Extension(auth): Extension<AuthenticatedAdmin>) -> Json<Value> {
    Json(json!({"id": auth.id, "role": "admin", "type": "admin"}))
}

async fn health(State(state): State<AdminState>) -> Json<Value> {
    let settings = &state.service.settings;
    let datasources: Vec<Value> = settings
        .datasources
        .iter()
        .map(|d| json!({"name": d.name, "engine": d.engine, "readonly": d.readonly}))
        .collect();
    // 链被篡改时 status 仍 200（服务在跑），但 audit_chain 显式报警，供看板直接着色。
    let (chain, tampered_at) = match &state.service.audit {
        Some(log) => match log.verify_chain() {
            Ok(ChainVerdict::Valid) => ("valid", Value::Null),
            Ok(ChainVerdict::Tampered { event_id }) => ("tampered", json!(event_id)),
            Ok(ChainVerdict::Broken { reason }) => ("error", json!(reason)),
            Err(e) => ("error", json!(e.message)),
        },
        None => ("error", json!("audit log not configured")),
    };
    let mut body = json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "datasources": datasources,
        "audit_chain": chain
    });
    if !tampered_at.is_null() {
        body["audit_chain_detail"] = tampered_at;
    }
    Json(body)
}

async fn audit_events(
    State(state): State<AdminState>,
    Query(raw): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let actor = admin_actor("");
    let mut filters = Map::new();
    let mut limit = DEFAULT_LIMIT;
    let mut offset = 0i64;
    let mut order = Order::Desc;
    for (key, value) in raw {
        match key.as_str() {
            "limit" => {
                limit = value.parse::<i64>().map_err(|_| {
                    ApiError(McpError::new(
                        "MCP_BAD_REQUEST",
                        json!({"field": "limit", "value": value}),
                    ))
                })?;
            }
            "offset" => {
                offset = value.parse::<i64>().map_err(|_| {
                    ApiError(McpError::new(
                        "MCP_BAD_REQUEST",
                        json!({"field": "offset", "value": value}),
                    ))
                })?;
            }
            "order" => {
                order = Order::parse(&value).map_err(|e| {
                    reject(
                        &state,
                        &actor,
                        e,
                        json!({"surface": "admin/audit-events", "field": "order"}),
                    )
                })?;
            }
            other if FILTER_KEYS.contains(&other) => {
                filters.insert(other.to_string(), json!(value));
            }
            _ => {
                let e = McpError::new(
                    "MCP_FILTER_UNSUPPORTED",
                    json!({"filter": key, "supported": FILTER_KEYS}),
                );
                return Err(reject(
                    &state,
                    &actor,
                    e,
                    json!({"surface": "admin", "filter": key}),
                ));
            }
        }
    }
    // 超上限夹紧而不是报错：管理面分页是展示需求，不构成契约破坏。
    let limit = limit.clamp(1, MAX_LIMIT);
    let offset = offset.max(0);
    let Some(log) = state.service.audit.clone() else {
        return Err(ApiError(McpError::new(
            "MCP_AUDIT_ERROR",
            json!({"hint": "审计未启用"}),
        )));
    };
    let (events, total) = log
        .query_page(Some(&Value::Object(filters)), limit, offset, order)
        .map_err(|e| reject(&state, &actor, e, json!({"surface": "admin/audit-events"})))?;
    Ok(Json(json!({
        "events": events,
        "total": total,
        "limit": limit,
        "offset": offset,
        "order": if order == Order::Asc { "asc" } else { "desc" }
    })))
}

fn non_empty(value: &str) -> Value {
    if value.is_empty() {
        Value::Null
    } else {
        json!(value)
    }
}

/// 审核队列条目的唯一形状：HTTP 管理面与 MCP `admin_review_queue` 共用。
pub(crate) fn queue_item(rec: &CapabilityRecord) -> Value {
    json!({
        "id": rec.capability_id,
        "kind": rec.kind,
        "group": rec.group,
        "risk": rec.risk,
        "status": rec.status,
        "version": rec.version,
        "definition_hash": rec.definition_hash,
        "submitted_by": non_empty(&rec.submitted_by),
        "submitted_at": non_empty(&rec.submitted_at),
        "last_changed_at": non_empty(&rec.updated_at)
    })
}

async fn review_queue(State(state): State<AdminState>) -> Result<Json<Value>, ApiError> {
    let actor = admin_actor("");
    let records = state
        .store
        .review_queue()
        .map_err(|e| reject(&state, &actor, e, json!({"surface": "admin/review-queue"})))?;
    let items: Vec<Value> = records.iter().map(queue_item).collect();
    Ok(Json(json!({"items": items, "total": items.len()})))
}

fn capability_item(rec: &CapabilityRecord) -> Value {
    json!({
        "id": rec.capability_id,
        "kind": rec.kind,
        "group": rec.group,
        "risk": rec.risk,
        "status": rec.status,
        "discoverable": rec.discoverable,
        "callable": rec.callable,
        "version": rec.version,
        "definition_hash": rec.definition_hash,
        "successor_id": rec.successor_id,
        "last_changed_at": non_empty(&rec.updated_at),
        "submitted_by": non_empty(&rec.submitted_by),
        "submitted_at": non_empty(&rec.submitted_at)
    })
}

/// `GET /admin/capabilities/{id}`：摘要字段 + `definition` 原文。
///
/// 只读：不写审计（读详情不构成治理变更）。内置 id 没有注册表行 → 404（与不存在同构）。
async fn capability_detail(
    State(state): State<AdminState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let record = state.store.get(&id).map_err(ApiError)?.ok_or_else(|| {
        ApiError(McpError::new(
            "MCP_CAPABILITY_NOT_FOUND",
            json!({"capability_id": id}),
        ))
    })?;
    let mut item = capability_item(&record);
    item["definition"] = record.definition;
    item["definition_updated_at"] = state
        .store
        .latest_version_created_at(&id)
        .map_err(ApiError)?
        .map_or(Value::Null, |s| non_empty(&s));
    Ok(Json(item))
}

/// `GET /admin/capabilities/{id}/versions`：版本历史（升序）。没有版本行时给空数组。
async fn capability_versions(
    State(state): State<AdminState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let rows = state.store.versions(&id).map_err(ApiError)?;
    let items: Vec<Value> = rows
        .iter()
        .map(|v| {
            json!({
                "version": v.version,
                "definition": v.definition,
                "definition_hash": v.definition_hash,
                "status": non_empty(&v.status),
                "created_at": non_empty(&v.created_at),
                "submitted_by": non_empty(&v.submitted_by),
                "submitted_at": non_empty(&v.submitted_at),
                "approved_by": non_empty(&v.approved_by),
                "approved_at": non_empty(&v.approved_at),
                "approval_id": non_empty(&v.approval_id)
            })
        })
        .collect();
    Ok(Json(json!({"items": items, "total": items.len()})))
}

#[derive(Debug, Deserialize)]
struct DefinitionBody {
    definition: Option<Value>,
    approval_id: Option<String>,
}

/// 模板定义可能以两种形状进入：注册源形状（顶层 `template_id`/`sql`）或详情端点返回的
/// 存储形状（`{"template": {...}, "base_sql": ..., "dialect": ...}`）。
///
/// 判定以"存在 `template` 对象"为准，统一抽出 `TemplateDef` 源定义再交给 TemplateRegistry；
/// 否则前端"GET 详情 → 原样回写"的流程会被 `deny_unknown_fields` 判成非法定义。
fn template_source(definition: &Value) -> Value {
    definition
        .get("template")
        .filter(|v| v.is_object())
        .cloned()
        .unwrap_or_else(|| definition.clone())
}

/// `PUT /admin/capabilities/{id}/definition`：修订能力定义（版本 +1，历史 append-only）。
///
/// 状态规则（唯一判定点，registry 不做策略层判断）：
/// - `draft`：直接改，状态保持 draft；`approval_id` 可选，仅作外部票号留痕；
/// - `published`：必须带 `approval_id`；批准人一律取 token 派生 id（可与提审人相同）；
/// - `review`：必须先退回 draft；`deprecated` / 其它：拒绝。
///
/// `kind == "template"` 一律走 `TemplateRegistry`（parse → 编译 → sqlguard），
/// 管理面无法绕过模板 SQL 门禁；`template_id` 必须与路径 id 一致。
async fn update_definition(
    State(state): State<AdminState>,
    Extension(auth): Extension<AuthenticatedAdmin>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<DefinitionBody>,
) -> Result<Json<Value>, ApiError> {
    const SURFACE: &str = "admin/capabilities/definition";
    let actor = actor_of(&auth, &headers)
        .map_err(|e| reject(&state, &admin_actor(""), e, json!({"surface": SURFACE})))?;
    // 定义变更是治理变更：审计未启用即整条拒绝。
    require_audit(&state, &actor, SURFACE)?;
    if is_builtin_id(&id) {
        return Err(reject(
            &state,
            &actor,
            McpError::new("MCP_BUILTIN_IMMUTABLE", json!({"capability_id": id})),
            json!({"capability_id": id}),
        ));
    }
    let Some(definition) = body.definition.filter(Value::is_object) else {
        return Err(reject(
            &state,
            &actor,
            McpError::validation("definition 必须是 JSON 对象".to_string()),
            json!({"surface": SURFACE, "capability_id": id}),
        ));
    };
    let record = state
        .store
        .get(&id)
        .map_err(|e| reject(&state, &actor, e, json!({"capability_id": id})))?
        .ok_or_else(|| {
            reject(
                &state,
                &actor,
                McpError::new("MCP_CAPABILITY_NOT_FOUND", json!({"capability_id": id})),
                json!({"capability_id": id}),
            )
        })?;
    let approval_id = body
        .approval_id
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    // 定义没有实际变化：不插版本、不写事件（避免为"点了一次保存"制造噪声）。
    if record.kind != "template"
        && jumcp_core::canonical::definition_hash(&definition) == record.definition_hash
    {
        return Ok(Json(json!({
            "id": record.capability_id,
            "status": record.status,
            "version": record.version,
            "definition_hash": record.definition_hash,
            "discoverable": record.discoverable,
            "callable": record.callable,
            "last_changed_at": non_empty(&record.updated_at)
        })));
    }
    let updated = match record.status.as_str() {
        "draft" => {
            let approval = approval_id.unwrap_or_default();
            if record.kind == "template" {
                let registry = state.service.templates.as_ref().ok_or_else(|| {
                    ApiError(McpError::new(
                        "MCP_STORE_ERROR",
                        json!({"hint": "模板注册表未注入（TemplateRegistry 未就绪）"}),
                    ))
                })?;
                registry
                    .revise_draft(&id, &template_source(&definition), &approval, &actor)
                    .map_err(|e| reject(&state, &actor, e, json!({"capability_id": id})))?;
                state
                    .store
                    .get(&id)
                    .map_err(|e| reject(&state, &actor, e, json!({"capability_id": id})))?
                    .ok_or_else(|| {
                        ApiError(McpError::store(format!("revise_draft 后记录缺失: {id}")))
                    })?
            } else {
                state
                    .store
                    .revise_draft_definition(&id, definition, &approval, &actor)
                    .map_err(|e| reject(&state, &actor, e, json!({"capability_id": id})))?
            }
        }
        "published" => {
            let approval = approval_id.ok_or_else(|| {
                reject(
                    &state,
                    &actor,
                    McpError::new(
                        "MCP_APPROVAL_REQUIRED",
                        json!({"capability_id": id, "missing": ["approval_id"]}),
                    ),
                    json!({"capability_id": id}),
                )
            })?;
            // 批准人（当前 token 主体）由鉴权派生，允许与提审人相同。
            if record.kind == "template" {
                let registry = state.service.templates.as_ref().ok_or_else(|| {
                    ApiError(McpError::new(
                        "MCP_STORE_ERROR",
                        json!({"hint": "模板注册表未注入（TemplateRegistry 未就绪）"}),
                    ))
                })?;
                let source = template_source(&definition);
                let receipt = registry
                    .preflight(&source)
                    .map_err(|e| reject(&state, &actor, e, json!({"capability_id": id})))?;
                registry
                    .publish_version(
                        &id,
                        &source,
                        &approval,
                        &record.submitted_by,
                        &actor,
                        &receipt,
                    )
                    .map_err(|e| reject(&state, &actor, e, json!({"capability_id": id})))?;
                state
                    .store
                    .get(&id)
                    .map_err(|e| reject(&state, &actor, e, json!({"capability_id": id})))?
                    .ok_or_else(|| {
                        ApiError(McpError::store(format!("publish_version 后记录缺失: {id}")))
                    })?
            } else {
                state
                    .store
                    .publish_version(&id, definition, &approval, &record.submitted_by, &actor)
                    .map_err(|e| reject(&state, &actor, e, json!({"capability_id": id})))?
            }
        }
        "review" => {
            return Err(reject(
                &state,
                &actor,
                McpError::new(
                    "MCP_STATE_TRANSITION_INVALID",
                    json!({"capability_id": id, "from": "review", "to": "draft", "hint": "review 状态必须先退回 draft 才能改定义"}),
                ),
                json!({"capability_id": id}),
            ));
        }
        other => {
            return Err(reject(
                &state,
                &actor,
                McpError::new(
                    "MCP_STATE_TRANSITION_INVALID",
                    json!({"capability_id": id, "from": other, "to": "draft|published"}),
                ),
                json!({"capability_id": id}),
            ));
        }
    };
    Ok(Json(json!({
        "id": updated.capability_id,
        "status": updated.status,
        "version": updated.version,
        "definition_hash": updated.definition_hash,
        "discoverable": updated.discoverable,
        "callable": updated.callable,
        "last_changed_at": non_empty(&updated.updated_at)
    })))
}

/// 列表页只需要摘要：description 参与 `q` 匹配，但完整 definition 不回传（避免把 schema 塞进管理面）。
fn description_of(rec: &CapabilityRecord) -> String {
    rec.definition
        .get("description")
        .and_then(|d| d.as_str())
        .or_else(|| {
            rec.definition
                .get("template")
                .and_then(|t| t.get("description"))
                .and_then(|d| d.as_str())
        })
        .unwrap_or_default()
        .to_string()
}

async fn capabilities(
    State(state): State<AdminState>,
    Query(raw): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let actor = admin_actor("");
    let mut statuses: Vec<String> = Vec::new();
    let mut group: Option<String> = None;
    let mut query: Option<String> = None;
    let mut limit = DEFAULT_LIMIT;
    let mut offset = 0i64;
    for (key, value) in raw {
        match key.as_str() {
            "status" => {
                statuses = value
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect();
                if let Some(bad) = statuses.iter().find(|s| !STATUSES.contains(&s.as_str())) {
                    let e = McpError::new(
                        "MCP_BAD_REQUEST",
                        json!({"field": "status", "value": bad, "supported": STATUSES}),
                    );
                    return Err(reject(
                        &state,
                        &actor,
                        e,
                        json!({"surface": "admin/capabilities", "field": "status"}),
                    ));
                }
            }
            "group" => group = Some(value),
            "q" => query = Some(value.to_lowercase()),
            "limit" => {
                limit = parse_page_param(&state, &actor, "admin/capabilities", "limit", &value)?
            }
            "offset" => {
                offset = parse_page_param(&state, &actor, "admin/capabilities", "offset", &value)?
            }
            _ => {
                let e = McpError::new(
                    "MCP_FILTER_UNSUPPORTED",
                    json!({"filter": key, "supported": ["status","group","q","limit","offset"]}),
                );
                return Err(reject(
                    &state,
                    &actor,
                    e,
                    json!({"surface": "admin/capabilities", "filter": key}),
                ));
            }
        }
    }
    let limit = limit.clamp(1, MAX_LIMIT);
    let offset = offset.max(0);
    let all = state
        .store
        .all()
        .map_err(|e| reject(&state, &actor, e, json!({"surface": "admin/capabilities"})))?;
    let filtered: Vec<&CapabilityRecord> = all
        .iter()
        .filter(|rec| statuses.is_empty() || statuses.contains(&rec.status))
        .filter(|rec| group.as_ref().is_none_or(|g| &rec.group == g))
        .filter(|rec| {
            query.as_ref().is_none_or(|needle| {
                rec.capability_id.to_lowercase().contains(needle)
                    || description_of(rec).to_lowercase().contains(needle)
            })
        })
        .collect();
    let total = filtered.len() as i64;
    let items: Vec<Value> = filtered
        .into_iter()
        .skip(offset as usize)
        .take(limit as usize)
        .map(capability_item)
        .collect();
    Ok(Json(json!({
        "items": items,
        "total": total,
        "limit": limit,
        "offset": offset
    })))
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

/// draft → review。registry 不写 admin_change（状态推进本身不是治理变更），在此层补记。
async fn submit(
    State(state): State<AdminState>,
    Extension(auth): Extension<AuthenticatedAdmin>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let actor = actor_of(&auth, &headers).map_err(|e| {
        reject(
            &state,
            &admin_actor(""),
            e,
            json!({"surface": "admin/submit"}),
        )
    })?;
    // 审计未启用就不写：submit 会推进状态并写 admin_change，缺审计即整条拒绝。
    require_audit(&state, &actor, "admin/submit")?;
    let before = state
        .store
        .get(&id)
        .map_err(|e| reject(&state, &actor, e, json!({"capability_id": id})))?
        .ok_or_else(|| {
            reject(
                &state,
                &actor,
                McpError::new("MCP_CAPABILITY_NOT_FOUND", json!({"capability_id": id})),
                json!({"capability_id": id}),
            )
        })?;
    let record = state
        .store
        .submit_for_review(&id, &actor.actor_id)
        .map_err(|e| reject(&state, &actor, e, json!({"capability_id": id})))?;
    let mut payload = audit_base("capability", &record.capability_id, "submit", "ok");
    payload.insert("from_status".into(), json!(before.status));
    payload.insert("to_status".into(), json!(record.status));
    payload.insert(
        "changes".into(),
        json!([jumcp_core::registry::change(
            "status",
            json!(before.status),
            json!(record.status)
        )]),
    );
    payload.insert(
        "data".into(),
        json!({
            "submitted_by": actor.actor_id,
            "version": record.version,
            "definition_hash_before": record.definition_hash,
            "definition_hash_after": record.definition_hash
        }),
    );
    state
        .service
        .audit_event("admin_change", Value::Object(payload), &actor)
        .map_err(|e| reject(&state, &actor, e, json!({"capability_id": id})))?;
    Ok(Json(json!({
        "id": record.capability_id,
        "status": record.status,
        "version": record.version,
        "last_changed_at": non_empty(&record.updated_at),
        "submitted_by": non_empty(&record.submitted_by),
        "submitted_at": non_empty(&record.submitted_at)
    })))
}

#[derive(Debug, Deserialize)]
struct PublishBody {
    approval_id: Option<String>,
}

/// review → published。`approval_id` 必填，批准人取 token 派生身份。
async fn publish(
    State(state): State<AdminState>,
    Extension(auth): Extension<AuthenticatedAdmin>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<PublishBody>,
) -> Result<Json<Value>, ApiError> {
    let actor = actor_of(&auth, &headers).map_err(|e| {
        reject(
            &state,
            &admin_actor(""),
            e,
            json!({"surface": "admin/publish"}),
        )
    })?;
    // 审计未启用就不写：发布是最关键的治理变更，缺审计必须整条拒绝。
    require_audit(&state, &actor, "admin/publish")?;
    let approval_id = body
        .approval_id
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            reject(
                &state,
                &actor,
                McpError::new("MCP_APPROVAL_REQUIRED", json!({"missing": ["approval_id"]})),
                json!({"capability_id": id}),
            )
        })?;
    // 批准人一律取 token 派生 id：请求体不再接受 approved_by。
    let record = state
        .store
        .promote_to_published(&id, &approval_id, &actor)
        .map_err(|e| reject(&state, &actor, e, json!({"capability_id": id})))?;
    Ok(Json(json!({
        "id": record.capability_id,
        "status": record.status,
        "version": record.version,
        "discoverable": record.discoverable,
        "callable": record.callable,
        "last_changed_at": non_empty(&record.updated_at)
    })))
}

#[derive(Debug, Deserialize)]
struct RejectBody {
    reason: Option<String>,
}

/// `POST /admin/capabilities/{id}/reject`：review → draft。
/// registry 不写 admin_change（与 submit 同口径），在此层补记并把 reason 放进 payload。
async fn reject_capability(
    State(state): State<AdminState>,
    Extension(auth): Extension<AuthenticatedAdmin>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<RejectBody>,
) -> Result<Json<Value>, ApiError> {
    const SURFACE: &str = "admin/capabilities/reject";
    let actor = actor_of(&auth, &headers)
        .map_err(|e| reject(&state, &admin_actor(""), e, json!({"surface": SURFACE})))?;
    // 审计未启用就不写：退回会推进状态并写 admin_change，缺审计即整条拒绝。
    require_audit(&state, &actor, SURFACE)?;
    let reason = body.reason.unwrap_or_default().trim().to_string();
    // 空原因/超长由 store 拒绝（唯一判定点）；这里只负责把 trim 后的值送审计与响应。
    let record = state
        .store
        .reject_review(&id, &reason)
        .map_err(|e| reject(&state, &actor, e, json!({"capability_id": id})))?;
    let mut payload = audit_base("capability", &record.capability_id, "reject", "ok");
    payload.insert("from_status".into(), json!("review"));
    payload.insert("to_status".into(), json!(record.status));
    payload.insert(
        "changes".into(),
        json!([jumcp_core::registry::change(
            "status",
            json!("review"),
            json!(record.status)
        )]),
    );
    payload.insert("reason".into(), json!(reason));
    payload.insert(
        "data".into(),
        json!({
            "version": record.version,
            "definition_hash_before": record.definition_hash,
            "definition_hash_after": record.definition_hash
        }),
    );
    state
        .service
        .audit_event("admin_change", Value::Object(payload), &actor)
        .map_err(|e| reject(&state, &actor, e, json!({"capability_id": id})))?;
    Ok(Json(json!({
        "id": record.capability_id,
        "status": record.status,
        "version": record.version,
        "last_changed_at": non_empty(&record.updated_at),
        "reason": reason
    })))
}

#[derive(Debug, Deserialize)]
struct VisibilityBody {
    discoverable: Option<bool>,
    callable: Option<bool>,
}

async fn visibility(
    State(state): State<AdminState>,
    Extension(auth): Extension<AuthenticatedAdmin>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<VisibilityBody>,
) -> Result<Json<Value>, ApiError> {
    let actor = actor_of(&auth, &headers).map_err(|e| {
        reject(
            &state,
            &admin_actor(""),
            e,
            json!({"surface": "admin/visibility"}),
        )
    })?;
    // 审计未启用就不写：可见性变更同样必须在审计链上留痕。
    require_audit(&state, &actor, "admin/visibility")?;
    let record = state
        .store
        .set_visibility(&id, body.discoverable, body.callable, &actor)
        .map_err(|e| reject(&state, &actor, e, json!({"capability_id": id})))?;
    Ok(Json(json!({
        "id": record.capability_id,
        "discoverable": record.discoverable,
        "callable": record.callable,
        "version": record.version,
        "last_changed_at": non_empty(&record.updated_at)
    })))
}

// ---------------------------- 工具统计（审计聚合） ----------------------------

/// 统计窗口默认/上限（天）：越界夹紧而不是报错。
const STATS_DEFAULT_WINDOW_DAYS: i64 = 7;
const STATS_MAX_WINDOW_DAYS: i64 = 30;
/// 统计列表默认条数（上限与审计查询共用 `MAX_LIMIT`）。
const STATS_DEFAULT_LIMIT: i64 = 200;
/// 单次审计扫描上限：任一事件类型命中该上限即把 `truncated` 置 true（窗口过大时如实标记，不静默）。
const STATS_SCAN_LIMIT: i64 = 50_000;
/// `recent_tokens` 保留最近多少个非空 token 样本。
const RECENT_TOKENS: usize = 20;
/// 单 id 明细的默认/上限窗口（天）：审计表只追加，明细必须带时间边界。
const CALLS_DEFAULT_WINDOW_DAYS: i64 = 30;
const CALLS_MAX_WINDOW_DAYS: i64 = 90;

/// 一条审计事件归一化成"一次能力调用"。
///
/// `tool_call` 的能力 id 取 `payload.tool`（任意非空工具名：常驻工具、维护工具以及未来扩展的
/// 内置工具都自动纳入台账），`template_run` 取 `payload.template_id`；其它事件类型与缺 id 的
/// 行直接丢弃。`capability_search` / `capability_describe` 是独立事件类型，不算能力调用。
/// 能力统计与单 id 明细共用这一处解析，避免两个接口对同一批事件给出不同结论。
#[derive(Debug, Clone)]
struct CallObservation {
    id: String,
    event_id: String,
    ts: String,
    session_id: String,
    event_type: String,
    object_type: String,
    actor: Value,
    outcome: String,
    tokens: Option<i64>,
    cache: Option<String>,
    elapsed_ms: Option<i64>,
    version: Option<i64>,
    error_code: Option<String>,
    error_message: Option<String>,
    arguments: Value,
    executed_script: Option<String>,
    bound_params: Value,
}

/// 调用对象 id：只读新载荷的 `object_id`（旧行没有该字段，不参与台账/明细）。
fn observation_id(env: &Envelope) -> Option<&str> {
    match env.event_type.as_str() {
        "template_run" | "tool_call" => env
            .payload
            .get("object_id")?
            .as_str()
            .filter(|id| !id.is_empty()),
        _ => None,
    }
}

fn observation(env: &Envelope) -> Option<CallObservation> {
    if !matches!(env.event_type.as_str(), "tool_call" | "template_run") {
        return None;
    }
    let id = observation_id(env)?.to_string();
    let data = env
        .payload
        .get("data")
        .cloned()
        .unwrap_or_else(|| json!({}));
    Some(CallObservation {
        id,
        event_id: env.event_id.clone(),
        ts: env.ts.clone(),
        session_id: env.session_id.clone(),
        event_type: env.event_type.clone(),
        object_type: env
            .payload
            .get("object_type")
            .and_then(Value::as_str)
            .unwrap_or("tool")
            .to_string(),
        actor: env.actor.clone(),
        outcome: env
            .payload
            .get("outcome")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string(),
        tokens: data.get("tokens").and_then(|v| v.as_i64()),
        cache: data.get("cache").and_then(|v| v.as_str()).map(String::from),
        elapsed_ms: data.get("elapsed_ms").and_then(|v| v.as_i64()),
        version: data.get("version").and_then(|v| v.as_i64()),
        error_code: env
            .payload
            .get("error_code")
            .and_then(|v| v.as_str())
            .map(String::from),
        error_message: env
            .payload
            .get("error_message")
            .and_then(|v| v.as_str())
            .map(String::from),
        arguments: data.get("arguments").cloned().unwrap_or(Value::Null),
        executed_script: data
            .get("executed_script")
            .and_then(|v| v.as_str())
            .map(String::from),
        bound_params: data.get("bound_params").cloned().unwrap_or(Value::Null),
    })
}

/// 窗口内按事件类型扫描（单次上限 `STATS_SCAN_LIMIT`）；返回 (观测, 是否命中扫描上限)。
fn scan_in_window(
    log: &AuditLog,
    event_type: &str,
    ts_after: &str,
) -> Result<(Vec<CallObservation>, bool), McpError> {
    let filter = json!({"event_type": event_type, "ts_after": ts_after});
    let (rows, _total) = log.query_page(Some(&filter), STATS_SCAN_LIMIT, 0, Order::Asc)?;
    let truncated = rows.len() as i64 >= STATS_SCAN_LIMIT;
    Ok((rows.iter().filter_map(observation).collect(), truncated))
}

/// 单个 id 的明细扫描：按事件类型翻页，只保留该 id 的事件。
///
/// 为什么带时间窗：审计表只追加，不带边界的明细查询会随运行时间线性变慢。
/// 为什么不用一次全表 `query`：明细页不该把整张审计表读进内存；翻页时逐页过滤，
/// 内存只与"这个 id 在窗口内的调用次数"成正比，`total` 仍是精确值。
/// 返回 `Envelope` 而非投影：明细要原样回放入参/脚本，投影会丢掉 payload 其余内容。
fn scan_by_id(
    log: &AuditLog,
    event_type: &str,
    id: &str,
    ts_after: &str,
) -> Result<Vec<Envelope>, McpError> {
    const PAGE: i64 = 5_000;
    let filter = json!({"event_type": event_type, "ts_after": ts_after});
    let mut out: Vec<Envelope> = Vec::new();
    let mut offset = 0i64;
    loop {
        let (rows, _total) = log.query_page(Some(&filter), PAGE, offset, Order::Asc)?;
        let got = rows.len() as i64;
        out.extend(
            rows.into_iter()
                .filter(|env| observation_id(env) == Some(id)),
        );
        if got < PAGE {
            return Ok(out);
        }
        offset += got;
    }
}

/// 统计对象的元信息：注册表全部行 + 全部 builtin（含 admin_*）+ 仅出现在审计里的 id。
struct ToolMeta {
    kind: String,
    group: String,
    /// 分类标签（`basic|analysis|admin|custom`）：统计台账与发现面同一映射。
    category: String,
    lifecycle_status: String,
    risk: Option<String>,
    discoverable: Option<bool>,
    callable: Option<bool>,
    version: Option<i64>,
    last_changed_at: Option<String>,
    source: String,
}

fn tool_universe(store: &CapabilityStore) -> Result<HashMap<String, ToolMeta>, McpError> {
    let mut out: HashMap<String, ToolMeta> = HashMap::new();
    // builtin 定义只在代码里：分组沿用发现面的同一份常量（常驻="builtin"，维护="admin"）。
    for id in BUILTIN_CAPABILITY_IDS {
        let group = if ADMIN_CAPABILITY_IDS.contains(id) {
            crate::discovery::ADMIN_GROUP
        } else {
            crate::discovery::BUILTIN_GROUP
        };
        out.insert(
            (*id).to_string(),
            ToolMeta {
                kind: "tool".into(),
                group: group.into(),
                lifecycle_status: "published".into(),
                category: crate::mcp::builtin_category(id).to_string(),
                risk: Some("read".into()),
                discoverable: Some(true),
                callable: Some(true),
                version: None,
                last_changed_at: None,
                source: "builtin".into(),
            },
        );
    }
    // 注册表行不能影子覆盖 builtin（store 本身也拒写 builtin id）。
    for rec in store.all()? {
        out.entry(rec.capability_id).or_insert(ToolMeta {
            kind: rec.kind,
            group: rec.group,
            category: crate::catalog::CATEGORY_ANALYSIS.to_string(),
            lifecycle_status: rec.status,
            risk: Some(rec.risk),
            discoverable: Some(rec.discoverable),
            callable: Some(rec.callable),
            version: Some(rec.version),
            last_changed_at: (!rec.updated_at.is_empty()).then_some(rec.updated_at),
            source: "managed".into(),
        });
    }
    Ok(out)
}

#[derive(Default)]
struct ToolAggregate {
    calls: i64,
    success: i64,
    failure: i64,
    cache_hits: i64,
    retries: i64,
    /// 非空 tokens 样本，按时间升序。
    tokens: Vec<i64>,
    last_call_at: Option<String>,
    last_outcome: Option<String>,
}

/// 单个能力的聚合口径（契约，测试锁定）：
/// - success = outcome ∈ {ok, summary}；failure = outcome ∈ {error, unknown, budget_rejected}；
///   其余 outcome 只计入 calls，不臆造成成功或失败。
/// - retries：按 `(session_id, id)` 分组（组内按时间升序），某次 outcome ∈ {error, unknown}
///   **且该会话内存在紧邻的下一次调用**时计 1（失败后确实在同一轮里又试了一次）；
///   其它会话的调用不得吞掉这个判定（`list` 是全局时间序，必须先分组）。
/// - tokens：按时间升序收集；`recent_tokens` 取最后 `RECENT_TOKENS` 个（保持时间升序）。
fn aggregate(list: &[CallObservation]) -> ToolAggregate {
    let mut agg = ToolAggregate {
        calls: list.len() as i64,
        ..Default::default()
    };
    for o in list {
        match o.outcome.as_str() {
            "ok" | "summary" => agg.success += 1,
            "error" | "unknown" | "budget_rejected" => agg.failure += 1,
            _ => {}
        }
    }
    agg.cache_hits = list
        .iter()
        .filter(|o| o.cache.as_deref() == Some("hit"))
        .count() as i64;
    agg.tokens = list.iter().filter_map(|o| o.tokens).collect();
    agg.last_call_at = list.last().map(|o| o.ts.clone());
    agg.last_outcome = list.last().map(|o| o.outcome.clone());
    // 重试必须按 `(session_id, id)` 分组后判：`list` 是全局按 ts 排序的，其它会话的调用
    // 会插在两次尝试之间；只有组内（已按时间升序）紧邻的下一次才算"同一轮内的再次尝试"。
    let mut by_session: HashMap<&str, Vec<&CallObservation>> = HashMap::new();
    for o in list {
        by_session.entry(o.session_id.as_str()).or_default().push(o);
    }
    agg.retries = by_session
        .values()
        .map(|seq| {
            seq.windows(2)
                .filter(|w| matches!(w[0].outcome.as_str(), "error" | "unknown"))
                .count() as i64
        })
        .sum();
    agg
}

fn round_to(value: f64, digits: i32) -> f64 {
    let factor = 10f64.powi(digits);
    (value * factor).round() / factor
}

/// 最近秩法：rank = ceil(0.95 * n)，取第 rank 小（1-based）。样本非空。
///
/// `samples` 是按时间收集的，取分位数前必须复制并升序排序；直接按原序取位会得到
/// “第几次调用”的 token 数而不是分位数。
fn percentile95(samples: &[i64]) -> i64 {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let n = sorted.len();
    let rank = (95 * n).div_ceil(100);
    sorted[rank.saturating_sub(1).min(n - 1)]
}

fn health_of(calls: i64, success: i64) -> &'static str {
    if calls == 0 {
        return "unknown";
    }
    let rate = success as f64 / calls as f64;
    if rate >= 0.95 {
        "ok"
    } else if rate >= 0.80 {
        "degraded"
    } else {
        "failing"
    }
}

fn null_or<T: Into<Value>>(value: Option<T>) -> Value {
    value.map_or(Value::Null, |v| v.into())
}

fn tool_stats_item(id: &str, meta: &ToolMeta, list: &[CallObservation]) -> Value {
    let agg = aggregate(list);
    let calls = agg.calls;
    let success_rate = if calls == 0 {
        Value::Null
    } else {
        json!(round_to(agg.success as f64 / calls as f64, 4))
    };
    // 命中率只对模板有意义：非 template 或窗口内无调用时给 null（不是 0）。
    let hit_rate = if meta.kind == "template" && calls > 0 {
        json!(round_to(agg.cache_hits as f64 / calls as f64, 4))
    } else {
        Value::Null
    };
    let recent_tokens: Vec<i64> = agg
        .tokens
        .iter()
        .rev()
        .take(RECENT_TOKENS)
        .rev()
        .copied()
        .collect();
    json!({
        "id": id,
        "kind": meta.kind,
        "group": meta.group,
        "category": meta.category,
        "lifecycle_status": meta.lifecycle_status,
        "risk": null_or(meta.risk.clone()),
        "discoverable": null_or(meta.discoverable),
        "callable": null_or(meta.callable),
        "version": null_or(meta.version),
        "last_changed_at": null_or(meta.last_changed_at.clone()),
        "source": meta.source,
        "health": health_of(calls, agg.success),
        "calls": calls,
        "success": agg.success,
        "failure": agg.failure,
        "success_rate": success_rate,
        "cache_hits": agg.cache_hits,
        "hit_rate": hit_rate,
        "retries": agg.retries,
        "avg_response_tokens": if agg.tokens.is_empty() {
            Value::Null
        } else {
            json!(round_to(agg.tokens.iter().sum::<i64>() as f64 / agg.tokens.len() as f64, 1))
        },
        "p95_response_tokens": if agg.tokens.is_empty() {
            Value::Null
        } else {
            json!(percentile95(&agg.tokens))
        },
        "max_response_tokens": null_or(agg.tokens.iter().max().copied()),
        "last_call_at": null_or(agg.last_call_at),
        "last_outcome": null_or(agg.last_outcome),
        "recent_tokens": recent_tokens,
    })
}

async fn tool_stats(
    State(state): State<AdminState>,
    Query(raw): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let actor = admin_actor("");
    let mut window_days = STATS_DEFAULT_WINDOW_DAYS;
    let mut limit = STATS_DEFAULT_LIMIT;
    for (key, value) in raw {
        match key.as_str() {
            "window_days" => {
                window_days =
                    parse_page_param(&state, &actor, "admin/tools/stats", "window_days", &value)?;
            }
            "limit" => {
                limit = parse_page_param(&state, &actor, "admin/tools/stats", "limit", &value)?;
            }
            _ => {
                let e = McpError::new(
                    "MCP_FILTER_UNSUPPORTED",
                    json!({"filter": key, "supported": ["window_days", "limit"]}),
                );
                return Err(reject(
                    &state,
                    &actor,
                    e,
                    json!({"surface": "admin/tools/stats", "filter": key}),
                ));
            }
        }
    }
    let window_days = window_days.clamp(1, STATS_MAX_WINDOW_DAYS);
    let limit = limit.clamp(1, MAX_LIMIT);
    let to = Utc::now();
    let from = to - chrono::Duration::days(window_days);
    let from_iso = from.to_rfc3339();
    let to_iso = to.to_rfc3339();

    let Some(log) = state.service.audit.clone() else {
        return Err(ApiError(McpError::new(
            "MCP_AUDIT_ERROR",
            json!({"hint": "审计未启用"}),
        )));
    };
    let mut truncated = false;
    let mut observations: Vec<CallObservation> = Vec::new();
    for event_type in ["tool_call", "template_run"] {
        let (mut rows, cut) = scan_in_window(&log, event_type, &from_iso)
            .map_err(|e| reject(&state, &actor, e, json!({"surface": "admin/tools/stats"})))?;
        truncated |= cut;
        observations.append(&mut rows);
    }
    // 时间升序；等 ts 时保持扫描顺序（rowid 顺序 = 审计写入顺序）。
    observations.sort_by(|a, b| a.ts.cmp(&b.ts));

    let mut universe = tool_universe(&state.store)
        .map_err(|e| reject(&state, &actor, e, json!({"surface": "admin/tools/stats"})))?;
    let mut by_id: HashMap<String, Vec<CallObservation>> = HashMap::new();
    for o in observations {
        by_id.entry(o.id.clone()).or_default().push(o);
    }
    // 不在注册表也不在 builtin 里的 id 也要出现一行（审计里确实被调用过）。
    for id in by_id.keys() {
        universe.entry(id.clone()).or_insert(ToolMeta {
            kind: "unknown".into(),
            group: String::new(),
            category: crate::catalog::CATEGORY_CUSTOM.to_string(),
            lifecycle_status: "unknown".into(),
            risk: None,
            discoverable: None,
            callable: None,
            version: None,
            last_changed_at: None,
            source: "managed".into(),
        });
    }

    let mut rows: Vec<(i64, String, Value)> = universe
        .iter()
        .map(|(id, meta)| {
            let list = by_id.get(id).map_or(&[][..], |v| v.as_slice());
            let item = tool_stats_item(id, meta, list);
            let calls = item["calls"].as_i64().unwrap_or_default();
            (calls, id.clone(), item)
        })
        .collect();
    // 契约排序：calls 降序、其次 id 升序；超过 limit 即截断并置 truncated。
    rows.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    if rows.len() > limit as usize {
        truncated = true;
    }
    let tools: Vec<Value> = rows
        .into_iter()
        .take(limit as usize)
        .map(|(_, _, item)| item)
        .collect();

    Ok(Json(json!({
        "window": {"days": window_days, "from": from_iso, "to": to_iso},
        "truncated": truncated,
        "tools": tools
    })))
}

/// `GET /admin/tools/{id}/calls`：单个能力的调用明细（时间倒序，缺字段一律 null）。
///
/// `window_days` 默认 30、夹紧 1..=90：响应结构不变，但 `total` 的语义是
/// "窗口内该 id 的调用数"（审计表只追加，明细查询必须带时间边界才不会越跑越慢）。
/// 调用明细的主体投影：`id`/`role` 恒有；审计 actor 里带 `source`/`token_id` 时原样带出。
///
/// 旧事件（改造前）没有这两个键，投影时直接省略（消费方按可选字段处理）。
fn call_actor_json(actor: &Value) -> Value {
    let mut out = json!({
        "id": actor.get("id").cloned().unwrap_or(Value::Null),
        "role": actor.get("role").cloned().unwrap_or(Value::Null),
    });
    if let Some(obj) = out.as_object_mut() {
        for key in ["source", "token_id"] {
            if let Some(value) = actor.get(key).filter(|v| !v.is_null()) {
                obj.insert(key.to_string(), value.clone());
            }
        }
    }
    out
}

async fn tool_calls(
    State(state): State<AdminState>,
    Path(id): Path<String>,
    Query(raw): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let actor = admin_actor("");
    let mut limit = DEFAULT_LIMIT;
    let mut offset = 0i64;
    let mut window_days = CALLS_DEFAULT_WINDOW_DAYS;
    for (key, value) in raw {
        match key.as_str() {
            "limit" => {
                limit = parse_page_param(&state, &actor, "admin/tools/calls", "limit", &value)?
            }
            "offset" => {
                offset = parse_page_param(&state, &actor, "admin/tools/calls", "offset", &value)?
            }
            "window_days" => {
                window_days =
                    parse_page_param(&state, &actor, "admin/tools/calls", "window_days", &value)?
            }
            _ => {
                let e = McpError::new(
                    "MCP_FILTER_UNSUPPORTED",
                    json!({"filter": key, "supported": ["limit", "offset", "window_days"]}),
                );
                return Err(reject(
                    &state,
                    &actor,
                    e,
                    json!({"surface": "admin/tools/calls", "filter": key}),
                ));
            }
        }
    }
    let limit = limit.clamp(1, MAX_LIMIT);
    let offset = offset.max(0);
    let window_days = window_days.clamp(1, CALLS_MAX_WINDOW_DAYS);
    let ts_after = (Utc::now() - chrono::Duration::days(window_days)).to_rfc3339();
    let Some(log) = state.service.audit.clone() else {
        return Err(ApiError(McpError::new(
            "MCP_AUDIT_ERROR",
            json!({"hint": "审计未启用"}),
        )));
    };
    let mut observations = scan_by_id(&log, "tool_call", &id, &ts_after)
        .map_err(|e| reject(&state, &actor, e, json!({"surface": "admin/tools/calls"})))?;
    observations.extend(
        scan_by_id(&log, "template_run", &id, &ts_after)
            .map_err(|e| reject(&state, &actor, e, json!({"surface": "admin/tools/calls"})))?,
    );
    // 时间倒序；等 ts 时后写入的排前面（先升序稳定排序，再整体反转）。
    observations.sort_by(|a, b| a.ts.cmp(&b.ts));
    observations.reverse();

    let total = observations.len();
    let calls: Vec<Value> = observations
        .into_iter()
        .skip(offset as usize)
        .take(limit as usize)
        .map(|env| {
            // scan_by_id 只返回 tool_call/template_run 且 id 匹配，投影必然成功。
            let o = observation(&env).expect("scan_by_id 返回的事件必可投影");
            json!({
                "event_id": o.event_id,
                "ts": o.ts,
                "event_type": o.event_type,
                "object_type": o.object_type,
                "object_id": o.id,
                "actor": call_actor_json(&o.actor),
                "outcome": o.outcome,
                "tokens": null_or(o.tokens),
                "cache": null_or(o.cache),
                "elapsed_ms": null_or(o.elapsed_ms),
                "version": null_or(o.version),
                "error_code": null_or(o.error_code),
                "error_message": null_or(o.error_message),
                "arguments": o.arguments,
                "executed_script": null_or(o.executed_script),
                "bound_params": o.bound_params
            })
        })
        .collect();
    Ok(Json(json!({
        "id": id,
        "total": total,
        "limit": limit,
        "offset": offset,
        "calls": calls
    })))
}

// ---------------------------- 人工维护写端点 ----------------------------

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

#[derive(Debug, Deserialize)]
struct CreateCapabilityBody {
    definition: Option<Value>,
}

/// `POST /admin/capabilities`：人工登记草案，与 MCP `admin_register_template` 走同一条路径
/// （`TemplateRegistry::register` = 定义门禁 + 编译 + store 落 draft），只产草案。
async fn create_capability(
    State(state): State<AdminState>,
    Extension(auth): Extension<AuthenticatedAdmin>,
    headers: HeaderMap,
    Json(body): Json<CreateCapabilityBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    const SURFACE: &str = "admin/capabilities/create";
    let actor = actor_of(&auth, &headers)
        .map_err(|e| reject(&state, &admin_actor(""), e, json!({"surface": SURFACE})))?;
    require_audit(&state, &actor, SURFACE)?;
    let Some(definition) = body.definition.filter(|v| v.is_object()) else {
        return Err(reject(
            &state,
            &actor,
            McpError::validation("definition 必须是模板定义对象".to_string()),
            json!({"surface": SURFACE}),
        ));
    };
    // 内置 id 不可写：模板门的 id 格式校验会先拒掉下划线形式的 builtin id（400），
    // 因此这里先按"是否内置"判定，保证契约的 409。
    if let Some(id) = definition.get("template_id").and_then(|v| v.as_str())
        && is_builtin_id(id)
    {
        return Err(reject(
            &state,
            &actor,
            McpError::new("MCP_BUILTIN_IMMUTABLE", json!({"capability_id": id})),
            json!({"capability_id": id}),
        ));
    }
    let Some(registry) = state.service.templates.clone() else {
        return Err(ApiError(McpError::new(
            "MCP_STORE_ERROR",
            json!({"hint": "模板注册表未注入（TemplateRegistry 未就绪）"}),
        )));
    };
    // 组必须先存在（配置 policy 组或管理组），否则不落库；缺 group 字段交给模板门禁报 400。
    let group = definition
        .get("group")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if !group.is_empty() {
        state.service.ensure_group_exists(group).map_err(|e| {
            reject(
                &state,
                &actor,
                e,
                json!({"surface": SURFACE, "group": group}),
            )
        })?;
    }
    let compiled = registry
        .register(&definition)
        .map_err(|e| reject(&state, &actor, e, json!({"surface": SURFACE})))?;
    let record = match state.store.get(&compiled.def.template_id) {
        Ok(Some(r)) => r,
        Ok(None) => {
            return Err(ApiError(McpError::store(format!(
                "register 后记录缺失: {}",
                compiled.def.template_id
            ))));
        }
        Err(e) => return Err(reject(&state, &actor, e, json!({"surface": SURFACE}))),
    };
    // 审计失败上抛：draft 已落库但审计缺行属于治理面静默失效（draft 不可见不可调，不回滚）。
    let mut payload = audit_base("capability", &record.capability_id, "create", "ok");
    payload.insert("to_status".into(), json!("draft"));
    payload.insert(
        "changes".into(),
        json!(jumcp_core::registry::definition_changes(
            &json!({}),
            &record.definition
        )),
    );
    payload.insert(
        "data".into(),
        json!({
            "definition_hash_before": Value::Null,
            "definition_hash_after": record.definition_hash,
            "version": record.version
        }),
    );
    state
        .service
        .audit_event("admin_change", Value::Object(payload), &actor)
        .map_err(|e| {
            reject(
                &state,
                &actor,
                e,
                json!({"capability_id": record.capability_id}),
            )
        })?;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": record.capability_id,
            "status": record.status,
            "version": record.version,
            "definition_hash": record.definition_hash,
            "last_changed_at": non_empty(&record.updated_at)
        })),
    ))
}

#[derive(Debug, Deserialize)]
struct DeprecateBody {
    successor_id: Option<String>,
}

/// `POST /admin/capabilities/{id}/deprecate`：下架并记录 successor（`admin_change` 在此层写）。
async fn deprecate_capability(
    State(state): State<AdminState>,
    Extension(auth): Extension<AuthenticatedAdmin>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<DeprecateBody>,
) -> Result<Json<Value>, ApiError> {
    const SURFACE: &str = "admin/capabilities/deprecate";
    let actor = actor_of(&auth, &headers)
        .map_err(|e| reject(&state, &admin_actor(""), e, json!({"surface": SURFACE})))?;
    require_audit(&state, &actor, SURFACE)?;
    // builtin 没有注册表行：先判可写性，否则会先落成 404 而给不出契约要求的 409。
    if is_builtin_id(&id) {
        return Err(reject(
            &state,
            &actor,
            McpError::new("MCP_BUILTIN_IMMUTABLE", json!({"capability_id": id})),
            json!({"capability_id": id}),
        ));
    }
    let before = state
        .store
        .get(&id)
        .map_err(|e| reject(&state, &actor, e, json!({"capability_id": id})))?
        .ok_or_else(|| {
            reject(
                &state,
                &actor,
                McpError::new("MCP_CAPABILITY_NOT_FOUND", json!({"capability_id": id})),
                json!({"capability_id": id}),
            )
        })?;
    let successor_id = body.successor_id.unwrap_or_default();
    // 空 successor 由 store 拒绝（MCP_VALIDATION_ERROR），这里不重复判定。
    let record = state
        .store
        .deprecate(&id, &successor_id)
        .map_err(|e| reject(&state, &actor, e, json!({"capability_id": id})))?;
    let before_values = json!({
        "status": before.status,
        "successor_id": before.successor_id,
        "discoverable": before.discoverable,
        "callable": before.callable
    });
    let after_values = json!({
        "status": record.status,
        "successor_id": record.successor_id,
        "discoverable": record.discoverable,
        "callable": record.callable
    });
    let mut payload = audit_base("capability", &record.capability_id, "deprecate", "ok");
    payload.insert("from_status".into(), json!(before.status));
    payload.insert("to_status".into(), json!(record.status));
    payload.insert(
        "changes".into(),
        json!(jumcp_core::registry::field_changes(
            &before_values,
            &after_values,
            &["status", "successor_id", "discoverable", "callable"]
        )),
    );
    payload.insert(
        "data".into(),
        json!({
            "version": record.version,
            "definition_hash_before": record.definition_hash,
            "definition_hash_after": record.definition_hash
        }),
    );
    state
        .service
        .audit_event("admin_change", Value::Object(payload), &actor)
        .map_err(|e| reject(&state, &actor, e, json!({"capability_id": id})))?;
    Ok(Json(json!({
        "id": record.capability_id,
        "status": record.status,
        "successor_id": record.successor_id,
        "version": record.version,
        "last_changed_at": non_empty(&record.updated_at)
    })))
}

// ---------------------------- 访问令牌（平台生成 + 数据库持久化） ----------------------------

/// 令牌条目的唯一形状：永不包含明文或 hash。
fn token_item(record: &AccessTokenRecord) -> Value {
    record.to_json()
}

/// 稳定去重（保序、去空白）：组/内置工具列表都走这一处。
fn normalize_names(values: Vec<String>) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for value in values {
        let value = value.trim();
        if !value.is_empty() && seen.insert(value.to_string()) {
            out.push(value.to_string());
        }
    }
    out
}

/// `GET /admin/tokens`：全量列表（含已撤销/已过期），状态在读取时计算。
async fn list_tokens(State(state): State<AdminState>) -> Result<Json<Value>, ApiError> {
    let actor = admin_actor("");
    let records = state
        .store
        .list_access_tokens()
        .map_err(|e| reject(&state, &actor, e, json!({"surface": "admin/tokens"})))?;
    let items: Vec<Value> = records.iter().map(token_item).collect();
    Ok(Json(json!({"items": items, "total": items.len()})))
}

#[derive(Debug, Deserialize)]
struct CreateTokenBody {
    name: Option<String>,
    role: Option<String>,
    #[serde(default)]
    groups: Vec<String>,
    #[serde(default)]
    builtin_tools: Vec<String>,
    source: Option<String>,
    caller: Option<String>,
    expires_at: Option<serde_json::Value>,
}

/// `POST /admin/tokens`：创建令牌；明文只在响应的 `token` 字段出现一次。
async fn create_token(
    State(state): State<AdminState>,
    Extension(auth): Extension<AuthenticatedAdmin>,
    headers: HeaderMap,
    Json(body): Json<CreateTokenBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    const SURFACE: &str = "admin/tokens/create";
    let actor = actor_of(&auth, &headers)
        .map_err(|e| reject(&state, &admin_actor(""), e, json!({"surface": SURFACE})))?;
    require_audit(&state, &actor, SURFACE)?;
    let missing = |field: &str| {
        reject(
            &state,
            &actor,
            McpError::validation(format!("{field} 必填")),
            json!({"surface": SURFACE, "field": field}),
        )
    };
    let name = body
        .name
        .map(|s| s.trim().to_string())
        .ok_or_else(|| missing("name"))?;
    let role = body
        .role
        .map(|s| s.trim().to_string())
        .ok_or_else(|| missing("role"))?;
    let source = body
        .source
        .map(|s| s.trim().to_string())
        .ok_or_else(|| missing("source"))?;
    let caller = body
        .caller
        .map(|s| s.trim().to_string())
        .ok_or_else(|| missing("caller"))?;
    // expires_at 允许 null / 缺省；非字符串一律 400（不静默当成无过期）。
    let expires_at = match body.expires_at {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => {
            let s = s.trim().to_string();
            (!s.is_empty()).then_some(s)
        }
        Some(_) => {
            return Err(reject(
                &state,
                &actor,
                McpError::validation("expires_at 必须是 RFC3339 字符串或 null".to_string()),
                json!({"surface": SURFACE, "field": "expires_at"}),
            ));
        }
    };
    let groups = normalize_names(body.groups);
    let builtin_tools = normalize_names(body.builtin_tools);

    // role 必须是已定义角色（配置策略；管理面不能凭空造角色）。
    if !state.service.settings.policy.roles.contains_key(&role) {
        let mut known: Vec<&String> = state.service.settings.policy.roles.keys().collect();
        known.sort();
        return Err(reject(
            &state,
            &actor,
            McpError::validation(format!("未定义角色: {role}")),
            json!({"surface": SURFACE, "field": "role", "known_roles": known}),
        ));
    }
    // groups 必须来自有效策略（config + managed）。
    let policy = state
        .service
        .effective_policy()
        .map_err(|e| reject(&state, &actor, e, json!({"surface": SURFACE})))?;
    let known_groups: HashSet<&str> = policy.groups.iter().map(|g| g.code.as_str()).collect();
    let unknown: Vec<&String> = groups
        .iter()
        .filter(|g| !known_groups.contains(g.as_str()))
        .collect();
    if !unknown.is_empty() {
        let mut available: Vec<&str> = known_groups.iter().copied().collect();
        available.sort();
        return Err(reject(
            &state,
            &actor,
            McpError::validation("groups 含未知组".to_string()),
            json!({"surface": SURFACE, "field": "groups", "unknown": unknown, "available": available}),
        ));
    }
    if !is_principal_id(&caller) {
        return Err(reject(
            &state,
            &actor,
            McpError::validation("caller 非法".to_string()),
            json!({"surface": SURFACE, "field": "caller", "rule": "^[a-z0-9._-]{2,32}$"}),
        ));
    }
    let new = NewAccessToken {
        name,
        role,
        groups,
        builtin_tools,
        source,
        caller,
        expires_at,
        tenant: "default".into(),
        created_by: actor.actor_id.clone(),
    };
    let issued = state
        .store
        .create_access_token(&new, &actor)
        .map_err(|e| reject(&state, &actor, e, json!({"surface": SURFACE})))?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"token": issued.token, "item": token_item(&issued.record)})),
    ))
}

/// `POST /admin/tokens/{id}/revoke`：前端契约直接返回 `AccessTokenItem`（不再包一层 `item`）。
async fn revoke_token(
    State(state): State<AdminState>,
    Extension(auth): Extension<AuthenticatedAdmin>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    const SURFACE: &str = "admin/tokens/revoke";
    let actor = actor_of(&auth, &headers)
        .map_err(|e| reject(&state, &admin_actor(""), e, json!({"surface": SURFACE})))?;
    require_audit(&state, &actor, SURFACE)?;
    let record = state.store.revoke_access_token(&id, &actor).map_err(|e| {
        let detail = if e.code == "MCP_TOKEN_NOT_FOUND" {
            json!({"token_id": id})
        } else {
            json!({"surface": SURFACE, "token_id": id})
        };
        reject(&state, &actor, e, detail)
    })?;
    Ok(Json(token_item(&record)))
}

/// `POST /admin/tokens/{id}/reissue`：重新签发 active 令牌，明文只在本响应出现一次。
///
/// 库里只有 hash，明文不可回读；管理台用这条路给同一记录换一把新钥匙。
/// 撤销/过期令牌返回 409（store 判定），且不改行、不写审计。
async fn reissue_token(
    State(state): State<AdminState>,
    Extension(auth): Extension<AuthenticatedAdmin>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    const SURFACE: &str = "admin/tokens/reissue";
    let actor = actor_of(&auth, &headers)
        .map_err(|e| reject(&state, &admin_actor(""), e, json!({"surface": SURFACE})))?;
    require_audit(&state, &actor, SURFACE)?;
    let issued = state.store.reissue_access_token(&id, &actor).map_err(|e| {
        let detail = if e.code == "MCP_TOKEN_NOT_FOUND" {
            json!({"token_id": id})
        } else {
            json!({"surface": SURFACE, "token_id": id})
        };
        reject(&state, &actor, e, detail)
    })?;
    Ok(Json(json!({
        "token": issued.token,
        "item": token_item(&issued.record)
    })))
}

/// `DELETE /admin/tokens/{id}`：硬删除已撤销/已过期记录。
///
/// active 令牌必须先撤销（409，`MCP_STATE_TRANSITION_INVALID`）：
/// 直接删掉在用令牌会让持有者的下一次调用从"可解释的 401"变成"记录凭空消失"。
async fn delete_token(
    State(state): State<AdminState>,
    Extension(auth): Extension<AuthenticatedAdmin>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    const SURFACE: &str = "admin/tokens/delete";
    let actor = actor_of(&auth, &headers)
        .map_err(|e| reject(&state, &admin_actor(""), e, json!({"surface": SURFACE})))?;
    require_audit(&state, &actor, SURFACE)?;
    state.store.delete_access_token(&id, &actor).map_err(|e| {
        let detail = if e.code == "MCP_TOKEN_NOT_FOUND" {
            json!({"token_id": id})
        } else {
            json!({"surface": SURFACE, "token_id": id})
        };
        reject(&state, &actor, e, detail)
    })?;
    Ok(Json(json!({"deleted": true, "id": id})))
}

/// 内置工具的中文标签（前端令牌表单直接展示；name 与 BUILTIN_CAPABILITY_IDS 一致）。
fn builtin_label(id: &str) -> &'static str {
    match id {
        "search_capabilities" => "检索能力",
        "describe_capability" => "描述能力",
        "list_groups" => "组索引",
        "db_query_readonly" => "只读查询",
        "db_run_template" => "模板执行",
        "admin_register_template" => "登记模板草案",
        "admin_review_queue" => "审核队列",
        "admin_audit_queries" => "审计查询",
        "admin_set_visibility" => "改可见性",
        _ => "内置工具",
    }
}

/// `GET /admin/tokens/options`：角色（含默认组/预算）、组（含未撤销 token 引用数）、
/// 内置工具（中文标签 + builtin/admin 分组）。
async fn token_options(State(state): State<AdminState>) -> Result<Json<Value>, ApiError> {
    let actor = admin_actor("");
    let policy = state.service.effective_policy().map_err(|e| {
        reject(
            &state,
            &actor,
            e,
            json!({"surface": "admin/tokens/options"}),
        )
    })?;
    let token_counts = state.store.token_counts_by_group().map_err(|e| {
        reject(
            &state,
            &actor,
            e,
            json!({"surface": "admin/tokens/options"}),
        )
    })?;
    let mut role_codes: Vec<&String> = policy.roles.keys().collect();
    role_codes.sort();
    // 角色：code 是权限标识，name 是中文展示名；旧语义（name=code）已删除。
    let roles: Vec<Value> = role_codes
        .iter()
        .map(|code| {
            let def = &policy.roles[*code];
            json!({
                "code": code,
                "name": def.name,
                "default_groups": def.default_groups,
                "default_budget": def.default_budget
            })
        })
        .collect();
    let policy_codes: HashSet<&str> = state
        .service
        .settings
        .policy
        .groups
        .iter()
        .map(|g| g.code.as_str())
        .collect();
    let groups: Vec<Value> = policy
        .groups
        .iter()
        .map(|group| {
            json!({
                "code": group.code,
                "name": group.name,
                "source": if policy_codes.contains(group.code.as_str()) { "policy" } else { "managed" },
                "priority": group.priority,
                "min_budget": group.min_budget,
                "token_count": token_counts.get(&group.code).copied().unwrap_or(0)
            })
        })
        .collect();
    let builtin_tools: Vec<Value> = BUILTIN_CAPABILITY_IDS
        .iter()
        .map(|id| {
            json!({
                "name": id,
                "label": builtin_label(id),
                "group": if ADMIN_CAPABILITY_IDS.contains(id) { "admin" } else { "builtin" }
            })
        })
        .collect();
    Ok(Json(json!({
        "roles": roles,
        "groups": groups,
        "builtin_tools": builtin_tools
    })))
}

// ---------------------------- 组维护（管理面新增组） ----------------------------

/// 管理组条目的唯一形状：`source` 区分配置组与 API 管理的组。
///
/// `code` 是唯一权限标识（能力/令牌/scope/审计都用它），`name` 只是中文展示名；
/// 调用方不得用 `name` 做任何权限判定。
struct GroupItem<'a> {
    code: &'a str,
    name: &'a str,
    priority: i64,
    min_budget: i64,
    roles: &'a [String],
    source: &'a str,
    capability_count: i64,
    token_count: i64,
}

fn group_item(item: GroupItem<'_>) -> Value {
    json!({
        "code": item.code,
        "name": item.name,
        "priority": item.priority,
        "min_budget": item.min_budget,
        "roles": item.roles,
        "source": item.source,
        "capability_count": item.capability_count,
        "token_count": item.token_count
    })
}

/// 角色门禁：必须是 `settings.policy.roles` 里已定义的角色 code；去重保序。未知角色 → 400。
fn normalize_roles(state: &AdminState, roles: Vec<String>) -> Result<Vec<String>, McpError> {
    let policy = &state.service.settings.policy;
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    let mut unknown: Vec<String> = Vec::new();
    for role in roles {
        let role = role.trim();
        if role.is_empty() {
            continue;
        }
        if !policy.roles.contains_key(role) {
            unknown.push(role.to_string());
        } else if seen.insert(role.to_string()) {
            out.push(role.to_string());
        }
    }
    if !unknown.is_empty() {
        let mut known: Vec<String> = policy.roles.keys().cloned().collect();
        known.sort();
        return Err(McpError::new(
            "MCP_VALIDATION_ERROR",
            json!({
                "field": "roles",
                "unknown_roles": unknown,
                "known_roles": known
            }),
        ));
    }
    Ok(out)
}

/// 组 code + 展示名 + 数值字段的公共门禁：返回规范化错误（400/409 由错误码决定）。
fn validate_group_fields(
    state: &AdminState,
    actor: &Actor,
    code: &str,
    name: &str,
    priority: i64,
    min_budget: i64,
) -> Result<(), ApiError> {
    if let Err(message) = validate_group_name(code) {
        return Err(reject(
            state,
            actor,
            McpError::validation(message),
            json!({"group": code, "field": "code"}),
        ));
    }
    if is_reserved_group_name(code) {
        return Err(reject(
            state,
            actor,
            McpError::new(
                "MCP_GROUP_RESERVED",
                json!({"group": code, "reserved": RESERVED_GROUP_NAMES}),
            ),
            json!({"group": code}),
        ));
    }
    if let Err(message) = validate_display_name("name", name) {
        return Err(reject(
            state,
            actor,
            McpError::validation(message),
            json!({"group": code, "field": "name"}),
        ));
    }
    if priority < 0 {
        return Err(reject(
            state,
            actor,
            McpError::validation(format!("priority 不能为负: {priority}")),
            json!({"group": code, "field": "priority"}),
        ));
    }
    if min_budget <= 0 {
        return Err(reject(
            state,
            actor,
            McpError::validation(format!("min_budget 必须为正: {min_budget}")),
            json!({"group": code, "field": "min_budget"}),
        ));
    }
    Ok(())
}

/// `GET /admin/groups?q=&source=&role=`：配置组（`source=policy`）+ 管理组（`source=managed`）。
///
/// 过滤只影响返回集合与 `total`：`q` 对 code/name 做小写包含匹配；`source` 只接受
/// policy|managed（其他值 400）；`role` 按角色 code 精确匹配该条目的授权角色。
/// policy 组的 `roles` 由 `policy.roles` 反查（哪些角色的 default_groups 含该组 code）。
#[derive(Debug, Deserialize)]
struct GroupListQuery {
    q: Option<String>,
    source: Option<String>,
    role: Option<String>,
}

async fn list_groups(
    State(state): State<AdminState>,
    Query(query): Query<GroupListQuery>,
) -> Result<Json<Value>, ApiError> {
    let actor = admin_actor("");
    let policy = &state.service.settings.policy;
    let counts = state
        .store
        .group_capability_counts()
        .map_err(|e| reject(&state, &actor, e, json!({"surface": "admin/groups"})))?;
    let token_counts = state
        .store
        .token_counts_by_group()
        .map_err(|e| reject(&state, &actor, e, json!({"surface": "admin/groups"})))?;
    let count_of = |code: &str| counts.get(code).copied().unwrap_or(0);
    let tokens_of = |code: &str| token_counts.get(code).copied().unwrap_or(0);
    let mut items: Vec<Value> = Vec::new();
    let mut configured: HashSet<&str> = HashSet::new();
    for group in &policy.groups {
        configured.insert(group.code.as_str());
        let mut roles: Vec<String> = policy
            .roles
            .iter()
            .filter(|(_, def)| def.default_groups.contains(&group.code))
            .map(|(code, _)| code.clone())
            .collect();
        roles.sort();
        items.push(group_item(GroupItem {
            code: &group.code,
            name: &group.name,
            priority: group.priority,
            min_budget: group.min_budget,
            roles: &roles,
            source: "policy",
            capability_count: count_of(&group.code),
            token_count: tokens_of(&group.code),
        }));
    }
    let managed = state
        .store
        .managed_groups()
        .map_err(|e| reject(&state, &actor, e, json!({"surface": "admin/groups"})))?;
    for group in managed {
        // 配置里同 code 组优先展示（有效策略里也是配置组先到先得）。
        if configured.contains(group.code.as_str()) {
            continue;
        }
        items.push(group_item(GroupItem {
            code: &group.code,
            name: &group.name,
            priority: group.priority,
            min_budget: group.min_budget,
            roles: &group.roles,
            source: "managed",
            capability_count: count_of(&group.code),
            token_count: tokens_of(&group.code),
        }));
    }
    let source_filter = match query.source.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(source @ ("policy" | "managed")) => Some(source),
        Some(other) => {
            return Err(reject(
                &state,
                &actor,
                McpError::validation(format!("source 非法: {other}（只接受 policy|managed）")),
                json!({"surface": "admin/groups", "field": "source"}),
            ));
        }
    };
    let q = query
        .q
        .map(|q| q.trim().to_lowercase())
        .filter(|q| !q.is_empty());
    let role = query
        .role
        .map(|role| role.trim().to_string())
        .filter(|role| !role.is_empty());
    items.retain(|item| {
        if let Some(source) = source_filter
            && item["source"].as_str() != Some(source)
        {
            return false;
        }
        if let Some(role) = &role
            && !item["roles"]
                .as_array()
                .is_some_and(|roles| roles.iter().any(|r| r.as_str() == Some(role.as_str())))
        {
            return false;
        }
        if let Some(q) = &q {
            let code = item["code"].as_str().unwrap_or_default().to_lowercase();
            let name = item["name"].as_str().unwrap_or_default().to_lowercase();
            if !code.contains(q.as_str()) && !name.contains(q.as_str()) {
                return false;
            }
        }
        true
    });
    let total = items.len();
    Ok(Json(json!({"items": items, "total": total})))
}

#[derive(Debug, Deserialize)]
struct CreateGroupBody {
    code: Option<String>,
    name: Option<String>,
    priority: Option<i64>,
    min_budget: Option<i64>,
    roles: Option<Vec<String>>,
}

/// `POST /admin/groups`：新增管理组，下一次 scope 解析即可用。
///
/// body `{ code, name, priority, min_budget, roles }`；兼容旧客户端只发 `name`
/// （旧语义里 name 就是 code，此时展示名也回退成该值）。
async fn create_group(
    State(state): State<AdminState>,
    Extension(auth): Extension<AuthenticatedAdmin>,
    headers: HeaderMap,
    Json(body): Json<CreateGroupBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    const SURFACE: &str = "admin/groups/create";
    let actor = actor_of(&auth, &headers)
        .map_err(|e| reject(&state, &admin_actor(""), e, json!({"surface": SURFACE})))?;
    require_audit(&state, &actor, SURFACE)?;
    let name_input = body.name;
    let raw_name = name_input
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    // name 显式传空是参数错误；完全省略才回退 code（兼容旧客户端）。
    if name_input.is_some() && raw_name.is_none() {
        return Err(reject(
            &state,
            &actor,
            McpError::with_message(
                "MCP_VALIDATION_ERROR",
                "name 不能为空".to_string(),
                json!({"field": "name", "group": body.code.as_deref()}),
            ),
            json!({"surface": SURFACE, "field": "name"}),
        ));
    }
    let Some(code) = body
        .code
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        // 旧客户端把 code 放在 name 里：兼容读取，不改变新契约。
        .or_else(|| raw_name.clone())
    else {
        return Err(reject(
            &state,
            &actor,
            McpError::validation("code 必填".to_string()),
            json!({"surface": SURFACE, "field": "code"}),
        ));
    };
    let name = raw_name.unwrap_or_else(|| code.clone());
    let priority = body.priority.unwrap_or(0);
    let min_budget = body.min_budget.unwrap_or(800);
    validate_group_fields(&state, &actor, &code, &name, priority, min_budget)?;
    let policy = &state.service.settings.policy;
    if policy.groups.iter().any(|g| g.code == code) {
        return Err(reject(
            &state,
            &actor,
            McpError::new(
                "MCP_GROUP_CONFLICT",
                json!({"group": code, "source": "policy"}),
            ),
            json!({"group": code}),
        ));
    }
    let managed = state
        .store
        .managed_groups()
        .map_err(|e| reject(&state, &actor, e, json!({"surface": SURFACE})))?;
    if managed.iter().any(|g| g.code == code) {
        return Err(reject(
            &state,
            &actor,
            McpError::new(
                "MCP_GROUP_CONFLICT",
                json!({"group": code, "source": "managed"}),
            ),
            json!({"group": code}),
        ));
    }
    // 组总数上限与配置校验同一常量（配置组 + 管理组）。
    if policy.groups.len() + managed.len() >= MAX_GROUPS {
        return Err(reject(
            &state,
            &actor,
            McpError::validation(format!(
                "组数已达上限 {MAX_GROUPS}（配置 {} + 管理 {}）",
                policy.groups.len(),
                managed.len()
            )),
            json!({"group": code, "limit": MAX_GROUPS}),
        ));
    }
    let roles = normalize_roles(&state, body.roles.unwrap_or_default()).map_err(|e| {
        reject(
            &state,
            &actor,
            e,
            json!({"surface": SURFACE, "group": code}),
        )
    })?;
    let created = state
        .store
        .insert_managed_group(
            &ManagedGroup {
                code: code.clone(),
                name: name.clone(),
                priority,
                min_budget,
                roles,
                updated_at: String::new(),
            },
            &actor,
        )
        .map_err(|e| reject(&state, &actor, e, json!({"group": code})))?;
    Ok((
        StatusCode::CREATED,
        Json(group_item(GroupItem {
            code: &created.code,
            name: &created.name,
            priority: created.priority,
            min_budget: created.min_budget,
            roles: &created.roles,
            source: "managed",
            capability_count: 0,
            token_count: 0,
        })),
    ))
}

#[derive(Debug, Deserialize)]
struct UpdateGroupBody {
    code: Option<String>,
    name: Option<String>,
    priority: Option<i64>,
    min_budget: Option<i64>,
    roles: Option<Vec<String>>,
}

/// `PUT /admin/groups/{code}`：只能改管理组；配置组与保留组 409。
///
/// code 不可改：路径即 code，body 里出现 `code` 一律 400（拒绝静默忽略）。
async fn update_group(
    State(state): State<AdminState>,
    Extension(auth): Extension<AuthenticatedAdmin>,
    Path(code): Path<String>,
    headers: HeaderMap,
    Json(body): Json<UpdateGroupBody>,
) -> Result<Json<Value>, ApiError> {
    const SURFACE: &str = "admin/groups/update";
    let actor = actor_of(&auth, &headers)
        .map_err(|e| reject(&state, &admin_actor(""), e, json!({"surface": SURFACE})))?;
    require_audit(&state, &actor, SURFACE)?;
    if body.code.is_some() {
        return Err(reject(
            &state,
            &actor,
            McpError::with_message(
                "MCP_VALIDATION_ERROR",
                "code 不可修改（路径即 code）".to_string(),
                json!({"field": "code", "group": code}),
            ),
            json!({"surface": SURFACE, "field": "code", "group": code}),
        ));
    }
    if is_reserved_group_name(&code) {
        return Err(reject(
            &state,
            &actor,
            McpError::new(
                "MCP_GROUP_RESERVED",
                json!({"group": code, "reserved": RESERVED_GROUP_NAMES}),
            ),
            json!({"group": code}),
        ));
    }
    if state
        .service
        .settings
        .policy
        .groups
        .iter()
        .any(|g| g.code == code)
    {
        return Err(reject(
            &state,
            &actor,
            McpError::new(
                "MCP_GROUP_CONFLICT",
                json!({"group": code, "source": "policy", "hint": "配置组只能改配置文件"}),
            ),
            json!({"group": code}),
        ));
    }
    let existing = state
        .store
        .managed_group(&code)
        .map_err(|e| reject(&state, &actor, e, json!({"group": code})))?
        .ok_or_else(|| {
            reject(
                &state,
                &actor,
                McpError::new("MCP_GROUP_NOT_FOUND", json!({"group": code})),
                json!({"group": code}),
            )
        })?;
    let name = match body.name {
        Some(name) => name.trim().to_string(),
        None => existing.name.clone(),
    };
    let priority = body.priority.unwrap_or(existing.priority);
    let min_budget = body.min_budget.unwrap_or(existing.min_budget);
    validate_group_fields(&state, &actor, &code, &name, priority, min_budget)?;
    let roles = match body.roles {
        Some(roles) => normalize_roles(&state, roles).map_err(|e| {
            reject(
                &state,
                &actor,
                e,
                json!({"surface": SURFACE, "group": code}),
            )
        })?,
        None => existing.roles.clone(),
    };
    let updated = state
        .store
        .update_managed_group(
            &ManagedGroup {
                code: code.clone(),
                name,
                priority,
                min_budget,
                roles,
                updated_at: String::new(),
            },
            &actor,
        )
        .map_err(|e| reject(&state, &actor, e, json!({"group": code})))?;
    let count = state
        .store
        .group_capability_count(&code)
        .map_err(|e| reject(&state, &actor, e, json!({"group": code})))?;
    let token_count = state
        .store
        .group_token_count(&code)
        .map_err(|e| reject(&state, &actor, e, json!({"group": code})))?;
    Ok(Json(group_item(GroupItem {
        code: &updated.code,
        name: &updated.name,
        priority: updated.priority,
        min_budget: updated.min_budget,
        roles: &updated.roles,
        source: "managed",
        capability_count: count,
        token_count,
    })))
}

/// `DELETE /admin/groups/{code}`：只能删管理组；仍被能力/令牌引用 → 409（双计数）。
async fn delete_group(
    State(state): State<AdminState>,
    Extension(auth): Extension<AuthenticatedAdmin>,
    Path(code): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    const SURFACE: &str = "admin/groups/delete";
    let actor = actor_of(&auth, &headers)
        .map_err(|e| reject(&state, &admin_actor(""), e, json!({"surface": SURFACE})))?;
    require_audit(&state, &actor, SURFACE)?;
    if is_reserved_group_name(&code) {
        return Err(reject(
            &state,
            &actor,
            McpError::new(
                "MCP_GROUP_RESERVED",
                json!({"group": code, "reserved": RESERVED_GROUP_NAMES}),
            ),
            json!({"group": code}),
        ));
    }
    if state
        .service
        .settings
        .policy
        .groups
        .iter()
        .any(|g| g.code == code)
    {
        return Err(reject(
            &state,
            &actor,
            McpError::new(
                "MCP_GROUP_CONFLICT",
                json!({"group": code, "source": "policy", "hint": "配置组只能改配置文件"}),
            ),
            json!({"group": code}),
        ));
    }
    let name = state
        .store
        .managed_group(&code)
        .map_err(|e| reject(&state, &actor, e, json!({"group": code})))?
        .map(|g| g.name)
        .unwrap_or_else(|| code.clone());
    state
        .store
        .remove_managed_group(&code, &actor)
        .map_err(|e| {
            // 引用计数是删除拒绝的可解释信息：原样带到 HTTP 错误体里。
            let detail = if e.code == "MCP_GROUP_IN_USE" {
                e.data.clone()
            } else {
                json!({"group": code})
            };
            reject(&state, &actor, e, detail)
        })?;
    Ok(Json(json!({"deleted": true, "code": code, "name": name})))
}

/// `GET /admin/roles`：角色 code + 中文展示名 + 默认组/预算。
///
/// 权限判定始终用 code（token.role、审计 actor.role、组 roles 都是 code）。
async fn list_roles(State(state): State<AdminState>) -> Result<Json<Value>, ApiError> {
    let actor = admin_actor("");
    let policy = state
        .service
        .effective_policy()
        .map_err(|e| reject(&state, &actor, e, json!({"surface": "admin/roles"})))?;
    let mut codes: Vec<&String> = policy.roles.keys().collect();
    codes.sort();
    let items: Vec<Value> = codes
        .iter()
        .map(|code| {
            let def = &policy.roles[*code];
            json!({
                "code": code,
                "name": def.name,
                "default_groups": def.default_groups,
                "default_budget": def.default_budget
            })
        })
        .collect();
    Ok(Json(json!({"items": items})))
}

// ---------------------------- 上游 MCP router ----------------------------

/// `POST/PUT /admin/upstreams` 的请求体：与 `UpstreamItem` 同形，去掉运行态字段。
///
/// 字段可选是给 PUT 用的：省略 = 保留现有值（PUT 是整对象提交，但允许省略未改字段）。
#[derive(Debug, Deserialize)]
struct UpstreamInput {
    id: Option<String>,
    name: Option<String>,
    transport: Option<String>,
    url: Option<String>,
    command: Option<String>,
    args: Option<Vec<String>>,
    env: Option<BTreeMap<String, String>>,
    headers: Option<BTreeMap<String, String>>,
    group: Option<String>,
    category: Option<String>,
    read_only: Option<bool>,
    enabled: Option<bool>,
    timeout_ms: Option<i64>,
}

/// 上游管理面的唯一来源：`Service::upstreams()`（未启用 router 时显式报错，不伪造空列表）。
fn require_router<'a>(
    state: &'a AdminState,
    actor: &Actor,
    surface: &str,
) -> Result<&'a Arc<crate::upstream::UpstreamManager>, ApiError> {
    state.service.upstreams().ok_or_else(|| {
        reject(
            state,
            actor,
            McpError::new(
                "MCP_UPSTREAM_INVALID",
                json!({"hint": "router 上游管理器未启用（本进程未挂 UpstreamManager）"}),
            ),
            json!({"surface": surface}),
        )
    })
}

/// 已脱敏的 `***` 只能用于"保留旧值"：按 key 回填现有配置；没有旧值时拒绝落库。
fn restore_masked(
    field: &str,
    existing: &BTreeMap<String, String>,
    submitted: Option<BTreeMap<String, String>>,
) -> Result<BTreeMap<String, String>, McpError> {
    let Some(submitted) = submitted else {
        return Ok(existing.clone());
    };
    let mut out = BTreeMap::new();
    for (key, value) in submitted {
        if value == "***" {
            match existing.get(&key) {
                Some(old) => {
                    out.insert(key, old.clone());
                }
                None => {
                    return Err(McpError::new(
                        "MCP_UPSTREAM_INVALID",
                        json!({
                            "field": field,
                            "key": key,
                            "hint": "*** 是脱敏占位符，新建或新增键时必须给真实值或 env:NAME 引用"
                        }),
                    ));
                }
            }
        } else {
            out.insert(key, value);
        }
    }
    Ok(out)
}

/// 请求体 → 上游配置（含开关与组存在性校验）。`existing` 非空即 PUT：省略字段保留旧值。
fn build_upstream(
    state: &AdminState,
    actor: &Actor,
    surface: &str,
    path_id: Option<&str>,
    body: UpstreamInput,
    existing: Option<&McpUpstream>,
) -> Result<McpUpstream, ApiError> {
    if let (Some(path_id), Some(body_id)) = (path_id, body.id.as_deref())
        && path_id != body_id
    {
        return Err(reject(
            state,
            actor,
            McpError::new(
                "MCP_UPSTREAM_INVALID",
                json!({"field": "id", "hint": "id 不可修改（路径即 id）"}),
            ),
            json!({"surface": surface, "id": path_id}),
        ));
    }
    let id = path_id.map(str::to_string).or(body.id).unwrap_or_default();
    let fallback = |pick: Option<String>, old: Option<String>| pick.or(old).unwrap_or_default();
    let name = body
        .name
        .map(|n| n.trim().to_string())
        .or_else(|| existing.map(|e| e.name.clone()))
        .unwrap_or_default();
    let transport = fallback(
        body.transport.map(|t| t.trim().to_ascii_lowercase()),
        existing.map(|e| e.transport.clone()),
    );
    let url = fallback(body.url, existing.map(|e| e.url.clone()));
    let command = fallback(body.command, existing.map(|e| e.command.clone()));
    // transport 决定字段归属：切换传输时另一侧的旧值直接清空，避免"改 http 后仍带 command"
    // 这类陈旧字段把合法请求挡在校验之外。显式给了噪声字段同样被归一化（不落库）。
    let (url, command) = match transport.as_str() {
        "http" => (url, String::new()),
        "stdio" => (String::new(), command),
        _ => (url, command),
    };
    let args = body
        .args
        .or_else(|| existing.map(|e| e.args.clone()))
        .unwrap_or_default();
    let old_env = existing.map(|e| e.env.clone()).unwrap_or_default();
    let old_headers = existing.map(|e| e.headers.clone()).unwrap_or_default();
    let env = restore_masked("env", &old_env, body.env)?;
    let headers = restore_masked("headers", &old_headers, body.headers)?;
    let group = fallback(body.group, existing.map(|e| e.group.clone()));
    let category = fallback(body.category, existing.map(|e| e.category.clone()));
    let category = if category.is_empty() {
        crate::catalog::CATEGORY_CUSTOM.to_string()
    } else {
        category
    };
    let read_only = body
        .read_only
        .or_else(|| existing.map(|e| e.read_only))
        .unwrap_or(true);
    let enabled = body
        .enabled
        .or_else(|| existing.map(|e| e.enabled))
        .unwrap_or(true);
    let timeout_ms = body
        .timeout_ms
        .or_else(|| existing.map(|e| e.timeout_ms))
        .unwrap_or(10_000);
    let upstream = McpUpstream {
        id,
        name,
        transport,
        url,
        command,
        args,
        env,
        headers,
        group,
        category,
        read_only,
        enabled,
        timeout_ms,
        created_at: existing.map(|e| e.created_at.clone()).unwrap_or_default(),
        updated_at: String::new(),
    };
    // 组必须存在于有效策略：上游工具的可见性完全由该组决定，悬空组等于永远不可见。
    let policy = state.service.effective_policy().map_err(|e| {
        reject(
            state,
            actor,
            e,
            json!({"surface": surface, "id": upstream.id}),
        )
    })?;
    if !policy.groups.iter().any(|g| g.code == upstream.group) {
        let mut known: Vec<&str> = policy.groups.iter().map(|g| g.code.as_str()).collect();
        known.sort();
        return Err(reject(
            state,
            actor,
            McpError::new(
                "MCP_UPSTREAM_INVALID",
                json!({"field": "group", "group": upstream.group, "known_groups": known}),
            ),
            json!({"surface": surface, "id": upstream.id}),
        ));
    }
    // http 上游的地址在写边界就完整解析：只允许 http(s)、禁止 userinfo（与建连时同一约束）。
    if upstream.transport == "http" {
        let parsed = reqwest::Url::parse(&upstream.url).map_err(|_| {
            reject(
                state,
                actor,
                McpError::new(
                    "MCP_UPSTREAM_INVALID",
                    json!({"field": "url", "value": upstream.url, "hint": "必须是绝对 URL"}),
                ),
                json!({"surface": surface, "id": upstream.id}),
            )
        })?;
        if !matches!(parsed.scheme(), "http" | "https")
            || !parsed.username().is_empty()
            || parsed.password().is_some()
        {
            return Err(reject(
                state,
                actor,
                McpError::new(
                    "MCP_UPSTREAM_INVALID",
                    json!({
                        "field": "url",
                        "hint": "只允许 http/https，且不得携带 userinfo（凭证走 headers/env:NAME）"
                    }),
                ),
                json!({"surface": surface, "id": upstream.id}),
            ));
        }
    }
    if upstream.transport == "stdio" && !state.service.settings.upstream_stdio {
        return Err(reject(
            state,
            actor,
            McpError::new(
                "MCP_UPSTREAM_STDIO_DISABLED",
                json!({"id": upstream.id, "hint": "设置 JUMCP_MCP_UPSTREAM_STDIO=1 后重启"}),
            ),
            json!({"surface": surface, "id": upstream.id}),
        ));
    }
    if !upstream.read_only && !state.service.settings.upstream_write {
        return Err(reject(
            state,
            actor,
            McpError::new(
                "MCP_UPSTREAM_WRITE_FORBIDDEN",
                json!({"id": upstream.id, "hint": "设置 JUMCP_MCP_ALLOW_WRITE_UPSTREAM=1 后重启"}),
            ),
            json!({"surface": surface, "id": upstream.id}),
        ));
    }
    upstream.validate().map_err(|e| {
        reject(
            state,
            actor,
            e,
            json!({"surface": surface, "id": upstream.id}),
        )
    })?;
    Ok(upstream)
}

/// 管理面条目：脱敏配置 + 运行态（status 取缓存，不触发探测）。
fn upstream_item(def: &McpUpstream, runtime: &crate::upstream::UpstreamRuntime) -> Value {
    let mut item = def.to_public_value();
    if let Some(obj) = item.as_object_mut() {
        obj.insert("status".into(), json!(runtime.status));
        obj.insert("status_message".into(), json!(runtime.status_message));
        obj.insert("tool_count".into(), json!(runtime.tool_count));
        obj.insert(
            "last_checked_at".into(),
            runtime
                .last_checked_at
                .as_ref()
                .map_or(Value::Null, |t| json!(t)),
        );
    }
    item
}

/// `GET /admin/upstreams`：全部上游 + 缓存运行态。
async fn list_upstreams(State(state): State<AdminState>) -> Result<Json<Value>, ApiError> {
    const SURFACE: &str = "admin/upstreams";
    let actor = admin_actor("");
    let router = require_router(&state, &actor, SURFACE)?;
    let views = router
        .views()
        .map_err(|e| reject(&state, &actor, e, json!({"surface": SURFACE})))?;
    let items: Vec<Value> = views
        .iter()
        .map(|(def, runtime)| upstream_item(def, runtime))
        .collect();
    Ok(Json(json!({"items": items, "total": items.len()})))
}

/// `GET /admin/upstreams/{id}`。
async fn get_upstream(
    State(state): State<AdminState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    const SURFACE: &str = "admin/upstreams/get";
    let actor = admin_actor("");
    let router = require_router(&state, &actor, SURFACE)?;
    let def = router
        .store()
        .upstream(&id)
        .map_err(|e| reject(&state, &actor, e, json!({"surface": SURFACE})))?
        .ok_or_else(|| {
            reject(
                &state,
                &actor,
                McpError::new("MCP_UPSTREAM_NOT_FOUND", json!({"id": id})),
                json!({"surface": SURFACE, "id": id}),
            )
        })?;
    let runtime = router.runtime(&def);
    Ok(Json(upstream_item(&def, &runtime)))
}

/// `POST /admin/upstreams`：新建上游；下一次 `tools/list` 即包含它的工具（无需重启）。
async fn create_upstream(
    State(state): State<AdminState>,
    Extension(auth): Extension<AuthenticatedAdmin>,
    headers: HeaderMap,
    Json(body): Json<UpstreamInput>,
) -> Result<Json<Value>, ApiError> {
    const SURFACE: &str = "admin/upstreams/create";
    let actor = actor_of(&auth, &headers)
        .map_err(|e| reject(&state, &admin_actor(""), e, json!({"surface": SURFACE})))?;
    require_audit(&state, &actor, SURFACE)?;
    let router = require_router(&state, &actor, SURFACE)?;
    let upstream = build_upstream(&state, &actor, SURFACE, None, body, None)?;
    let created = router
        .store()
        .insert_upstream(&upstream, &actor)
        .map_err(|e| {
            reject(
                &state,
                &actor,
                e,
                json!({"surface": SURFACE, "id": upstream.id}),
            )
        })?;
    router.invalidate(&created.id);
    let runtime = router.runtime(&created);
    Ok(Json(upstream_item(&created, &runtime)))
}

/// `PUT /admin/upstreams/{id}`：id 不可改；配置指纹变了就丢连接与工具缓存。
async fn update_upstream(
    State(state): State<AdminState>,
    Extension(auth): Extension<AuthenticatedAdmin>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<UpstreamInput>,
) -> Result<Json<Value>, ApiError> {
    const SURFACE: &str = "admin/upstreams/update";
    let actor = actor_of(&auth, &headers)
        .map_err(|e| reject(&state, &admin_actor(""), e, json!({"surface": SURFACE})))?;
    require_audit(&state, &actor, SURFACE)?;
    let router = require_router(&state, &actor, SURFACE)?;
    let existing = router
        .store()
        .upstream(&id)
        .map_err(|e| reject(&state, &actor, e, json!({"surface": SURFACE})))?
        .ok_or_else(|| {
            reject(
                &state,
                &actor,
                McpError::new("MCP_UPSTREAM_NOT_FOUND", json!({"id": id})),
                json!({"surface": SURFACE, "id": id}),
            )
        })?;
    let upstream = build_upstream(&state, &actor, SURFACE, Some(&id), body, Some(&existing))?;
    let updated = router
        .store()
        .update_upstream(&upstream, &actor)
        .map_err(|e| reject(&state, &actor, e, json!({"surface": SURFACE, "id": id})))?;
    if existing.definition_hash() != updated.definition_hash() {
        router.invalidate(&updated.id);
    }
    let runtime = router.runtime(&updated);
    Ok(Json(upstream_item(&updated, &runtime)))
}

/// `DELETE /admin/upstreams/{id}`。
async fn delete_upstream(
    State(state): State<AdminState>,
    Extension(auth): Extension<AuthenticatedAdmin>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    const SURFACE: &str = "admin/upstreams/delete";
    let actor = actor_of(&auth, &headers)
        .map_err(|e| reject(&state, &admin_actor(""), e, json!({"surface": SURFACE})))?;
    require_audit(&state, &actor, SURFACE)?;
    let router = require_router(&state, &actor, SURFACE)?;
    router
        .store()
        .delete_upstream(&id, &actor)
        .map_err(|e| reject(&state, &actor, e, json!({"surface": SURFACE, "id": id})))?;
    router.remove(&id);
    Ok(Json(json!({"deleted": true})))
}

/// `POST /admin/upstreams/{id}/refresh`：立即建连 + 拉取工具。
///
/// 连接失败仍返回 200，由 `status`/`status_message` 表达（UI 直接展示，不把探测失败
/// 变成 HTTP 错误）；连接成功则刷新缓存与 `last_checked_at`。
async fn refresh_upstream(
    State(state): State<AdminState>,
    Extension(auth): Extension<AuthenticatedAdmin>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    const SURFACE: &str = "admin/upstreams/refresh";
    let actor = actor_of(&auth, &headers)
        .map_err(|e| reject(&state, &admin_actor(""), e, json!({"surface": SURFACE})))?;
    require_audit(&state, &actor, SURFACE)?;
    let router = require_router(&state, &actor, SURFACE)?;
    let runtime = router
        .refresh(&id)
        .await
        .map_err(|e| reject(&state, &actor, e, json!({"surface": SURFACE, "id": id})))?;
    let def = router
        .store()
        .upstream(&id)
        .map_err(|e| reject(&state, &actor, e, json!({"surface": SURFACE, "id": id})))?
        .ok_or_else(|| {
            reject(
                &state,
                &actor,
                McpError::new("MCP_UPSTREAM_NOT_FOUND", json!({"id": id})),
                json!({"surface": SURFACE, "id": id}),
            )
        })?;
    let mut payload = audit_base(
        "upstream",
        &id,
        "refresh",
        if runtime.status == "error" {
            "error"
        } else {
            "ok"
        },
    );
    if runtime.status == "error" {
        payload.insert("error_code".into(), json!("MCP_UPSTREAM_UNREACHABLE"));
        payload.insert("error_message".into(), json!(runtime.status_message));
    }
    payload.insert(
        "data".into(),
        json!({
            "transport": def.transport,
            "group": def.group,
            "category": def.category,
            "read_only": def.read_only,
            "tool_count": runtime.tool_count,
            "status": runtime.status
        }),
    );
    if let Err(e) = state
        .service
        .audit_event("admin_change", Value::Object(payload), &actor)
    {
        return Err(reject(
            &state,
            &actor,
            e,
            json!({"surface": SURFACE, "id": id}),
        ));
    }
    Ok(Json(upstream_item(&def, &runtime)))
}

/// 供测试与文档引用的路由清单（避免文档与实现漂移）。
pub fn routes() -> Vec<(&'static str, &'static str)> {
    vec![
        ("GET", "/health"),
        ("GET", "/admin/me"),
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
    ]
}
