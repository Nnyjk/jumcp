//! 可安装 MCP 包：目录（Discover）→ 安装成一条已配置上游 → 卸载。
//!
//! 与 `upstreams` 的关系：安装只负责"按包定义填参数"，落库后就是普通上游——同一张表、
//! 同一套校验/审计/删除；包坐标（`catalog_id`/`catalog_version`）只用于归属与卸载判定，
//! 不改变运行时语义。因此这里不复制上游的校验/审计逻辑，只做包特有的三件事：
//! 字段必填检查、env 组装、二进制可用性检查。

use super::*;
// 上游的写路径共享件：包安装必须与 /admin/upstreams 用同一套组校验、单项输出与存储句柄。
use super::upstreams::{ensure_upstream_group, require_router, upstream_item};
use jumcp_core::registry::{InstallablePackage, catalog, package as find_package};

/// `POST /admin/mcp/catalog/{id}/install` 的请求体。
///
/// `config` 是字段 key → 值的扁平对象（不做嵌套）：包定义里的字段是平铺的，嵌套只会
/// 让"缺哪个字段"的报错变成路径问题。
#[derive(Debug, Deserialize)]
pub(super) struct InstallInput {
    upstream_id: Option<String>,
    name: Option<String>,
    group: Option<String>,
    enabled: Option<bool>,
    #[serde(default)]
    config: BTreeMap<String, String>,
}

/// 包定义里对外暴露的字段：**不含 `env`**——写进哪个环境变量是服务端实现细节，
/// 前端只需要渲染控件与提示。
fn fields_payload(pkg: &InstallablePackage) -> Vec<Value> {
    pkg.fields
        .iter()
        .map(|field| {
            json!({
                "key": field.key,
                "label": field.label,
                "kind": field.kind.as_str(),
                "required": field.required,
                "secret": field.secret,
                "default": field.default,
                "placeholder": field.placeholder,
                "help": field.help,
            })
        })
        .collect()
}

fn tools_payload(pkg: &InstallablePackage) -> Vec<Value> {
    pkg.tools
        .iter()
        .map(|tool| json!({"name": tool.name, "summary": tool.summary}))
        .collect()
}

/// 一个目录条目：包定义 + 本机可用性 + 该包的已安装上游（运行态取缓存，不触发探测）。
fn catalog_item(
    state: &AdminState,
    pkg: &InstallablePackage,
    views: &[(McpUpstream, crate::upstream::UpstreamRuntime)],
) -> Value {
    let installations: Vec<Value> = views
        .iter()
        .filter(|(def, _)| def.catalog_id.as_deref() == Some(pkg.id))
        .map(|(def, runtime)| {
            json!({
                "upstream_id": def.id,
                "name": def.name,
                "enabled": def.enabled,
                "group": def.group,
                "status": runtime.status,
                "status_message": runtime.status_message,
                "tool_count": runtime.tool_count,
                "catalog_version": def.catalog_version,
                "updated_at": def.updated_at,
            })
        })
        .collect();
    json!({
        "id": pkg.id,
        "name": pkg.name,
        "summary": pkg.summary,
        "version": pkg.version,
        "tags": pkg.tags,
        "transport": pkg.transport,
        "requires_stdio": pkg.transport == "stdio",
        "stdio_enabled": state.service.settings.upstream_stdio,
        // 与安装校验同一个函数：目录里显示"可安装"而安装报缺失会让人无从下手。
        "binary_present": crate::upstream::bundled_binary(pkg.command).is_some(),
        "default_upstream_id": pkg.default_upstream_id,
        "fields": fields_payload(pkg),
        "tools": tools_payload(pkg),
        "installations": installations,
    })
}

/// `GET /admin/mcp/catalog`：全部可安装包 + 各自的已安装上游。
pub(super) async fn list_catalog(State(state): State<AdminState>) -> Result<Json<Value>, ApiError> {
    const SURFACE: &str = "admin/mcp/catalog";
    let actor = admin_actor("");
    let router = require_router(&state, &actor, SURFACE)?;
    let views = router
        .views()
        .map_err(|e| reject(&state, &actor, e, json!({"surface": SURFACE})))?;
    let items: Vec<Value> = catalog()
        .iter()
        .map(|pkg| catalog_item(&state, pkg, &views))
        .collect();
    Ok(Json(json!({"items": items})))
}

/// `POST /admin/mcp/catalog/{id}/install`：按包定义 + `config` 生成一条 stdio 上游。
///
/// 失败语义（顺序即判定顺序）：包不存在 → 404；stdio 开关关闭 → 400；二进制缺失 → 400；
/// 组/必填字段缺失或出现未声明的配置键 → 400（message 指名字段 label / 未知键，且不建行）；
/// id 已存在 → 409（由存储层给出）。
pub(super) async fn install_package(
    State(state): State<AdminState>,
    Extension(auth): Extension<AuthenticatedAdmin>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<InstallInput>,
) -> Result<Json<Value>, ApiError> {
    const SURFACE: &str = "admin/mcp/catalog/install";
    let actor = actor_of(&auth, &headers)
        .map_err(|e| reject(&state, &admin_actor(""), e, json!({"surface": SURFACE})))?;
    require_audit(&state, &actor, SURFACE)?;
    let router = require_router(&state, &actor, SURFACE)?;
    let pkg = find_package(&id).ok_or_else(|| {
        reject(
            &state,
            &actor,
            McpError::new("MCP_CATALOG_NOT_FOUND", json!({"id": id})),
            json!({"surface": SURFACE, "id": id}),
        )
    })?;

    // 主机能力先于请求校验：开关关着的机器不该因为请求缺字段而报"缺字段"，否则管理员会去
    // 补配置，真正挡住安装的却是部署事实。顺序固定为 开关 → 二进制 → 组/字段。
    if pkg.transport == "stdio" && !state.service.settings.upstream_stdio {
        return Err(reject(
            &state,
            &actor,
            McpError::new(
                "MCP_UPSTREAM_STDIO_DISABLED",
                json!({
                    "package": id,
                    "hint": "设置 JUMCP_MCP_UPSTREAM_STDIO=1 后重启",
                }),
            ),
            json!({"surface": SURFACE, "id": id}),
        ));
    }
    // 与目录的 binary_present 同一个解析函数：这里失败只会是"文件真的不在"。
    if crate::upstream::bundled_binary(pkg.command).is_none() {
        return Err(reject(
            &state,
            &actor,
            McpError::new(
                "MCP_UPSTREAM_BINARY_MISSING",
                json!({
                    "package": id,
                    "command": pkg.command,
                    "hint": "把同版本的连接器二进制放到本服务同目录，或加入 PATH",
                }),
            ),
            json!({"surface": SURFACE, "id": id}),
        ));
    }

    let upstream_id = body
        .upstream_id
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| pkg.default_upstream_id.to_string());
    let name = body
        .name
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| pkg.name.to_string());
    let enabled = body.enabled.unwrap_or(true);
    let group = body.group.unwrap_or_default().trim().to_string();
    if group.is_empty() {
        return Err(reject(
            &state,
            &actor,
            McpError::with_message(
                "MCP_VALIDATION_ERROR",
                "缺少必填字段：group（授权组）".into(),
                json!({"field": "group", "label": "group（授权组）"}),
            ),
            json!({"surface": SURFACE, "id": id}),
        ));
    }

    // 字段按包定义顺序写 env：缺省用 default；必填字段最终为空即拒绝（空串等同未填）。
    let mut env = BTreeMap::new();
    for field in pkg.fields {
        let value = body
            .config
            .get(field.key)
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
            .or(field.default)
            .unwrap_or_default();
        if value.is_empty() {
            if field.required {
                return Err(reject(
                    &state,
                    &actor,
                    McpError::with_message(
                        "MCP_VALIDATION_ERROR",
                        format!("缺少必填字段：{}", field.label),
                        json!({"field": field.key, "label": field.label}),
                    ),
                    json!({"surface": SURFACE, "id": id, "upstream_id": upstream_id}),
                ));
            }
            continue;
        }
        env.insert(field.env.to_string(), value.to_string());
    }

    // 未知键一律拒绝：静默丢弃会让 `Allowed_Schemas` 这类笔误看起来"配置成功"，
    // 实际写进 env 的仍是空范围。
    let mut unknown: Vec<&str> = body
        .config
        .keys()
        .map(String::as_str)
        .filter(|key| !pkg.fields.iter().any(|field| field.key == *key))
        .collect();
    if !unknown.is_empty() {
        unknown.sort_unstable();
        return Err(reject(
            &state,
            &actor,
            McpError::with_message(
                "MCP_VALIDATION_ERROR",
                format!("未知配置字段：{}", unknown.join(",")),
                json!({"field": unknown[0], "unknown": unknown}),
            ),
            json!({"surface": SURFACE, "id": id, "upstream_id": upstream_id}),
        ));
    }

    let upstream = McpUpstream {
        id: upstream_id,
        name,
        transport: pkg.transport.to_string(),
        url: String::new(),
        command: pkg.command.to_string(),
        args: pkg.args.iter().map(|arg| (*arg).to_string()).collect(),
        env,
        headers: BTreeMap::new(),
        group: group.clone(),
        category: crate::catalog::CATEGORY_CUSTOM.to_string(),
        read_only: true,
        enabled,
        timeout_ms: 30_000,
        catalog_id: Some(pkg.id.to_string()),
        catalog_version: Some(pkg.version.to_string()),
        created_at: String::new(),
        updated_at: String::new(),
    };
    ensure_upstream_group(&state, &actor, SURFACE, &group, &upstream.id)?;
    upstream.validate().map_err(|e| {
        reject(
            &state,
            &actor,
            e,
            json!({"surface": SURFACE, "id": id, "upstream_id": upstream.id}),
        )
    })?;

    let created = router
        .store()
        .insert_upstream(&upstream, &actor)
        .map_err(|e| {
            reject(
                &state,
                &actor,
                e,
                json!({"surface": SURFACE, "id": id, "upstream_id": upstream.id}),
            )
        })?;
    router.invalidate(&created.id);
    let runtime = router.runtime(&created);
    Ok(Json(json!({"upstream": upstream_item(&created, &runtime)})))
}

/// `DELETE /admin/mcp/catalog/{id}/install/{upstream_id}`：只删本包装出来的上游。
///
/// 归属判定在删除之前：同名 id 的手工上游不归任何包，删它必须走 `/admin/upstreams/{id}`
/// （否则"卸载包"变成了删除别人配置的通用入口）。
pub(super) async fn uninstall_package(
    State(state): State<AdminState>,
    Extension(auth): Extension<AuthenticatedAdmin>,
    Path((id, upstream_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    const SURFACE: &str = "admin/mcp/catalog/uninstall";
    let actor = actor_of(&auth, &headers)
        .map_err(|e| reject(&state, &admin_actor(""), e, json!({"surface": SURFACE})))?;
    require_audit(&state, &actor, SURFACE)?;
    let router = require_router(&state, &actor, SURFACE)?;
    let pkg = find_package(&id).ok_or_else(|| {
        reject(
            &state,
            &actor,
            McpError::new("MCP_CATALOG_NOT_FOUND", json!({"id": id})),
            json!({"surface": SURFACE, "id": id}),
        )
    })?;
    let def = router
        .store()
        .upstream(&upstream_id)
        .map_err(|e| reject(&state, &actor, e, json!({"surface": SURFACE})))?
        .filter(|def| def.catalog_id.as_deref() == Some(pkg.id))
        .ok_or_else(|| {
            reject(
                &state,
                &actor,
                McpError::new(
                    "MCP_UPSTREAM_NOT_FOUND",
                    json!({"id": upstream_id, "package": id, "hint": "该上游不是这个包安装的"}),
                ),
                json!({"surface": SURFACE, "id": id, "upstream_id": upstream_id}),
            )
        })?;
    router
        .store()
        .delete_upstream(&def.id, &actor)
        .map_err(|e| {
            reject(
                &state,
                &actor,
                e,
                json!({"surface": SURFACE, "id": id, "upstream_id": def.id}),
            )
        })?;
    router.remove(&def.id);
    Ok(Json(json!({"deleted": true})))
}
