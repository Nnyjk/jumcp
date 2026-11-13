//! 平台生成的 MCP access token：密文持久化、按 hash 鉴权、撤销与组引用计数。
//!
//! 为什么落 registry 同一个 SQLite：token 与组/能力必须同库同事务视图，
//! 否则"删组前检查 token 引用"会出现跨库竞态。
//!
//! 明文只在创建或重新签发时返回一次（`jumcp_<64 hex>`），库里只存 sha256；审计只写前缀，
//! 永远不写明文或 hash。

use crate::audit::IntoAuditOrigin;
use crate::canonical::sha256_hex;
use crate::config::{AgentPrincipal, TenantPolicy, is_principal_id};
use crate::errors::McpError;
use crate::registry::{ADMIN_CAPABILITY_IDS, AdminChange, BUILTIN_CAPABILITY_IDS, CapabilityStore};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, Row};
use serde_json::{Value, json};
use std::collections::HashMap;

/// 明文 token 前缀：客户端/日志可据此识别 token 类型（前缀本身不是秘密）。
pub const TOKEN_PLAINTEXT_PREFIX: &str = "jumcp_";
/// 名称/来源长度上限（按字符计）。
pub const TOKEN_FIELD_MAX: usize = 64;
/// 引导令牌的固定名称：运维据此辨认"从 JUMCP_AGENT_TOKENS 导入"的记录。
pub const BOOTSTRAP_TOKEN_NAME: &str = "环境引导令牌";

pub const TOKEN_STATUS_ACTIVE: &str = "active";
pub const TOKEN_STATUS_REVOKED: &str = "revoked";
pub const TOKEN_STATUS_EXPIRED: &str = "expired";

/// `access_tokens` 的一行。故意不 derive Debug：避免把 `token_hash` 打进日志。
#[derive(Clone)]
pub struct AccessTokenRecord {
    pub id: String,
    pub name: String,
    pub token_hash: String,
    pub token_prefix: String,
    pub tenant: String,
    pub role: String,
    pub groups: Vec<String>,
    pub builtin_tools: Vec<String>,
    pub source: String,
    pub caller: String,
    pub enabled: bool,
    pub created_at: String,
    pub created_by: String,
    /// 空串 = 无过期时间（输出层转 null）。
    pub expires_at: String,
    /// 空串 = 未撤销。
    pub revoked_at: String,
    pub last_used_at: String,
}

impl std::fmt::Debug for AccessTokenRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccessTokenRecord")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("token_prefix", &self.token_prefix)
            .field("role", &self.role)
            .field("revoked", &!self.revoked_at.is_empty())
            .finish_non_exhaustive()
    }
}

impl AccessTokenRecord {
    pub fn status_at(&self, now: DateTime<Utc>) -> &'static str {
        if !self.revoked_at.is_empty() {
            TOKEN_STATUS_REVOKED
        } else if self.is_expired_at(now) {
            TOKEN_STATUS_EXPIRED
        } else {
            TOKEN_STATUS_ACTIVE
        }
    }

    pub fn status(&self) -> &'static str {
        self.status_at(Utc::now())
    }

    fn is_expired_at(&self, now: DateTime<Utc>) -> bool {
        if self.expires_at.is_empty() {
            return false;
        }
        match parse_rfc3339(&self.expires_at) {
            // 存储值不可解析按过期处理（fail-closed），绝不让损坏行变成永久有效 token。
            Some(at) => at <= now,
            None => true,
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "token_prefix": self.token_prefix,
            "tenant": self.tenant,
            "role": self.role,
            "groups": self.groups,
            "builtin_tools": self.builtin_tools,
            "source": self.source,
            "caller": self.caller,
            "status": self.status(),
            "created_at": self.created_at,
            "created_by": self.created_by,
            "expires_at": null_if_empty(&self.expires_at),
            "last_used_at": null_if_empty(&self.last_used_at),
            "revoked_at": null_if_empty(&self.revoked_at),
        })
    }
}

fn null_if_empty(value: &str) -> Value {
    if value.is_empty() {
        Value::Null
    } else {
        json!(value)
    }
}

fn parse_rfc3339(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|at| at.with_timezone(&Utc))
}

/// 创建令牌的入参（角色/组的**策略成员资格**由调用方在合并策略上校验）。
#[derive(Debug, Clone)]
pub struct NewAccessToken {
    pub name: String,
    pub role: String,
    pub groups: Vec<String>,
    pub builtin_tools: Vec<String>,
    pub source: String,
    pub caller: String,
    pub expires_at: Option<String>,
    pub tenant: String,
    pub created_by: String,
}

impl NewAccessToken {
    /// 与策略无关的字段门禁：字段级非法一律 400（`MCP_VALIDATION_ERROR`）。
    pub fn validate(&self) -> Result<(), McpError> {
        let name = self.name.trim();
        let name_len = name.chars().count();
        if name_len == 0 || name_len > TOKEN_FIELD_MAX {
            return Err(McpError::new(
                "MCP_VALIDATION_ERROR",
                json!({"field": "name", "min": 1, "max": TOKEN_FIELD_MAX, "len": name_len}),
            ));
        }
        if self.role.trim().is_empty() {
            return Err(McpError::new(
                "MCP_VALIDATION_ERROR",
                json!({"field": "role", "hint": "role 必填"}),
            ));
        }
        if !is_principal_id(&self.caller) {
            return Err(McpError::new(
                "MCP_VALIDATION_ERROR",
                json!({"field": "caller", "rule": "^[a-z0-9._-]{2,32}$", "value": self.caller}),
            ));
        }
        if !valid_source(&self.source) {
            return Err(McpError::new(
                "MCP_VALIDATION_ERROR",
                json!({"field": "source", "rule": "^[a-z0-9._-]{1,64}$", "value": self.source}),
            ));
        }
        if self.groups.is_empty() {
            return Err(McpError::new(
                "MCP_VALIDATION_ERROR",
                json!({"field": "groups", "hint": "groups 不能为空"}),
            ));
        }
        // allowlist 同时约束内置与上游工具：内置 id，或上游暴露名 `<server_id>__<tool>`。
        // 上游名不要求此刻已注册（令牌可以先于上游创建），但必须符合命名规则。
        let unknown: Vec<&str> = self
            .builtin_tools
            .iter()
            .map(String::as_str)
            .filter(|id| {
                !BUILTIN_CAPABILITY_IDS.contains(id) && !crate::registry::is_upstream_tool_name(id)
            })
            .collect();
        if !unknown.is_empty() {
            return Err(McpError::new(
                "MCP_VALIDATION_ERROR",
                json!({
                    "field": "builtin_tools",
                    "unknown": unknown,
                    "allowed": BUILTIN_CAPABILITY_IDS,
                    "upstream_rule": "<server_id>__<tool>（两侧 [A-Za-z0-9_-]，整名 ≤64）"
                }),
            ));
        }
        let admin_ids: Vec<&str> = self
            .builtin_tools
            .iter()
            .map(String::as_str)
            .filter(|id| ADMIN_CAPABILITY_IDS.contains(id))
            .collect();
        if !admin_ids.is_empty() && !self.groups.iter().any(|g| g == "admin") {
            return Err(McpError::new(
                "MCP_VALIDATION_ERROR",
                json!({
                    "field": "builtin_tools",
                    "offending": admin_ids,
                    "hint": "admin_* 内置工具要求 groups 含 admin（双重判定）"
                }),
            ));
        }
        if let Some(expires_at) = &self.expires_at {
            let Some(at) = parse_rfc3339(expires_at) else {
                return Err(McpError::new(
                    "MCP_VALIDATION_ERROR",
                    json!({"field": "expires_at", "hint": "必须是 RFC3339 时间", "value": expires_at}),
                ));
            };
            if at <= Utc::now() {
                return Err(McpError::new(
                    "MCP_VALIDATION_ERROR",
                    json!({"field": "expires_at", "hint": "过期时间必须晚于当前时间", "value": expires_at}),
                ));
            }
        }
        Ok(())
    }
}

fn valid_source(source: &str) -> bool {
    let len = source.chars().count();
    (1..=TOKEN_FIELD_MAX).contains(&len)
        && source.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

/// 创建成功的结果：明文只在这里出现一次。
#[derive(Clone)]
pub struct IssuedAccessToken {
    pub token: String,
    pub record: AccessTokenRecord,
}

impl std::fmt::Debug for IssuedAccessToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssuedAccessToken")
            .field("record", &self.record)
            .finish_non_exhaustive()
    }
}

pub(crate) const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS access_tokens (
    id TEXT PRIMARY KEY, name TEXT NOT NULL, token_hash TEXT NOT NULL UNIQUE,
    token_prefix TEXT NOT NULL DEFAULT '', tenant TEXT NOT NULL DEFAULT 'default',
    role TEXT NOT NULL, groups_json TEXT NOT NULL DEFAULT '[]',
    builtin_tools_json TEXT NOT NULL DEFAULT '[]', source TEXT NOT NULL DEFAULT '',
    caller TEXT NOT NULL DEFAULT '', enabled INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT '', created_by TEXT NOT NULL DEFAULT '',
    expires_at TEXT NOT NULL DEFAULT '', revoked_at TEXT NOT NULL DEFAULT '',
    last_used_at TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_access_tokens_hash ON access_tokens(token_hash);
"#;

/// 打开库时的建表/补列（旧库没有该表时直接创建；不破坏既有表）。
pub(crate) fn ensure_schema(conn: &Connection) -> Result<(), McpError> {
    conn.execute_batch(SCHEMA)
        .map_err(|e| McpError::store(e.to_string()))?;
    for column in [
        "builtin_tools_json",
        "token_prefix",
        "tenant",
        "source",
        "caller",
        "enabled",
        "created_at",
        "created_by",
        "expires_at",
        "revoked_at",
        "last_used_at",
    ] {
        crate::registry::ensure_column(conn, "access_tokens", column)?;
    }
    Ok(())
}

fn row_to_token(row: &Row) -> rusqlite::Result<AccessTokenRecord> {
    Ok(AccessTokenRecord {
        id: row.get(0)?,
        name: row.get(1)?,
        token_hash: row.get(2)?,
        token_prefix: row.get(3)?,
        tenant: row.get(4)?,
        role: row.get(5)?,
        groups: decode_list(row.get(6)?, 6)?,
        builtin_tools: decode_list(row.get(7)?, 7)?,
        source: row.get(8)?,
        caller: row.get(9)?,
        enabled: row.get::<_, i64>(10)? != 0,
        created_at: row.get(11)?,
        created_by: row.get(12)?,
        expires_at: row.get(13)?,
        revoked_at: row.get(14)?,
        last_used_at: row.get(15)?,
    })
}

/// 列表列可能是空串（旧库 `ensure_column` 的默认值）→ 视为空集，而不是解码失败。
fn decode_list(raw: String, column: usize) -> rusqlite::Result<Vec<String>> {
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(&raw).map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "access token list json decode failed",
            )),
        )
    })
}

const COLUMNS: &str = "id, name, token_hash, token_prefix, tenant, role, groups_json, builtin_tools_json, source, caller, enabled, created_at, created_by, expires_at, revoked_at, last_used_at";

fn now_iso() -> String {
    Utc::now().to_rfc3339()
}

/// 已持有 store 连接锁时的组引用计数（组删除门禁与单组查询共用）。
pub(crate) fn group_token_count_locked(conn: &Connection, name: &str) -> Result<i64, McpError> {
    let mut stmt = conn
        .prepare("SELECT groups_json FROM access_tokens WHERE revoked_at=''")
        .map_err(|e| McpError::store(e.to_string()))?;
    let mapped = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| McpError::store(e.to_string()))?;
    let mut count = 0;
    for row in mapped {
        let raw = row.map_err(|e| McpError::store(e.to_string()))?;
        let groups = decode_list(raw, 0).map_err(|e| McpError::store(e.to_string()))?;
        if groups.iter().any(|g| g == name) {
            count += 1;
        }
    }
    Ok(count)
}

fn random_hex(len: usize) -> String {
    let mut out = String::new();
    while out.len() < len {
        out.push_str(&uuid::Uuid::new_v4().simple().to_string());
    }
    out.truncate(len);
    out
}

fn new_token_id() -> String {
    format!("tok_{}", random_hex(16))
}

fn display_prefix(token: &str) -> String {
    token.chars().take(12).collect()
}

fn encode_list(values: &[String]) -> Result<String, McpError> {
    serde_json::to_string(values).map_err(|e| McpError::store(format!("列表序列化失败: {e}")))
}

impl CapabilityStore {
    fn audit_required(&self, hint: &str) -> Result<(), McpError> {
        if self.audit_available() {
            return Ok(());
        }
        Err(McpError::new("MCP_AUDIT_ERROR", json!({"hint": hint})))
    }

    /// 创建令牌：生成明文/ID/哈希，落库后写 `admin_change`（载荷不含明文与 hash）。
    pub fn create_access_token(
        &self,
        new: &NewAccessToken,
        actor: &crate::entry::Actor,
    ) -> Result<IssuedAccessToken, McpError> {
        self.audit_required("审计未启用，令牌写入拒绝")?;
        new.validate()?;
        let token = format!("{TOKEN_PLAINTEXT_PREFIX}{}", random_hex(64));
        let hash = sha256_hex(&token);
        let prefix = display_prefix(&token);
        let created_at = now_iso();
        let groups_json = encode_list(&new.groups)?;
        let tools_json = encode_list(&new.builtin_tools)?;
        let expires_at = new.expires_at.clone().unwrap_or_default();
        let mut id = new_token_id();
        {
            let conn = self.db().lock().map_err(|_| McpError::lock())?;
            let mut stored = false;
            for _ in 0..5 {
                let result = conn.execute(
                    &format!(
                        "INSERT INTO access_tokens ({COLUMNS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,1,?11,?12,?13,'','')"
                    ),
                    rusqlite::params![
                        id,
                        new.name.trim(),
                        hash,
                        prefix,
                        new.tenant,
                        new.role,
                        groups_json,
                        tools_json,
                        new.source,
                        new.caller,
                        created_at,
                        new.created_by,
                        expires_at
                    ],
                );
                match result {
                    Ok(_) => {
                        stored = true;
                        break;
                    }
                    // 主键碰撞（概率极低）重新生成 id；其余错误（含 token_hash 唯一冲突）上抛。
                    Err(rusqlite::Error::SqliteFailure(e, _))
                        if e.code == rusqlite::ErrorCode::ConstraintViolation =>
                    {
                        let taken: i64 = conn
                            .query_row(
                                "SELECT COUNT(*) FROM access_tokens WHERE id=?",
                                [&id],
                                |r| r.get(0),
                            )
                            .unwrap_or(0);
                        if taken > 0 {
                            id = new_token_id();
                            continue;
                        }
                        return Err(McpError::store(
                            "令牌哈希冲突：明文 token 已存在（拒绝复用）".to_string(),
                        ));
                    }
                    Err(e) => return Err(McpError::store(e.to_string())),
                }
            }
            if !stored {
                return Err(McpError::store("令牌 ID 生成冲突".to_string()));
            }
        }
        let record = self
            .get_access_token(&id)?
            .ok_or_else(|| McpError::store("create_access_token 后记录缺失".to_string()))?;
        self.write_admin_audit(
            AdminChange::new("token", &id, "token_create")
                .with_origin(actor.into_audit_origin())
                .with_data(json!({
                    "name": record.name,
                    "role": record.role,
                    "groups": record.groups,
                    "builtin_tools": record.builtin_tools,
                    "source": record.source,
                    "caller": record.caller,
                    "expires_at": null_if_empty(&record.expires_at),
                    "token_prefix": record.token_prefix
                })),
        )?;
        Ok(IssuedAccessToken { token, record })
    }

    /// 重新签发：保留 id/name/role/groups/builtin_tools/source/caller/expires_at，
    /// 只换明文/hash/前缀并清空 `last_used_at`。
    ///
    /// 明文不可回读，管理台只能通过这条路给同一记录换一把新钥匙（旧明文立即失效）。
    /// 仅 active 可重签：撤销/过期返回 409，且不改行、不写审计。
    pub fn reissue_access_token(
        &self,
        id: &str,
        actor: &crate::entry::Actor,
    ) -> Result<IssuedAccessToken, McpError> {
        self.audit_required("审计未启用，令牌重新签发拒绝")?;
        let record = self
            .get_access_token(id)?
            .ok_or_else(|| McpError::new("MCP_TOKEN_NOT_FOUND", json!({"token_id": id})))?;
        let status = record.status();
        if status != TOKEN_STATUS_ACTIVE {
            return Err(McpError::new(
                "MCP_STATE_TRANSITION_INVALID",
                json!({
                    "token_id": id,
                    "status": status,
                    "hint": "只有未撤销且未过期的令牌可以重新签发"
                }),
            ));
        }
        let token = format!("{TOKEN_PLAINTEXT_PREFIX}{}", random_hex(64));
        let hash = sha256_hex(&token);
        let prefix = display_prefix(&token);
        {
            let conn = self.db().lock().map_err(|_| McpError::lock())?;
            // 条件更新挡住"检查后、更新前被并发撤销"的窗口。
            let changed = conn
                .execute(
                    "UPDATE access_tokens SET token_hash=?, token_prefix=?, enabled=1, last_used_at='' \
                     WHERE id=? AND revoked_at=''",
                    rusqlite::params![hash, prefix, id],
                )
                .map_err(|e| McpError::store(e.to_string()))?;
            if changed == 0 {
                return Err(McpError::new(
                    "MCP_STATE_TRANSITION_INVALID",
                    json!({"token_id": id, "hint": "令牌状态已变化，请刷新后重试"}),
                ));
            }
        }
        let updated = self
            .get_access_token(id)?
            .ok_or_else(|| McpError::store("reissue_access_token 后记录缺失".to_string()))?;
        self.write_admin_audit(
            AdminChange::new("token", id, "token_reissue")
                .with_origin(actor.into_audit_origin())
                .with_data(json!({
                    "name": updated.name,
                    "role": updated.role,
                    "groups": updated.groups,
                    "builtin_tools": updated.builtin_tools,
                    "source": updated.source,
                    "caller": updated.caller,
                    "expires_at": null_if_empty(&updated.expires_at),
                    "token_prefix": updated.token_prefix
                })),
        )?;
        Ok(IssuedAccessToken {
            token,
            record: updated,
        })
    }

    pub fn get_access_token(&self, id: &str) -> Result<Option<AccessTokenRecord>, McpError> {
        let conn = self.db().lock().map_err(|_| McpError::lock())?;
        let mut stmt = conn
            .prepare(&format!("SELECT {COLUMNS} FROM access_tokens WHERE id=?"))
            .map_err(|e| McpError::store(e.to_string()))?;
        let mut rows = stmt
            .query_map([id], row_to_token)
            .map_err(|e| McpError::store(e.to_string()))?;
        match rows.next() {
            Some(row) => Ok(Some(row.map_err(|e| McpError::store(e.to_string()))?)),
            None => Ok(None),
        }
    }

    fn access_token_by_hash(&self, hash: &str) -> Result<Option<AccessTokenRecord>, McpError> {
        let conn = self.db().lock().map_err(|_| McpError::lock())?;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {COLUMNS} FROM access_tokens WHERE token_hash=?"
            ))
            .map_err(|e| McpError::store(e.to_string()))?;
        let mut rows = stmt
            .query_map([hash], row_to_token)
            .map_err(|e| McpError::store(e.to_string()))?;
        match rows.next() {
            Some(row) => Ok(Some(row.map_err(|e| McpError::store(e.to_string()))?)),
            None => Ok(None),
        }
    }

    /// Bearer 鉴权：只接受 active（未撤销且未过期）；成功时刷新 `last_used_at`。
    ///
    /// 刷新失败只记日志：已认证请求不因"记录使用时间"失败而 500。
    pub fn authenticate_access_token(
        &self,
        presented: &str,
    ) -> Result<Option<AccessTokenRecord>, McpError> {
        if presented.is_empty() {
            return Ok(None);
        }
        let hash = sha256_hex(presented);
        let Some(mut record) = self.access_token_by_hash(&hash)? else {
            return Ok(None);
        };
        if !record.enabled || !record.revoked_at.is_empty() || record.is_expired_at(Utc::now()) {
            return Ok(None);
        }
        let used_at = now_iso();
        {
            let conn = self.db().lock().map_err(|_| McpError::lock())?;
            if let Err(e) = conn.execute(
                "UPDATE access_tokens SET last_used_at=? WHERE id=?",
                rusqlite::params![used_at, record.id],
            ) {
                tracing::warn!(token_id = %record.id, error = %e, "刷新 token last_used_at 失败");
            } else {
                record.last_used_at = used_at;
            }
        }
        Ok(Some(record))
    }

    /// 全量列表：`created_at` 倒序、等值时 id 升序（稳定分页顺序）。
    pub fn list_access_tokens(&self) -> Result<Vec<AccessTokenRecord>, McpError> {
        let conn = self.db().lock().map_err(|_| McpError::lock())?;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {COLUMNS} FROM access_tokens ORDER BY created_at DESC, id ASC"
            ))
            .map_err(|e| McpError::store(e.to_string()))?;
        let mapped = stmt
            .query_map([], row_to_token)
            .map_err(|e| McpError::store(e.to_string()))?;
        let mut out = Vec::new();
        for row in mapped {
            out.push(row.map_err(|e| McpError::store(e.to_string()))?);
        }
        Ok(out)
    }

    /// 撤销：只置 `enabled=0` + `revoked_at`，不删行；重复撤销 409 且不重复写审计。
    pub fn revoke_access_token(
        &self,
        id: &str,
        actor: &crate::entry::Actor,
    ) -> Result<AccessTokenRecord, McpError> {
        self.audit_required("审计未启用，令牌撤销拒绝")?;
        let record = self
            .get_access_token(id)?
            .ok_or_else(|| McpError::new("MCP_TOKEN_NOT_FOUND", json!({"token_id": id})))?;
        if !record.revoked_at.is_empty() {
            return Err(McpError::new(
                "MCP_TOKEN_ALREADY_REVOKED",
                json!({"token_id": id, "revoked_at": record.revoked_at}),
            ));
        }
        let now = now_iso();
        {
            let conn = self.db().lock().map_err(|_| McpError::lock())?;
            let changed = conn
                .execute(
                    "UPDATE access_tokens SET enabled=0, revoked_at=? WHERE id=? AND revoked_at=''",
                    rusqlite::params![now, id],
                )
                .map_err(|e| McpError::store(e.to_string()))?;
            if changed == 0 {
                // 并发撤销：另一请求已置 revoked_at，按 409 处理（不重复写审计）。
                return Err(McpError::new(
                    "MCP_TOKEN_ALREADY_REVOKED",
                    json!({"token_id": id}),
                ));
            }
        }
        let updated = self
            .get_access_token(id)?
            .ok_or_else(|| McpError::store("revoke_access_token 后记录缺失".to_string()))?;
        self.write_admin_audit(
            AdminChange::new("token", id, "token_revoke")
                .with_origin(actor.into_audit_origin())
                .with_data(json!({
                    "name": updated.name,
                    "role": updated.role,
                    "groups": updated.groups,
                    "source": updated.source,
                    "caller": updated.caller,
                    "token_prefix": updated.token_prefix,
                    "revoked_at": updated.revoked_at
                })),
        )?;
        Ok(updated)
    }

    /// 硬删除：只有 `revoked` / `expired` 可删；`active` 必须先撤销（409）。
    ///
    /// 撤销/过期记录留着不再有治理价值，删除让组引用计数与列表自然回落。
    /// 审计先判可用性：没有审计就不删行（治理面不能出现"删除无痕迹"）。
    /// 快照只含描述字段，**绝不写明文或 hash**。
    pub fn delete_access_token(
        &self,
        id: &str,
        actor: &crate::entry::Actor,
    ) -> Result<AccessTokenRecord, McpError> {
        self.audit_required("审计未启用，令牌删除拒绝")?;
        // 锁内只做 DELETE：状态判定用第一次读到的快照；并发下若已被他人删除，changed==0 按 404。
        // 不在同一持有期内跨多次 self.db() 取锁，避免自死锁。
        let record = self
            .get_access_token(id)?
            .ok_or_else(|| McpError::new("MCP_TOKEN_NOT_FOUND", json!({"token_id": id})))?;
        let status = record.status();
        if status != TOKEN_STATUS_REVOKED && status != TOKEN_STATUS_EXPIRED {
            return Err(McpError::new(
                "MCP_STATE_TRANSITION_INVALID",
                json!({"token_id": id, "status": status, "hint": "先撤销令牌再删除"}),
            ));
        }
        {
            let conn = self.db().lock().map_err(|_| McpError::lock())?;
            let changed = conn
                .execute("DELETE FROM access_tokens WHERE id=?", [id])
                .map_err(|e| McpError::store(e.to_string()))?;
            if changed == 0 {
                return Err(McpError::new(
                    "MCP_TOKEN_NOT_FOUND",
                    json!({"token_id": id}),
                ));
            }
        }
        self.write_admin_audit(
            AdminChange::new("token", id, "token_delete")
                .with_origin(actor.into_audit_origin())
                .with_data(json!({
                    "name": record.name,
                    "role": record.role,
                    "groups": record.groups,
                    "builtin_tools": record.builtin_tools,
                    "source": record.source,
                    "caller": record.caller,
                    "token_prefix": record.token_prefix,
                    "status": status,
                    "created_at": record.created_at,
                    "expires_at": null_if_empty(&record.expires_at),
                    "revoked_at": null_if_empty(&record.revoked_at)
                })),
        )?;
        Ok(record)
    }

    /// 令牌总数（含已撤销）：启动引导据此判断"表是否为空"。
    pub fn access_token_total(&self) -> Result<i64, McpError> {
        let conn = self.db().lock().map_err(|_| McpError::lock())?;
        conn.query_row("SELECT COUNT(*) FROM access_tokens", [], |r| r.get(0))
            .map_err(|e| McpError::store(e.to_string()))
    }

    /// 未撤销令牌数（组删除门禁与 options 计数共用同一口径）。
    pub fn token_count(&self) -> Result<i64, McpError> {
        let conn = self.db().lock().map_err(|_| McpError::lock())?;
        conn.query_row(
            "SELECT COUNT(*) FROM access_tokens WHERE revoked_at=''",
            [],
            |r| r.get(0),
        )
        .map_err(|e| McpError::store(e.to_string()))
    }

    /// 每个组被多少个未撤销 token 引用（列表页避免逐组 N+1）。
    pub fn token_counts_by_group(&self) -> Result<HashMap<String, i64>, McpError> {
        let conn = self.db().lock().map_err(|_| McpError::lock())?;
        let mut stmt = conn
            .prepare("SELECT groups_json FROM access_tokens WHERE revoked_at=''")
            .map_err(|e| McpError::store(e.to_string()))?;
        let mapped = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| McpError::store(e.to_string()))?;
        let mut out: HashMap<String, i64> = HashMap::new();
        for row in mapped {
            let raw = row.map_err(|e| McpError::store(e.to_string()))?;
            for group in decode_list(raw, 0).map_err(|e| McpError::store(e.to_string()))? {
                *out.entry(group).or_insert(0) += 1;
            }
        }
        Ok(out)
    }

    /// `GET /admin/groups` 的 `token_count`：单组查询（管理面逐组展示）。
    pub fn group_token_count(&self, name: &str) -> Result<i64, McpError> {
        let conn = self.db().lock().map_err(|_| McpError::lock())?;
        group_token_count_locked(&conn, name)
    }

    /// 启动引导：`access_tokens` 为空时把 `JUMCP_AGENT_TOKENS` 的静态 token 一次性导入。
    ///
    /// 导入的是同一明文的 sha256，因此原静态 token 仍可继续使用；`builtin_tools`
    /// 取全部内置 id（保持旧行为：常驻工具恒可见、admin 组令牌可见维护工具）。
    pub fn import_bootstrap_tokens(
        &self,
        principals: &[AgentPrincipal],
        policy: &TenantPolicy,
    ) -> Result<usize, McpError> {
        let mut imported = 0usize;
        let all_builtin: Vec<String> = BUILTIN_CAPABILITY_IDS
            .iter()
            .map(|id| (*id).to_string())
            .collect();
        for principal in principals {
            let hash = sha256_hex(&principal.token);
            {
                let conn = self.db().lock().map_err(|_| McpError::lock())?;
                let exists: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM access_tokens WHERE token_hash=?",
                        [&hash],
                        |r| r.get(0),
                    )
                    .map_err(|e| McpError::store(e.to_string()))?;
                if exists > 0 {
                    continue;
                }
            }
            let groups = policy
                .role(&principal.role)
                .map(|def| def.default_groups.clone())
                .unwrap_or_default();
            if groups.is_empty() {
                return Err(McpError::new(
                    "MCP_VALIDATION_ERROR",
                    json!({"field": "groups", "agent": principal.id, "hint": "角色没有默认组，无法导入引导令牌"}),
                ));
            }
            let mut stored = false;
            {
                let conn = self.db().lock().map_err(|_| McpError::lock())?;
                for _ in 0..5 {
                    let id = new_token_id();
                    let result = conn.execute(
                        &format!(
                            "INSERT INTO access_tokens ({COLUMNS}) VALUES (?1,?2,?3,?4,'default',?5,?6,?7,'bootstrap',?8,1,?9,'env','','','')"
                        ),
                        rusqlite::params![
                            id,
                            BOOTSTRAP_TOKEN_NAME,
                            hash,
                            display_prefix(&principal.token),
                            principal.role,
                            encode_list(&groups)?,
                            encode_list(&all_builtin)?,
                            principal.id,
                            now_iso()
                        ],
                    );
                    match result {
                        Ok(_) => {
                            stored = true;
                            break;
                        }
                        Err(rusqlite::Error::SqliteFailure(e, _))
                            if e.code == rusqlite::ErrorCode::ConstraintViolation =>
                        {
                            // 只可能是 id 碰撞（hash 已在上面排除）→ 换 id 重试。
                            continue;
                        }
                        Err(e) => return Err(McpError::store(e.to_string())),
                    }
                }
            }
            if stored {
                imported += 1;
            } else {
                return Err(McpError::store("引导令牌 ID 生成冲突".to_string()));
            }
        }
        Ok(imported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::Actor;
    use crate::registry::CapabilityStore;
    use serde_json::json;

    fn store() -> CapabilityStore {
        CapabilityStore::open(
            ":memory:",
            Some(std::sync::Arc::new(
                crate::audit::AuditLog::open(":memory:").unwrap(),
            )),
        )
        .unwrap()
    }

    fn actor() -> Actor {
        let mut actor = Actor::new("default", "admin");
        actor.actor_id = "alice".into();
        actor.source = "admin-api".into();
        actor
    }

    fn new_token(name: &str) -> NewAccessToken {
        NewAccessToken {
            name: name.into(),
            role: "analyst".into(),
            groups: vec!["db.read.core".into()],
            builtin_tools: vec!["search_capabilities".into()],
            source: "agent".into(),
            caller: "agent-analyst".into(),
            expires_at: None,
            tenant: "default".into(),
            created_by: "alice".into(),
        }
    }

    #[test]
    fn create_authenticate_list_revoke_roundtrip() {
        let s = store();
        let issued = s.create_access_token(&new_token("v1"), &actor()).unwrap();
        assert!(issued.token.starts_with("jumcp_"));
        assert_eq!(issued.token.len(), 6 + 64);
        assert_eq!(issued.record.id.len(), 20);
        assert!(issued.record.id.starts_with("tok_"));
        assert_eq!(issued.record.status(), "active");

        let authed = s.authenticate_access_token(&issued.token).unwrap().unwrap();
        assert_eq!(authed.id, issued.record.id);
        assert_eq!(authed.caller, "agent-analyst");
        assert!(
            !authed.last_used_at.is_empty(),
            "鉴权成功必须刷新 last_used_at"
        );

        let list = s.list_access_tokens().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(s.token_count().unwrap(), 1);
        assert_eq!(s.token_counts_by_group().unwrap()["db.read.core"], 1);

        let revoked = s.revoke_access_token(&issued.record.id, &actor()).unwrap();
        assert_eq!(revoked.status(), "revoked");
        assert!(!revoked.enabled);
        assert!(
            s.authenticate_access_token(&issued.token)
                .unwrap()
                .is_none()
        );
        assert_eq!(s.token_count().unwrap(), 0, "已撤销 token 不再计数");

        // 重复撤销：409 语义且不重复写审计。
        let e = s
            .revoke_access_token(&issued.record.id, &actor())
            .unwrap_err();
        assert_eq!(e.code, "MCP_TOKEN_ALREADY_REVOKED");
        let events = s
            .audit_log()
            .unwrap()
            .query(Some(&json!({"event_type": "admin_change"})))
            .unwrap();
        let revokes = events
            .iter()
            .filter(|e| e.payload["action"] == json!("token_revoke"))
            .count();
        assert_eq!(revokes, 1);
        let create = events
            .iter()
            .find(|e| e.payload["action"] == json!("token_create"))
            .expect("token_create");
        assert_eq!(create.actor["source"], json!("admin-api"));
        assert!(create.actor.get("token_id").is_none());
        // 审计载荷绝不含明文/hash。
        let raw = create.payload.to_string();
        assert!(!raw.contains(&issued.token));
        assert!(!raw.contains(&issued.record.token_hash));
        assert_eq!(
            create.payload["data"]["token_prefix"],
            json!(issued.record.token_prefix)
        );
    }

    #[test]
    fn reissue_rotates_secret_and_keeps_metadata() {
        let s = store();
        let mut spec = new_token("rotate-me");
        spec.expires_at = Some((Utc::now() + chrono::Duration::hours(1)).to_rfc3339());
        let issued = s.create_access_token(&spec, &actor()).unwrap();
        assert!(
            s.authenticate_access_token(&issued.token)
                .unwrap()
                .is_some()
        );

        let reissued = s.reissue_access_token(&issued.record.id, &actor()).unwrap();
        assert_ne!(reissued.token, issued.token);
        assert!(reissued.token.starts_with(TOKEN_PLAINTEXT_PREFIX));
        assert_eq!(reissued.record.id, issued.record.id);
        assert_eq!(reissued.record.name, "rotate-me");
        assert_eq!(reissued.record.role, "analyst");
        assert_eq!(reissued.record.groups, vec!["db.read.core".to_string()]);
        assert_eq!(
            reissued.record.builtin_tools,
            vec!["search_capabilities".to_string()]
        );
        assert_eq!(reissued.record.source, "agent");
        assert_eq!(reissued.record.caller, "agent-analyst");
        assert_eq!(reissued.record.expires_at, issued.record.expires_at);
        assert!(
            reissued.record.last_used_at.is_empty(),
            "重签发清空使用时间"
        );
        assert_ne!(reissued.record.token_hash, issued.record.token_hash);
        assert_ne!(reissued.record.token_prefix, issued.record.token_prefix);
        assert_eq!(s.access_token_total().unwrap(), 1, "重签发不新增行");

        assert!(
            s.authenticate_access_token(&issued.token)
                .unwrap()
                .is_none(),
            "旧明文立即失效"
        );
        assert!(
            s.authenticate_access_token(&reissued.token)
                .unwrap()
                .is_some(),
            "新明文立即可用"
        );

        let events = s
            .audit_log()
            .unwrap()
            .query(Some(&json!({"event_type": "admin_change"})))
            .unwrap();
        let reissue = events
            .iter()
            .find(|e| e.payload["action"] == json!("token_reissue"))
            .expect("token_reissue 审计");
        assert_eq!(reissue.actor["source"], json!("admin-api"));
        let raw = reissue.payload.to_string();
        assert!(!raw.contains(&issued.token));
        assert!(!raw.contains(&reissued.token));
        assert!(!raw.contains(&reissued.record.token_hash));
        assert_eq!(
            reissue.payload["data"]["token_prefix"],
            json!(reissued.record.token_prefix)
        );

        // 撤销后不可重签：409 且哈希/前缀不变。
        let revoked = s.revoke_access_token(&issued.record.id, &actor()).unwrap();
        let before = (revoked.token_hash.clone(), revoked.token_prefix.clone());
        let e = s
            .reissue_access_token(&issued.record.id, &actor())
            .unwrap_err();
        assert_eq!(e.code, "MCP_STATE_TRANSITION_INVALID");
        let after = s.get_access_token(&issued.record.id).unwrap().unwrap();
        assert_eq!((after.token_hash, after.token_prefix), before);
    }

    #[test]
    fn expired_token_is_rejected() {
        let s = store();
        let mut spec = new_token("expiring");
        spec.expires_at = Some((Utc::now() + chrono::Duration::seconds(2)).to_rfc3339());
        let issued = s.create_access_token(&spec, &actor()).unwrap();
        assert_eq!(issued.record.status(), "active");
        // 直接改库模拟时间流逝（store 不提供改过期时间的 API）。
        {
            let conn = s.db().lock().unwrap();
            conn.execute(
                "UPDATE access_tokens SET expires_at=? WHERE id=?",
                rusqlite::params![
                    (Utc::now() - chrono::Duration::seconds(1)).to_rfc3339(),
                    issued.record.id
                ],
            )
            .unwrap();
        }
        assert!(
            s.authenticate_access_token(&issued.token)
                .unwrap()
                .is_none()
        );
        let rec = s.get_access_token(&issued.record.id).unwrap().unwrap();
        assert_eq!(rec.status(), "expired");
        assert_eq!(s.token_count().unwrap(), 1, "过期未撤销仍计入组引用");
    }

    #[test]
    fn bootstrap_imports_only_once_and_tokens_still_work() {
        let s = store();
        let principals = crate::config::AgentTokens::parse(
            "agent-analyst:analyst:tok_agent_analyst_0001,agent-admin:admin:tok_agent_admin_000001",
        )
        .unwrap();
        let mut roles = std::collections::HashMap::new();
        roles.insert(
            "analyst".into(),
            crate::config::RoleDef {
                name: "analyst".into(),
                default_groups: vec!["db.read.core".into()],
                default_budget: 2000,
            },
        );
        roles.insert(
            "admin".into(),
            crate::config::RoleDef {
                name: "admin".into(),
                default_groups: vec!["admin".into()],
                default_budget: 2000,
            },
        );
        let policy = TenantPolicy {
            roles,
            ..Default::default()
        };
        assert_eq!(
            s.import_bootstrap_tokens(principals.principals(), &policy)
                .unwrap(),
            2
        );
        assert_eq!(
            s.import_bootstrap_tokens(principals.principals(), &policy)
                .unwrap(),
            0,
            "重复启动不得重复导入"
        );
        let rec = s
            .authenticate_access_token("tok_agent_admin_000001")
            .unwrap()
            .unwrap();
        assert_eq!(rec.role, "admin");
        assert_eq!(rec.source, "bootstrap");
        assert_eq!(rec.groups, vec!["admin".to_string()]);
        assert!(
            rec.builtin_tools
                .contains(&"admin_review_queue".to_string()),
            "引导令牌保留内置工具全量，admin 组仍受双重判定"
        );
    }

    #[test]
    fn validation_rejects_bad_fields_and_admin_tools_without_admin_group() {
        type Mutation = Box<dyn Fn(&mut NewAccessToken)>;
        let s = store();
        let cases: Vec<Mutation> = vec![
            Box::new(|t| t.name = "".into()),
            Box::new(|t| t.name = "x".repeat(65)),
            Box::new(|t| t.caller = "BAD".into()),
            Box::new(|t| t.caller = "a".into()),
            Box::new(|t| t.source = "".into()),
            Box::new(|t| t.source = "Agent".into()),
            Box::new(|t| t.groups.clear()),
            Box::new(|t| t.builtin_tools = vec!["not_a_tool".into()]),
            Box::new(|t| t.builtin_tools = vec!["admin_review_queue".into()]),
            Box::new(|t| t.expires_at = Some("not-a-time".into())),
            Box::new(|t| t.expires_at = Some("2020-01-01T00:00:00Z".into())),
        ];
        for mutate in cases {
            let mut spec = new_token("bad");
            mutate(&mut spec);
            let e = spec.validate().unwrap_err();
            assert_eq!(e.code, "MCP_VALIDATION_ERROR", "{spec:?}");
        }
        // 有 admin 组时 admin_* 合法。
        let mut ok = new_token("admin-tool");
        ok.groups = vec!["admin".into()];
        ok.builtin_tools = vec!["admin_review_queue".into()];
        ok.validate().unwrap();
        // 存储层也会再判一次（不依赖 HTTP 层）。
        let e = s
            .create_access_token(
                &{
                    let mut bad = new_token("bad");
                    bad.builtin_tools = vec!["admin_review_queue".into()];
                    bad
                },
                &actor(),
            )
            .unwrap_err();
        assert_eq!(e.code, "MCP_VALIDATION_ERROR");
    }

    #[test]
    fn create_without_audit_is_rejected_and_persists_nothing() {
        let s = CapabilityStore::open(":memory:", None).unwrap();
        let e = s
            .create_access_token(&new_token("noaudit"), &actor())
            .unwrap_err();
        assert_eq!(e.code, "MCP_AUDIT_ERROR");
        assert_eq!(s.access_token_total().unwrap(), 0);
    }
}
