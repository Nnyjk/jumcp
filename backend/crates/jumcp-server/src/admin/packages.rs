//! 内置 MCP 包：目录（Discover）→ 安装成一条 HTTP 上游 → 改配置 → 卸载。
//!
//! 包定义来自 `jumcp_server::builtin::plugin(id).package()`；安装产物是一条普通上游，
//! URL 指向本进程的内嵌端点（`/internal/mcp/{package_id}/{upstream_id}`），不再拉起
//! stdio 子进程。与 `upstreams` 的关系仍是"落库后就是普通上游"：同一张表、同一套校验/
//! 审计/删除；包坐标只用于归属、卸载判定与"包管理字段不可手改"的门禁。
//!
//! 字段值落库进 `env`（字段声明里的 `env` 键名），secret 字段加密后存
//! `enc:v1:...`；目录只回显非密字段与 `secrets_set`。

use super::upstreams::{ensure_upstream_group, require_router, upstream_item};
use super::*;
use crate::builtin::{BuiltinEndpoint, plugin};
use jumcp_core::builtin::{BuiltinField, BuiltinMcp, BuiltinPackage};
use jumcp_core::secret::{SecretKey, is_encrypted};

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

/// `PATCH /admin/mcp/catalog/{id}/install/{upstream_id}/config` 的请求体。
#[derive(Debug, Deserialize)]
pub(super) struct ConfigInput {
    #[serde(default)]
    config: BTreeMap<String, String>,
}

/// 包定义里对外暴露的字段：**不含 `env`**——写进哪个环境变量是服务端实现细节，
/// 前端只需要渲染控件与提示。
fn fields_payload(pkg: &BuiltinPackage) -> Vec<Value> {
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

fn tools_payload(pkg: &BuiltinPackage) -> Vec<Value> {
    pkg.tools
        .iter()
        .map(|tool| json!({"name": tool.name, "summary": tool.summary}))
        .collect()
}

/// 非密字段的当前值（secret 绝不回显，明文与密文都不给）。
fn config_payload(pkg: &BuiltinPackage, def: &McpUpstream) -> Value {
    let mut config = serde_json::Map::new();
    for field in pkg.fields.iter().filter(|field| !field.secret) {
        if let Some(value) = def.env.get(field.env).filter(|value| !value.is_empty()) {
            config.insert(field.key.to_string(), json!(value));
        }
    }
    Value::Object(config)
}

/// 密字段是否已配置；值本身永不出现在响应里。
fn secrets_set_payload(pkg: &BuiltinPackage, def: &McpUpstream) -> Value {
    let mut flags = serde_json::Map::new();
    for field in pkg.fields.iter().filter(|field| field.secret) {
        flags.insert(field.key.to_string(), json!(is_configured(field, def)));
    }
    Value::Object(flags)
}

/// 一条安装（目录 install 列表与 PATCH 响应共用）。
fn installation_payload(pkg: &BuiltinPackage, def: &McpUpstream) -> Value {
    json!({
        "upstream_id": def.id,
        "name": def.name,
        "group": def.group,
        "enabled": def.enabled,
        "transport": def.transport,
        "url": def.url,
        "config": config_payload(pkg, def),
        "secrets_set": secrets_set_payload(pkg, def),
        "catalog_version": def.catalog_version,
        "created_at": def.created_at,
        "updated_at": def.updated_at,
    })
}

/// 一个目录条目：包定义 + 该包的已安装上游。
fn catalog_item(
    pkg: &BuiltinPackage,
    views: &[(McpUpstream, crate::upstream::UpstreamRuntime)],
) -> Value {
    let installations: Vec<Value> = views
        .iter()
        .filter(|(def, _)| def.catalog_id.as_deref() == Some(pkg.id))
        .map(|(def, _)| installation_payload(pkg, def))
        .collect();
    json!({
        "id": pkg.id,
        "name": pkg.name,
        "summary": pkg.summary,
        "version": pkg.version,
        "tags": pkg.tags,
        "transport": "http",
        "default_upstream_id": pkg.default_upstream_id,
        "fields": fields_payload(pkg),
        "tools": tools_payload(pkg),
        "installations": installations,
    })
}

/// `GET /admin/mcp/catalog`：全部内置包 + 各自的已安装上游。
pub(super) async fn list_catalog(State(state): State<AdminState>) -> Result<Json<Value>, ApiError> {
    const SURFACE: &str = "admin/mcp/catalog";
    let actor = admin_actor("");
    let router = require_router(&state, &actor, SURFACE)?;
    let views = router
        .views()
        .map_err(|e| reject(&state, &actor, e, json!({"surface": SURFACE})))?;
    let items: Vec<Value> = crate::builtin::plugins()
        .iter()
        .map(|plugin| catalog_item(plugin.package(), &views))
        .collect();
    Ok(Json(json!({"items": items})))
}

/// 内嵌端点：没有它就没法给出安装 URL（stdio 模式）。
fn require_builtin_endpoint(
    state: &AdminState,
    actor: &Actor,
    surface: &str,
    package: &str,
) -> Result<BuiltinEndpoint, ApiError> {
    state
        .service
        .upstreams()
        .and_then(|router| router.builtin_endpoint().cloned())
        .ok_or_else(|| {
            reject(
                state,
                actor,
                McpError::with_message(
                    "MCP_BUILTIN_UNAVAILABLE",
                    "本进程没有内置 MCP 端点：内置包只在 HTTP 模式可用".into(),
                    json!({
                        "package": package,
                        "hint": "以 JUMCP_TRANSPORT=streamable-http 启动并注入 JUMCP_SECRET_KEY",
                    }),
                ),
                json!({"surface": surface, "package": package}),
            )
        })
}

/// 密钥用途：来源与失败码不同，其余（空值检查、503、hint）同构。
#[derive(Clone, Copy)]
enum KeyKind {
    /// 落库加密（`JUMCP_SECRET_KEY`）。
    Storage,
    /// 传输解密（`JUMCP_TRANSPORT_KEY`）。
    Transport,
}

/// 取一把密钥；缺失即 503（HTTP 模式启动期两把都 fail-closed，这里是防御性回退）。
fn require_key(
    state: &AdminState,
    actor: &Actor,
    surface: &str,
    package: &str,
    kind: KeyKind,
) -> Result<Arc<SecretKey>, ApiError> {
    let (key, code, message) = match kind {
        KeyKind::Storage => (
            state.service.settings.secret_key.clone(),
            "MCP_BUILTIN_UNAVAILABLE",
            "JUMCP_SECRET_KEY 未配置：无法加密/解密包字段",
        ),
        KeyKind::Transport => (
            state.transport_key.clone(),
            "MCP_TRANSPORT_KEY_MISSING",
            "JUMCP_TRANSPORT_KEY 未设置：HTTP 模式必须注入 64 位十六进制密钥（生成方式：openssl rand -hex 32）",
        ),
    };
    key.map(Arc::new).ok_or_else(|| {
        reject(
            state,
            actor,
            McpError::with_message(
                code,
                message.into(),
                json!({"package": package, "hint": "openssl rand -hex 32"}),
            ),
            json!({"surface": surface, "package": package}),
        )
    })
}

/// 审计脱敏集合：包声明的 secret 字段 env，不靠键名里的 password/token 字样猜。
fn secret_envs(pkg: &BuiltinPackage) -> Vec<&'static str> {
    pkg.fields
        .iter()
        .filter(|field| field.secret)
        .map(|field| field.env)
        .collect()
}

/// 单个 secret 密文的长度上限（字符数）：正常密码的密文远小于它，超长按非法请求拒绝。
const TRANSPORT_CIPHERTEXT_MAX: usize = 4096;

/// 传输密文 → 明文：`secret=true` 的非空值必须是 `enc:v1:` 密文。
///
/// 拒绝路径全部显式，不存在"解不开就当明文用"或降级空串：
/// - 非密文 → `MCP_TRANSPORT_CIPHERTEXT_REQUIRED`；
/// - 换过密钥/被篡改 → `MCP_TRANSPORT_DECRYPT_FAILED`；
/// - 解出来是空串 → `MCP_VALIDATION_ERROR`（空值语义由调用方决定，密文不得绕过）。
///
/// 就地替换成明文，后续 `values_to_env` 仍用**落库密钥**重新加密。
fn decode_transport_secrets(
    pkg: &BuiltinPackage,
    values: &mut BTreeMap<&'static str, String>,
    key: &SecretKey,
) -> Result<(), McpError> {
    for field in pkg.fields.iter().filter(|field| field.secret) {
        let Some(value) = values.get(field.key).cloned() else {
            continue;
        };
        if !is_encrypted(&value) {
            return Err(McpError::with_message(
                "MCP_TRANSPORT_CIPHERTEXT_REQUIRED",
                format!("secret 字段必须密文传输：{}", field.key),
                json!({"field": field.key}),
            ));
        }
        if value.len() > TRANSPORT_CIPHERTEXT_MAX {
            return Err(McpError::with_message(
                "MCP_VALIDATION_ERROR",
                format!("secret 字段密文过长：{}", field.key),
                json!({"field": field.key, "max_len": TRANSPORT_CIPHERTEXT_MAX}),
            ));
        }
        let plaintext = key.decrypt(&value).map_err(|_| {
            McpError::with_message(
                "MCP_TRANSPORT_DECRYPT_FAILED",
                format!("secret 字段密文无法解密：{}", field.key),
                json!({"field": field.key}),
            )
        })?;
        if plaintext.is_empty() {
            return Err(McpError::with_message(
                "MCP_VALIDATION_ERROR",
                format!("secret 字段不能为空：{}", field.key),
                json!({"field": field.key}),
            ));
        }
        values.insert(field.key, plaintext);
    }
    Ok(())
}

/// 校验并归一化配置：未知键 / 必填缺失一律 400。
///
/// 返回值按字段 key 索引，只含非空值（空串等同未填）。
fn resolve_values(
    pkg: &BuiltinPackage,
    input: &BTreeMap<String, String>,
) -> Result<BTreeMap<&'static str, String>, McpError> {
    let mut unknown: Vec<&str> = input
        .keys()
        .map(String::as_str)
        .filter(|key| !pkg.fields.iter().any(|field| field.key == *key))
        .collect();
    if !unknown.is_empty() {
        unknown.sort_unstable();
        return Err(McpError::with_message(
            "MCP_VALIDATION_ERROR",
            format!("未知配置字段：{}", unknown.join(",")),
            json!({"field": unknown[0], "unknown": unknown}),
        ));
    }
    let mut values = BTreeMap::new();
    for field in pkg.fields {
        let value = input
            .get(field.key)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .or(field.default)
            .unwrap_or_default();
        if value.is_empty() {
            if field.required {
                return Err(McpError::with_message(
                    "MCP_VALIDATION_ERROR",
                    format!("缺少必填字段：{}", field.label),
                    json!({"field": field.key, "label": field.label}),
                ));
            }
            continue;
        }
        values.insert(field.key, value.to_string());
    }
    Ok(values)
}

/// 字段值 → 落库 env：secret 加密，其余明文。
fn values_to_env(
    pkg: &BuiltinPackage,
    values: &BTreeMap<&'static str, String>,
    key: &SecretKey,
) -> Result<BTreeMap<String, String>, McpError> {
    let mut env = BTreeMap::new();
    for field in pkg.fields {
        let Some(value) = values.get(field.key).filter(|value| !value.is_empty()) else {
            continue;
        };
        let stored = if field.secret {
            key.encrypt(value)
                .map_err(|e| McpError::config(format!("{} 加密失败：{e}", field.label)))?
        } else {
            value.clone()
        };
        env.insert(field.env.to_string(), stored);
    }
    Ok(env)
}

/// 已有行里某个非密字段的当前值；空值等同"未配置"。
///
/// secret 不在这里解密：PATCH 未提交的 secret 只需知道"库里有没有值"，旧密文解不开
/// （`JUMCP_SECRET_KEY` 已轮换）不能让改 url 这类无关字段整体失败。
fn stored_value(field: &BuiltinField, def: &McpUpstream) -> Option<String> {
    def.env
        .get(field.env)
        .filter(|value| !value.is_empty())
        .cloned()
}

/// 库里是否已有该字段的值；secret 只看存在性，不看内容。
fn is_configured(field: &BuiltinField, def: &McpUpstream) -> bool {
    def.env
        .get(field.env)
        .is_some_and(|value| !value.is_empty())
}

/// `POST /admin/mcp/catalog/{id}/install`：按包定义 + `config` 生成一条 HTTP 上游。
///
/// 失败语义（顺序即判定顺序）：包不存在 → 404；无内嵌端点/无密钥 → 503；
/// 组/必填/未知键的配置 → 400（message 指名字段 label / 未知键，且不建行）；
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
    let pkg = plugin(&id).map(BuiltinMcp::package).ok_or_else(|| {
        reject(
            &state,
            &actor,
            McpError::new("MCP_CATALOG_NOT_FOUND", json!({"id": id})),
            json!({"surface": SURFACE, "id": id}),
        )
    })?;
    let endpoint = require_builtin_endpoint(&state, &actor, SURFACE, pkg.id)?;
    let key = require_key(&state, &actor, SURFACE, pkg.id, KeyKind::Storage)?;
    let transport_key = require_key(&state, &actor, SURFACE, pkg.id, KeyKind::Transport)?;

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

    let mut values = resolve_values(pkg, &body.config).map_err(|e| {
        reject(
            &state,
            &actor,
            e,
            json!({"surface": SURFACE, "id": id, "upstream_id": upstream_id}),
        )
    })?;
    decode_transport_secrets(pkg, &mut values, &transport_key).map_err(|e| {
        reject(
            &state,
            &actor,
            e,
            json!({"surface": SURFACE, "id": id, "upstream_id": upstream_id}),
        )
    })?;
    let env = values_to_env(pkg, &values, &key).map_err(|e| {
        reject(
            &state,
            &actor,
            e,
            json!({"surface": SURFACE, "id": id, "upstream_id": upstream_id}),
        )
    })?;

    let upstream = McpUpstream {
        id: upstream_id.clone(),
        name,
        transport: "http".to_string(),
        url: endpoint.url_for(pkg.id, &upstream_id),
        command: String::new(),
        args: Vec::new(),
        env,
        headers: BTreeMap::new(),
        group: group.clone(),
        category: crate::catalog::CATEGORY_CUSTOM.to_string(),
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

    let envs = secret_envs(pkg);
    let created = router
        .store()
        .insert_upstream(&upstream, &envs, &actor)
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

/// `PATCH /admin/mcp/catalog/{id}/install/{upstream_id}/config`：部分更新包字段。
///
/// 只改出现的 key；secret 传空串 = 保持原值（否则浏览器回填的空输入会清掉密码）；
/// 合并后重新校验必填，secret 以"已有非空或本次提供"为准。
pub(super) async fn update_package_config(
    State(state): State<AdminState>,
    Extension(auth): Extension<AuthenticatedAdmin>,
    Path((id, upstream_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(body): Json<ConfigInput>,
) -> Result<Json<Value>, ApiError> {
    const SURFACE: &str = "admin/mcp/catalog/config";
    let actor = actor_of(&auth, &headers)
        .map_err(|e| reject(&state, &admin_actor(""), e, json!({"surface": SURFACE})))?;
    require_audit(&state, &actor, SURFACE)?;
    let router = require_router(&state, &actor, SURFACE)?;
    let pkg = plugin(&id).map(BuiltinMcp::package).ok_or_else(|| {
        reject(
            &state,
            &actor,
            McpError::new("MCP_CATALOG_NOT_FOUND", json!({"id": id})),
            json!({"surface": SURFACE, "id": id}),
        )
    })?;
    let key = require_key(&state, &actor, SURFACE, pkg.id, KeyKind::Storage)?;
    let transport_key = require_key(&state, &actor, SURFACE, pkg.id, KeyKind::Transport)?;
    let existing = router
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

    // 未知键先拒：静默丢弃会让 `Url` 这类大小写笔误看起来"改成功了"。
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

    let data = json!({"surface": SURFACE, "id": id, "upstream_id": upstream_id});
    // 先取出"显式提交"的值：secret 的传输密文只在这一层解码。
    let mut provided: BTreeMap<&'static str, String> = BTreeMap::new();
    for field in pkg.fields {
        if let Some(value) = body.config.get(field.key).map(|value| value.trim()) {
            // secret 传空串 = 保持原值：浏览器回填的空输入框不能顺手清掉密码。
            if !(field.secret && value.is_empty()) {
                provided.insert(field.key, value.to_string());
            }
        }
    }
    decode_transport_secrets(pkg, &mut provided, &transport_key)
        .map_err(|e| reject(&state, &actor, e, data.clone()))?;

    // 未显式提交的字段：非密字段取当前明文（校验与回写都要真值），secret 只核对
    // "库里有没有值"——解密不在这条路径上。
    let mut merged: BTreeMap<&'static str, String> = BTreeMap::new();
    for field in pkg.fields {
        if let Some(value) = provided.get(field.key) {
            merged.insert(field.key, value.clone());
            continue;
        }
        if !field.secret
            && let Some(current) = stored_value(field, &existing)
        {
            merged.insert(field.key, current);
        }
    }
    // 这里只做必填校验：PATCH 的语义是"在现有值上改"，default 不参与
    // （安装时已经落库）。
    for field in pkg.fields {
        let configured = match merged.get(field.key) {
            Some(value) => !value.trim().is_empty(),
            None => is_configured(field, &existing),
        };
        if !configured && field.required {
            return Err(reject(
                &state,
                &actor,
                McpError::with_message(
                    "MCP_VALIDATION_ERROR",
                    format!("缺少必填字段：{}", field.label),
                    json!({"field": field.key, "label": field.label}),
                ),
                data.clone(),
            ));
        }
    }
    // 保留包字段之外的 env（用户自加的 `env:NAME` 引用不该被配置编辑顺手删掉）；
    // secret 未显式改写时保留磁盘上的密文：重加密只会换来一段无意义的新密文。
    let mut env = existing.env.clone();
    for field in pkg.fields {
        let explicit = body
            .config
            .get(field.key)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty());
        match merged.get(field.key) {
            Some(value) if explicit.is_some() => {
                let stored = if field.secret {
                    key.encrypt(value)
                        .map_err(|e| McpError::config(format!("{} 加密失败：{e}", field.label)))
                        .map_err(|e| reject(&state, &actor, e, data.clone()))?
                } else {
                    value.clone()
                };
                env.insert(field.env.to_string(), stored);
            }
            Some(value) if !field.secret => {
                if value.is_empty() {
                    env.remove(field.env);
                } else {
                    env.insert(field.env.to_string(), value.clone());
                }
            }
            _ => {}
        }
    }

    let updated = McpUpstream {
        env,
        ..existing.clone()
    };
    updated
        .validate()
        .map_err(|e| reject(&state, &actor, e, data.clone()))?;
    let envs = secret_envs(pkg);
    let saved = router
        .store()
        .update_upstream(&updated, &envs, &actor)
        .map_err(|e| reject(&state, &actor, e, data.clone()))?;
    router.invalidate(&saved.id);
    Ok(Json(
        json!({"installation": installation_payload(pkg, &saved)}),
    ))
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
    let pkg = plugin(&id).map(BuiltinMcp::package).ok_or_else(|| {
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

#[cfg(test)]
mod tests {
    use super::*;
    use jumcp_core::builtin::FieldKind;

    static FIELDS: &[BuiltinField] = &[BuiltinField {
        key: "database",
        label: "数据库",
        kind: FieldKind::Text,
        required: false,
        secret: false,
        default: Some("default"),
        placeholder: None,
        help: None,
        env: "TEST_DATABASE",
    }];

    static PACKAGE: BuiltinPackage = BuiltinPackage {
        id: "test-fields",
        name: "测试包",
        summary: "只用于校验默认值归一化",
        version: "0.0.0",
        tags: &[],
        default_upstream_id: "test",
        fields: FIELDS,
        tools: &[],
    };

    #[test]
    fn default_applies_only_when_value_is_absent() {
        let values = resolve_values(&PACKAGE, &BTreeMap::new()).expect("default");
        assert_eq!(values.get("database").map(String::as_str), Some("default"));

        let empty = BTreeMap::from([("database".to_string(), "  ".to_string())]);
        let values = resolve_values(&PACKAGE, &empty).expect("trimmed to default");
        assert_eq!(values.get("database").map(String::as_str), Some("default"));
    }
}
