use super::*;

#[derive(Debug, Deserialize)]
pub(super) struct DefinitionBody {
    definition: Option<Value>,
    approval_id: Option<String>,
}

/// 模板定义可能以两种形状进入：注册源形状（顶层 `template_id`/`sql`）或详情端点返回的
/// 存储形状（`{"template": {...}, "base_sql": ..., "dialect": ...}`）。
///
/// 判定以"存在 `template` 对象"为准，统一抽出 `TemplateDef` 源定义再交给 TemplateRegistry；
/// 否则前端"GET 详情 → 原样回写"的流程会被 `deny_unknown_fields` 判成非法定义。
pub(super) fn template_source(definition: &Value) -> Value {
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
/// - `published`：必须带 `approval_id`；批准人一律取会话账号派生（可与提审人相同）；
/// - `review`：必须先退回 draft；`deprecated` / 其它：拒绝。
///
/// `kind == "template"` 一律走 `TemplateRegistry`（parse → 编译 → sqlguard），
/// 管理面无法绕过模板 SQL 门禁；`template_id` 必须与路径 id 一致。
pub(super) async fn update_definition(
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
            // 批准人（当前会话账号）由鉴权派生，允许与提审人相同。
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

/// draft → review。registry 不写 admin_change（状态推进本身不是治理变更），在此层补记。
pub(super) async fn submit(
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
pub(super) struct PublishBody {
    approval_id: Option<String>,
}

/// review → published。`approval_id` 必填，批准人取会话账号派生身份。
pub(super) async fn publish(
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
    // 批准人一律取会话账号派生 id：请求体不接受 approved_by。
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
pub(super) struct RejectBody {
    reason: Option<String>,
}

/// `POST /admin/capabilities/{id}/reject`：review → draft。
/// registry 不写 admin_change（与 submit 同口径），在此层补记并把 reason 放进 payload。
pub(super) async fn reject_capability(
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
pub(super) struct VisibilityBody {
    discoverable: Option<bool>,
    callable: Option<bool>,
}

pub(super) async fn visibility(
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

#[derive(Debug, Deserialize)]
pub(super) struct CreateCapabilityBody {
    definition: Option<Value>,
}

/// `POST /admin/capabilities`：人工登记草案，与 MCP `admin_register_template` 走同一条路径
/// （`TemplateRegistry::register` = 定义门禁 + 编译 + store 落 draft），只产草案。
pub(super) async fn create_capability(
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
    // 内置 id 不可写：模板门的 id 格式要求点号命名空间，builtin id 会先被判成 400，
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
pub(super) struct DeprecateBody {
    successor_id: Option<String>,
}

/// `POST /admin/capabilities/{id}/deprecate`：下架并记录 successor（`admin_change` 在此层写）。
pub(super) async fn deprecate_capability(
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
