use super::upstreams::require_router;
use super::*;

/// 管理组条目的唯一形状：`source` 区分配置组、API 管理的组与代码内置组。
///
/// `code` 是唯一权限标识（能力/令牌/scope/审计都用它），`name` 只是中文展示名；
/// 调用方不得用 `name` 做任何权限判定。`grantable=false` 留给不能出现在令牌/scope
/// 组列表里的条目——当前所有组都可授权（`basic` 是代码内置的普通授权组）。
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
/// `basic` 是代码内置条目（`source=builtin`），不来自 policy/managed 任何一张表，但和其余组
/// 一样可授权、可配组规则；计数 = 常驻内置工具数 + 本组可见的上游工具数（读缓存，不触网）。
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
    let exposure = state
        .store
        .group_exposure()
        .map_err(|e| reject(&state, &actor, e, json!({"surface": "admin/groups"})))?;
    let count_of = |code: &str| {
        counts.get(code).copied().unwrap_or(0) + exposed_tool_count(&state, &exposure, code)
    };
    let tokens_of = |code: &str| token_counts.get(code).copied().unwrap_or(0);
    let mut items: Vec<Value> = vec![group_item(GroupItem {
        // 代码内置组：不来自 policy / managed 任何一张表，但可被授予、可被组规则使用。
        code: crate::discovery::BASIC_GROUP,
        name: "基础工具",
        priority: 0,
        min_budget: 800,
        roles: &[],
        source: "builtin",
        grantable: true,
        capability_count: RESIDENT_CAPABILITY_IDS.len() as i64
            + exposed_tool_count(&state, &exposure, crate::discovery::BASIC_GROUP),
        token_count: tokens_of(crate::discovery::BASIC_GROUP),
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

/// 组内可见的上游工具数：只数已缓存的工具清单（不触网），从未拉取过的上游计 0。
fn exposed_tool_count(
    state: &AdminState,
    exposure: &jumcp_core::registry::GroupExposure,
    group: &str,
) -> i64 {
    let Some(router) = state.service.upstreams() else {
        return 0;
    };
    let Ok(defs) = state.store.upstreams() else {
        return 0;
    };
    defs.iter()
        .filter(|def| def.enabled)
        .map(|def| {
            router
                .cached_tools(&def.id)
                .iter()
                .filter(|tool| exposure.visible(group, &def.id, &tool.name))
                .count() as i64
        })
        .sum()
}

/// 组必须存在：`basic` / 配置组 / 管理组；其余 404。
fn ensure_group(
    state: &AdminState,
    actor: &Actor,
    code: &str,
    surface: &str,
) -> Result<(), ApiError> {
    let policy = state
        .service
        .effective_policy()
        .map_err(|e| reject(state, actor, e, json!({"surface": surface, "group": code})))?;
    if policy.group(code).is_some() {
        return Ok(());
    }
    Err(reject(
        state,
        actor,
        McpError::new("MCP_GROUP_NOT_FOUND", json!({"group": code})),
        json!({"surface": surface, "group": code}),
    ))
}

/// 组视角的暴露矩阵：每台上游的第一层授予 + 每个缓存工具的规则与生效可见性。
///
/// 只读缓存（`cached_tools`），不触网；需要最新工具清单时先 `POST /admin/upstreams/{id}/refresh`。
fn exposure_view(state: &AdminState, group: &str) -> Result<Value, McpError> {
    let exposure = state.store.group_exposure()?;
    let defs = state.store.upstreams()?;
    let router = state.service.upstreams();
    let mut upstreams: Vec<Value> = Vec::new();
    let mut tools: Vec<Value> = Vec::new();
    // 停用的上游一样要列：它的规则仍存在于库里，只读视图不列出来就永远清不掉。
    for def in defs.iter() {
        let enabled = def.enabled;
        let cached = router
            .as_ref()
            .map(|r| r.cached_tools(&def.id))
            .unwrap_or_default();
        upstreams.push(json!({
            "id": def.id,
            "name": def.name,
            "category": def.category,
            "admin_only": crate::upstream::admin_only(def, None),
            "enabled": enabled,
            "granted": exposure.granted(group, &def.id),
            "tool_count": cached.len()
        }));
        for tool in cached {
            tools.push(json!({
                "upstream_id": def.id,
                "tool": tool.name,
                "exposed": crate::upstream::exposed_tool_name(&def.id, &tool.name),
                "state": exposure
                    .rule(group, &def.id, &tool.name)
                    .map(|s| s.as_str())
                    .unwrap_or("inherit"),
                "visible": exposure.visible(group, &def.id, &tool.name),
                "admin_only": crate::upstream::admin_only(def, Some(&tool.name)),
                "orphaned": false
            }));
        }
    }
    // 上游已经不再提供的工具：规则仍然存在，必须列出来才清得掉（不静默丢弃配置）。
    for def in defs.iter() {
        let cached = router
            .as_ref()
            .map(|r| r.cached_tools(&def.id))
            .unwrap_or_default();
        for (tool, state) in exposure.rules_of(group, &def.id) {
            if cached.iter().any(|t| t.name == tool) {
                continue;
            }
            tools.push(json!({
                "upstream_id": def.id,
                "tool": tool,
                "exposed": crate::upstream::exposed_tool_name(&def.id, tool),
                "state": state.as_str(),
                "visible": exposure.visible(group, &def.id, tool),
                "admin_only": crate::upstream::admin_only(def, Some(tool)),
                "orphaned": true
            }));
        }
    }
    Ok(json!({"group": group, "upstreams": upstreams, "tools": tools}))
}

/// `GET /admin/groups/{code}/exposure`：某组的暴露矩阵（授了哪些上游、每个工具的规则）。
pub(super) async fn group_exposure(
    State(state): State<AdminState>,
    Path(code): Path<String>,
) -> Result<Json<Value>, ApiError> {
    const SURFACE: &str = "admin/groups/exposure";
    let actor = admin_actor("");
    ensure_group(&state, &actor, &code, SURFACE)?;
    let view = exposure_view(&state, &code).map_err(|e| {
        reject(
            &state,
            &actor,
            e,
            json!({"surface": SURFACE, "group": code}),
        )
    })?;
    Ok(Json(view))
}

/// `PUT /admin/groups/{code}/exposure` 的请求体：两层都是**合并**语义。
///
/// `upstreams`：`true` 授予整台、`false` 撤销；`tools`：
/// `upstream_id → tool_name → "include" | "exclude" | null`（`null` = 删规则、回到继承）。
#[derive(Debug, Default, Deserialize)]
pub(super) struct GroupExposureBody {
    #[serde(default)]
    upstreams: BTreeMap<String, bool>,
    #[serde(default)]
    tools: BTreeMap<String, BTreeMap<String, Option<String>>>,
}

/// `PUT /admin/groups/{code}/exposure`：写第一层授予与第二层逐工具规则。
///
/// 整请求先校验再落库：任何一条非法（上游不存在、工具未在缓存清单、维护通道工具进非
/// admin 组、state 非法）都整体拒绝，不做部分写入——半写的矩阵比拒绝更难排查。
pub(super) async fn update_group_exposure(
    State(state): State<AdminState>,
    Extension(auth): Extension<AuthenticatedAdmin>,
    Path(code): Path<String>,
    headers: HeaderMap,
    Json(body): Json<GroupExposureBody>,
) -> Result<Json<Value>, ApiError> {
    const SURFACE: &str = "admin/groups/update_exposure";
    let actor = actor_of(&auth, &headers)
        .map_err(|e| reject(&state, &admin_actor(""), e, json!({"surface": SURFACE})))?;
    require_audit(&state, &actor, SURFACE)?;
    ensure_group(&state, &actor, &code, SURFACE)?;
    let router = require_router(&state, &actor, SURFACE)?;
    let def_of = |id: &str| -> Result<McpUpstream, ApiError> {
        router
            .store()
            .upstream(id)
            .map_err(|e| {
                reject(
                    &state,
                    &actor,
                    e,
                    json!({"surface": SURFACE, "group": code}),
                )
            })?
            .ok_or_else(|| {
                reject(
                    &state,
                    &actor,
                    McpError::new("MCP_UPSTREAM_NOT_FOUND", json!({"id": id})),
                    json!({"surface": SURFACE, "group": code, "upstream_id": id}),
                )
            })
    };
    let not_eligible = |def: &McpUpstream, tool: Option<&str>| -> ApiError {
        let reason = if def.category == crate::catalog::CATEGORY_ADMIN {
            "upstream_category_admin"
        } else {
            "admin_prefix"
        };
        reject(
            &state,
            &actor,
            McpError::new(
                "MCP_UPSTREAM_TOOL_NOT_ELIGIBLE",
                json!({
                    "group": code,
                    "upstream_id": def.id,
                    "tool": tool,
                    "reason": reason,
                    "hint": "admin 类别上游与 admin_ 前缀工具只能进 admin 组"
                }),
            ),
            json!({"surface": SURFACE, "group": code, "upstream_id": def.id}),
        )
    };
    let mut grants: Vec<(String, bool)> = Vec::new();
    for (id, granted) in &body.upstreams {
        let def = def_of(id)?;
        if *granted && !crate::upstream::group_allows(&def, None, &code) {
            return Err(not_eligible(&def, None));
        }
        grants.push((id.clone(), *granted));
    }
    let mut rules: Vec<(String, Vec<(String, Option<ToolRuleState>)>)> = Vec::new();
    for (id, per_tool) in &body.tools {
        let def = def_of(id)?;
        let cached = router.cached_tools(id);
        let mut changes: Vec<(String, Option<ToolRuleState>)> = Vec::new();
        for (tool, raw) in per_tool {
            let next = match raw.as_deref() {
                None => None,
                Some("include") => Some(ToolRuleState::Include),
                Some("exclude") => Some(ToolRuleState::Exclude),
                Some(other) => {
                    return Err(reject(
                        &state,
                        &actor,
                        McpError::validation(format!(
                            "工具规则只能取 include|exclude|null（收到 {other}）"
                        )),
                        json!({
                            "surface": SURFACE, "group": code,
                            "upstream_id": id, "tool": tool, "field": "state"
                        }),
                    ));
                }
            };
            if next.is_some() && !cached.iter().any(|t| &t.name == tool) {
                return Err(reject(
                    &state,
                    &actor,
                    McpError::new(
                        "MCP_UPSTREAM_TOOL_UNKNOWN",
                        json!({"upstream_id": id, "unknown": [tool]}),
                    ),
                    json!({"surface": SURFACE, "group": code, "upstream_id": id}),
                ));
            }
            if next == Some(ToolRuleState::Include)
                && !crate::upstream::group_allows(&def, Some(tool), &code)
            {
                return Err(not_eligible(&def, Some(tool)));
            }
            changes.push((tool.clone(), next));
        }
        rules.push((id.clone(), changes));
    }
    if grants.len().saturating_add(rules.len()) > GROUP_TOOL_RULE_MAX * 4 {
        return Err(reject(
            &state,
            &actor,
            McpError::validation(format!(
                "单次最多提交 {} 条暴露变更（收到 {}）",
                GROUP_TOOL_RULE_MAX * 4,
                grants.len() + rules.len()
            )),
            json!({"surface": SURFACE, "group": code, "field": "upstreams|tools"}),
        ));
    }
    let upstream_changes = state
        .store
        .apply_group_upstreams(&code, &grants, &actor)
        .map_err(|e| {
            reject(
                &state,
                &actor,
                e,
                json!({"surface": SURFACE, "group": code}),
            )
        })?;
    let mut rule_changes: Vec<Value> = Vec::new();
    for (id, changes) in &rules {
        rule_changes.extend(
            state
                .store
                .apply_group_tool_rules(&code, id, changes, &actor)
                .map_err(|e| {
                    reject(
                        &state,
                        &actor,
                        e,
                        json!({"surface": SURFACE, "group": code, "upstream_id": id}),
                    )
                })?,
        );
    }
    let view = exposure_view(&state, &code).map_err(|e| {
        reject(
            &state,
            &actor,
            e,
            json!({"surface": SURFACE, "group": code}),
        )
    })?;
    Ok(Json(json!({
        "exposure": view,
        "changes": {"upstreams": upstream_changes.len(), "tools": rule_changes.len()}
    })))
}
