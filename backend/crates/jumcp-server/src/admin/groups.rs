use super::*;

/// 管理组条目的唯一形状：`source` 区分配置组、API 管理的组与代码内置组。
///
/// `code` 是唯一权限标识（能力/令牌/scope/审计都用它），`name` 只是中文展示名；
/// 调用方不得用 `name` 做任何权限判定。`grantable=false` 表示该组不能出现在令牌/
/// scope 的组列表里（目前只有 `basic`：它不是授权组）。
pub(super) struct GroupItem<'a> {
    code: &'a str,
    name: &'a str,
    priority: i64,
    min_budget: i64,
    roles: &'a [String],
    source: &'a str,
    grantable: bool,
    capability_count: i64,
    token_count: i64,
}

pub(super) fn group_item(item: GroupItem<'_>) -> Value {
    json!({
        "code": item.code,
        "name": item.name,
        "priority": item.priority,
        "min_budget": item.min_budget,
        "roles": item.roles,
        "source": item.source,
        "grantable": item.grantable,
        "capability_count": item.capability_count,
        "token_count": item.token_count
    })
}

/// 角色门禁：必须是 `settings.policy.roles` 里已定义的角色 code；去重保序。未知角色 → 400。
pub(super) fn normalize_roles(
    state: &AdminState,
    roles: Vec<String>,
) -> Result<Vec<String>, McpError> {
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
pub(super) fn validate_group_fields(
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

/// `GET /admin/groups?q=&source=&role=`：代码内置组（`source=builtin`）+ 配置组
/// （`source=policy`）+ 管理组（`source=managed`）。
///
/// 过滤只影响返回集合与 `total`：`q` 对 code/name 做小写包含匹配；`source` 只接受
/// policy|managed|builtin（其他值 400）；`role` 按角色 code 精确匹配该条目的授权角色。
/// policy 组的 `roles` 由 `policy.roles` 反查（哪些角色的 default_groups 含该组 code）。
/// `basic` 组不对应任何行：成员是常驻内置工具 + 被逐工具覆盖到 basic 的上游工具。
#[derive(Debug, Deserialize)]
pub(super) struct GroupListQuery {
    q: Option<String>,
    source: Option<String>,
    role: Option<String>,
}

pub(super) async fn list_groups(
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
    let basic_upstream_tools = state
        .store
        .count_basic_upstream_tools()
        .map_err(|e| reject(&state, &actor, e, json!({"surface": "admin/groups"})))?;
    let count_of = |code: &str| counts.get(code).copied().unwrap_or(0);
    let tokens_of = |code: &str| token_counts.get(code).copied().unwrap_or(0);
    let mut items: Vec<Value> = vec![group_item(GroupItem {
        // 合成条目：basic 由代码内置，不来自 policy / managed 任何一张表。
        code: crate::discovery::BASIC_GROUP,
        name: "基础工具",
        priority: 0,
        min_budget: 800,
        roles: &[],
        source: "builtin",
        grantable: false,
        capability_count: RESIDENT_CAPABILITY_IDS.len() as i64 + basic_upstream_tools,
        token_count: 0,
    })];
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
            grantable: true,
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
            grantable: true,
            capability_count: count_of(&group.code),
            token_count: tokens_of(&group.code),
        }));
    }
    let source_filter = match query.source.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(source @ ("policy" | "managed" | "builtin")) => Some(source),
        Some(other) => {
            return Err(reject(
                &state,
                &actor,
                McpError::validation(format!(
                    "source 非法: {other}（只接受 policy|managed|builtin）"
                )),
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
pub(super) struct CreateGroupBody {
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
pub(super) async fn create_group(
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
            grantable: true,
            capability_count: 0,
            token_count: 0,
        })),
    ))
}

#[derive(Debug, Deserialize)]
pub(super) struct UpdateGroupBody {
    code: Option<String>,
    name: Option<String>,
    priority: Option<i64>,
    min_budget: Option<i64>,
    roles: Option<Vec<String>>,
}

/// `PUT /admin/groups/{code}`：只能改管理组；配置组与保留组 409。
///
/// code 不可改：路径即 code，body 里出现 `code` 一律 400（拒绝静默忽略）。
pub(super) async fn update_group(
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
        grantable: true,
        capability_count: count,
        token_count,
    })))
}

/// `DELETE /admin/groups/{code}`：只能删管理组；仍被能力/令牌引用 → 409（双计数）。
pub(super) async fn delete_group(
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
pub(super) async fn list_roles(State(state): State<AdminState>) -> Result<Json<Value>, ApiError> {
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
