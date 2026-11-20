use super::*;

/// 令牌条目的唯一形状：永不包含明文或 hash。
pub(super) fn token_item(record: &AccessTokenRecord) -> Value {
    record.to_json()
}

/// 稳定去重（保序、去空白）：组/内置工具列表都走这一处。
pub(super) fn normalize_names(values: Vec<String>) -> Vec<String> {
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
pub(super) async fn list_tokens(State(state): State<AdminState>) -> Result<Json<Value>, ApiError> {
    let actor = admin_actor("");
    let records = state
        .store
        .list_access_tokens()
        .map_err(|e| reject(&state, &actor, e, json!({"surface": "admin/tokens"})))?;
    let items: Vec<Value> = records.iter().map(token_item).collect();
    Ok(Json(json!({"items": items, "total": items.len()})))
}

#[derive(Debug, Deserialize)]
pub(super) struct CreateTokenBody {
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
pub(super) async fn create_token(
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

/// `POST /admin/tokens/{id}/revoke`：前端契约直接返回 `AccessTokenItem`（不包 `item`）。
pub(super) async fn revoke_token(
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
pub(super) async fn reissue_token(
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
pub(super) async fn delete_token(
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
pub(super) fn builtin_label(id: &str) -> &'static str {
    match id {
        "search_capabilities" => "检索能力",
        "describe_capability" => "描述能力",
        "list_groups" => "组索引",
        "db_run_template" => "模板执行",
        "admin_register_template" => "登记模板草案",
        "admin_review_queue" => "审核队列",
        "admin_audit_queries" => "审计查询",
        "admin_tool_candidates" => "定制化候选评分",
        "admin_set_visibility" => "改可见性",
        "admin_publish_capability" => "发布能力",
        _ => "内置工具",
    }
}

/// `GET /admin/tokens/options`：角色（含默认组/预算）、组（含未撤销 token 引用数）、
/// 内置工具（中文标签 + builtin/admin 分组）。
pub(super) async fn token_options(
    State(state): State<AdminState>,
) -> Result<Json<Value>, ApiError> {
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
    // 角色：code 是权限标识，name 是中文展示名。
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
                "group": if ADMIN_CAPABILITY_IDS.contains(id) { "admin" } else { crate::discovery::BASIC_GROUP }
            })
        })
        .collect();
    // 上游工具授予面：只读现有缓存（views/cached_tools 都不触网），未 refresh 的上游
    // 也出现（tool_count=0、tools 为空），前端才能提示"安装后先刷新一次"。
    let (tool_wildcards, upstreams, upstream_tools) = match &state.service.upstreams {
        None => (Vec::new(), Vec::new(), Vec::new()),
        Some(manager) => {
            let wildcards = vec![json!({
                "value": jumcp_core::registry::TOOL_WILDCARD_ALL,
                "label": "全部工具（含后续安装的上游）"
            })];
            let overrides = state.store.upstream_tool_groups().map_err(|e| {
                reject(
                    &state,
                    &actor,
                    e,
                    json!({"surface": "admin/tokens/options"}),
                )
            })?;
            let mut views = manager.views().map_err(|e| {
                reject(
                    &state,
                    &actor,
                    e,
                    json!({"surface": "admin/tokens/options"}),
                )
            })?;
            views.sort_by(|a, b| a.0.id.cmp(&b.0.id));
            let mut upstreams: Vec<Value> = Vec::with_capacity(views.len());
            let mut upstream_tools: Vec<Value> = Vec::new();
            for (def, runtime) in views {
                upstreams.push(json!({
                    "id": def.id,
                    "name": def.name,
                    "group": def.group,
                    "enabled": def.enabled,
                    "tool_count": runtime.tool_count,
                    "wildcard": format!("{}__*", def.id),
                }));
                for tool in manager.cached_tools(&def.id) {
                    // 与 tools/list 共用同一条生效组判定：覆盖到 basic 但已不合格的工具
                    // 在公共通道里不可见，不能出现在可授予清单里（勾选后看不到也调不到）。
                    let overridden = overrides.iter().any(|o| {
                        o.upstream_id == def.id
                            && o.tool_name == tool.name
                            && o.group_code == crate::discovery::BASIC_GROUP
                    });
                    let Some(group) =
                        crate::upstream::effective_tool_group(&def, &tool, overridden)
                    else {
                        continue;
                    };
                    upstream_tools.push(json!({
                        // 暴露名是 allowlist 与 tools/list 的同一形状（非法字符已转 `-`），
                        // 按原始名拼接会给出一个根本授权不了的字符串。
                        "name": crate::upstream::exposed_tool_name(&def.id, &tool.name),
                        "upstream_id": def.id,
                        "tool": tool.name,
                        "group": group,
                        "label": format!("{} / {}", def.name, tool.name),
                    }));
                }
            }
            upstream_tools.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
            (wildcards, upstreams, upstream_tools)
        }
    };
    Ok(Json(json!({
        "roles": roles,
        "groups": groups,
        "builtin_tools": builtin_tools,
        "tool_wildcards": tool_wildcards,
        "upstreams": upstreams,
        "upstream_tools": upstream_tools
    })))
}
