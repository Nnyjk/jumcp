//! capability 存储：双布尔 + 版本不可原地改 + definition_hash + 审批留痕 + deprecated 带 successor。
//!
//! 为什么已发布定义的编辑必须开新 version：历史版本要可回放，`capability_versions` 只追加，
//! 触发器挡住定义列的任何 UPDATE。本模块只管 schema、连接与内置 id 写保护；记录/审计投影、
//! 生命周期、管理组与上游注册表见同目录兄弟模块。

use crate::audit::AuditLog;
use crate::errors::McpError;
use rusqlite::Connection;
use std::sync::Mutex;

mod admins;
mod groups;
mod lifecycle;
mod record;
mod sessions;
mod upstreams;

pub use admins::{
    ADMIN_PASSWORD_MIN_CHARS, ADMIN_STATE_ACTIVE, ADMIN_STATE_DISABLED, ADMIN_USERNAME_MAX,
    ADMIN_USERNAME_MIN, AdminUser, BOOTSTRAP_ADMIN_REQUIRED, normalize_admin_username,
    validate_admin_password,
};
pub use groups::ManagedGroup;
pub use record::{
    AdminChange, CapabilityRecord, CapabilityVersion, change, definition_changes, field_changes,
};
pub use sessions::{ADMIN_SESSION_ID_BYTES, AdminSession};
pub use upstreams::{
    EXPOSED_TOOL_NAME_MAX, McpUpstream, RESERVED_UPSTREAM_IDS, TOOL_WILDCARD_ALL,
    UPSTREAM_CATEGORIES, UPSTREAM_ID_MAX_LEN, UPSTREAM_MAX_ITEMS, UPSTREAM_NAME_SEP,
    UPSTREAM_TIMEOUT_MAX_MS, UPSTREAM_TOOL_GROUP_MAX, UPSTREAM_TRANSPORTS, UpstreamTool,
    UpstreamToolGroup, is_tool_allowlist_entry, is_upstream_tool_name, is_valid_upstream_id,
    tool_allowlist_matches,
};

#[cfg(test)]
mod tests;

pub const STATUSES: &[&str] = &["draft", "review", "published", "deprecated"];
pub const KINDS: &[&str] = &["tool", "template", "endpoint"];
/// 能力风险闭集：工具对数据源只读，不存在 write/admin。
pub const RISKS: &[&str] = &["read", "sensitive"];

/// 内置常驻工具的能力 id：定义只存在于代码里，注册表只能读取、不能写入。
///
/// 单一来源：`jumcp-server` 的 `RESIDENT_TOOL_NAMES` 直接引用本常量，
/// 因此"哪些 id 是内置"在发现面与存储层是同一份列表（重复写两份字符串必然漂移）。
pub const RESIDENT_CAPABILITY_IDS: &[&str] = &[
    "search_capabilities",
    "describe_capability",
    "list_groups",
    "db_run_template",
];

/// 内置维护工具的能力 id：`group="admin"`，只有 scope.groups 含 admin 时才可见可调。
///
/// 与常驻工具同属 builtin（写保护一致），但**不恒可见**：维护通道不能泄露给普通 agent。
pub const ADMIN_CAPABILITY_IDS: &[&str] = &[
    "admin_register_template",
    "admin_review_queue",
    "admin_audit_queries",
    "admin_tool_candidates",
    "admin_set_visibility",
    "admin_publish_capability",
];

/// 全部内置 id：常驻 ∪ 维护。
///
/// 这是注册表写保护的唯一集合（`ensure_mutable`），也是"哪些 id 归代码所有"的完整答案；
/// 两个子集必须与它一致（`builtin_ids_partition_matches_subsets` 单测守着，防止三处漂移）。
pub const BUILTIN_CAPABILITY_IDS: &[&str] = &[
    "search_capabilities",
    "describe_capability",
    "list_groups",
    "db_run_template",
    "admin_register_template",
    "admin_review_queue",
    "admin_audit_queries",
    "admin_tool_candidates",
    "admin_set_visibility",
    "admin_publish_capability",
];

pub fn is_builtin_id(id: &str) -> bool {
    BUILTIN_CAPABILITY_IDS.contains(&id)
}

/// 发现面谓词（可见 = 能被检索到 / 被平铺）。
const VISIBLE_WHERE: &str = "status='published' AND discoverable=1";
/// 调用面谓词（可调用 ≠ 可见：`discoverable=false, callable=true` 是合法的系统直调状态）。
const CALLABLE_WHERE: &str = "status='published' AND callable=1";

pub struct CapabilityStore {
    conn: Mutex<Connection>,
    audit: Option<std::sync::Arc<AuditLog>>,
}

impl CapabilityStore {
    pub fn open(path: &str, audit: Option<std::sync::Arc<AuditLog>>) -> Result<Self, McpError> {
        if path != ":memory:"
            && let Some(parent) = std::path::Path::new(path).parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| McpError::store(e.to_string()))?;
        }
        let conn = Connection::open(path).map_err(|e| McpError::store(e.to_string()))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
            .map_err(|e| McpError::store(e.to_string()))?;
        conn.execute_batch(r#"
CREATE TABLE IF NOT EXISTS capabilities (
    capability_id TEXT PRIMARY KEY, kind TEXT NOT NULL, group_name TEXT NOT NULL, risk TEXT NOT NULL,
    status TEXT NOT NULL, discoverable INTEGER NOT NULL DEFAULT 0, callable INTEGER NOT NULL DEFAULT 0,
    version INTEGER NOT NULL DEFAULT 1, definition_json TEXT NOT NULL, definition_hash TEXT NOT NULL, successor_id TEXT NOT NULL DEFAULT '',
    updated_at TEXT NOT NULL DEFAULT '', submitted_by TEXT NOT NULL DEFAULT '', submitted_at TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_cap_status_group ON capabilities(status, group_name);
CREATE TABLE IF NOT EXISTS capability_versions (
    capability_id TEXT NOT NULL, version INTEGER NOT NULL, definition_json TEXT NOT NULL,
    definition_hash TEXT NOT NULL, approval_id TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT '', submitted_by TEXT NOT NULL DEFAULT '',
    submitted_at TEXT NOT NULL DEFAULT '', approved_by TEXT NOT NULL DEFAULT '',
    approved_at TEXT NOT NULL DEFAULT '', status TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (capability_id, version)
);
CREATE TRIGGER IF NOT EXISTS trg_ver_no_update BEFORE UPDATE OF definition_json, definition_hash, capability_id, version ON capability_versions BEGIN SELECT RAISE(ABORT, 'capability_versions definitions are append-only'); END;
CREATE TRIGGER IF NOT EXISTS trg_ver_no_delete BEFORE DELETE ON capability_versions BEGIN SELECT RAISE(ABORT, 'capability_versions is append-only'); END;
CREATE TABLE IF NOT EXISTS groups (
    name TEXT PRIMARY KEY, display_name TEXT NOT NULL DEFAULT '',
    priority INTEGER NOT NULL DEFAULT 0, min_budget INTEGER NOT NULL DEFAULT 800,
    roles_json TEXT NOT NULL DEFAULT '[]', updated_at TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS mcp_upstreams (
    id TEXT PRIMARY KEY, name TEXT NOT NULL, transport TEXT NOT NULL,
    url TEXT NOT NULL DEFAULT '', command TEXT NOT NULL DEFAULT '',
    args_json TEXT NOT NULL DEFAULT '[]', env_json TEXT NOT NULL DEFAULT '{}',
    headers_json TEXT NOT NULL DEFAULT '{}', group_code TEXT NOT NULL,
    category TEXT NOT NULL DEFAULT 'custom',
    enabled INTEGER NOT NULL DEFAULT 1, timeout_ms INTEGER NOT NULL DEFAULT 10000,
    created_at TEXT NOT NULL DEFAULT '', updated_at TEXT NOT NULL DEFAULT '',
    catalog_id TEXT NOT NULL DEFAULT '', catalog_version TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_upstream_group ON mcp_upstreams(group_code);
-- 逐工具组覆盖：只存显式覆盖（无行 = 继承 mcp_upstreams.group_code），目前只允许 basic。
CREATE TABLE IF NOT EXISTS mcp_upstream_tool_groups (
    upstream_id TEXT NOT NULL, tool_name TEXT NOT NULL, group_code TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT '', PRIMARY KEY (upstream_id, tool_name)
);
-- 平台管理员账号：管理台登录的唯一身份来源；口令只以 Argon2id 哈希落库。
CREATE TABLE IF NOT EXISTS admin_users (
    id TEXT PRIMARY KEY, username TEXT NOT NULL COLLATE NOCASE UNIQUE,
    password_hash TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'active',
    created_at TEXT NOT NULL, updated_at TEXT NOT NULL, last_login_at TEXT
);
-- 管理台会话：cookie 值即 id（32 字节 CSPRNG hex），admin_user_id 指回管理员账号。
CREATE TABLE IF NOT EXISTS admin_sessions (
    id TEXT PRIMARY KEY, admin_user_id TEXT NOT NULL, created_at INTEGER,
    expires_at INTEGER, last_seen_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_admin_sessions_expires ON admin_sessions(expires_at);
"#).map_err(|e| McpError::store(e.to_string()))?;
        rebuild_admin_sessions_if_stale(&conn)?;
        // 已有库补列：CREATE TABLE IF NOT EXISTS 不会为已存在的表加列。
        for (table, column) in [
            ("capabilities", "updated_at"),
            ("capabilities", "submitted_by"),
            ("capabilities", "submitted_at"),
            ("capability_versions", "created_at"),
            ("capability_versions", "submitted_by"),
            ("capability_versions", "submitted_at"),
            ("capability_versions", "approved_by"),
            ("capability_versions", "approved_at"),
            ("capability_versions", "status"),
            // 管理组：`groups.name` 存组 code，展示名单独一列。
            ("groups", "display_name"),
            // 内置包安装出来的上游带包坐标；空串 = 手工创建的上游。
            ("mcp_upstreams", "catalog_id"),
            ("mcp_upstreams", "catalog_version"),
        ] {
            ensure_column(&conn, table, column)?;
        }
        // 定义列仍不可原地改；生命周期/审批元数据允许更新（submit/publish 要在版本行上盖章）。
        // 已有库的 trigger 可能屏蔽一切 UPDATE，必须重建；这是 DDL，不改任何历史行。
        conn.execute_batch(
            r#"DROP TRIGGER IF EXISTS trg_ver_no_update;
CREATE TRIGGER trg_ver_no_update BEFORE UPDATE OF definition_json, definition_hash, capability_id, version ON capability_versions BEGIN SELECT RAISE(ABORT, 'capability_versions definitions are append-only'); END;"#,
        )
        .map_err(|e| McpError::store(e.to_string()))?;
        crate::access::ensure_schema(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            audit,
        })
    }

    /// store 内部连接（access token 模块与组引用计数共用同一把锁/同一个库）。
    pub(crate) fn db(&self) -> &Mutex<Connection> {
        &self.conn
    }

    pub(crate) fn audit_available(&self) -> bool {
        self.audit.is_some()
    }

    #[cfg(test)]
    pub(crate) fn audit_log(&self) -> Option<&std::sync::Arc<AuditLog>> {
        self.audit.as_ref()
    }
}

pub(crate) fn ensure_column(conn: &Connection, table: &str, column: &str) -> Result<(), McpError> {
    if table_has_column(conn, table, column)? {
        return Ok(());
    }
    conn.execute(
        &format!("ALTER TABLE {table} ADD COLUMN {column} TEXT NOT NULL DEFAULT ''"),
        [],
    )
    .map_err(|e| McpError::store(e.to_string()))?;
    Ok(())
}

fn table_has_column(conn: &Connection, table: &str, column: &str) -> Result<bool, McpError> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|e| McpError::store(e.to_string()))?;
    let names = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(|e| McpError::store(e.to_string()))?;
    for name in names {
        if name.map_err(|e| McpError::store(e.to_string()))? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

/// 旧库的 `admin_sessions` 用 `token_id` 指回已删除的共享令牌；会话都是临时数据，
/// 直接整表重建（旧 cookie 一律失效，浏览器重新登录即可）。
/// 这是本库唯一被重建的表——审计、能力、令牌、上游一律不动。
fn rebuild_admin_sessions_if_stale(conn: &Connection) -> Result<(), McpError> {
    if table_has_column(conn, "admin_sessions", "admin_user_id")? {
        return Ok(());
    }
    conn.execute_batch(
        r#"DROP TABLE IF EXISTS admin_sessions;
CREATE TABLE admin_sessions (
    id TEXT PRIMARY KEY, admin_user_id TEXT NOT NULL, created_at INTEGER,
    expires_at INTEGER, last_seen_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_admin_sessions_expires ON admin_sessions(expires_at);"#,
    )
    .map_err(|e| McpError::store(e.to_string()))
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}
