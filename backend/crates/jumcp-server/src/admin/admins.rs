//! 管理员账号管理：`/admin/admins` 的 CRUD。
//!
//! 为什么这些 handler 自己写审计而不是让存储层写：审计要记"谁改了什么"和字段级 diff，
//! 存储层只拿得到新值；同时口令与哈希一律不进审计载荷（只记字段名与是否变更）。

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, StatusCode};
use jumcp_core::entry::Actor;
use jumcp_core::errors::McpError;
use jumcp_core::registry::AdminUser;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    AdminState, ApiError, AuthenticatedAdmin, actor_of, admin_actor, audit_base, reject,
    require_audit,
};

#[derive(Debug, Deserialize)]
pub(super) struct CreateAdminBody {
    username: String,
    password: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct UpdateAdminBody {
    username: Option<String>,
    password: Option<String>,
    state: Option<String>,
}

fn changed_fields(
    user: &AdminUser,
    username: Option<&str>,
    state: Option<&str>,
    password_changed: bool,
) -> Vec<&'static str> {
    let mut fields = Vec::new();
    if username.is_some_and(|name| name != user.username) {
        fields.push("username");
    }
    if password_changed {
        fields.push("password");
    }
    if state.is_some_and(|s| s != user.state) {
        fields.push("state");
    }
    fields
}

/// `GET /admin/admins`：账号清单（不含任何哈希，也不含口令）。
pub(super) async fn list_admins(
    State(state): State<AdminState>,
    Extension(auth): Extension<AuthenticatedAdmin>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let actor = actor_of(&auth, &headers).map_err(|e| {
        reject(
            &state,
            &admin_actor(""),
            e,
            json!({"surface": "admin/admins"}),
        )
    })?;
    let users = state
        .store
        .list_admin_users()
        .map_err(|e| reject(&state, &actor, e, json!({"surface": "admin/admins"})))?;
    let items: Vec<Value> = users.iter().map(AdminUser::view).collect();
    Ok(Json(json!({"items": items, "total": items.len()})))
}

/// `POST /admin/admins`：新建管理员（初始 state=active）。
pub(super) async fn create_admin(
    State(state): State<AdminState>,
    Extension(auth): Extension<AuthenticatedAdmin>,
    headers: HeaderMap,
    Json(body): Json<CreateAdminBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    const SURFACE: &str = "admin/admins/create";
    let actor = actor_of(&auth, &headers)
        .map_err(|e| reject(&state, &admin_actor(""), e, json!({"surface": SURFACE})))?;
    require_audit(&state, &actor, SURFACE)?;
    let user = state
        .store
        .create_admin_user(&body.username, &body.password)
        .map_err(|e| {
            reject(
                &state,
                &actor,
                e,
                json!({"surface": SURFACE, "username": body.username}),
            )
        })?;
    audit_change(&state, &actor, "admin.create", &user, &[], SURFACE)?;
    Ok((StatusCode::CREATED, Json(json!({"admin": user.view()}))))
}

/// `PATCH /admin/admins/{id}`：改用户名 / 改口令 / 启用禁用。
///
/// 改口令或禁用由存储层在同一事务里吊销该账号全部会话——响应之后再发来的旧 cookie 一律 401。
pub(super) async fn update_admin(
    State(state): State<AdminState>,
    Extension(auth): Extension<AuthenticatedAdmin>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<UpdateAdminBody>,
) -> Result<Json<Value>, ApiError> {
    const SURFACE: &str = "admin/admins/update";
    let actor = actor_of(&auth, &headers)
        .map_err(|e| reject(&state, &admin_actor(""), e, json!({"surface": SURFACE})))?;
    if body.username.is_none() && body.password.is_none() && body.state.is_none() {
        return Err(reject(
            &state,
            &actor,
            McpError::validation("PATCH 至少提供 username/password/state 之一".to_string()),
            json!({"surface": SURFACE, "id": id}),
        ));
    }
    require_audit(&state, &actor, SURFACE)?;
    let before = state
        .store
        .admin_user(&id)
        .map_err(|e| reject(&state, &actor, e, json!({"surface": SURFACE, "id": id})))?
        .ok_or_else(|| {
            reject(
                &state,
                &actor,
                McpError::new("MCP_ADMIN_USER_NOT_FOUND", json!({"id": id})),
                json!({"surface": SURFACE, "id": id}),
            )
        })?;
    let changes = changed_fields(
        &before,
        body.username.as_deref(),
        body.state.as_deref(),
        body.password.is_some(),
    );
    let updated = state
        .store
        .update_admin_user(
            &id,
            body.username.as_deref(),
            body.password.as_deref(),
            body.state.as_deref(),
        )
        .map_err(|e| reject(&state, &actor, e, json!({"surface": SURFACE, "id": id})))?;
    audit_change(&state, &actor, "admin.update", &updated, &changes, SURFACE)?;
    Ok(Json(json!({"admin": updated.view()})))
}

/// `DELETE /admin/admins/{id}`：删账号并吊销其会话；删除自己与删掉最后一名启用账号都是 409。
pub(super) async fn delete_admin(
    State(state): State<AdminState>,
    Extension(auth): Extension<AuthenticatedAdmin>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    const SURFACE: &str = "admin/admins/delete";
    let actor = actor_of(&auth, &headers)
        .map_err(|e| reject(&state, &admin_actor(""), e, json!({"surface": SURFACE})))?;
    if id == auth.user_id {
        return Err(reject(
            &state,
            &actor,
            McpError::new(
                "MCP_ADMIN_SELF_DELETE",
                json!({"id": id, "username": auth.id}),
            ),
            json!({"surface": SURFACE, "id": id}),
        ));
    }
    require_audit(&state, &actor, SURFACE)?;
    let before = state
        .store
        .admin_user(&id)
        .map_err(|e| reject(&state, &actor, e, json!({"surface": SURFACE, "id": id})))?
        .ok_or_else(|| {
            reject(
                &state,
                &actor,
                McpError::new("MCP_ADMIN_USER_NOT_FOUND", json!({"id": id})),
                json!({"surface": SURFACE, "id": id}),
            )
        })?;
    state
        .store
        .delete_admin_user(&id)
        .map_err(|e| reject(&state, &actor, e, json!({"surface": SURFACE, "id": id})))?;
    audit_change(&state, &actor, "admin.delete", &before, &[], SURFACE)?;
    Ok(StatusCode::NO_CONTENT)
}

/// 账号变更审计：只记字段名与目标账号，`password`/`password_hash` 永不出现在载荷里。
fn audit_change(
    state: &AdminState,
    actor: &Actor,
    action: &str,
    user: &AdminUser,
    changes: &[&str],
    surface: &str,
) -> Result<(), ApiError> {
    let mut payload = audit_base("admin_user", &user.id, action, "ok");
    payload.insert(
        "data".into(),
        json!({"username": user.username, "state": user.state}),
    );
    if !changes.is_empty() {
        // 与能力/组/上游审计共用 {field,before,after} 形状，管理台才能按字段名渲染；
        // 管理账户审计只记录字段名，口令等敏感值永不进载荷。
        let changes: Vec<Value> = changes
            .iter()
            .map(|field| json!({"field": field, "before": null, "after": null}))
            .collect();
        payload.insert("changes".into(), json!(changes));
    }
    state
        .service
        .audit_event("admin_change", Value::Object(payload), actor)
        .map(|_| ())
        .map_err(|e| reject(state, actor, e, json!({"surface": surface})))
}
