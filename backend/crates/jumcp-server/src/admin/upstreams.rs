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
        category,
        enabled,
        timeout_ms,
        // 包坐标由安装/卸载路径维护，普通编辑（PUT）原样带过，绝不在这里清空。
        catalog_id: existing.and_then(|e| e.catalog_id.clone()),
        catalog_version: existing.and_then(|e| e.catalog_version.clone()),
        created_at: existing.map(|e| e.created_at.clone()).unwrap_or_default(),
        updated_at: String::new(),
    };
    // 上游本体不携带授权组：可见性完全由 `mcp_group_upstreams` / `mcp_group_tool_rules`
    // 决定，接入一台上游不产生任何暴露。
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
    // 加密字段脱节，因此只允许改名称/启停/超时。
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

/// 上游工具清单（上游视角）：缓存里的工具、每个工具的维护通道标记、授予这台上游的组。
///
/// 组与工具的暴露关系在组视角（`GET /admin/groups/{code}/exposure`）维护；这里只回答
/// "这台上游现在有哪些工具、整台被哪些组授予"，因此不触网、不写库。
pub(super) fn upstream_tools_view(
    def: &McpUpstream,
    cached: &[jumcp_core::registry::UpstreamTool],
    exposure: &jumcp_core::registry::GroupExposure,
) -> Value {
    let tools: Vec<Value> = cached
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "exposed_name": crate::upstream::exposed_tool_name(&def.id, &tool.name),
                "description": tool.description,
                "admin_only": crate::upstream::admin_only(def, Some(&tool.name)),
            })
        })
        .collect();
    json!({
        "groups": exposure.groups_granting(&def.id),
        "tools": tools,
        "total": tools.len()
    })
}

pub(super) fn upstream_tools_payload(
    def: &McpUpstream,
    runtime: &crate::upstream::UpstreamRuntime,
    cached: &[jumcp_core::registry::UpstreamTool],
    exposure: &jumcp_core::registry::GroupExposure,
) -> Value {
    let mut payload = upstream_tools_view(def, cached, exposure);
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
    let exposure = router
        .store()
        .group_exposure()
        .map_err(|e| reject(&state, &actor, e, json!({"surface": SURFACE, "id": id})))?;
    Ok(Json(upstream_tools_payload(
        &def, &runtime, &cached, &exposure,
    )))
}
