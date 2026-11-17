//! 上游 MCP server 注册表：配置、脱敏审计 diff 与逐工具组覆盖（router 的动态插入面）。

use super::record::{AdminChange, field_changes};
use super::{CapabilityStore, is_builtin_id, now_iso};
use crate::audit::IntoAuditOrigin;
use crate::canonical::definition_hash;
use crate::errors::McpError;
use rusqlite::Row;
use serde_json::{Value, json};

/// 上游配置里会进审计 diff 的标量字段（env/headers 另有脱敏 diff，见 `upstream_changes`）。
const UPSTREAM_CHANGE_FIELDS: &[&str] = &[
    "name",
    "transport",
    "url",
    "command",
    "args",
    "group",
    "category",
    "read_only",
    "enabled",
    "timeout_ms",
];

/// 上游传输闭集：`http` = MCP Streamable HTTP；`stdio` = 部署级开关控制的本地子进程。
pub const UPSTREAM_TRANSPORTS: &[&str] = &["http", "stdio"];
/// 上游分类闭集：与内置工具三组同名，另加 `custom` 给接入方自定的上游。
pub const UPSTREAM_CATEGORIES: &[&str] = &["basic", "analysis", "admin", "custom"];
/// 保留的上游 id：不得作为上游名（会被误读成授权组或内置来源）。
pub const RESERVED_UPSTREAM_IDS: &[&str] = &["admin", "analysis", "basic", "builtin", "jumcp"];
pub const UPSTREAM_ID_MAX_LEN: usize = 32;
pub const UPSTREAM_MAX_ITEMS: usize = 64;
/// 单台上游可被覆盖到 `basic` 的工具数上限（与 args/env/headers 的条目上限同量级）。
pub const UPSTREAM_TOOL_GROUP_MAX: usize = 64;
pub const UPSTREAM_TIMEOUT_MAX_MS: i64 = 120_000;
/// 对外工具名（`<server_id>__<tool>`）的字节上限；超限拒绝接入，不截断。
pub const EXPOSED_TOOL_NAME_MAX: usize = 64;
/// 暴露名分隔符：`tools/call` 的前缀回退与 token allowlist 都按它识别上游工具。
pub const UPSTREAM_NAME_SEP: &str = "__";

/// 一个上游 MCP server 的持久化配置。
///
/// `env`/`headers` 的值允许是字面量或 `env:NAME` 引用；`env:NAME` 只在建连时解析，
/// 缺失即建连失败。审计与 GET 输出一律走 `to_public_value`（字面密钥脱敏成 `***`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpUpstream {
    pub id: String,
    pub name: String,
    pub transport: String,
    pub url: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: std::collections::BTreeMap<String, String>,
    pub headers: std::collections::BTreeMap<String, String>,
    pub group: String,
    pub category: String,
    pub read_only: bool,
    pub enabled: bool,
    pub timeout_ms: i64,
    /// 安装来源：可安装包的 id（`None` = 手工创建）。安装语义只读这两列，运行时一律按普通上游。
    pub catalog_id: Option<String>,
    /// 安装来源的包版本：管理台据此提示"包已升级、可重装"。
    pub catalog_version: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// 上游 `tools/list` 的一条工具：调用方只拿得到投影后的名字，原始名字用于回派与审计。
#[derive(Debug, Clone, PartialEq)]
pub struct UpstreamTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub annotations: Option<Value>,
}

/// 逐工具组覆盖：**只存显式覆盖**，无行 = 继承 `mcp_upstreams.group_code`。
///
/// 目前只允许 `group_code = basic`：单工具进公共基础通道是"开放一个只读工具"，
/// 其他组仍按整台上游授权，避免出现"逐工具拆授权组"的第二套模型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamToolGroup {
    pub upstream_id: String,
    pub tool_name: String,
    pub group_code: String,
}

/// 上游 id 门禁：`^[a-z][a-z0-9_-]{1,31}$`，且不含连续 `__`。
///
/// 禁止 `__` 是为了让 `<server_id>__<tool>` 的前缀唯一可反解：分隔符只能出现一次在
/// server id 之后，`tools/call` 的旧名回退与审计归属才不会歧义。
pub fn is_valid_upstream_id(id: &str) -> bool {
    let mut bytes = id.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() || !(2..=UPSTREAM_ID_MAX_LEN).contains(&id.len()) {
        return false;
    }
    !id.contains(UPSTREAM_NAME_SEP)
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// 上游工具暴露名：`<server_id>__<tool>`、整名 ≤64、两侧只含 `[A-Za-z0-9_-]`。
///
/// token 的 allowlist 用它识别"这是上游工具名"（内置 id 不含 `__`，两者不会互相误判）。
pub fn is_upstream_tool_name(name: &str) -> bool {
    if name.len() > EXPOSED_TOOL_NAME_MAX {
        return false;
    }
    let Some((server, tool)) = name.split_once(UPSTREAM_NAME_SEP) else {
        return false;
    };
    !server.is_empty()
        && !tool.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// allowlist 的"全部工具"条目。
pub const TOOL_WILDCARD_ALL: &str = "*";

/// `<server_id>__*` → server id；其余形状返回 `None`。
///
/// 只接受"整名以 `__*` 结尾、前缀是合法上游 id"的形状。`a__b__*`、`__*`、`*__x` 都不算通配：
/// server id 本身禁止 `__`，所以前缀能唯一反解到某个上游，不会误伤别的前缀。
fn upstream_wildcard_server(entry: &str) -> Option<&str> {
    let server = entry.strip_suffix("__*")?;
    is_valid_upstream_id(server).then_some(server)
}

/// allowlist 条目是否合法：内置 id、上游暴露名，或两种通配（`*`、`<server_id>__*`）。
///
/// 校验与匹配共用 [`upstream_wildcard_server`] 的解析：各写一遍必然出现
/// "校验放过、`tools/list` 不认"的双套口径。
pub fn is_tool_allowlist_entry(entry: &str) -> bool {
    entry == TOOL_WILDCARD_ALL
        || upstream_wildcard_server(entry).is_some()
        || super::is_builtin_id(entry)
        || is_upstream_tool_name(entry)
}

/// allowlist 条目是否命中某个工具名（内置 id 或上游暴露名）。
///
/// `tools/list`、发现面投影与 `tools/call` 三处共用这一处实现，避免
/// "列表里没有、调用却放行"。非法条目只可能来自历史数据，按精确匹配处理。
pub fn tool_allowlist_matches(entry: &str, name: &str) -> bool {
    if entry == TOOL_WILDCARD_ALL || entry == name {
        return true;
    }
    upstream_wildcard_server(entry).is_some_and(|server| {
        name.strip_prefix(server)
            .is_some_and(|rest| rest.starts_with(UPSTREAM_NAME_SEP))
    })
}

impl McpUpstream {
    /// 配置门禁：结构、传输必填项、闭集、上限。组存在性由管理面用 effective policy 判。
    pub fn validate(&self) -> Result<(), McpError> {
        if !is_valid_upstream_id(&self.id) {
            return Err(McpError::new(
                "MCP_UPSTREAM_INVALID",
                json!({
                    "field": "id",
                    "value": self.id,
                    "pattern": "^[a-z][a-z0-9_-]{1,31}$（不含连续 __）"
                }),
            ));
        }
        if RESERVED_UPSTREAM_IDS.contains(&self.id.as_str()) || is_builtin_id(&self.id) {
            return Err(McpError::new(
                "MCP_UPSTREAM_CONFLICT",
                json!({"field": "id", "id": self.id, "reserved": RESERVED_UPSTREAM_IDS}),
            ));
        }
        crate::config::validate_display_name("name", &self.name).map_err(|m| {
            McpError::new("MCP_UPSTREAM_INVALID", json!({"field": "name", "error": m}))
        })?;
        if !UPSTREAM_TRANSPORTS.contains(&self.transport.as_str()) {
            return Err(McpError::new(
                "MCP_UPSTREAM_INVALID",
                json!({"field": "transport", "value": self.transport, "allowed": UPSTREAM_TRANSPORTS}),
            ));
        }
        match self.transport.as_str() {
            "http" => {
                if !(self.url.starts_with("http://") || self.url.starts_with("https://")) {
                    return Err(McpError::new(
                        "MCP_UPSTREAM_INVALID",
                        json!({"field": "url", "value": self.url, "hint": "只接受 http(s):// 绝对地址"}),
                    ));
                }
                if !self.command.is_empty() {
                    return Err(McpError::new(
                        "MCP_UPSTREAM_INVALID",
                        json!({"field": "command", "hint": "http 上游不接受 command"}),
                    ));
                }
            }
            _ => {
                if self.command.trim().is_empty() {
                    return Err(McpError::new(
                        "MCP_UPSTREAM_INVALID",
                        json!({"field": "command", "hint": "stdio 上游必须声明 command"}),
                    ));
                }
                if !self.url.is_empty() {
                    return Err(McpError::new(
                        "MCP_UPSTREAM_INVALID",
                        json!({"field": "url", "hint": "stdio 上游不接受 url"}),
                    ));
                }
            }
        }
        if self.args.len() > UPSTREAM_MAX_ITEMS {
            return Err(McpError::new(
                "MCP_UPSTREAM_INVALID",
                json!({"field": "args", "count": self.args.len(), "limit": UPSTREAM_MAX_ITEMS}),
            ));
        }
        for (field, map) in [("env", &self.env), ("headers", &self.headers)] {
            if map.len() > UPSTREAM_MAX_ITEMS {
                return Err(McpError::new(
                    "MCP_UPSTREAM_INVALID",
                    json!({"field": field, "count": map.len(), "limit": UPSTREAM_MAX_ITEMS}),
                ));
            }
            for (key, value) in map {
                if key.trim().is_empty() || value.trim().is_empty() {
                    return Err(McpError::new(
                        "MCP_UPSTREAM_INVALID",
                        json!({"field": field, "key": key}),
                    ));
                }
            }
        }
        if self.group.trim().is_empty() {
            return Err(McpError::new(
                "MCP_UPSTREAM_INVALID",
                json!({"field": "group"}),
            ));
        }
        if !UPSTREAM_CATEGORIES.contains(&self.category.as_str()) {
            return Err(McpError::new(
                "MCP_UPSTREAM_INVALID",
                json!({"field": "category", "value": self.category, "allowed": UPSTREAM_CATEGORIES}),
            ));
        }
        if !(1..=UPSTREAM_TIMEOUT_MAX_MS).contains(&self.timeout_ms) {
            return Err(McpError::new(
                "MCP_UPSTREAM_INVALID",
                json!({"field": "timeout_ms", "value": self.timeout_ms, "range": [1, UPSTREAM_TIMEOUT_MAX_MS]}),
            ));
        }
        Ok(())
    }

    /// 配置指纹：连接池按它失效，审计用它标记"这次调用对应哪一版上游配置"。
    ///
    /// 包含 env/headers 的值——只输出哈希，不输出原文。
    pub fn definition_hash(&self) -> String {
        definition_hash(&json!({
            "id": self.id,
            "transport": self.transport,
            "url": self.url,
            "command": self.command,
            "args": self.args,
            "env": self.env,
            "headers": self.headers,
            "group": self.group,
            "category": self.category,
            "read_only": self.read_only,
            "enabled": self.enabled,
            "timeout_ms": self.timeout_ms
        }))
    }

    /// 对外表示（管理面 GET / 审计 data）：字段与契约一致，密钥值只输出 `***` 或 `env:NAME`。
    pub fn to_public_value(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "transport": self.transport,
            "url": if self.url.is_empty() { Value::Null } else { json!(self.url) },
            "command": if self.command.is_empty() { Value::Null } else { json!(self.command) },
            "args": self.args,
            "env": masked_map(&self.env),
            "headers": masked_map(&self.headers),
            "group": self.group,
            "category": self.category,
            "read_only": self.read_only,
            "enabled": self.enabled,
            "timeout_ms": self.timeout_ms,
            "catalog_id": self.catalog_id.as_deref(),
            "catalog_version": self.catalog_version.as_deref(),
            "updated_at": self.updated_at
        })
    }

    /// 审计 `data`：只放非密钥的配置事实，绝不带 env/headers 的值。
    ///
    /// 包坐标只在存在时出现：手工上游的审计记录不应凭空多出两个 null 字段。
    pub fn audit_data(&self) -> Value {
        let mut data = json!({
            "transport": self.transport,
            "group": self.group,
            "category": self.category,
            "read_only": self.read_only,
            "enabled": self.enabled
        });
        if let Some(map) = data.as_object_mut() {
            for (key, value) in [
                ("catalog_id", &self.catalog_id),
                ("catalog_version", &self.catalog_version),
            ] {
                if let Some(value) = value.as_deref().filter(|v| !v.is_empty()) {
                    map.insert(key.to_string(), json!(value));
                }
            }
        }
        data
    }
}

/// 密钥键判定：命中即把字面值脱敏；`env:NAME` 是引用而非密钥，原样保留。
fn is_secret_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [
        "authorization",
        "cookie",
        "token",
        "secret",
        "password",
        "api-key",
        "apikey",
        // DSN 内嵌 `用户名:密码@主机`，整串按密钥处理。
        "dsn",
    ]
    .iter()
    .any(|needle| key.contains(needle))
}

fn mask_value(key: &str, value: &str) -> Value {
    if value.starts_with("env:") || !is_secret_key(key) {
        json!(value)
    } else {
        json!("***")
    }
}

fn masked_map(map: &std::collections::BTreeMap<String, String>) -> Value {
    let mut out = serde_json::Map::new();
    for (key, value) in map {
        out.insert(key.clone(), mask_value(key, value));
    }
    Value::Object(out)
}

/// env/headers 的审计 diff：只输出键与脱敏值，`changed_keys` 指出真正变了的键。
fn secret_map_changes(
    field: &str,
    before: &std::collections::BTreeMap<String, String>,
    after: &std::collections::BTreeMap<String, String>,
) -> Option<Value> {
    let changed: Vec<&String> = before
        .keys()
        .chain(after.keys())
        .filter(|k| before.get(*k) != after.get(*k))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    if changed.is_empty() {
        return None;
    }
    Some(json!({
        "field": field,
        "before": masked_map(before),
        "after": masked_map(after),
        "changed_keys": changed
    }))
}

/// 上游配置的真实变更（新建/删除时一侧为空对象）；密钥值永远不进审计。
fn upstream_changes(before: &McpUpstream, after: &McpUpstream) -> Vec<Value> {
    let mut out = field_changes(
        &before.to_public_value(),
        &after.to_public_value(),
        UPSTREAM_CHANGE_FIELDS,
    );
    for (field, old, new) in [
        ("env", &before.env, &after.env),
        ("headers", &before.headers, &after.headers),
    ] {
        if let Some(change) = secret_map_changes(field, old, new) {
            out.push(change);
        }
    }
    out
}

/// 空配置：用于表达"新建/删除时的另一侧"，所有字段取零值。
fn empty_upstream() -> McpUpstream {
    McpUpstream {
        id: String::new(),
        name: String::new(),
        transport: String::new(),
        url: String::new(),
        command: String::new(),
        args: Vec::new(),
        env: std::collections::BTreeMap::new(),
        headers: std::collections::BTreeMap::new(),
        group: String::new(),
        category: String::new(),
        read_only: true,
        enabled: true,
        timeout_ms: 0,
        catalog_id: None,
        catalog_version: None,
        created_at: String::new(),
        updated_at: String::new(),
    }
}

impl CapabilityStore {
    /// 全部上游（按 id 升序，输出稳定）。
    pub fn upstreams(&self) -> Result<Vec<McpUpstream>, McpError> {
        let conn = self.conn.lock().map_err(|_| McpError::lock())?;
        let mut stmt = conn
            .prepare("SELECT * FROM mcp_upstreams ORDER BY id")
            .map_err(|e| McpError::store(e.to_string()))?;
        let mapped = stmt
            .query_map([], row_to_upstream)
            .map_err(|e| McpError::store(e.to_string()))?;
        let mut rows = Vec::new();
        for row in mapped {
            rows.push(row.map_err(|e| McpError::store(e.to_string()))?);
        }
        Ok(rows)
    }

    pub fn upstream(&self, id: &str) -> Result<Option<McpUpstream>, McpError> {
        let conn = self.conn.lock().map_err(|_| McpError::lock())?;
        let mut stmt = conn
            .prepare("SELECT * FROM mcp_upstreams WHERE id=?")
            .map_err(|e| McpError::store(e.to_string()))?;
        let mapped = stmt
            .query_map([id], row_to_upstream)
            .map_err(|e| McpError::store(e.to_string()))?;
        let mut rows = Vec::new();
        for row in mapped {
            rows.push(row.map_err(|e| McpError::store(e.to_string()))?);
        }
        Ok(rows.into_iter().next())
    }

    fn require_upstream(&self, id: &str) -> Result<McpUpstream, McpError> {
        self.upstream(id)?
            .ok_or_else(|| McpError::new("MCP_UPSTREAM_NOT_FOUND", json!({"id": id})))
    }

    /// 新建上游：同 id 已存在 → 409 `MCP_UPSTREAM_CONFLICT`（不静默覆盖）。
    pub fn insert_upstream(
        &self,
        upstream: &McpUpstream,
        actor: impl IntoAuditOrigin,
    ) -> Result<McpUpstream, McpError> {
        let origin = actor.into_audit_origin();
        upstream.validate()?;
        {
            let conn = self.conn.lock().map_err(|_| McpError::lock())?;
            let exists: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM mcp_upstreams WHERE id=?",
                    [&upstream.id],
                    |r| r.get(0),
                )
                .map_err(|e| McpError::store(e.to_string()))?;
            if exists > 0 {
                return Err(McpError::new(
                    "MCP_UPSTREAM_CONFLICT",
                    json!({"id": upstream.id, "hint": "上游 id 已存在"}),
                ));
            }
            let now = now_iso();
            conn.execute(
                "INSERT INTO mcp_upstreams (id, name, transport, url, command, args_json, env_json, headers_json, group_code, category, read_only, enabled, timeout_ms, created_at, updated_at, catalog_id, catalog_version) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?14,?15,?16)",
                rusqlite::params![
                    upstream.id,
                    upstream.name,
                    upstream.transport,
                    upstream.url,
                    upstream.command,
                    serde_json::to_string(&upstream.args).map_err(|e| McpError::store(e.to_string()))?,
                    serde_json::to_string(&upstream.env).map_err(|e| McpError::store(e.to_string()))?,
                    serde_json::to_string(&upstream.headers).map_err(|e| McpError::store(e.to_string()))?,
                    upstream.group,
                    upstream.category,
                    upstream.read_only as i64,
                    upstream.enabled as i64,
                    upstream.timeout_ms,
                    now,
                    upstream.catalog_id.as_deref().unwrap_or_default(),
                    upstream.catalog_version.as_deref().unwrap_or_default()
                ],
            )
            .map_err(|e| McpError::store(e.to_string()))?;
        }
        let created = self.require_upstream(&upstream.id)?;
        self.write_admin_audit(
            AdminChange::new("upstream", &created.id, "create")
                .with_origin(origin)
                .with_changes(upstream_changes(&empty_upstream(), &created))
                .with_data(created.audit_data()),
        )?;
        Ok(created)
    }

    /// 修改上游：id 是主键不可改（调用方用路径 id 读取再替换）；无实际变化不写审计。
    ///
    /// 包坐标（`catalog_id`/`catalog_version`）不在这条 UPDATE 的列里：安装来源只由安装/
    /// 卸载路径维护，普通编辑既不写也不清空它们。
    pub fn update_upstream(
        &self,
        upstream: &McpUpstream,
        actor: impl IntoAuditOrigin,
    ) -> Result<McpUpstream, McpError> {
        let origin = actor.into_audit_origin();
        upstream.validate()?;
        let before = self.require_upstream(&upstream.id)?;
        let changes = upstream_changes(&before, upstream);
        if changes.is_empty() {
            return Ok(before);
        }
        {
            let conn = self.conn.lock().map_err(|_| McpError::lock())?;
            let changed = conn
                .execute(
                    "UPDATE mcp_upstreams SET name=?2, transport=?3, url=?4, command=?5, args_json=?6, env_json=?7, headers_json=?8, group_code=?9, category=?10, read_only=?11, enabled=?12, timeout_ms=?13, updated_at=?14 WHERE id=?1",
                    rusqlite::params![
                        upstream.id,
                        upstream.name,
                        upstream.transport,
                        upstream.url,
                        upstream.command,
                        serde_json::to_string(&upstream.args).map_err(|e| McpError::store(e.to_string()))?,
                        serde_json::to_string(&upstream.env).map_err(|e| McpError::store(e.to_string()))?,
                        serde_json::to_string(&upstream.headers).map_err(|e| McpError::store(e.to_string()))?,
                        upstream.group,
                        upstream.category,
                        upstream.read_only as i64,
                        upstream.enabled as i64,
                        upstream.timeout_ms,
                        now_iso()
                    ],
                )
                .map_err(|e| McpError::store(e.to_string()))?;
            if changed == 0 {
                return Err(McpError::new(
                    "MCP_UPSTREAM_NOT_FOUND",
                    json!({"id": upstream.id}),
                ));
            }
        }
        let after = self.require_upstream(&upstream.id)?;
        self.write_admin_audit(
            AdminChange::new("upstream", &after.id, "update")
                .with_origin(origin)
                .with_changes(changes)
                .with_data(after.audit_data()),
        )?;
        Ok(after)
    }

    /// 删除上游：不存在 → 404；逐工具组覆盖在同一事务里级联删除；删除前的一侧配置进审计 changes。
    pub fn delete_upstream(&self, id: &str, actor: impl IntoAuditOrigin) -> Result<(), McpError> {
        let origin = actor.into_audit_origin();
        let before = self.require_upstream(id)?;
        {
            let conn = self.conn.lock().map_err(|_| McpError::lock())?;
            let tx = conn
                .unchecked_transaction()
                .map_err(|e| McpError::store(e.to_string()))?;
            tx.execute(
                "DELETE FROM mcp_upstream_tool_groups WHERE upstream_id=?",
                [id],
            )
            .map_err(|e| McpError::store(e.to_string()))?;
            tx.execute("DELETE FROM mcp_upstreams WHERE id=?", [id])
                .map_err(|e| McpError::store(e.to_string()))?;
            tx.commit().map_err(|e| McpError::store(e.to_string()))?;
        }
        self.write_admin_audit(
            AdminChange::new("upstream", id, "delete")
                .with_origin(origin)
                .with_changes(upstream_changes(&before, &empty_upstream()))
                .with_data(before.audit_data()),
        )
    }

    /// 全部逐工具组覆盖（按 `upstream_id, tool_name` 升序，输出稳定）。
    ///
    /// 目前只有 `basic` 一种取值：投影阶段用 `(upstream_id, tool_name) → group_code` 决定
    /// 单个上游工具的生效组（无行 = 继承上游组）。
    pub fn upstream_tool_groups(&self) -> Result<Vec<UpstreamToolGroup>, McpError> {
        let conn = self.conn.lock().map_err(|_| McpError::lock())?;
        let mut stmt = conn
            .prepare(
                "SELECT upstream_id, tool_name, group_code FROM mcp_upstream_tool_groups ORDER BY upstream_id, tool_name",
            )
            .map_err(|e| McpError::store(e.to_string()))?;
        let mapped = stmt
            .query_map([], row_to_tool_group)
            .map_err(|e| McpError::store(e.to_string()))?;
        let mut rows = Vec::new();
        for row in mapped {
            rows.push(row.map_err(|e| McpError::store(e.to_string()))?);
        }
        Ok(rows)
    }

    /// 单台上游的覆盖行（管理面读视图用），按 `tool_name` 升序。
    pub fn upstream_tool_groups_of(&self, id: &str) -> Result<Vec<UpstreamToolGroup>, McpError> {
        let conn = self.conn.lock().map_err(|_| McpError::lock())?;
        let mut stmt = conn
            .prepare(
                "SELECT upstream_id, tool_name, group_code FROM mcp_upstream_tool_groups WHERE upstream_id=? ORDER BY tool_name",
            )
            .map_err(|e| McpError::store(e.to_string()))?;
        let mapped = stmt
            .query_map([id], row_to_tool_group)
            .map_err(|e| McpError::store(e.to_string()))?;
        let mut rows = Vec::new();
        for row in mapped {
            rows.push(row.map_err(|e| McpError::store(e.to_string()))?);
        }
        Ok(rows)
    }

    /// 归入 `basic` 的上游工具总数（`GET /admin/groups` 的 basic 条目计数）。
    pub fn count_basic_upstream_tools(&self) -> Result<i64, McpError> {
        let conn = self.conn.lock().map_err(|_| McpError::lock())?;
        conn.query_row(
            "SELECT COUNT(*) FROM mcp_upstream_tool_groups WHERE group_code=?",
            [crate::config::BASIC_GROUP_NAME],
            |r| r.get(0),
        )
        .map_err(|e| McpError::store(e.to_string()))
    }

    /// 合并式写入逐工具组覆盖：`set` 建/改行，`clear` 删行；单台上游上限
    /// [`UPSTREAM_TOOL_GROUP_MAX`]。
    ///
    /// 语义：**合并**，不是整体替换——一次 PUT 只表达"这几个工具进 basic / 出 basic"。
    /// 幂等：与现状无差异的行不写，无任何变化时不写审计并返回空 changes。
    /// 上限检查与写入在同一事务内，避免两个并发 PUT 各自看到"未超限"后一起写超。
    pub fn apply_upstream_tool_groups(
        &self,
        id: &str,
        set: &[(String, String)],
        clear: &[String],
        actor: impl IntoAuditOrigin,
    ) -> Result<Vec<Value>, McpError> {
        let origin = actor.into_audit_origin();
        let basic = crate::config::BASIC_GROUP_NAME;
        for (_, group) in set {
            if group != basic {
                return Err(McpError::new(
                    "MCP_VALIDATION_ERROR",
                    json!({"field": "tools", "group": group, "hint": "逐工具覆盖只支持 basic"}),
                ));
            }
        }
        let before = self.upstream_tool_groups_of(id)?;
        let known: std::collections::HashMap<&str, &str> = before
            .iter()
            .map(|r| (r.tool_name.as_str(), r.group_code.as_str()))
            .collect();
        {
            let conn = self.conn.lock().map_err(|_| McpError::lock())?;
            let tx = conn
                .unchecked_transaction()
                .map_err(|e| McpError::store(e.to_string()))?;
            let exists: i64 = tx
                .query_row("SELECT COUNT(*) FROM mcp_upstreams WHERE id=?", [id], |r| {
                    r.get(0)
                })
                .map_err(|e| McpError::store(e.to_string()))?;
            if exists == 0 {
                return Err(McpError::new("MCP_UPSTREAM_NOT_FOUND", json!({"id": id})));
            }
            let now = now_iso();
            {
                let mut upsert = tx
                    .prepare(
                        "INSERT INTO mcp_upstream_tool_groups (upstream_id, tool_name, group_code, updated_at) VALUES (?1,?2,?3,?4) ON CONFLICT(upstream_id, tool_name) DO UPDATE SET group_code=excluded.group_code, updated_at=excluded.updated_at",
                    )
                    .map_err(|e| McpError::store(e.to_string()))?;
                for (tool, group) in set {
                    if known.get(tool.as_str()) == Some(&group.as_str()) {
                        continue;
                    }
                    upsert
                        .execute(rusqlite::params![id, tool, group, &now])
                        .map_err(|e| McpError::store(e.to_string()))?;
                }
            }
            {
                let mut delete = tx
                    .prepare(
                        "DELETE FROM mcp_upstream_tool_groups WHERE upstream_id=?1 AND tool_name=?2",
                    )
                    .map_err(|e| McpError::store(e.to_string()))?;
                for tool in clear {
                    if !known.contains_key(tool.as_str()) {
                        continue;
                    }
                    delete
                        .execute(rusqlite::params![id, tool])
                        .map_err(|e| McpError::store(e.to_string()))?;
                }
            }
            let count: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM mcp_upstream_tool_groups WHERE upstream_id=? AND group_code=?",
                    rusqlite::params![id, basic],
                    |r| r.get(0),
                )
                .map_err(|e| McpError::store(e.to_string()))?;
            if count > UPSTREAM_TOOL_GROUP_MAX as i64 {
                return Err(McpError::new(
                    "MCP_VALIDATION_ERROR",
                    json!({
                        "field": "tools",
                        "tool_count": count,
                        "limit": UPSTREAM_TOOL_GROUP_MAX,
                        "hint": "单台上游的 basic 工具数超过上限",
                    }),
                ));
            }
            tx.commit().map_err(|e| McpError::store(e.to_string()))?;
        }
        let after = self.upstream_tool_groups_of(id)?;
        let mut changes: Vec<Value> = Vec::new();
        for row in &after {
            if known.get(row.tool_name.as_str()) != Some(&row.group_code.as_str()) {
                changes.push(json!({
                    "field": "tool_group",
                    "tool": row.tool_name,
                    "before": known.get(row.tool_name.as_str()).map(|g| json!(g)),
                    "after": row.group_code,
                }));
            }
        }
        for row in &before {
            if !after.iter().any(|r| r.tool_name == row.tool_name) {
                changes.push(json!({
                    "field": "tool_group",
                    "tool": row.tool_name,
                    "before": row.group_code,
                    "after": Value::Null,
                }));
            }
        }
        changes.sort_by(|a, b| {
            let key = |v: &Value| {
                (
                    v.get("tool")
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .to_string(),
                    v.get("field")
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .to_string(),
                )
            };
            key(a).cmp(&key(b))
        });
        if changes.is_empty() {
            return Ok(changes);
        }
        let basic_total = after.iter().filter(|r| r.group_code == basic).count();
        self.write_admin_audit(
            AdminChange::new("upstream", id, "update_tool_groups")
                .with_origin(origin)
                .with_changes(changes.clone())
                .with_data(json!({"basic_total": basic_total})),
        )?;
        Ok(changes)
    }
}
/// 空串列值 → `None`：SQLite 用 `TEXT NOT NULL DEFAULT ''` 表达"没有"，对外契约用 null。
fn non_empty_opt(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

fn row_to_tool_group(row: &Row) -> rusqlite::Result<UpstreamToolGroup> {
    Ok(UpstreamToolGroup {
        upstream_id: row.get("upstream_id")?,
        tool_name: row.get("tool_name")?,
        group_code: row.get("group_code")?,
    })
}

fn row_to_upstream(row: &Row) -> rusqlite::Result<McpUpstream> {
    let args_json: String = row.get("args_json")?;
    let env_json: String = row.get("env_json")?;
    let headers_json: String = row.get("headers_json")?;
    let decode = |raw: &str, column: usize| -> rusqlite::Result<Value> {
        serde_json::from_str(raw).map_err(|_| {
            rusqlite::Error::FromSqlConversionFailure(
                column,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "upstream config json decode failed",
                )),
            )
        })
    };
    let args: Vec<String> = serde_json::from_value(decode(&args_json, 5)?).map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            5,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "upstream args decode failed",
            )),
        )
    })?;
    let env: std::collections::BTreeMap<String, String> =
        serde_json::from_value(decode(&env_json, 6)?).map_err(|_| {
            rusqlite::Error::FromSqlConversionFailure(
                6,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "upstream env decode failed",
                )),
            )
        })?;
    let headers: std::collections::BTreeMap<String, String> =
        serde_json::from_value(decode(&headers_json, 7)?).map_err(|_| {
            rusqlite::Error::FromSqlConversionFailure(
                7,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "upstream headers decode failed",
                )),
            )
        })?;
    Ok(McpUpstream {
        id: row.get("id")?,
        name: row.get("name")?,
        transport: row.get("transport")?,
        url: row.get("url")?,
        command: row.get("command")?,
        args,
        env,
        headers,
        group: row.get("group_code")?,
        category: row.get("category")?,
        read_only: row.get::<_, i64>("read_only")? != 0,
        enabled: row.get::<_, i64>("enabled")? != 0,
        timeout_ms: row.get("timeout_ms")?,
        catalog_id: non_empty_opt(row.get::<_, String>("catalog_id")?),
        catalog_version: non_empty_opt(row.get::<_, String>("catalog_version")?),
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}
