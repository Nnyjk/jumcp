//! capability 存储：双布尔 + 版本不可原地改 + definition_hash + 审批留痕 + deprecated 带 successor。
//! 为什么已有发布定义走新 version：编辑即新版本（§4.4.4），capability_versions append-only。

use crate::audit::{ActorRef, AuditEvent, AuditLog, IntoAuditOrigin};
use crate::canonical::{canonical_json, definition_hash};
use crate::config::{is_reserved_group_name, validate_group_name};
use crate::errors::McpError;
use rusqlite::{Connection, Row};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Mutex;

/// 组变更审计的字段集：只写真实变化的字段。
const GROUP_CHANGE_FIELDS: &[&str] = &["name", "priority", "min_budget", "roles"];

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

pub const STATUSES: &[&str] = &["draft", "review", "published", "deprecated"];
pub const KINDS: &[&str] = &["tool", "template", "endpoint"];
/// 能力风险闭集：工具对数据源只读，不存在 write/admin（§1.3）。
pub const RISKS: &[&str] = &["read", "sensitive"];

/// 内置常驻工具的能力 id：定义只存在于代码里，注册表只能读取、不能写入。
///
/// 单一来源：`jumcp-server` 的 `RESIDENT_TOOL_NAMES` 直接引用本常量，
/// 因此"哪些 id 是内置"在发现面与存储层是同一份列表（重复写两份字符串必然漂移）。
pub const RESIDENT_CAPABILITY_IDS: &[&str] = &[
    "search_capabilities",
    "describe_capability",
    "list_groups",
    "db_query_readonly",
    "db_run_template",
];

/// 内置维护工具的能力 id：`group="admin"`，只有 scope.groups 含 admin 时才可见可调。
///
/// 与常驻工具同属 builtin（写保护一致），但**不恒可见**：维护通道不能泄露给普通 agent。
pub const ADMIN_CAPABILITY_IDS: &[&str] = &[
    "admin_register_template",
    "admin_review_queue",
    "admin_audit_queries",
    "admin_set_visibility",
];

/// 全部内置 id：常驻 ∪ 维护。
///
/// 这是注册表写保护的唯一集合（`ensure_mutable`），也是"哪些 id 归代码所有"的完整答案；
/// 两个子集必须与它一致（`builtin_ids_partition_matches_subsets` 单测守着，防止三处漂移）。
pub const BUILTIN_CAPABILITY_IDS: &[&str] = &[
    "search_capabilities",
    "describe_capability",
    "list_groups",
    "db_query_readonly",
    "db_run_template",
    "admin_register_template",
    "admin_review_queue",
    "admin_audit_queries",
    "admin_set_visibility",
];

pub fn is_builtin_id(id: &str) -> bool {
    BUILTIN_CAPABILITY_IDS.contains(&id)
}

/// 发现面谓词（可见 = 能被检索到 / 被平铺）。
const VISIBLE_WHERE: &str = "status='published' AND discoverable=1";
/// 调用面谓词（可调用 ≠ 可见：`discoverable=false, callable=true` 是合法的系统直调状态）。
const CALLABLE_WHERE: &str = "status='published' AND callable=1";

/// 内置 id 的写保护：维护通道不能注册/发布/下架/改可见性，否则会影子覆盖内置工具定义。
fn ensure_mutable(id: &str) -> Result<(), McpError> {
    if is_builtin_id(id) {
        return Err(McpError::new(
            "MCP_BUILTIN_IMMUTABLE",
            json!({"capability_id": id}),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct CapabilityRecord {
    pub capability_id: String,
    pub kind: String,
    pub group: String,
    pub risk: String,
    pub status: String,
    pub discoverable: bool,
    pub callable: bool,
    pub version: i64,
    pub definition: Value,
    pub definition_hash: String,
    pub successor_id: String,
    /// 最近一次状态/定义/可见性变更时间（ISO8601）；审核队列据此排序。
    pub updated_at: String,
    /// 提审人身份：由鉴权派生，不来自请求头自述。
    pub submitted_by: String,
    /// 最近一次提审时间；退回后清空。
    pub submitted_at: String,
}

/// `capability_versions` 的一行：定义 append-only，生命周期/审批元数据可盖章。
#[derive(Debug, Clone)]
pub struct CapabilityVersion {
    pub version: i64,
    pub definition: Value,
    pub definition_hash: String,
    pub status: String,
    pub created_at: String,
    pub submitted_by: String,
    pub submitted_at: String,
    pub approved_by: String,
    pub approved_at: String,
    pub approval_id: String,
}

/// 一条管理变更的结构化描述；`write_admin_audit` 只接受它，避免调用点各拼一份 JSON。
#[derive(Debug, Clone, Default)]
pub struct AdminChange {
    pub actor: String,
    /// 来源（`admin-api` / MCP token 的 source）；空时审计层默认 `admin-api`。
    pub actor_source: Option<String>,
    /// 派生该变更的数据库 token id（MCP token 事件）。
    pub actor_token_id: Option<String>,
    pub object_type: String,
    pub object_id: String,
    pub action: String,
    pub outcome: String,
    pub from_status: Option<String>,
    pub to_status: Option<String>,
    pub changes: Vec<Value>,
    pub approval: Option<Value>,
    pub reason: Option<String>,
    pub data: Value,
}

impl AdminChange {
    pub fn new(object_type: &str, object_id: &str, action: &str) -> Self {
        Self {
            object_type: object_type.into(),
            object_id: object_id.into(),
            action: action.into(),
            outcome: "ok".into(),
            data: json!({}),
            ..Default::default()
        }
    }

    pub fn with_actor(mut self, actor: &str) -> Self {
        self.actor = actor.into();
        self
    }
    /// 由 `Actor`/`&str` 统一带出来源与 token_id（审计里可分辨"谁、从哪、用哪个 token"）。
    pub fn with_origin(mut self, origin: crate::audit::AuditOrigin) -> Self {
        self.actor = origin.id;
        self.actor_source = origin.source;
        self.actor_token_id = origin.token_id;
        self
    }
    pub fn with_status(mut self, from: Option<&str>, to: Option<&str>) -> Self {
        self.from_status = from.map(String::from);
        self.to_status = to.map(String::from);
        self
    }
    pub fn with_changes(mut self, changes: Vec<Value>) -> Self {
        self.changes = changes;
        self
    }
    pub fn with_approval(mut self, approval: Option<Value>) -> Self {
        self.approval = approval;
        self
    }
    pub fn with_reason(mut self, reason: &str) -> Self {
        self.reason = Some(reason.into());
        self
    }
    pub fn with_version(mut self, version: i64) -> Self {
        if let Some(obj) = self.data.as_object_mut() {
            obj.insert("version".into(), json!(version));
        }
        self
    }
    pub fn with_data(mut self, data: Value) -> Self {
        if let (Some(target), Some(extra)) = (self.data.as_object_mut(), data.as_object()) {
            for (k, v) in extra {
                target.insert(k.clone(), v.clone());
            }
        } else {
            self.data = data;
        }
        self
    }

    fn into_payload(self) -> Value {
        let mut map = crate::audit::payload(
            &self.object_type,
            &self.object_id,
            &self.action,
            &self.outcome,
        );
        map.insert(
            "from_status".into(),
            self.from_status.map_or(Value::Null, Value::String),
        );
        map.insert(
            "to_status".into(),
            self.to_status.map_or(Value::Null, Value::String),
        );
        map.insert("changes".into(), Value::Array(self.changes));
        map.insert("approval".into(), self.approval.unwrap_or(Value::Null));
        map.insert(
            "reason".into(),
            self.reason.map_or(Value::Null, Value::String),
        );
        map.insert("data".into(), self.data);
        Value::Object(map)
    }
}

/// 单个字段的真实前后值。
pub fn change(field: &str, before: Value, after: Value) -> Value {
    json!({"field": field, "before": before, "after": after})
}

/// 定义变更的嵌套展开深度上限：超过该深度的子树作为一个字段整体输出，避免异常深定义爆炸。
const DEFINITION_CHANGE_MAX_DEPTH: usize = 6;

/// 能力定义的真实前后值，递归展开对象为点路径（如 `template.description`）。
///
/// 数组/标量是叶子，输出完整 before/after；新增字段 before=null，删除字段 after=null。
/// 组装路径超过 `DEFINITION_CHANGE_MAX_DEPTH` 段时，该子树整体作为一行。
pub fn definition_changes(old: &Value, new: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    walk_definition_changes(old, new, "", 0, &mut out);
    out
}

fn walk_definition_changes(
    before: &Value,
    after: &Value,
    path: &str,
    depth: usize,
    out: &mut Vec<Value>,
) {
    if before == after {
        return;
    }
    if depth >= DEFINITION_CHANGE_MAX_DEPTH {
        out.push(change(root_path(path), before.clone(), after.clone()));
        return;
    }
    // 缺失的一侧按空对象处理：新增/删除的子树也要逐字段展开（before/after 分别为 null）。
    let empty = serde_json::Map::new();
    let pair = match (before.as_object(), after.as_object()) {
        (Some(b), Some(a)) => Some((b, a)),
        (Some(b), None) if after.is_null() => Some((b, &empty)),
        (None, Some(a)) if before.is_null() => Some((&empty, a)),
        _ => None,
    };
    let Some((before_obj, after_obj)) = pair else {
        out.push(change(root_path(path), before.clone(), after.clone()));
        return;
    };
    let mut keys: Vec<&String> = before_obj.keys().chain(after_obj.keys()).collect();
    keys.sort();
    keys.dedup();
    for key in keys {
        let child_path = if path.is_empty() {
            key.clone()
        } else {
            format!("{path}.{key}")
        };
        let before_child = before_obj.get(key).unwrap_or(&Value::Null);
        let after_child = after_obj.get(key).unwrap_or(&Value::Null);
        walk_definition_changes(before_child, after_child, &child_path, depth + 1, out);
    }
}

fn root_path(path: &str) -> &str {
    if path.is_empty() { "<root>" } else { path }
}

/// 指定字段的真实前后值（组变更等固定字段集）。
pub fn field_changes(old: &Value, new: &Value, fields: &[&str]) -> Vec<Value> {
    fields
        .iter()
        .filter_map(|f| {
            let before = old.get(*f).cloned().unwrap_or(Value::Null);
            let after = new.get(*f).cloned().unwrap_or(Value::Null);
            (before != after).then(|| change(f, before, after))
        })
        .collect()
}

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
    category TEXT NOT NULL DEFAULT 'custom', read_only INTEGER NOT NULL DEFAULT 1,
    enabled INTEGER NOT NULL DEFAULT 1, timeout_ms INTEGER NOT NULL DEFAULT 10000,
    created_at TEXT NOT NULL DEFAULT '', updated_at TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_upstream_group ON mcp_upstreams(group_code);
"#).map_err(|e| McpError::store(e.to_string()))?;
        // 旧库补列：CREATE TABLE IF NOT EXISTS 不会为已存在的表加列。
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
            // 管理组：旧库 `groups.name` 存的是组 code，新展示名单独一列。
            ("groups", "display_name"),
        ] {
            ensure_column(&conn, table, column)?;
        }
        // 定义列仍不可原地改；生命周期/审批元数据允许更新（submit/publish 要在版本行上盖章）。
        // 旧库的 trigger 屏蔽一切 UPDATE，必须重建；这是 DDL，不改任何历史行。
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

    pub fn get(&self, id: &str) -> Result<Option<CapabilityRecord>, McpError> {
        let conn = self.conn.lock().map_err(|_| McpError::lock())?;
        let mut stmt = conn
            .prepare("SELECT * FROM capabilities WHERE capability_id=?")
            .map_err(|e| McpError::store(e.to_string()))?;
        let mapped = stmt
            .query_map([id], row_to_record)
            .map_err(|e| McpError::store(e.to_string()))?;
        let mut rows: Vec<CapabilityRecord> = Vec::new();
        for row in mapped {
            let rec = row.map_err(|e| McpError::store(e.to_string()))?;
            rows.push(rec);
        }
        Ok(rows.into_iter().next())
    }

    pub fn get_version(
        &self,
        id: &str,
        version: i64,
    ) -> Result<Option<CapabilityRecord>, McpError> {
        let conn = self.conn.lock().map_err(|_| McpError::lock())?;
        let mut stmt = conn.prepare("SELECT c.*, v.definition_json AS vdef, v.definition_hash AS vhash FROM capabilities c LEFT JOIN capability_versions v ON v.capability_id=c.capability_id AND v.version=? WHERE c.capability_id=?").map_err(|e| McpError::store(e.to_string()))?;
        let mapped = stmt
            .query_map(rusqlite::params![version, id], |r| {
                let def: Option<String> = r.get("vdef")?;
                let h: Option<String> = r.get("vhash")?;
                Ok((row_to_record(r)?, def, h))
            })
            .map_err(|e| McpError::store(e.to_string()))?;
        let mut rows: Vec<(CapabilityRecord, Option<String>, Option<String>)> = Vec::new();
        for row in mapped {
            let item = row.map_err(|e| McpError::store(e.to_string()))?;
            rows.push(item);
        }
        let (rec, def, h) = match rows.into_iter().next() {
            Some(t) => t,
            None => return Ok(None),
        };
        let def = match def {
            Some(d) => d,
            None => return Ok(None),
        };
        let definition: Value = serde_json::from_str(&def)
            .map_err(|e| McpError::store(format!("definition json decode failed: {e}")))?;
        let definition_hash_value: String = h.unwrap_or_default();
        Ok(Some(CapabilityRecord {
            definition,
            definition_hash: definition_hash_value,
            version,
            ..rec
        }))
    }

    /// 版本历史（升序）。历史行可能来自旧库：空字符串就是"未知"，由 API 层输出 null。
    pub fn versions(&self, id: &str) -> Result<Vec<CapabilityVersion>, McpError> {
        let conn = self.conn.lock().map_err(|_| McpError::lock())?;
        let mut stmt = conn
            .prepare(
                "SELECT version, definition_json, definition_hash, status, created_at, submitted_by, submitted_at, approved_by, approved_at, approval_id FROM capability_versions WHERE capability_id=? ORDER BY version ASC",
            )
            .map_err(|e| McpError::store(e.to_string()))?;
        let mapped = stmt
            .query_map([id], |r| {
                let raw: String = r.get(1)?;
                let definition = serde_json::from_str(&raw).map_err(|_| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Text,
                        Box::new(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "version definition json decode failed",
                        )),
                    )
                })?;
                Ok(CapabilityVersion {
                    version: r.get(0)?,
                    definition,
                    definition_hash: r.get(2)?,
                    status: r.get(3)?,
                    created_at: r.get(4)?,
                    submitted_by: r.get(5)?,
                    submitted_at: r.get(6)?,
                    approved_by: r.get(7)?,
                    approved_at: r.get(8)?,
                    approval_id: r.get(9)?,
                })
            })
            .map_err(|e| McpError::store(e.to_string()))?;
        let mut rows = Vec::new();
        for row in mapped {
            rows.push(row.map_err(|e| McpError::store(e.to_string()))?);
        }
        Ok(rows)
    }

    /// 最新版本的创建时间（详情页 `definition_updated_at`）；没有版本行时为 None。
    pub fn latest_version_created_at(&self, id: &str) -> Result<Option<String>, McpError> {
        let conn = self.conn.lock().map_err(|_| McpError::lock())?;
        let mut stmt = conn
            .prepare("SELECT created_at FROM capability_versions WHERE capability_id=? ORDER BY version DESC LIMIT 1")
            .map_err(|e| McpError::store(e.to_string()))?;
        let mut rows = stmt
            .query_map([id], |r| r.get::<_, String>(0))
            .map_err(|e| McpError::store(e.to_string()))?;
        match rows.next() {
            Some(row) => Ok(Some(row.map_err(|e| McpError::store(e.to_string()))?)),
            None => Ok(None),
        }
    }

    pub fn all(&self) -> Result<Vec<CapabilityRecord>, McpError> {
        let conn = self.conn.lock().map_err(|_| McpError::lock())?;
        let mut stmt = conn
            .prepare("SELECT * FROM capabilities ORDER BY capability_id")
            .map_err(|e| McpError::store(e.to_string()))?;
        let mapped = stmt
            .query_map([], row_to_record)
            .map_err(|e| McpError::store(e.to_string()))?;
        let mut rows: Vec<CapabilityRecord> = Vec::new();
        for row in mapped {
            let rec = row.map_err(|e| McpError::store(e.to_string()))?;
            rows.push(rec);
        }
        Ok(rows)
    }

    /// 可见行：published ∧ discoverable ∧ group ∈ scope。
    ///
    /// 发现面每个请求都实时读这里（SQLite 本地读），所以管理面 publish / 改可见性后
    /// 下一个请求立刻生效——不需要任何缓存失效机制。
    pub fn visible(&self, groups: &[String]) -> Result<Vec<CapabilityRecord>, McpError> {
        self.rows_where(VISIBLE_WHERE, groups)
    }

    /// 可调用行：published ∧ callable ∧ group ∈ scope。
    ///
    /// 与 `visible` 是两条不同的谓词（§4.4.1：`discoverable=false, callable=true` 是合法的
    /// "系统直调"状态）——可见性只决定谁能看见，不决定能否调用。两条谓词共用
    /// `rows_where` 里的同一处 SQL 拼装，不会各自漂移。
    pub fn callable(&self, groups: &[String]) -> Result<Vec<CapabilityRecord>, McpError> {
        self.rows_where(CALLABLE_WHERE, groups)
    }

    /// 唯一的行查询实现：谓词由调用点传入常量，SQL 只在这里拼一次。
    fn rows_where(
        &self,
        predicate: &str,
        groups: &[String],
    ) -> Result<Vec<CapabilityRecord>, McpError> {
        if groups.is_empty() {
            return Ok(vec![]);
        }
        let placeholders = groups.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT * FROM capabilities WHERE {predicate} AND group_name IN ({placeholders}) ORDER BY capability_id"
        );
        let conn = self.conn.lock().map_err(|_| McpError::lock())?;
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| McpError::store(e.to_string()))?;
        let bound: Vec<Box<dyn rusqlite::ToSql>> = groups
            .iter()
            .map(|g| Box::new(g.clone()) as Box<dyn rusqlite::ToSql>)
            .collect();
        let refs: Vec<&dyn rusqlite::ToSql> = bound
            .iter()
            .map(|b| b.as_ref() as &dyn rusqlite::ToSql)
            .collect();
        let mapped = stmt
            .query_map(refs.as_slice(), row_to_record)
            .map_err(|e| McpError::store(e.to_string()))?;
        let mut rows: Vec<CapabilityRecord> = Vec::new();
        for row in mapped {
            rows.push(row.map_err(|e| McpError::store(e.to_string()))?);
        }
        Ok(rows)
    }

    /// 可见 id 列表：与 `visible` 同一口径（谓词只有一处 SQL）。
    pub fn visible_ids(&self, groups: &[String]) -> Result<Vec<String>, McpError> {
        Ok(self
            .visible(groups)?
            .into_iter()
            .map(|r| r.capability_id)
            .collect())
    }

    pub fn register(
        &self,
        id: &str,
        kind: &str,
        group: &str,
        risk: &str,
        definition: Value,
    ) -> Result<CapabilityRecord, McpError> {
        ensure_mutable(id)?;
        if !KINDS.contains(&kind) {
            return Err(McpError::validation(format!("kind 非法: {kind}")));
        }
        if !RISKS.contains(&risk) {
            return Err(McpError::validation(format!("risk 非法: {risk}")));
        }
        let dhash = definition_hash(&definition);
        let canon = canonical_json(&definition);
        {
            let mut conn = self.conn.lock().map_err(|_| McpError::lock())?;
            let tx = conn
                .transaction()
                .map_err(|e| McpError::store(e.to_string()))?;
            let now = now_iso();
            tx.execute("INSERT INTO capabilities (capability_id, kind, group_name, risk, status, discoverable, callable, version, definition_json, definition_hash, updated_at) VALUES (?1,?2,?3,?4,'draft',0,0,1,?5,?6,?7)",
                rusqlite::params![id, kind, group, risk, canon, dhash, now]).map_err(|e| McpError::store(e.to_string()))?;
            tx.execute("INSERT INTO capability_versions (capability_id, version, definition_json, definition_hash, approval_id, created_at, status) VALUES (?1,1,?2,?3,'',?4,'draft')",
                rusqlite::params![id, canon, dhash, now]).map_err(|e| McpError::store(e.to_string()))?;
            tx.commit().map_err(|e| McpError::store(e.to_string()))?;
        }
        let rec = self.get(id)?;
        match rec {
            Some(r) => Ok(r),
            None => Err(McpError::store(format!("register 后记录缺失: {id}"))),
        }
    }

    fn require(&self, id: &str) -> Result<CapabilityRecord, McpError> {
        match self.get(id)? {
            Some(r) => Ok(r),
            None => Err(McpError::new(
                "MCP_CAPABILITY_NOT_FOUND",
                json!({"capability_id": id}),
            )),
        }
    }

    /// review → published：盖章当前版本行 + 更新生命周期 + 写一条可读审计。
    fn transition_to_published(
        &self,
        id: &str,
        approval_id: &str,
        submitted_by: &str,
        approved_by: impl IntoAuditOrigin,
    ) -> Result<CapabilityRecord, McpError> {
        let approved_origin = approved_by.into_audit_origin();
        let approved_by = approved_origin.id.as_str();
        if approval_id.is_empty() {
            return Err(McpError::new(
                "MCP_APPROVAL_REQUIRED",
                json!({"capability_id": id, "missing": ["approval_id"]}),
            ));
        }
        let record = self.require(id)?;
        if record.status == "deprecated" {
            return Err(McpError::new(
                "MCP_STATE_TRANSITION_INVALID",
                json!({"capability_id": id, "from": record.status, "to": "published"}),
            ));
        }
        let now = now_iso();
        {
            let conn = self.conn.lock().map_err(|_| McpError::lock())?;
            conn.execute(
                "UPDATE capabilities SET status='published', updated_at=? WHERE capability_id=?",
                rusqlite::params![now, id],
            )
            .map_err(|e| McpError::store(e.to_string()))?;
            conn.execute(
                "UPDATE capability_versions SET status='published', approved_by=?, approved_at=?, approval_id=? WHERE capability_id=? AND version=?",
                rusqlite::params![approved_by, now, approval_id, id, record.version],
            )
            .map_err(|e| McpError::store(e.to_string()))?;
        }
        let approval =
            crate::audit::external_approval(approval_id, submitted_by, approved_by, &now);
        self.write_admin_audit(
            AdminChange::new("capability", id, "publish")
                .with_origin(approved_origin)
                .with_status(Some(&record.status), Some("published"))
                .with_changes(vec![change(
                    "status",
                    json!(record.status),
                    json!("published"),
                )])
                .with_approval(Some(approval))
                .with_version(record.version)
                .with_data(json!({
                    "definition_hash_before": record.definition_hash,
                    "definition_hash_after": record.definition_hash
                })),
        )?;
        let rec = self.get(id)?;
        match rec {
            Some(r) => Ok(r),
            None => Err(McpError::store(format!("publish 后记录缺失: {id}"))),
        }
    }

    /// draft → review。`submitted_by` 必须是鉴权派生身份。
    pub fn submit_for_review(
        &self,
        id: &str,
        submitted_by: &str,
    ) -> Result<CapabilityRecord, McpError> {
        ensure_mutable(id)?;
        let record = self.require(id)?;
        if record.status != "draft" {
            return Err(McpError::new(
                "MCP_STATE_TRANSITION_INVALID",
                json!({"capability_id": id, "from": record.status, "to": "review"}),
            ));
        }
        {
            let now = now_iso();
            let conn = self.conn.lock().map_err(|_| McpError::lock())?;
            conn.execute(
                "UPDATE capabilities SET status='review', submitted_by=?, submitted_at=?, updated_at=? WHERE capability_id=?",
                rusqlite::params![submitted_by, now, now, id],
            )
            .map_err(|e| McpError::store(e.to_string()))?;
            conn.execute(
                "UPDATE capability_versions SET status='review', submitted_by=?, submitted_at=?, approved_by='', approved_at='', approval_id='' WHERE capability_id=? AND version=?",
                rusqlite::params![submitted_by, now, id, record.version],
            )
            .map_err(|e| McpError::store(e.to_string()))?;
        }
        let rec = self.get(id)?;
        match rec {
            Some(r) => Ok(r),
            None => Err(McpError::store(format!(
                "submit_for_review 后记录缺失: {id}"
            ))),
        }
    }

    /// review → draft（退回）。`submitted_by` 清空：退回后重新提审时重新记录提审人。
    /// 审计不在这里写（与 `submit_for_review` 一致），由 HTTP 层把 reason 放进 `admin_change`。
    pub fn reject_review(&self, id: &str, reason: &str) -> Result<CapabilityRecord, McpError> {
        ensure_mutable(id)?;
        let record = self.require(id)?;
        if record.status != "review" {
            return Err(McpError::new(
                "MCP_STATE_TRANSITION_INVALID",
                json!({"capability_id": id, "from": record.status, "to": "draft"}),
            ));
        }
        let trimmed = reason.trim();
        if trimmed.is_empty() {
            return Err(McpError::new(
                "MCP_VALIDATION_ERROR",
                json!({"capability_id": id, "field": "reason", "hint": "退回必须说明原因"}),
            ));
        }
        // 原因长度按字符计（中文原因不应因 UTF-8 字节数提前触顶）。
        let len = trimmed.chars().count();
        if len > 500 {
            return Err(McpError::new(
                "MCP_VALIDATION_ERROR",
                json!({"capability_id": id, "field": "reason", "max": 500, "len": len}),
            ));
        }
        {
            let conn = self.conn.lock().map_err(|_| McpError::lock())?;
            conn.execute(
                "UPDATE capabilities SET status='draft', submitted_by='', submitted_at='', updated_at=? WHERE capability_id=?",
                rusqlite::params![now_iso(), id],
            )
            .map_err(|e| McpError::store(e.to_string()))?;
            conn.execute(
                "UPDATE capability_versions SET status='draft', submitted_by='', submitted_at='', approved_by='', approved_at='', approval_id='' WHERE capability_id=? AND version=?",
                rusqlite::params![id, record.version],
            )
            .map_err(|e| McpError::store(e.to_string()))?;
        }
        let rec = self.get(id)?;
        match rec {
            Some(r) => Ok(r),
            None => Err(McpError::store(format!("reject_review 后记录缺失: {id}"))),
        }
    }

    /// review → published。`submitted_by`/`approved_by` 是事实字段，由鉴权派生，请求体无法自述。
    pub fn promote_to_published(
        &self,
        id: &str,
        approval_id: &str,
        approved_by: impl IntoAuditOrigin,
    ) -> Result<CapabilityRecord, McpError> {
        ensure_mutable(id)?;
        let record = self.require(id)?;
        if record.status != "review" {
            return Err(McpError::new(
                "MCP_STATE_TRANSITION_INVALID",
                json!({"capability_id": id, "from": record.status, "to": "published"}),
            ));
        }
        self.transition_to_published(id, approval_id, &record.submitted_by, approved_by)
    }

    /// 已发布定义只插入新 version；哈希变化须新 `approval_id`。单个事务内：校验状态→插入新 version 行→更新指针(status 保持不变，不经过 review 中间态)。
    pub fn publish_version(
        &self,
        id: &str,
        definition: Value,
        approval_id: &str,
        submitted_by: &str,
        approved_by: impl IntoAuditOrigin,
    ) -> Result<CapabilityRecord, McpError> {
        let approved_origin = approved_by.into_audit_origin();
        let approved_by = approved_origin.id.as_str();
        ensure_mutable(id)?;
        if approval_id.is_empty() {
            return Err(McpError::new(
                "MCP_APPROVAL_REQUIRED",
                json!({"capability_id": id, "missing": ["approval_id"]}),
            ));
        }
        let record = self.require(id)?;
        if record.status == "deprecated" {
            return Err(McpError::new(
                "MCP_STATE_TRANSITION_INVALID",
                json!({"capability_id": id, "from": record.status, "to": "publish_version"}),
            ));
        }
        let dhash = definition_hash(&definition);
        let changes = definition_changes(&record.definition, &definition);
        let new_version = record.version + 1;
        let canon = canonical_json(&definition);
        let now = now_iso();
        {
            let mut conn = self.conn.lock().map_err(|_| McpError::lock())?;
            let tx = conn
                .transaction()
                .map_err(|e| McpError::store(e.to_string()))?;
            tx.execute("INSERT INTO capability_versions (capability_id, version, definition_json, definition_hash, approval_id, created_at, submitted_by, submitted_at, approved_by, approved_at, status) VALUES (?1,?2,?3,?4,?5,?6,?7,?6,?8,?6,'published')",
                rusqlite::params![id, new_version, canon, dhash, approval_id, now, submitted_by, approved_by]).map_err(|_| McpError::new("MCP_VERSION_CONFLICT", json!({"capability_id": id, "version": new_version})))?;
            tx.execute("UPDATE capabilities SET version=?1, definition_json=?2, definition_hash=?3, updated_at=?4 WHERE capability_id=?5",
                rusqlite::params![new_version, canon, dhash, now, id]).map_err(|e| McpError::store(e.to_string()))?;
            tx.commit().map_err(|e| McpError::store(e.to_string()))?;
        }
        let approval =
            crate::audit::external_approval(approval_id, submitted_by, &approved_origin.id, &now);
        self.write_admin_audit(
            AdminChange::new("capability", id, "update_definition")
                .with_origin(approved_origin)
                .with_status(Some(&record.status), Some(&record.status))
                .with_changes(changes)
                .with_approval(Some(approval))
                .with_version(new_version)
                .with_data(json!({
                    "definition_hash_before": record.definition_hash,
                    "definition_hash_after": dhash
                })),
        )?;
        let rec = self.get(id)?;
        match rec {
            Some(r) => Ok(r),
            None => Err(McpError::store(format!("publish_version 后记录缺失: {id}"))),
        }
    }

    /// draft 定义修订：版本 +1、状态保持 draft、追加 `capability_versions`，审批不适用
    /// （草案还没有可批准的定义）；`approval_id` 只作为可选外部票号留痕，不再自动生成。
    ///
    /// 与 `publish_version` 的区别：后者面向已发布定义且要求真实审批；这里是"草案内编辑"，
    /// 版本历史同样 append-only（不允许原地改 definition_json）。
    pub fn revise_draft_definition(
        &self,
        id: &str,
        definition: Value,
        approval_id: &str,
        actor: impl IntoAuditOrigin,
    ) -> Result<CapabilityRecord, McpError> {
        let origin = actor.into_audit_origin();
        ensure_mutable(id)?;
        let record = self.require(id)?;
        if record.status != "draft" {
            return Err(McpError::new(
                "MCP_STATE_TRANSITION_INVALID",
                json!({"capability_id": id, "from": record.status, "to": "draft"}),
            ));
        }
        let dhash = definition_hash(&definition);
        let changes = definition_changes(&record.definition, &definition);
        let new_version = record.version + 1;
        let canon = canonical_json(&definition);
        let now = now_iso();
        {
            let mut conn = self.conn.lock().map_err(|_| McpError::lock())?;
            let tx = conn
                .transaction()
                .map_err(|e| McpError::store(e.to_string()))?;
            tx.execute("INSERT INTO capability_versions (capability_id, version, definition_json, definition_hash, approval_id, created_at, status) VALUES (?1,?2,?3,?4,?5,?6,'draft')",
                rusqlite::params![id, new_version, canon, dhash, approval_id, now]).map_err(|_| McpError::new("MCP_VERSION_CONFLICT", json!({"capability_id": id, "version": new_version})))?;
            tx.execute("UPDATE capabilities SET version=?1, definition_json=?2, definition_hash=?3, updated_at=?4 WHERE capability_id=?5",
                rusqlite::params![new_version, canon, dhash, now, id]).map_err(|e| McpError::store(e.to_string()))?;
            tx.commit().map_err(|e| McpError::store(e.to_string()))?;
        }
        self.write_admin_audit(
            AdminChange::new("capability", id, "update_definition")
                .with_origin(origin)
                .with_status(Some("draft"), Some("draft"))
                .with_changes(changes)
                .with_version(new_version)
                .with_data(json!({
                    "definition_hash_before": record.definition_hash,
                    "definition_hash_after": dhash
                })),
        )?;
        let rec = self.get(id)?;
        match rec {
            Some(r) => Ok(r),
            None => Err(McpError::store(format!(
                "revise_draft_definition 后记录缺失: {id}"
            ))),
        }
    }

    pub fn deprecate(&self, id: &str, successor_id: &str) -> Result<CapabilityRecord, McpError> {
        ensure_mutable(id)?;
        if successor_id.is_empty() {
            return Err(McpError::validation(
                "deprecated 必须带 successor_id".to_string(),
            ));
        }
        {
            let conn = self.conn.lock().map_err(|_| McpError::lock())?;
            conn.execute("UPDATE capabilities SET status='deprecated', discoverable=0, callable=0, successor_id=?1, updated_at=?2 WHERE capability_id=?3", rusqlite::params![successor_id, now_iso(), id]).map_err(|e| McpError::store(e.to_string()))?;
        }
        let rec = self.get(id)?;
        match rec {
            Some(r) => Ok(r),
            None => Err(McpError::new(
                "MCP_CAPABILITY_NOT_FOUND",
                json!({"capability_id": id}),
            )),
        }
    }

    /// 只改双布尔：`changes` 必须是真实前后值；没有实际变化就不落库、不写事件。
    pub fn set_visibility(
        &self,
        id: &str,
        discoverable: Option<bool>,
        callable: Option<bool>,
        actor: impl IntoAuditOrigin,
    ) -> Result<CapabilityRecord, McpError> {
        let origin = actor.into_audit_origin();
        ensure_mutable(id)?;
        let record = self.require(id)?;
        let mut changes: Vec<Value> = Vec::new();
        if let Some(d) = discoverable
            && d != record.discoverable
        {
            changes.push(change("discoverable", json!(record.discoverable), json!(d)));
        }
        if let Some(c) = callable
            && c != record.callable
        {
            changes.push(change("callable", json!(record.callable), json!(c)));
        }
        if changes.is_empty() {
            return Ok(record);
        }
        let now = now_iso();
        {
            let conn = self.conn.lock().map_err(|_| McpError::lock())?;
            let mut assigns = Vec::new();
            let mut vals: Vec<i64> = Vec::new();
            if let Some(d) = discoverable
                && d != record.discoverable
            {
                assigns.push("discoverable=?");
                vals.push(i64::from(d));
            }
            if let Some(c) = callable
                && c != record.callable
            {
                assigns.push("callable=?");
                vals.push(i64::from(c));
            }
            let sql = format!(
                "UPDATE capabilities SET {}, updated_at=? WHERE capability_id=?",
                assigns.join(", ")
            );
            let mut stmt = conn
                .prepare(&sql)
                .map_err(|e| McpError::store(e.to_string()))?;
            let mut bound: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
            for v in &vals {
                bound.push(Box::new(*v));
            }
            bound.push(Box::new(now));
            bound.push(Box::new(id.to_string()));
            let refs: Vec<&dyn rusqlite::ToSql> = bound
                .iter()
                .map(|b| b.as_ref() as &dyn rusqlite::ToSql)
                .collect();
            stmt.execute(refs.as_slice())
                .map_err(|e| McpError::store(e.to_string()))?;
        }
        self.write_admin_audit(
            AdminChange::new("capability", id, "visibility")
                .with_origin(origin)
                .with_status(Some(&record.status), Some(&record.status))
                .with_changes(changes)
                .with_version(record.version)
                .with_data(json!({
                    "definition_hash_before": record.definition_hash,
                    "definition_hash_after": record.definition_hash
                })),
        )?;
        let rec = self.get(id)?;
        match rec {
            Some(r) => Ok(r),
            None => Err(McpError::store(format!("set_visibility 后记录缺失: {id}"))),
        }
    }

    pub(crate) fn write_admin_audit(&self, change: AdminChange) -> Result<(), McpError> {
        // 治理事件不得匿名：actor 为空说明调用点漏传身份，宁可整条拒绝也不写“未认证调用方”。
        if change.actor.trim().is_empty() {
            return Err(McpError::audit(format!(
                "admin_change 缺少 actor（action={} object_id={}）",
                change.action, change.object_id
            )));
        }
        if let Some(audit) = &self.audit {
            audit
                .append(AuditEvent {
                    tenant: "default".into(),
                    actor: ActorRef {
                        actor_type: "admin".into(),
                        id: change.actor.clone(),
                        role: "admin".into(),
                        token_id: change.actor_token_id.clone(),
                        source: change
                            .actor_source
                            .clone()
                            .or_else(|| Some("admin-api".into())),
                    },
                    event_type: "admin_change".into(),
                    payload: change.into_payload(),
                    session_id: String::new(),
                    ts: String::new(),
                    event_id: String::new(),
                })
                .map_err(|e| McpError::audit(e.to_string()))?;
        }
        Ok(())
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

impl CapabilityStore {
    /// 审核队列：draft/review 两类（其余状态不待审），按最近变更倒序。
    pub fn review_queue(&self) -> Result<Vec<CapabilityRecord>, McpError> {
        let conn = self.conn.lock().map_err(|_| McpError::lock())?;
        let mut stmt = conn
            .prepare(
                "SELECT * FROM capabilities WHERE status IN ('draft','review') ORDER BY updated_at DESC, capability_id ASC",
            )
            .map_err(|e| McpError::store(e.to_string()))?;
        let mapped = stmt
            .query_map([], row_to_record)
            .map_err(|e| McpError::store(e.to_string()))?;
        let mut rows: Vec<CapabilityRecord> = Vec::new();
        for row in mapped {
            rows.push(row.map_err(|e| McpError::store(e.to_string()))?);
        }
        Ok(rows)
    }
}

/// 管理面新增的业务组：与配置里的 `GroupDef` 同语义，另带"授权给哪些角色"。
///
/// `code` 是唯一权限标识（`groups.name` 列沿用旧列名存 code），`name` 是展示名
/// （`groups.display_name` 列；旧行为空时回退 code）。角色列表是"该组对哪些角色可见"
/// 的唯一授权来源：`Service::effective_policy` 会把组 code 追加进这些角色的
/// `default_groups`，从而下一次 scope 解析就能用上。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedGroup {
    /// 组 code：能力/令牌/scope/审计一律用它。
    pub code: String,
    /// 中文展示名；不参与任何权限判定。
    pub name: String,
    pub priority: i64,
    pub min_budget: i64,
    pub roles: Vec<String>,
    pub updated_at: String,
}

impl ManagedGroup {
    /// 字段级门禁；保留组名用独立错误码（HTTP 409），与普通参数错误（400）区分。
    pub fn validate(&self) -> Result<(), McpError> {
        if is_reserved_group_name(&self.code) {
            return Err(McpError::new(
                "MCP_GROUP_RESERVED",
                json!({"group": self.code, "reserved": crate::config::RESERVED_GROUP_NAMES}),
            ));
        }
        validate_group_name(&self.code).map_err(McpError::validation)?;
        crate::config::validate_display_name("name", &self.name).map_err(McpError::validation)?;
        if self.priority < 0 {
            return Err(McpError::validation(format!(
                "priority 不能为负: {}",
                self.priority
            )));
        }
        if self.min_budget <= 0 {
            return Err(McpError::validation(format!(
                "min_budget 必须为正: {}",
                self.min_budget
            )));
        }
        Ok(())
    }

    pub fn to_value(&self) -> Value {
        json!({
            "code": self.code,
            "name": self.name,
            "priority": self.priority,
            "min_budget": self.min_budget,
            "roles": self.roles
        })
    }
}

impl CapabilityStore {
    /// 管理面维护的组（按 code 升序，输出稳定）。
    pub fn managed_groups(&self) -> Result<Vec<ManagedGroup>, McpError> {
        let conn = self.conn.lock().map_err(|_| McpError::lock())?;
        let mut stmt = conn
            .prepare("SELECT name, display_name, priority, min_budget, roles_json, updated_at FROM groups ORDER BY name")
            .map_err(|e| McpError::store(e.to_string()))?;
        let mapped = stmt
            .query_map([], row_to_group)
            .map_err(|e| McpError::store(e.to_string()))?;
        let mut rows = Vec::new();
        for row in mapped {
            rows.push(row.map_err(|e| McpError::store(e.to_string()))?);
        }
        Ok(rows)
    }

    pub fn managed_group(&self, code: &str) -> Result<Option<ManagedGroup>, McpError> {
        let conn = self.conn.lock().map_err(|_| McpError::lock())?;
        let mut stmt = conn
            .prepare("SELECT name, display_name, priority, min_budget, roles_json, updated_at FROM groups WHERE name=?")
            .map_err(|e| McpError::store(e.to_string()))?;
        let mapped = stmt
            .query_map([code], row_to_group)
            .map_err(|e| McpError::store(e.to_string()))?;
        let mut rows = Vec::new();
        for row in mapped {
            rows.push(row.map_err(|e| McpError::store(e.to_string()))?);
        }
        Ok(rows.into_iter().next())
    }

    fn require_managed_group(&self, code: &str) -> Result<ManagedGroup, McpError> {
        self.managed_group(code)?
            .ok_or_else(|| McpError::new("MCP_GROUP_NOT_FOUND", json!({"group": code})))
    }

    /// 新建管理组：重名（含 policy 组由 HTTP 层先判）→ 409；写入成功与 `admin_change` 同源。
    pub fn insert_managed_group(
        &self,
        group: &ManagedGroup,
        actor: impl IntoAuditOrigin,
    ) -> Result<ManagedGroup, McpError> {
        let origin = actor.into_audit_origin();
        group.validate()?;
        {
            let conn = self.conn.lock().map_err(|_| McpError::lock())?;
            let exists: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM groups WHERE name=?",
                    [&group.code],
                    |r| r.get(0),
                )
                .map_err(|e| McpError::store(e.to_string()))?;
            if exists > 0 {
                return Err(McpError::new(
                    "MCP_GROUP_CONFLICT",
                    json!({"group": group.code, "hint": "管理组已存在"}),
                ));
            }
            let roles = serde_json::to_string(&group.roles)
                .map_err(|e| McpError::store(format!("roles 序列化失败: {e}")))?;
            conn.execute(
                "INSERT INTO groups (name, display_name, priority, min_budget, roles_json, updated_at) VALUES (?1,?2,?3,?4,?5,?6)",
                rusqlite::params![group.code, group.name, group.priority, group.min_budget, roles, now_iso()],
            )
            .map_err(|e| McpError::store(e.to_string()))?;
        }
        let created = self.require_managed_group(&group.code)?;
        self.write_admin_audit(
            AdminChange::new("group", &created.code, "group_create")
                .with_origin(origin)
                .with_changes(field_changes(
                    &json!({}),
                    &created.to_value(),
                    GROUP_CHANGE_FIELDS,
                ))
                .with_data(json!({"group": created.to_value()})),
        )?;
        Ok(created)
    }

    /// 修改管理组：不存在 → 404；数值/角色由 `ManagedGroup::validate` 判定。
    ///
    /// code 是主键，不可改；`name` 只写展示列。
    pub fn update_managed_group(
        &self,
        group: &ManagedGroup,
        actor: impl IntoAuditOrigin,
    ) -> Result<ManagedGroup, McpError> {
        let origin = actor.into_audit_origin();
        group.validate()?;
        let before = self.require_managed_group(&group.code)?;
        let changes = field_changes(&before.to_value(), &group.to_value(), GROUP_CHANGE_FIELDS);
        if changes.is_empty() {
            return Ok(before);
        }
        {
            let conn = self.conn.lock().map_err(|_| McpError::lock())?;
            let roles = serde_json::to_string(&group.roles)
                .map_err(|e| McpError::store(format!("roles 序列化失败: {e}")))?;
            let changed = conn
                .execute(
                    "UPDATE groups SET display_name=?1, priority=?2, min_budget=?3, roles_json=?4, updated_at=?5 WHERE name=?6",
                    rusqlite::params![group.name, group.priority, group.min_budget, roles, now_iso(), group.code],
                )
                .map_err(|e| McpError::store(e.to_string()))?;
            if changed == 0 {
                return Err(McpError::new(
                    "MCP_GROUP_NOT_FOUND",
                    json!({"group": group.code}),
                ));
            }
        }
        let after = self.require_managed_group(&group.code)?;
        self.write_admin_audit(
            AdminChange::new("group", &after.code, "group_update")
                .with_origin(origin)
                .with_changes(changes)
                .with_data(json!({"group": after.to_value()})),
        )?;
        Ok(after)
    }

    /// 删除管理组：仍有 capability 引用 → 409（带 `capability_count`）。
    ///
    /// 引用计数与删除在同一把连接锁内完成，避免"查完为 0、删前被别的请求写进一条"的竞态。
    pub fn remove_managed_group(
        &self,
        code: &str,
        actor: impl IntoAuditOrigin,
    ) -> Result<(), McpError> {
        let origin = actor.into_audit_origin();
        if is_reserved_group_name(code) {
            return Err(McpError::new(
                "MCP_GROUP_RESERVED",
                json!({"group": code, "reserved": crate::config::RESERVED_GROUP_NAMES}),
            ));
        }
        let before = self.require_managed_group(code)?;
        {
            let conn = self.conn.lock().map_err(|_| McpError::lock())?;
            let used: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM capabilities WHERE group_name=?",
                    [code],
                    |r| r.get(0),
                )
                .map_err(|e| McpError::store(e.to_string()))?;
            let token_used = crate::access::group_token_count_locked(&conn, code)?;
            if used > 0 || token_used > 0 {
                return Err(McpError::new(
                    "MCP_GROUP_IN_USE",
                    json!({
                        "group": code,
                        "capability_count": used,
                        "token_count": token_used
                    }),
                ));
            }
            let changed = conn
                .execute("DELETE FROM groups WHERE name=?", [code])
                .map_err(|e| McpError::store(e.to_string()))?;
            if changed == 0 {
                return Err(McpError::new("MCP_GROUP_NOT_FOUND", json!({"group": code})));
            }
        }
        self.write_admin_audit(
            AdminChange::new("group", code, "group_delete")
                .with_origin(origin)
                .with_changes(field_changes(
                    &before.to_value(),
                    &json!({}),
                    GROUP_CHANGE_FIELDS,
                ))
                .with_data(json!({"group": before.to_value()})),
        )
    }

    /// 单个组的 capability 引用数（全部状态，包括 draft/deprecated）。
    pub fn group_capability_count(&self, code: &str) -> Result<i64, McpError> {
        let conn = self.conn.lock().map_err(|_| McpError::lock())?;
        conn.query_row(
            "SELECT COUNT(*) FROM capabilities WHERE group_name=?",
            [code],
            |r| r.get(0),
        )
        .map_err(|e| McpError::store(e.to_string()))
    }

    /// 全部组的引用数：管理面列表一次查完，避免逐组 N+1。
    pub fn group_capability_counts(&self) -> Result<HashMap<String, i64>, McpError> {
        let conn = self.conn.lock().map_err(|_| McpError::lock())?;
        let mut stmt = conn
            .prepare("SELECT group_name, COUNT(*) FROM capabilities GROUP BY group_name")
            .map_err(|e| McpError::store(e.to_string()))?;
        let mapped = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .map_err(|e| McpError::store(e.to_string()))?;
        let mut out = HashMap::new();
        for row in mapped {
            let (name, count) = row.map_err(|e| McpError::store(e.to_string()))?;
            out.insert(name, count);
        }
        Ok(out)
    }
}

// ---------------------------- 上游 MCP server（router 的动态插入面） ----------------------------

/// 上游传输闭集：`http` = MCP Streamable HTTP；`stdio` = 部署级开关控制的本地子进程。
pub const UPSTREAM_TRANSPORTS: &[&str] = &["http", "stdio"];
/// 上游分类闭集：与内置工具三组同名，另加 `custom` 给接入方自定的上游。
pub const UPSTREAM_CATEGORIES: &[&str] = &["basic", "analysis", "admin", "custom"];
/// 保留的上游 id：不得作为上游名（会被误读成授权组或内置来源）。
pub const RESERVED_UPSTREAM_IDS: &[&str] = &["admin", "analysis", "basic", "builtin", "jumcp"];
pub const UPSTREAM_ID_MAX_LEN: usize = 32;
pub const UPSTREAM_MAX_ITEMS: usize = 64;
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
            "updated_at": self.updated_at
        })
    }

    /// 审计 `data`：只放非密钥的配置事实，绝不带 env/headers 的值。
    pub fn audit_data(&self) -> Value {
        json!({
            "transport": self.transport,
            "group": self.group,
            "category": self.category,
            "read_only": self.read_only,
            "enabled": self.enabled
        })
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
                "INSERT INTO mcp_upstreams (id, name, transport, url, command, args_json, env_json, headers_json, group_code, category, read_only, enabled, timeout_ms, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?14)",
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
                    now
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

    /// 删除上游：不存在 → 404；删除前的一侧配置进审计 changes。
    pub fn delete_upstream(&self, id: &str, actor: impl IntoAuditOrigin) -> Result<(), McpError> {
        let origin = actor.into_audit_origin();
        let before = self.require_upstream(id)?;
        {
            let conn = self.conn.lock().map_err(|_| McpError::lock())?;
            conn.execute("DELETE FROM mcp_upstreams WHERE id=?", [id])
                .map_err(|e| McpError::store(e.to_string()))?;
        }
        self.write_admin_audit(
            AdminChange::new("upstream", id, "delete")
                .with_origin(origin)
                .with_changes(upstream_changes(&before, &empty_upstream()))
                .with_data(before.audit_data()),
        )
    }
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
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

fn row_to_group(row: &Row) -> rusqlite::Result<ManagedGroup> {
    let roles_json: String = row.get(4)?;
    let roles: Vec<String> = serde_json::from_str(&roles_json).map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            4,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "group roles json decode failed",
            )),
        )
    })?;
    let code: String = row.get(0)?;
    let display_name: String = row.get(1)?;
    // 旧行为空：`groups.name` 存的就是 code，展示名回退 code。
    let name = if display_name.trim().is_empty() {
        code.clone()
    } else {
        display_name
    };
    Ok(ManagedGroup {
        code,
        name,
        priority: row.get(2)?,
        min_budget: row.get(3)?,
        roles,
        updated_at: row.get(5)?,
    })
}

pub(crate) fn ensure_column(conn: &Connection, table: &str, column: &str) -> Result<(), McpError> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|e| McpError::store(e.to_string()))?;
    let names: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(|e| McpError::store(e.to_string()))?
        .filter_map(Result::ok)
        .collect();
    if names.iter().any(|n| n == column) {
        return Ok(());
    }
    conn.execute(
        &format!("ALTER TABLE {table} ADD COLUMN {column} TEXT NOT NULL DEFAULT ''"),
        [],
    )
    .map_err(|e| McpError::store(e.to_string()))?;
    Ok(())
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn row_to_record(row: &Row) -> rusqlite::Result<CapabilityRecord> {
    let def_json: String = row.get(8)?;
    let definition: Value = serde_json::from_str(&def_json).map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            8,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "definition json decode failed",
            )),
        )
    })?;
    Ok(CapabilityRecord {
        capability_id: row.get(0)?,
        kind: row.get(1)?,
        group: row.get(2)?,
        risk: row.get(3)?,
        status: row.get(4)?,
        discoverable: row.get::<_, i64>(5)? != 0,
        callable: row.get::<_, i64>(6)? != 0,
        version: row.get(7)?,
        definition,
        definition_hash: row.get(9)?,
        successor_id: row.get(10)?,
        updated_at: row.get(11)?,
        submitted_by: row.get(12)?,
        submitted_at: row.get(13)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn store() -> CapabilityStore {
        CapabilityStore::open(":memory:", None).unwrap()
    }
    /// 内置 id 必须严格分区：BUILTIN = RESIDENT ∪ ADMIN，且两个子集互不重叠。
    /// 只在其中一处加 id 会让"写保护"或"发现面投影"单边失效。
    #[test]
    fn builtin_ids_partition_matches_subsets() {
        assert_eq!(
            RESIDENT_CAPABILITY_IDS.len() + ADMIN_CAPABILITY_IDS.len(),
            BUILTIN_CAPABILITY_IDS.len()
        );
        for id in RESIDENT_CAPABILITY_IDS.iter().chain(ADMIN_CAPABILITY_IDS) {
            assert!(BUILTIN_CAPABILITY_IDS.contains(id), "{id} 不在写保护集合");
            assert!(is_builtin_id(id), "{id} 未被 is_builtin_id 识别");
        }
        for id in RESIDENT_CAPABILITY_IDS {
            assert!(
                !ADMIN_CAPABILITY_IDS.contains(id),
                "{id} 同时属于常驻与维护"
            );
        }
    }
    #[test]
    fn write_and_admin_risks_are_not_registerable() {
        let s = store();
        for risk in ["write", "admin"] {
            let e = s
                .register("cap1", "tool", "db.read.core", risk, json!({}))
                .unwrap_err();
            assert_eq!(e.code, "MCP_VALIDATION_ERROR");
        }
    }
    #[test]
    fn self_approval_publishes() {
        let s = store();
        s.register(
            "cap1",
            "tool",
            "db.read.core",
            "read",
            json!({"description":"x"}),
        )
        .unwrap();
        s.submit_for_review("cap1", "alice").unwrap();
        let r = s.promote_to_published("cap1", "ap-1", "alice").unwrap();
        assert_eq!(r.status, "published");
        let v1 = s.versions("cap1").unwrap().pop().unwrap();
        assert_eq!(v1.submitted_by, "alice");
        assert_eq!(v1.approved_by, "alice");
        assert_eq!(v1.approval_id, "ap-1");
        assert_eq!(v1.status, "published");
    }
    #[test]
    fn new_version_not_inplace() {
        let s = store();
        s.register(
            "cap1",
            "tool",
            "db.read.core",
            "read",
            json!({"description":"v1"}),
        )
        .unwrap();
        let r = s
            .publish_version("cap1", json!({"description":"v2"}), "ap-1", "alice", "bob")
            .unwrap();
        assert_eq!(r.version, 2);
        assert_eq!(
            s.get_version("cap1", 1)
                .unwrap()
                .unwrap()
                .definition_hash
                .len(),
            64
        );
    }
    #[test]
    fn deprecate_needs_successor() {
        let s = store();
        s.register("cap1", "tool", "db.read.core", "read", json!({}))
            .unwrap();
        assert!(s.deprecate("cap1", "").is_err());
        let r = s.deprecate("cap1", "cap2").unwrap();
        assert_eq!(r.status, "deprecated");
    }
    #[test]
    fn versions_append_only_trigger_blocks_update() {
        let s = store();
        s.register(
            "cap1",
            "tool",
            "db.read.core",
            "read",
            json!({"description":"v1"}),
        )
        .unwrap();
        let conn = s.conn.lock().unwrap();
        assert!(
            conn.execute(
                "UPDATE capability_versions SET definition_json='x' WHERE capability_id='cap1'",
                []
            )
            .is_err()
        );
        assert!(
            conn.execute(
                "DELETE FROM capability_versions WHERE capability_id='cap1'",
                []
            )
            .is_err()
        );
    }
    #[test]
    fn publish_version_old_unchanged_pointer_moves_audit_differs() {
        let audit = AuditLog::open(":memory:").unwrap();
        let s = CapabilityStore::open(":memory:", Some(std::sync::Arc::new(audit))).unwrap();
        s.register(
            "cap1",
            "tool",
            "db.read.core",
            "read",
            json!({"description":"v1","keep":1}),
        )
        .unwrap();
        s.submit_for_review("cap1", "alice").unwrap();
        s.promote_to_published("cap1", "ap-1", "bob").unwrap();
        let before = s.get("cap1").unwrap().unwrap();
        let before_hash = before.definition_hash.clone();
        let r = s
            .publish_version(
                "cap1",
                json!({"description":"v2","keep":1,"new":2}),
                "ap-2",
                "alice",
                "bob",
            )
            .unwrap();
        assert_eq!(r.version, 2);
        assert_eq!(r.status, "published");
        assert_ne!(r.definition_hash, before_hash);
        let v1 = s.get_version("cap1", 1).unwrap().unwrap();
        assert_eq!(v1.definition_hash, before_hash);
        assert_eq!(v1.definition["description"], json!("v1"));
        // 审计：capability/update_definition，changes 给出真实前后值，审批引用是外部票号。
        let rows = {
            let conn = s.audit.as_ref().unwrap();
            conn.query(None).unwrap()
        };
        let admin: Vec<_> = rows
            .into_iter()
            .filter(|e| e.event_type == "admin_change")
            .collect();
        assert!(!admin.is_empty());
        let publish = admin
            .iter()
            .find(|e| e.payload["action"] == json!("publish"))
            .expect("publish 事件");
        assert_eq!(publish.actor["id"], json!("bob"), "发布 actor 必须是批准人");
        let last = admin.last().unwrap();
        assert_eq!(last.payload["object_type"], json!("capability"));
        assert_eq!(last.payload["object_id"], json!("cap1"));
        assert_eq!(last.payload["action"], json!("update_definition"));
        assert_eq!(last.payload["outcome"], json!("ok"));
        assert_eq!(
            last.payload["data"]["definition_hash_before"],
            json!(before_hash)
        );
        assert_eq!(
            last.payload["data"]["definition_hash_after"],
            json!(r.definition_hash)
        );
        assert_ne!(
            last.payload["data"]["definition_hash_before"],
            last.payload["data"]["definition_hash_after"]
        );
        let changed: Vec<&str> = last.payload["changes"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| c["field"].as_str())
            .collect();
        assert!(
            changed.contains(&"new"),
            "changes 应含新增 key: {changed:?}"
        );
        assert!(
            changed.contains(&"description"),
            "changes 应含修改 key: {changed:?}"
        );
        assert_eq!(last.payload["approval"]["id"], json!("ap-2"));
        assert_eq!(last.payload["approval"]["source"], json!("external"));
        assert_eq!(last.payload["approval"]["verified"], json!(false));
        // capabilities 指针已指向新 version
        assert_eq!(s.get("cap1").unwrap().unwrap().version, 2);
    }
    #[test]
    fn publish_version_allows_same_person() {
        let s = store();
        s.register(
            "cap1",
            "tool",
            "db.read.core",
            "read",
            json!({"description":"v1"}),
        )
        .unwrap();
        let r = s
            .publish_version(
                "cap1",
                json!({"description":"v2"}),
                "ap-1",
                "alice",
                "alice",
            )
            .unwrap();
        assert_eq!(r.version, 2);
        let v2 = s.versions("cap1").unwrap().pop().unwrap();
        assert_eq!(v2.submitted_by, "alice");
        assert_eq!(v2.approved_by, "alice");
        assert_eq!(v2.approval_id, "ap-1");
        assert_eq!(v2.status, "published");
    }
    #[test]
    fn publish_version_transaction_rolls_back() {
        let s = store();
        s.register(
            "cap1",
            "tool",
            "db.read.core",
            "read",
            json!({"description":"v1"}),
        )
        .unwrap();
        s.submit_for_review("cap1", "alice").unwrap();
        s.promote_to_published("cap1", "ap-1", "bob").unwrap();
        let before = s.get("cap1").unwrap().unwrap();
        // 注入必失败约束：后开一个 trigger 让第二次插入冲突
        {
            let conn = s.conn.lock().unwrap();
            // 人为制造单次失败：插一行会导致 pointer 更新触发失败（用不合法的 version 重复）
            // 替代方案：在新事务里插入 versions 后 pointer 更新失败会回滚；这里用 MUST_FAIL 的 SQL 模拟
            let _ = conn.execute("CREATE TEMP TRIGGER trg_fail AFTER INSERT ON capability_versions BEGIN SELECT RAISE(ABORT, 'forced'); END;", []);
        }
        let res = s.publish_version("cap1", json!({"description":"v2"}), "ap-2", "alice", "bob");
        assert!(res.is_err());
        let after = s.get("cap1").unwrap().unwrap();
        assert_eq!(after.version, before.version);
        assert_eq!(after.definition_hash, before.definition_hash);
        assert!(s.get_version("cap1", 2).unwrap().is_none());
    }
    #[test]
    fn db_failure_get_visible_all_returns_err() {
        let s = store();
        s.register(
            "cap1",
            "tool",
            "db.read.core",
            "read",
            json!({"description":"v1"}),
        )
        .unwrap();
        {
            let conn = s.conn.lock().unwrap();
            let _ = conn.execute("DROP TABLE capabilities", []);
        }
        assert!(s.get("cap1").is_err());
        assert!(s.all().is_err());
        assert!(s.visible_ids(&["db.read.core".to_string()]).is_err());
    }
    #[test]
    fn empty_table_is_ok_empty() {
        let s = store();
        assert!(s.get("missing").unwrap().is_none());
        assert_eq!(s.all().unwrap().len(), 0);
        assert_eq!(
            s.visible_ids(&["db.read.core".to_string()]).unwrap().len(),
            0
        );
        assert_eq!(s.visible_ids(&[]).unwrap().len(), 0);
    }
    #[test]
    fn publish_version_conflict_keeps_pointer() {
        let s = store();
        s.register(
            "cap1",
            "tool",
            "db.read.core",
            "read",
            json!({"description":"v1"}),
        )
        .unwrap();
        // 预置 version 2 行：模拟并发/外部写入造成的版本占用。
        {
            let conn = s.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO capability_versions (capability_id, version, definition_json, definition_hash, approval_id) VALUES ('cap1', 2, '{}', 'x', 'ap-0')",
                [],
            )
            .unwrap();
        }
        let e = s
            .publish_version("cap1", json!({"description":"v2"}), "ap-1", "alice", "bob")
            .unwrap_err();
        assert_eq!(e.code, "MCP_VERSION_CONFLICT");
        // 事务回滚：指针不前进、定义不变。
        let rec = s.get("cap1").unwrap().unwrap();
        assert_eq!(rec.version, 1);
        assert_eq!(rec.definition["description"], json!("v1"));
    }

    /// 内置 id 在读侧由代码提供，写侧必须一律被拒：包括 register 自己——
    /// 否则维护通道可以"注册"一条同名能力，在发现面影子覆盖内置工具定义。
    #[test]
    fn builtin_ids_are_immutable_across_every_write_path() {
        let s = store();
        for id in BUILTIN_CAPABILITY_IDS {
            let errors = [
                s.register(
                    id,
                    "tool",
                    "db.read.core",
                    "read",
                    json!({"description":"影子"}),
                )
                .unwrap_err(),
                s.submit_for_review(id, "alice").unwrap_err(),
                s.reject_review(id, "理由").unwrap_err(),
                s.promote_to_published(id, "ap-1", "bob").unwrap_err(),
                s.publish_version(id, json!({"description":"v2"}), "ap-2", "alice", "bob")
                    .unwrap_err(),
                s.deprecate(id, "successor").unwrap_err(),
                s.set_visibility(id, Some(true), Some(true), "alice")
                    .unwrap_err(),
            ];
            for e in errors {
                assert_eq!(e.code, "MCP_BUILTIN_IMMUTABLE", "{id} 的写操作必须被拒");
                assert_eq!(e.data["capability_id"], json!(id));
            }
            // 拒绝发生在落库之前：注册表里不该出现这一行。
            assert!(s.get(id).unwrap().is_none(), "{id} 不得被写入注册表");
        }
    }

    #[test]
    fn visible_filters_by_status_discoverable_and_group() {
        let s = store();
        s.register(
            "cap.draft",
            "tool",
            "db.read.core",
            "read",
            json!({"description":"draft"}),
        )
        .unwrap();
        s.register(
            "cap.other_group",
            "tool",
            "db.admin.schema",
            "read",
            json!({"description":"other"}),
        )
        .unwrap();
        s.submit_for_review("cap.other_group", "alice").unwrap();
        s.promote_to_published("cap.other_group", "ap-1", "bob")
            .unwrap();
        s.set_visibility("cap.other_group", Some(true), Some(true), "alice")
            .unwrap();
        s.register(
            "cap.hidden",
            "tool",
            "db.read.core",
            "read",
            json!({"description":"hidden"}),
        )
        .unwrap();
        s.submit_for_review("cap.hidden", "alice").unwrap();
        s.promote_to_published("cap.hidden", "ap-3", "bob").unwrap();

        let groups = vec!["db.read.core".to_string()];
        let ids: Vec<String> = s
            .visible(&groups)
            .unwrap()
            .into_iter()
            .map(|r| r.capability_id)
            .collect();
        // draft 不可见；他组不可见；published 但 discoverable=false 不可见。
        assert!(ids.is_empty(), "可见集应为空，实际 {ids:?}");
        assert_eq!(s.visible_ids(&groups).unwrap(), ids);

        // §4.4.1 / D3：discoverable=false + callable=true 落在调用面，不在发现面。
        s.set_visibility("cap.hidden", Some(false), Some(true), "alice")
            .unwrap();
        assert!(
            s.visible(&groups).unwrap().is_empty(),
            "不可发现的能力不得进入可见集"
        );
        let callable: Vec<String> = s
            .callable(&groups)
            .unwrap()
            .into_iter()
            .map(|r| r.capability_id)
            .collect();
        assert_eq!(
            callable,
            vec!["cap.hidden".to_string()],
            "可调用集只看 published ∧ callable ∧ group"
        );
    }

    fn managed(code: &str, priority: i64, min_budget: i64, roles: &[&str]) -> ManagedGroup {
        ManagedGroup {
            code: code.into(),
            name: format!("{code} 展示名"),
            priority,
            min_budget,
            roles: roles.iter().map(|r| (*r).to_string()).collect(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn managed_group_crud_conflicts_and_reserved_names() {
        let s = store();
        let created = s
            .insert_managed_group(&managed("db.read.new", 7, 900, &["analyst"]), "alice")
            .unwrap();
        assert_eq!(created.code, "db.read.new");
        assert_eq!(created.name, "db.read.new 展示名");
        assert_eq!(created.roles, vec!["analyst".to_string()]);
        assert_eq!(s.managed_groups().unwrap().len(), 1);

        // 重名 → 409 语义的错误码。
        let e = s
            .insert_managed_group(&managed("db.read.new", 1, 800, &[]), "alice")
            .unwrap_err();
        assert_eq!(e.code, "MCP_GROUP_CONFLICT");

        // 修改（不存在 → 404）。
        let updated = s
            .update_managed_group(&managed("db.read.new", 3, 1200, &["analyst"]), "bob")
            .unwrap();
        assert_eq!(updated.priority, 3);
        assert_eq!(updated.min_budget, 1200);
        let e = s
            .update_managed_group(&managed("db.read.missing", 3, 1200, &[]), "bob")
            .unwrap_err();
        assert_eq!(e.code, "MCP_GROUP_NOT_FOUND");

        // 字段门禁：min_budget<=0 / priority<0 / 非法组名 → 校验错误。
        for bad in [
            managed("db.read.bad", 1, 0, &[]),
            managed("db.read.bad", -1, 800, &[]),
            ManagedGroup {
                code: "db read bad".into(),
                ..managed("x", 1, 800, &[])
            },
        ] {
            assert_eq!(bad.validate().unwrap_err().code, "MCP_VALIDATION_ERROR");
        }

        // 保留组名：三个写路径都必须拒绝（admin 是维护通道安全边界）。
        for name in ["builtin", "admin"] {
            assert_eq!(
                s.insert_managed_group(&managed(name, 1, 800, &[]), "alice")
                    .unwrap_err()
                    .code,
                "MCP_GROUP_RESERVED"
            );
            assert_eq!(
                s.update_managed_group(&managed(name, 1, 800, &[]), "alice")
                    .unwrap_err()
                    .code,
                "MCP_GROUP_RESERVED"
            );
            assert_eq!(
                s.remove_managed_group(name, "alice").unwrap_err().code,
                "MCP_GROUP_RESERVED"
            );
        }

        // 引用中的组不可删除，错误里带精确计数。
        s.register(
            "cap1",
            "tool",
            "db.read.new",
            "read",
            json!({"description":"x"}),
        )
        .unwrap();
        assert_eq!(s.group_capability_count("db.read.new").unwrap(), 1);
        let e = s.remove_managed_group("db.read.new", "alice").unwrap_err();
        assert_eq!(e.code, "MCP_GROUP_IN_USE");
        assert_eq!(e.data["capability_count"], json!(1));

        // 无引用的组可以删除。
        s.insert_managed_group(&managed("db.read.empty", 1, 800, &[]), "alice")
            .unwrap();
        s.remove_managed_group("db.read.empty", "alice").unwrap();
        assert!(s.managed_group("db.read.empty").unwrap().is_none());
    }

    #[test]
    fn legacy_managed_group_display_name_falls_back_to_code() {
        let s = store();
        // 模拟旧库行：display_name 列为默认空串（旧行只有 name=code）。
        {
            let conn = s.db().lock().unwrap();
            conn.execute(
                "INSERT INTO groups (name, priority, min_budget, roles_json) VALUES ('legacy.group', 3, 800, '[]')",
                [],
            )
            .unwrap();
        }
        let g = s.managed_group("legacy.group").unwrap().expect("旧行可读");
        assert_eq!(g.code, "legacy.group");
        assert_eq!(g.name, "legacy.group", "空展示名回退 code");
        // 编辑只写展示列，主键 code 不变。
        let updated = s
            .update_managed_group(
                &ManagedGroup {
                    code: "legacy.group".into(),
                    name: "旧配置组".into(),
                    priority: 4,
                    min_budget: 900,
                    roles: vec![],
                    updated_at: String::new(),
                },
                "alice",
            )
            .unwrap();
        assert_eq!(updated.code, "legacy.group");
        assert_eq!(updated.name, "旧配置组");
        assert!(s.managed_group("legacy.group").unwrap().is_some());
    }

    #[test]
    fn admin_audit_rejects_anonymous_actor() {
        let audit = AuditLog::open(":memory:").unwrap();
        let s = CapabilityStore::open(":memory:", Some(std::sync::Arc::new(audit))).unwrap();
        let e = s
            .write_admin_audit(AdminChange::new("capability", "cap1", "publish"))
            .unwrap_err();
        assert_eq!(e.code, "MCP_AUDIT_ERROR");
        assert!(
            s.audit.as_ref().unwrap().query(None).unwrap().is_empty(),
            "匿名 admin_change 不得落库"
        );
    }

    #[test]
    fn definition_changes_flatten_nested_objects() {
        let before = json!({
            "template": {
                "description": "hash probe",
                "constraints": {"auto_limit": 1000},
                "params": [{"name": "x"}]
            },
            "removed": 1
        });
        let after = json!({
            "template": {
                "description": "hash probe v2",
                "constraints": {"auto_limit": 2000},
                "params": [{"name": "x"}]
            },
            "added": true
        });
        let changes = definition_changes(&before, &after);
        let row = |field: &str| {
            changes
                .iter()
                .find(|c| c["field"] == json!(field))
                .unwrap_or_else(|| panic!("缺字段 {field}: {changes:?}"))
        };
        assert_eq!(row("template.description")["before"], json!("hash probe"));
        assert_eq!(row("template.description")["after"], json!("hash probe v2"));
        assert_eq!(
            row("template.constraints.auto_limit")["before"],
            json!(1000)
        );
        assert_eq!(row("template.constraints.auto_limit")["after"], json!(2000));
        assert_eq!(row("removed")["after"], Value::Null);
        assert_eq!(row("added")["before"], Value::Null);
        assert!(
            !changes.iter().any(|c| c["field"] == json!("template")),
            "对象必须展开，不得整份输出: {changes:?}"
        );
    }

    #[test]
    fn definition_changes_depth_cap_collapses_subtree() {
        fn nested(leaf: i64) -> Value {
            json!({"a":{"b":{"c":{"d":{"e":{"f":{"g": leaf}}}}}}})
        }
        let changes = definition_changes(&nested(1), &nested(2));
        assert_eq!(changes.len(), 1, "超过深度上限的子树整体输出: {changes:?}");
        assert_eq!(changes[0]["field"], json!("a.b.c.d.e.f"));
        assert_eq!(changes[0]["before"]["g"], json!(1));
        assert_eq!(changes[0]["after"]["g"], json!(2));
    }

    #[test]
    fn group_writes_emit_admin_change_audit() {
        let audit = AuditLog::open(":memory:").unwrap();
        let s = CapabilityStore::open(":memory:", Some(std::sync::Arc::new(audit))).unwrap();
        s.insert_managed_group(&managed("db.read.audited", 5, 800, &["analyst"]), "alice")
            .unwrap();
        s.update_managed_group(&managed("db.read.audited", 6, 800, &["analyst"]), "bob")
            .unwrap();
        s.remove_managed_group("db.read.audited", "alice").unwrap();
        let rows = s.audit.as_ref().unwrap().query(None).unwrap();
        let changes: Vec<_> = rows
            .iter()
            .filter(|e| e.event_type == "admin_change")
            .collect();
        assert_eq!(changes.len(), 3, "create/update/delete 各一条");
        assert_eq!(changes[0].payload["object_type"], json!("group"));
        // 审计 object_id 仍是 code；data 里带 code/name/roles。
        assert_eq!(changes[0].payload["object_id"], json!("db.read.audited"));
        assert_eq!(changes[0].payload["action"], json!("group_create"));
        assert_eq!(changes[0].actor["id"], json!("alice"));
        assert_eq!(
            changes[0].payload["data"]["group"]["code"],
            json!("db.read.audited")
        );
        assert_eq!(
            changes[0].payload["data"]["group"]["name"],
            json!("db.read.audited 展示名"),
            "组审计 data 必须带展示名"
        );
        assert_eq!(
            changes[0].payload["data"]["group"]["roles"],
            json!(["analyst"])
        );
        let field = |row: &Value, name: &str| {
            row["changes"]
                .as_array()
                .and_then(|c| c.iter().find(|x| x["field"] == json!(name)))
                .cloned()
        };
        let created_name = field(&changes[0].payload, "name").expect("create 审计含 name");
        assert_eq!(created_name["before"], Value::Null);
        assert_eq!(created_name["after"], json!("db.read.audited 展示名"));
        let created_priority =
            field(&changes[0].payload, "priority").expect("create 审计含 priority");
        assert_eq!(created_priority["before"], Value::Null);
        assert_eq!(created_priority["after"], json!(5));
        assert_eq!(changes[1].payload["action"], json!("group_update"));
        assert_eq!(changes[1].payload["object_id"], json!("db.read.audited"));
        let updated_priority =
            field(&changes[1].payload, "priority").expect("update 审计含 priority");
        assert_eq!(updated_priority["before"], json!(5));
        assert_eq!(updated_priority["after"], json!(6));
        assert_eq!(changes[2].payload["action"], json!("group_delete"));
        let deleted_priority =
            field(&changes[2].payload, "priority").expect("delete 审计含 priority");
        assert_eq!(deleted_priority["after"], Value::Null);
        for change in changes {
            assert_eq!(change.payload["outcome"], json!("ok"));
            assert!(
                change.payload["changes"]
                    .as_array()
                    .is_some_and(|c| !c.is_empty())
            );
            assert_eq!(change.payload["approval"], Value::Null);
        }
    }

    #[test]
    fn draft_revision_bumps_version_and_keeps_status() {
        let s = store();
        s.register(
            "cap1",
            "tool",
            "db.read.core",
            "read",
            json!({"description":"v1"}),
        )
        .unwrap();
        let r = s
            .revise_draft_definition(
                "cap1",
                json!({"description":"v2"}),
                "draft-edit:alice",
                "alice",
            )
            .unwrap();
        assert_eq!(r.version, 2);
        assert_eq!(r.status, "draft");
        assert_eq!(
            s.get_version("cap1", 1).unwrap().unwrap().definition["description"],
            json!("v1"),
            "旧版本必须原样保留（append-only）"
        );
        // 草案编辑不再要求 approval_id；空票号原样留在版本行，不伪造审批。
        let r = s
            .revise_draft_definition("cap1", json!({"description":"v3"}), "", "alice")
            .unwrap();
        assert_eq!(r.version, 3);
        let v3 = s.versions("cap1").unwrap().pop().unwrap();
        assert_eq!(v3.approval_id, "");
        assert_eq!(v3.status, "draft");
        assert!(v3.approved_by.is_empty());
        assert!(!v3.created_at.is_empty());
        // 非 draft 状态拒绝。
        s.submit_for_review("cap1", "alice").unwrap();
        let e = s
            .revise_draft_definition("cap1", json!({"description":"v4"}), "ap", "alice")
            .unwrap_err();
        assert_eq!(e.code, "MCP_STATE_TRANSITION_INVALID");
    }

    #[test]
    fn draft_revision_rejects_builtin() {
        let s = store();
        for id in BUILTIN_CAPABILITY_IDS {
            let e = s
                .revise_draft_definition(id, json!({}), "ap", "alice")
                .unwrap_err();
            assert_eq!(e.code, "MCP_BUILTIN_IMMUTABLE");
        }
    }
}
