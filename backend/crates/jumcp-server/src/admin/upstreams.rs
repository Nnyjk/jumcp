use super::*;

/// `POST/PUT /admin/upstreams` 的请求体：与 `UpstreamItem` 同形，去掉运行态字段。
///
/// 字段可选是给 PUT 用的：省略 = 保留现有值（PUT 是整对象提交，但允许省略未改字段）。
#[derive(Debug, Deserialize)]
pub(super) struct UpstreamInput {
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
    enabled: Option<bool>,
    timeout_ms: Option<i64>,
}

/// 上游管理面的唯一来源：`Service::upstreams()`（未启用 router 时显式报错，不伪造空列表）。
pub(super) fn require_router<'a>(
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
pub(super) fn restore_masked(
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
pub(super) fn build_upstream(
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
        enabled,
        timeout_ms,
        // 包坐标由安装/卸载路径维护，普通编辑（PUT）原样带过，绝不在这里清空。
        catalog_id: existing.and_then(|e| e.catalog_id.clone()),
        catalog_version: existing.and_then(|e| e.catalog_version.clone()),
        created_at: existing.map(|e| e.created_at.clone()).unwrap_or_default(),
        updated_at: String::new(),
    };
    // `basic` 是代码内置的逐工具通道：整台上游注册到 basic 会让它的全部工具（含写工具）
    // 对所有人可见，因此只允许逐工具加入。
    ensure_upstream_group(state, actor, surface, &upstream.group, &upstream.id)?;
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

/// 授权组门禁（上游写入与包安装共用）：`basic` 不可整台注册，组必须存在于有效策略。
///
/// 上游工具的可见性完全由该组决定，悬空组等于永远不可见；因此两处写入口必须同一判定。
pub(super) fn ensure_upstream_group(
    state: &AdminState,
    actor: &Actor,
    surface: &str,
    group: &str,
    id: &str,
) -> Result<(), ApiError> {
    if group == crate::discovery::BASIC_GROUP {
        return Err(reject(
            state,
            actor,
            McpError::new(
                "MCP_UPSTREAM_INVALID",
                json!({
                    "field": "group",
                    "group": group,
                    "hint": "basic 是内置组，只能通过逐工具配置把个别工具加入",
                }),
            ),
            json!({"surface": surface, "id": id}),
        ));
    }
    let policy = state
        .service
        .effective_policy()
        .map_err(|e| reject(state, actor, e, json!({"surface": surface, "id": id})))?;
    if !policy.groups.iter().any(|g| g.code == group) {
        let mut known: Vec<&str> = policy.groups.iter().map(|g| g.code.as_str()).collect();
        known.sort();
        return Err(reject(
            state,
            actor,
            McpError::new(
                "MCP_UPSTREAM_INVALID",
                json!({"field": "group", "group": group, "known_groups": known}),
            ),
            json!({"surface": surface, "id": id}),
        ));
    }
    Ok(())
}

/// 管理面条目：脱敏配置 + 运行态（status 取缓存，不触发探测）。
pub(super) fn upstream_item(
    def: &McpUpstream,
    runtime: &crate::upstream::UpstreamRuntime,
) -> Value {
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
pub(super) async fn list_upstreams(
    State(state): State<AdminState>,
) -> Result<Json<Value>, ApiError> {
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
pub(super) async fn get_upstream(
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
pub(super) async fn create_upstream(
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
        .insert_upstream(&upstream, &[], &actor)
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
pub(super) async fn update_upstream(
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
    // 包安装行的传输/地址/命令/环境变量/请求头由包管理：手改会与安装时生成的 URL、
    // 加密字段脱节，因此只允许改名称/组/启停/只读/超时。
    if existing.catalog_id.is_some() {
        let mut changed: Vec<&str> = Vec::new();
        if upstream.transport != existing.transport {
            changed.push("transport");
        }
        if upstream.url != existing.url {
            changed.push("url");
        }
        if upstream.command != existing.command {
            changed.push("command");
        }
        if upstream.args != existing.args {
            changed.push("args");
        }
        if upstream.env != existing.env {
            changed.push("env");
        }
        if upstream.headers != existing.headers {
            changed.push("headers");
        }
        if !changed.is_empty() {
            return Err(reject(
                &state,
                &actor,
                McpError::with_message(
                    "MCP_UPSTREAM_PACKAGE_MANAGED",
                    "该上游由内置包安装：传输/地址/命令/参数/环境变量/请求头由包管理".into(),
                    json!({
                        "id": id,
                        "fields": changed,
                        "catalog_id": existing.catalog_id,
                        "hint": "改字段请用 PATCH /admin/mcp/catalog/{id}/install/{upstream_id}/config",
                    }),
                ),
                json!({"surface": SURFACE, "id": id}),
            ));
        }
    }
    let updated = router
        .store()
        .update_upstream(&upstream, &[], &actor)
        .map_err(|e| reject(&state, &actor, e, json!({"surface": SURFACE, "id": id})))?;
    if existing.definition_hash() != updated.definition_hash() {
        router.invalidate(&updated.id);
    }
    let runtime = router.runtime(&updated);
    Ok(Json(upstream_item(&updated, &runtime)))
}

/// `DELETE /admin/upstreams/{id}`。
pub(super) async fn delete_upstream(
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
pub(super) async fn refresh_upstream(
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

/// `GET /admin/upstreams/{id}/tools` 与 `PUT .../tools/basic` 的共享视图。
///
/// 一行一个工具：`group` 是生效组（有 basic 覆盖且仍可加入时为 `basic`，否则继承上游组）；
/// `group_source` 说明这个组是逐工具覆盖来的还是继承上游的；`eligible=false` 表示该工具
/// 不能加入 basic（上游类别是 admin、或工具名以 `admin_` 开头——这些是维护通道，
/// 不能借 basic 变成对所有 agent 可见）；`orphaned=true` 表示有覆盖行但当前缓存里
/// 已没有这个工具（上游改名/刷新失败），留给管理面清理，不静默删用户配置。
pub(super) fn upstream_tools_view(
    def: &McpUpstream,
    cached: &[jumcp_core::registry::UpstreamTool],
    overrides: &[UpstreamToolGroup],
) -> Value {
    let mut tools: Vec<Value> = Vec::with_capacity(cached.len() + overrides.len());
    for tool in cached {
        let overridden = overrides.iter().any(|row| {
            row.tool_name == tool.name && row.group_code == crate::discovery::BASIC_GROUP
        });
        // 与 `/admin/tokens/options`、`tools/list` 共用同一份生效组判定；`None` 表示覆盖行
        // 已因上游改成 admin 类别/名字非法而失效，此时按上游组展示，`eligible=false` 提示清理。
        let group = crate::upstream::effective_tool_group(def, tool, overridden);
        let basic = group == Some(crate::discovery::BASIC_GROUP);
        tools.push(json!({
            "name": tool.name,
            "exposed_name": crate::upstream::exposed_tool_name(&def.id, &tool.name),
            "group": group.unwrap_or(def.group.as_str()),
            "group_source": if basic { "basic" } else { "upstream" },
            "basic": basic,
            "overridden": overridden,
            "eligible": crate::upstream::basic_eligible(def, tool),
            "orphaned": false,
            "description": tool.description,
        }));
    }
    for row in overrides {
        if cached.iter().any(|tool| tool.name == row.tool_name) {
            continue;
        }
        tools.push(json!({
            "name": row.tool_name,
            "exposed_name": crate::upstream::exposed_tool_name(&def.id, &row.tool_name),
            "group": crate::discovery::BASIC_GROUP,
            "group_source": "basic",
            "basic": true,
            "overridden": true,
            "eligible": false,
            "orphaned": true,
            "description": "",
        }));
    }
    let basic_total = tools
        .iter()
        .filter(|tool| tool["basic"].as_bool() == Some(true))
        .count();
    let orphaned_total = tools
        .iter()
        .filter(|tool| tool["orphaned"].as_bool() == Some(true))
        .count();
    json!({
        "tools": tools,
        "total": tools.len(),
        "basic_total": basic_total,
        "orphaned_total": orphaned_total
    })
}

pub(super) fn upstream_tools_payload(
    def: &McpUpstream,
    runtime: &crate::upstream::UpstreamRuntime,
    cached: &[jumcp_core::registry::UpstreamTool],
    overrides: &[UpstreamToolGroup],
) -> Value {
    let mut payload = upstream_tools_view(def, cached, overrides);
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("status".into(), json!(runtime.status));
        obj.insert("status_message".into(), json!(runtime.status_message));
        obj.insert(
            "last_checked_at".into(),
            runtime
                .last_checked_at
                .as_ref()
                .map_or(Value::Null, |t| json!(t)),
        );
    }
    payload
}

pub(super) fn upstream_def(
    state: &AdminState,
    actor: &Actor,
    router: &Arc<crate::upstream::UpstreamManager>,
    surface: &str,
    id: &str,
) -> Result<McpUpstream, ApiError> {
    router
        .store()
        .upstream(id)
        .map_err(|e| reject(state, actor, e, json!({"surface": surface, "id": id})))?
        .ok_or_else(|| {
            reject(
                state,
                actor,
                McpError::new("MCP_UPSTREAM_NOT_FOUND", json!({"id": id})),
                json!({"surface": surface, "id": id}),
            )
        })
}

/// `GET /admin/upstreams/{id}/tools`：只读缓存，不触网。
///
/// 与 `GET /admin/upstreams` 同一语义（`status` 取缓存）；需要最新工具清单时由调用方
/// 显式 `POST .../refresh`，GET 不做探测。
pub(super) async fn upstream_tools(
    State(state): State<AdminState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    const SURFACE: &str = "admin/upstreams/tools";
    let actor = admin_actor("");
    let router = require_router(&state, &actor, SURFACE)?;
    let def = upstream_def(&state, &actor, router, SURFACE, &id)?;
    let runtime = router.runtime(&def);
    let cached = router.cached_tools(&id);
    let overrides = router
        .store()
        .upstream_tool_groups_of(&id)
        .map_err(|e| reject(&state, &actor, e, json!({"surface": SURFACE, "id": id})))?;
    Ok(Json(upstream_tools_payload(
        &def, &runtime, &cached, &overrides,
    )))
}

/// `PUT /admin/upstreams/{id}/tools/basic`：把个别工具加入/移出 `basic` 组。
///
/// body 是 `{"tools": {"<raw_tool_name>": true|false|null}}` 的**合并**语义：
/// `true` 写入 basic，`false`/`null` 删除覆盖（回到继承上游组）。幂等：无真实变化时不写
/// 审计、`changed` 为空。校验是整请求粒度——未知工具/写工具/admin 类别命中任一条都整体
/// 拒绝，不做部分写入。
#[derive(Debug, Default, Deserialize)]
pub(super) struct BasicToolsInput {
    #[serde(default)]
    tools: BTreeMap<String, Option<bool>>,
}

pub(super) async fn set_upstream_basic_tools(
    State(state): State<AdminState>,
    Extension(auth): Extension<AuthenticatedAdmin>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<BasicToolsInput>,
) -> Result<Json<Value>, ApiError> {
    const SURFACE: &str = "admin/upstreams/tools/basic";
    let actor = actor_of(&auth, &headers)
        .map_err(|e| reject(&state, &admin_actor(""), e, json!({"surface": SURFACE})))?;
    require_audit(&state, &actor, SURFACE)?;
    let router = require_router(&state, &actor, SURFACE)?;
    let def = upstream_def(&state, &actor, router, SURFACE, &id)?;
    if body.tools.len() > UPSTREAM_TOOL_GROUP_MAX {
        return Err(reject(
            &state,
            &actor,
            McpError::validation(format!(
                "单次最多提交 {UPSTREAM_TOOL_GROUP_MAX} 个工具的组覆盖（收到 {}）",
                body.tools.len()
            )),
            json!({
                "surface": SURFACE,
                "id": id,
                "field": "tools",
                "tool_count": body.tools.len(),
                "limit": UPSTREAM_TOOL_GROUP_MAX
            }),
        ));
    }
    let cached = router.cached_tools(&id);
    let add = body
        .tools
        .iter()
        .filter(|(_, basic)| **basic == Some(true))
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    // `false` 与 `null` 都是"删除覆盖、回到继承上游组"。
    let clear = body
        .tools
        .iter()
        .filter(|(_, basic)| **basic != Some(true))
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    let unknown: Vec<String> = add
        .iter()
        .filter(|name| !cached.iter().any(|tool| &tool.name == *name))
        .cloned()
        .collect();
    if !unknown.is_empty() {
        return Err(reject(
            &state,
            &actor,
            McpError::new("MCP_UPSTREAM_TOOL_UNKNOWN", json!({"unknown": unknown})),
            json!({"surface": SURFACE, "id": id}),
        ));
    }
    for name in &add {
        if def.category == crate::catalog::CATEGORY_ADMIN || name.starts_with("admin_") {
            return Err(reject(
                &state,
                &actor,
                McpError::new(
                    "MCP_UPSTREAM_TOOL_NOT_ELIGIBLE",
                    json!({
                        "tool": name,
                        "reason": if def.category == crate::catalog::CATEGORY_ADMIN {
                            "upstream_category_admin"
                        } else {
                            "admin_prefix"
                        }
                    }),
                ),
                json!({"surface": SURFACE, "id": id}),
            ));
        }
    }
    let set = add
        .iter()
        .map(|name| (name.clone(), crate::discovery::BASIC_GROUP.to_string()))
        .collect::<Vec<_>>();
    let changed = router
        .store()
        .apply_upstream_tool_groups(&id, &set, &clear, &actor)
        .map_err(|e| reject(&state, &actor, e, json!({"surface": SURFACE, "id": id})))?;
    let overrides = router
        .store()
        .upstream_tool_groups_of(&id)
        .map_err(|e| reject(&state, &actor, e, json!({"surface": SURFACE, "id": id})))?;
    let runtime = router.runtime(&def);
    let mut payload = upstream_tools_payload(&def, &runtime, &cached, &overrides);
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("changed".into(), json!(changed));
    }
    Ok(Json(payload))
}
