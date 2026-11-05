//! Admin HTTP API（W4.1 最小版）：审核队列 / 状态流转 / 可见性 / 审计查询 + 健康检查。
//!
//! 边界：
//! - 只在 streamable-http 模式挂载（stdio 无管理面）；未配置 `HJZL_ADMIN_TOKENS` 时进程拒绝启动。
//! - 身份只能来自 token→actor 映射：`X-Admin-Actor` 仅用于一致性校验，永远以派生 id 为准。
//! - 全部拒绝写 `policy_decision`，成功写 `admin_change`（submit 在此层写，publish/visibility 由
//!   CapabilityStore 写，避免同一变更两条记录）。
//! - 审计查询只读：不提供任何 UPDATE/DELETE 入口，DB 层另有 append-only trigger 兜底。

use crate::service::Service;
use axum::extract::{Extension, Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use hjzl_core::audit::ChainVerdict;
use hjzl_core::config::AdminTokens;
use hjzl_core::entry::Actor;
use hjzl_core::errors::McpError;
use hjzl_core::registry::{CapabilityRecord, CapabilityStore, STATUSES};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::sync::Arc;

const ACTOR_HEADER: &str = "x-admin-actor";
const MAX_LIMIT: i64 = 200;
const DEFAULT_LIMIT: i64 = 50;
const FILTER_KEYS: &[&str] = &[
    "tenant",
    "event_type",
    "session_id",
    "actor_role",
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
        .route("/audit-events", get(audit_events))
        .route("/capabilities", get(capabilities))
        .route("/review-queue", get(review_queue))
        .route("/capabilities/{id}/submit", post(submit))
        .route("/capabilities/{id}/publish", post(publish))
        .route("/capabilities/{id}/visibility", post(visibility))
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
        | "MCP_SCHEMA_OUT_OF_SCOPE" => StatusCode::BAD_REQUEST,
        "MCP_STATE_TRANSITION_INVALID" | "MCP_VERSION_CONFLICT" => StatusCode::CONFLICT,
        "MCP_CAPABILITY_NOT_FOUND" => StatusCode::NOT_FOUND,
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

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let raw = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let (scheme, token) = raw.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
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
    let mut actor = Actor::new("default", "admin");
    actor.actor_type = "admin".to_string();
    actor.actor_id = auth.id.clone();
    Ok(actor)
}

/// 拒绝路径统一审计：写失败则把审计错误上抛（治理面不可静默失效）。
fn reject(state: &AdminState, actor: &Actor, e: McpError, detail: Value) -> ApiError {
    match state.service.policy_decision(actor, &e.code, detail) {
        Ok(()) => ApiError(e),
        Err(audit_error) => ApiError(audit_error),
    }
}

async fn require_token(State(state): State<AdminState>, mut req: Request, next: Next) -> Response {
    let derived = bearer(req.headers()).and_then(|token| state.tokens.authenticate(token));
    let Some(id) = derived else {
        let actor = Actor::new("default", "admin");
        let e = McpError::new(
            "MCP_UNAUTHORIZED",
            json!({"hint": "管理面需要 Authorization: Bearer <HJZL_ADMIN_TOKENS 中的 token>"}),
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
    let actor = Actor::new("default", "admin");
    let mut filters = Map::new();
    let mut limit = DEFAULT_LIMIT;
    let mut offset = 0i64;
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
        .query_page(Some(&Value::Object(filters)), limit, offset)
        .map_err(|e| reject(&state, &actor, e, json!({"surface": "admin/audit-events"})))?;
    Ok(Json(json!({
        "events": events,
        "total": total,
        "limit": limit,
        "offset": offset
    })))
}

fn queue_item(rec: &CapabilityRecord) -> Value {
    json!({
        "id": rec.capability_id,
        "kind": rec.kind,
        "group": rec.group,
        "risk": rec.risk,
        "status": rec.status,
        "version": rec.version,
        "updated": rec.updated_at,
        "definition_hash": rec.definition_hash
    })
}

async fn review_queue(State(state): State<AdminState>) -> Result<Json<Value>, ApiError> {
    let actor = Actor::new("default", "admin");
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
        "updated": rec.updated_at
    })
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
    let actor = Actor::new("default", "admin");
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
            "limit" => limit = parse_page_param(&state, &actor, "limit", &value)?,
            "offset" => offset = parse_page_param(&state, &actor, "offset", &value)?,
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
    field: &str,
    value: &str,
) -> Result<i64, ApiError> {
    value.parse::<i64>().map_err(|_| {
        reject(
            state,
            actor,
            McpError::new("MCP_BAD_REQUEST", json!({"field": field, "value": value})),
            json!({"surface": "admin/capabilities", "field": field}),
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
            &Actor::new("default", "admin"),
            e,
            json!({"surface": "admin/submit"}),
        )
    })?;
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
    state
        .service
        .audit_event(
            "admin_change",
            json!({
                "capability_id": id,
                "before_hash": before.definition_hash,
                "after_hash": record.definition_hash,
                "diff": format!("{}->{}", before.status, record.status),
                "approval_id": format!("submit:{}", actor.actor_id)
            }),
            &actor,
        )
        .map_err(|e| reject(&state, &actor, e, json!({"capability_id": id})))?;
    Ok(Json(json!({
        "id": record.capability_id,
        "status": record.status,
        "version": record.version,
        "updated": record.updated_at
    })))
}

#[derive(Debug, Deserialize)]
struct PublishBody {
    approval_id: Option<String>,
    approved_by: Option<String>,
}

/// review → published。四眼判定只在这里做一次（registry 是唯一执行点）。
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
            &Actor::new("default", "admin"),
            e,
            json!({"surface": "admin/publish"}),
        )
    })?;
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
    let approved_by = body
        .approved_by
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            reject(
                &state,
                &actor,
                McpError::new("MCP_APPROVAL_REQUIRED", json!({"missing": ["approved_by"]})),
                json!({"capability_id": id}),
            )
        })?;
    // approved_by 只做一致性校验：真正的批准人身份来自 token 派生，不采信请求体自述。
    if approved_by != auth.id {
        let e = McpError::new(
            "MCP_BAD_REQUEST",
            json!({"derived": auth.id, "declared": approved_by, "field": "approved_by"}),
        );
        return Err(reject(&state, &actor, e, json!({"capability_id": id})));
    }
    let record = state
        .store
        .promote_to_published(&id, &approval_id, &actor.actor_id, "")
        .map_err(|e| reject(&state, &actor, e, json!({"capability_id": id})))?;
    Ok(Json(json!({
        "id": record.capability_id,
        "status": record.status,
        "version": record.version,
        "updated": record.updated_at
    })))
}

#[derive(Debug, Deserialize)]
struct VisibilityBody {
    discoverable: Option<bool>,
    callable: Option<bool>,
    approval_id: Option<String>,
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
            &Actor::new("default", "admin"),
            e,
            json!({"surface": "admin/visibility"}),
        )
    })?;
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
    let record = state
        .store
        .set_visibility(
            &id,
            body.discoverable,
            body.callable,
            &approval_id,
            &actor.actor_id,
        )
        .map_err(|e| reject(&state, &actor, e, json!({"capability_id": id})))?;
    Ok(Json(json!({
        "id": record.capability_id,
        "discoverable": record.discoverable,
        "callable": record.callable,
        "version": record.version,
        "updated": record.updated_at
    })))
}

/// 供测试与文档引用的路由清单（避免文档与实现漂移）。
pub fn routes() -> Vec<(&'static str, &'static str)> {
    vec![
        ("GET", "/health"),
        ("GET", "/admin/audit-events"),
        ("GET", "/admin/capabilities"),
        ("GET", "/admin/review-queue"),
        ("POST", "/admin/capabilities/{id}/submit"),
        ("POST", "/admin/capabilities/{id}/publish"),
        ("POST", "/admin/capabilities/{id}/visibility"),
    ]
}
