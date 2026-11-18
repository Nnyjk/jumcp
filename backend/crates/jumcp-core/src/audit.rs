//! 追加审计：单表 + 哈希链 + trigger 禁 UPDATE/DELETE。
//! 为什么 trigger：防应用 bug/误操作改写历史，应用层不提供更新/删除 API。

use crate::canonical::{canonical_json, hash_concat};
use crate::errors::McpError;
use chrono::Utc;
use rusqlite::{Connection, Row};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::sync::Mutex;

pub const EVENT_TYPES: &[&str] = &[
    "tools_list_served",
    "capability_search",
    "capability_describe",
    "tool_call",
    "template_run",
    "policy_decision",
    "admin_change",
];

/// `admin_change` 的最小可读契约：主体/动作/对象/结果四件套必须显式给出。
///
/// 定义哈希、前后态、审批等属于事件语义，缺省用 `null`，不用哨兵字符串。
pub const ADMIN_REQUIRED_FIELDS: &[&str] = &["object_type", "object_id", "action", "outcome"];

/// 业务载荷结构版本（固定值）。能力/模板自身的版本在 `payload.data.version`。
pub const PAYLOAD_VERSION: i64 = 1;

/// 统一载荷骨架：所有 event_type 共用同一组键，缺省值一律 `null`/`[]`。
///
/// 为什么集中在这里：写入点分散在 store / mcp / admin 三处，各拼一份 JSON 必然漂移。
pub fn payload(
    object_type: &str,
    object_id: &str,
    action: &str,
    outcome: &str,
) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("object_type".into(), json!(object_type));
    map.insert("object_id".into(), json!(object_id));
    map.insert("action".into(), json!(action));
    map.insert("outcome".into(), json!(outcome));
    map.insert("error_code".into(), Value::Null);
    map.insert("error_message".into(), Value::Null);
    map.insert("from_status".into(), Value::Null);
    map.insert("to_status".into(), Value::Null);
    map.insert("changes".into(), json!([]));
    map.insert("approval".into(), Value::Null);
    map.insert("reason".into(), Value::Null);
    map.insert("schema_version".into(), json!(PAYLOAD_VERSION));
    map.insert("correlation_id".into(), Value::Null);
    map.insert("data".into(), json!({}));
    map
}

/// 外部审批引用（当前没有外部审批系统：`verified=false` 如实标注，不伪造已验证）。
pub fn external_approval(
    approval_id: &str,
    submitted_by: &str,
    approved_by: &str,
    approved_at: &str,
) -> Value {
    json!({
        "id": approval_id,
        "source": "external",
        "verified": false,
        "submitted_by": if submitted_by.is_empty() { Value::Null } else { json!(submitted_by) },
        "approved_by": if approved_by.is_empty() { Value::Null } else { json!(approved_by) },
        "approved_at": if approved_at.is_empty() { Value::Null } else { json!(approved_at) }
    })
}

/// 审计分页顺序：默认 `desc`（最新优先），`asc` 用于回放/定位。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    Asc,
    Desc,
}

impl Order {
    pub fn parse(raw: &str) -> Result<Self, McpError> {
        match raw {
            "asc" => Ok(Self::Asc),
            "desc" => Ok(Self::Desc),
            _ => Err(McpError::new(
                "MCP_BAD_REQUEST",
                json!({"field": "order", "value": raw, "supported": ["asc", "desc"]}),
            )),
        }
    }

    fn sql(self) -> &'static str {
        match self {
            Self::Asc => "ASC",
            Self::Desc => "DESC",
        }
    }
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS audit_events (
    event_id TEXT PRIMARY KEY, ts TEXT NOT NULL, tenant TEXT NOT NULL,
    actor_json TEXT NOT NULL, session_id TEXT NOT NULL, event_type TEXT NOT NULL,
    payload TEXT NOT NULL, prev_hash TEXT NOT NULL, hash TEXT NOT NULL,
    surface TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_audit_tenant_ts ON audit_events(tenant, ts);
CREATE INDEX IF NOT EXISTS idx_audit_type_ts ON audit_events(event_type, ts);
CREATE TRIGGER IF NOT EXISTS trg_audit_no_update BEFORE UPDATE ON audit_events BEGIN SELECT RAISE(ABORT, 'audit_events is append-only'); END;
CREATE TRIGGER IF NOT EXISTS trg_audit_no_delete BEFORE DELETE ON audit_events BEGIN SELECT RAISE(ABORT, 'audit_events is append-only'); END;
"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActorRef {
    pub actor_type: String,
    pub id: String,
    pub role: String,
    /// 派生身份的数据库 token id（可选；stdio / 管理面静态 token 不落该键）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_id: Option<String>,
    /// 调用来源（可选）：MCP token 的配置值 / 管理面 `admin-api`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}
impl Default for ActorRef {
    fn default() -> Self {
        Self {
            actor_type: "agent".into(),
            id: String::new(),
            role: String::new(),
            token_id: None,
            source: None,
        }
    }
}

impl ActorRef {
    /// agent 主体（tool_call / policy_decision / capability_search 等运行期事件）。
    pub fn agent(id: &str, role: &str) -> Self {
        Self {
            actor_type: "agent".into(),
            id: id.into(),
            role: role.into(),
            token_id: None,
            source: None,
        }
    }
    /// 管理面主体：`source` 固定 `admin-api`（来源与调用者在审计里可分辨）。
    pub fn admin(id: &str) -> Self {
        Self {
            actor_type: "admin".into(),
            id: id.into(),
            role: "admin".into(),
            token_id: None,
            source: Some("admin-api".into()),
        }
    }
    pub fn with_token(mut self, token_id: &str, source: &str) -> Self {
        if !token_id.is_empty() {
            self.token_id = Some(token_id.into());
        }
        if !source.is_empty() {
            self.source = Some(source.into());
        }
        self
    }
}

/// 审计主体的轻量描述：让 `CapabilityStore` 写 `admin_change` 时不必关心调用来自
/// HTTP 管理面还是 MCP token（来源/调用者/token_id 由这里统一带出）。
#[derive(Debug, Clone, Default)]
pub struct AuditOrigin {
    pub id: String,
    pub source: Option<String>,
    pub token_id: Option<String>,
}

/// store 写审计的 actor 参数：`&str` 只给 id（低层/测试），`&Actor` 带全量来源信息。
pub trait IntoAuditOrigin {
    fn into_audit_origin(self) -> AuditOrigin;
}

impl IntoAuditOrigin for &str {
    fn into_audit_origin(self) -> AuditOrigin {
        AuditOrigin {
            id: self.to_string(),
            source: Some("admin-api".into()),
            token_id: None,
        }
    }
}

impl IntoAuditOrigin for String {
    fn into_audit_origin(self) -> AuditOrigin {
        self.as_str().into_audit_origin()
    }
}

impl IntoAuditOrigin for &crate::entry::Actor {
    fn into_audit_origin(self) -> AuditOrigin {
        AuditOrigin {
            id: self.actor_id.clone(),
            source: (!self.source.is_empty()).then(|| self.source.clone()),
            token_id: (!self.token_id.is_empty()).then(|| self.token_id.clone()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AuditEvent {
    pub tenant: String,
    pub actor: ActorRef,
    pub event_type: String,
    pub payload: Value,
    pub session_id: String,
    pub ts: String,
    pub event_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainVerdict {
    Valid,
    Tampered { event_id: String },
    Broken { reason: String },
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Envelope {
    pub event_id: String,
    pub ts: String,
    pub tenant: String,
    pub actor: Value,
    pub session_id: String,
    pub event_type: String,
    pub payload: Value,
    pub prev_hash: String,
    pub hash: String,
    /// 事件来自哪个面：`admin` / `mcp`（旧行为空串）。**不参与哈希**，见 [`surface_of`]。
    #[serde(default)]
    pub surface: String,
}
impl Envelope {
    pub fn without_hash(&self) -> Value {
        json!({"event_id": self.event_id, "ts": self.ts, "tenant": self.tenant, "actor": self.actor, "session_id": self.session_id, "event_type": self.event_type, "payload": self.payload, "prev_hash": self.prev_hash})
    }
}

pub struct AuditLog {
    conn: Mutex<Connection>,
}

impl AuditLog {
    pub fn open(path: &str) -> Result<Self, McpError> {
        if path != ":memory:"
            && let Some(parent) = std::path::Path::new(path).parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| McpError::audit(e.to_string()))?;
        }
        let conn = Connection::open(path).map_err(|e| McpError::audit(e.to_string()))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
            .map_err(|e| McpError::audit(e.to_string()))?;
        conn.execute_batch(SCHEMA)
            .map_err(|e| McpError::audit(e.to_string()))?;
        // 旧库补列表：`CREATE TABLE IF NOT EXISTS` 不会给已存在的表加列。
        // 历史行的 surface 保持空串（迁移不重写审计行，也不动哈希链），统计侧把空串视为非平台面。
        ensure_surface_column(&conn)?;
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_audit_surface_ts ON audit_events(surface, ts);",
        )
        .map_err(|e| McpError::audit(e.to_string()))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn append(&self, event: AuditEvent) -> Result<Envelope, McpError> {
        if !EVENT_TYPES.contains(&event.event_type.as_str()) {
            return Err(McpError::audit(format!(
                "event_type {:?} 不在闭集中",
                event.event_type
            )));
        }
        if event.event_type == "admin_change" {
            for f in ADMIN_REQUIRED_FIELDS {
                let v = event
                    .payload
                    .get(*f)
                    .and_then(|x| x.as_str())
                    .unwrap_or_default();
                if v.is_empty() {
                    return Err(McpError::audit(format!("admin_change 必带字段缺失: {f}")));
                }
            }
        }
        let conn = self.conn.lock().map_err(|_| McpError::lock())?;
        // 空表合法返回空种子；查询出错必须上抛，禁止把 prev_hash 重置成空导致链分叉。
        let prev_hash: String = match conn.query_row(
            "SELECT hash FROM audit_events ORDER BY rowid DESC LIMIT 1",
            [],
            |r| r.get(0),
        ) {
            Ok(hash) => hash,
            Err(rusqlite::Error::QueryReturnedNoRows) => String::new(),
            Err(e) => return Err(McpError::audit(e.to_string())),
        };
        let event_id = if event.event_id.is_empty() {
            uuid::Uuid::new_v4().simple().to_string()
        } else {
            event.event_id.clone()
        };
        let ts = if event.ts.is_empty() {
            Utc::now().to_rfc3339()
        } else {
            event.ts.clone()
        };
        let surface = surface_of(&event.actor);
        let mut actor =
            json!({"type": event.actor.actor_type, "id": event.actor.id, "role": event.actor.role});
        if let Some(obj) = actor.as_object_mut() {
            if let Some(token_id) = &event.actor.token_id {
                obj.insert("token_id".into(), json!(token_id));
            }
            if let Some(source) = &event.actor.source {
                obj.insert("source".into(), json!(source));
            }
        }
        let draft = Envelope {
            event_id: event_id.clone(),
            ts: ts.clone(),
            tenant: event.tenant.clone(),
            actor: actor.clone(),
            session_id: event.session_id.clone(),
            event_type: event.event_type.clone(),
            payload: event.payload.clone(),
            prev_hash: prev_hash.clone(),
            hash: String::new(),
            surface: surface.to_string(),
        };
        let hash = hash_concat(&draft.without_hash(), &prev_hash);
        let env = Envelope {
            hash: hash.clone(),
            ..draft
        };
        conn.execute("INSERT INTO audit_events (event_id, ts, tenant, actor_json, session_id, event_type, payload, prev_hash, hash, surface) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            rusqlite::params![env.event_id, env.ts, env.tenant, canonical_json(&env.actor), env.session_id, env.event_type, canonical_json(&env.payload), env.prev_hash, env.hash, env.surface])
            .map_err(|e| McpError::audit(e.to_string()))?;
        Ok(env)
    }

    pub fn verify_chain(&self) -> Result<ChainVerdict, McpError> {
        let conn = self.conn.lock().map_err(|_| McpError::lock())?;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {COLUMNS} FROM audit_events ORDER BY rowid ASC"
            ))
            .map_err(|e| McpError::audit(e.to_string()))?;
        let rows_result = stmt.query_map([], row_to_envelope);
        let mapped = rows_result.map_err(|e| McpError::audit(e.to_string()))?;
        let mut rows: Vec<Envelope> = Vec::new();
        for row in mapped {
            let env = row.map_err(|e| McpError::audit(e.to_string()))?;
            rows.push(env);
        }
        let mut prev = String::new();
        for env in rows {
            if env.prev_hash != prev {
                return Ok(ChainVerdict::Tampered {
                    event_id: env.event_id,
                });
            }
            if hash_concat(&env.without_hash(), &env.prev_hash) != env.hash {
                return Ok(ChainVerdict::Tampered {
                    event_id: env.event_id,
                });
            }
            prev = env.hash;
        }
        Ok(ChainVerdict::Valid)
    }

    pub fn query(&self, filter: Option<&Value>) -> Result<Vec<Envelope>, McpError> {
        let (clauses, params) = build_filter(filter)?;
        let where_clause = if clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", clauses.join(" AND "))
        };
        let sql = format!(
            "SELECT {} FROM audit_events {where_clause} ORDER BY rowid ASC",
            COLUMNS
        );
        let conn = self.conn.lock().map_err(|_| McpError::lock())?;
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| McpError::audit(e.to_string()))?;
        let bound: Vec<Box<dyn rusqlite::ToSql>> = params
            .iter()
            .map(|p| Box::new(p.clone()) as Box<dyn rusqlite::ToSql>)
            .collect();
        let refs: Vec<&dyn rusqlite::ToSql> = bound
            .iter()
            .map(|b| b.as_ref() as &dyn rusqlite::ToSql)
            .collect();
        let mapped = stmt
            .query_map(refs.as_slice(), row_to_envelope)
            .map_err(|e| McpError::audit(e.to_string()))?;
        let mut rows: Vec<Envelope> = Vec::new();
        for row in mapped {
            rows.push(row.map_err(|e| McpError::audit(e.to_string()))?);
        }
        Ok(rows)
    }

    /// 分页查询（管理面）：同一套过滤语义 + COUNT(*) 给 total。
    /// 过滤键不认识时报 MCP_FILTER_UNSUPPORTED（调用方据此回 400），不静默忽略。
    pub fn query_page(
        &self,
        filter: Option<&Value>,
        limit: i64,
        offset: i64,
        order: Order,
    ) -> Result<(Vec<Envelope>, i64), McpError> {
        let (clauses, params) = build_filter(filter)?;
        let where_clause = if clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", clauses.join(" AND "))
        };
        let conn = self.conn.lock().map_err(|_| McpError::lock())?;
        let count_sql = format!("SELECT COUNT(*) FROM audit_events {where_clause}");
        let bound_count: Vec<Box<dyn rusqlite::ToSql>> = params
            .iter()
            .map(|p| Box::new(p.clone()) as Box<dyn rusqlite::ToSql>)
            .collect();
        let refs_count: Vec<&dyn rusqlite::ToSql> = bound_count
            .iter()
            .map(|b| b.as_ref() as &dyn rusqlite::ToSql)
            .collect();
        let total: i64 = conn
            .query_row(&count_sql, refs_count.as_slice(), |r| r.get(0))
            .map_err(|e| McpError::audit(e.to_string()))?;

        let sql = format!(
            "SELECT {} FROM audit_events {where_clause} ORDER BY rowid {} LIMIT ? OFFSET ?",
            COLUMNS,
            order.sql()
        );
        let mut bound: Vec<Box<dyn rusqlite::ToSql>> = params
            .iter()
            .map(|p| Box::new(p.clone()) as Box<dyn rusqlite::ToSql>)
            .collect();
        bound.push(Box::new(limit));
        bound.push(Box::new(offset));
        let refs: Vec<&dyn rusqlite::ToSql> = bound
            .iter()
            .map(|b| b.as_ref() as &dyn rusqlite::ToSql)
            .collect();
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| McpError::audit(e.to_string()))?;
        let mapped = stmt
            .query_map(refs.as_slice(), row_to_envelope)
            .map_err(|e| McpError::audit(e.to_string()))?;
        let mut rows: Vec<Envelope> = Vec::new();
        for row in mapped {
            rows.push(row.map_err(|e| McpError::audit(e.to_string()))?);
        }
        Ok((rows, total))
    }
}

const COLUMNS: &str =
    "event_id, ts, tenant, actor_json, session_id, event_type, payload, prev_hash, hash, surface";

/// 过滤键白名单 → (SQL 片段, 绑定值)。未知键报 MCP_FILTER_UNSUPPORTED（管理面据 400，而不是静默忽略）。
fn build_filter(filter: Option<&Value>) -> Result<(Vec<String>, Vec<String>), McpError> {
    let mut clauses = Vec::new();
    let mut params = Vec::new();
    let Some(obj) = filter.and_then(|f| f.as_object()) else {
        return Ok((clauses, params));
    };
    for (k, v) in obj {
        let vector: Vec<String> = match v.as_array() {
            Some(items) => items
                .iter()
                .filter_map(|i| i.as_str().map(String::from))
                .collect(),
            None => vec![v.as_str().unwrap_or_default().to_string()],
        };
        if vector.is_empty() {
            continue;
        }
        let placeholders = vec!["?"; vector.len()].join(",");
        let clause = match k.as_str() {
            "tenant" => format!("tenant IN ({placeholders})"),
            "event_type" => format!("event_type IN ({placeholders})"),
            "event_id" => format!("event_id IN ({placeholders})"),
            "object_type" => {
                format!("json_extract(payload, '$.object_type') IN ({placeholders})")
            }
            "object_id" => format!("json_extract(payload, '$.object_id') IN ({placeholders})"),
            "actor_id" => format!("json_extract(actor_json, '$.id') IN ({placeholders})"),
            "actor_role" => format!("json_extract(actor_json, '$.role') IN ({placeholders})"),
            "token_id" => format!("json_extract(actor_json, '$.token_id') IN ({placeholders})"),
            // 面标记：管理面要能按面筛（surface=admin 只看平台操作），统计侧用 surface_not 排除。
            "surface" => format!("surface IN ({placeholders})"),
            "surface_not" => format!("surface NOT IN ({placeholders})"),
            // 时间键是区间而不是集合：每个取值单独成一条 AND 条件，占位符数量与绑定值一一对应。
            "ts_after" => {
                clauses.extend(std::iter::repeat_n("ts >= ?".to_string(), vector.len()));
                params.extend(vector);
                continue;
            }
            "ts_before" => {
                clauses.extend(std::iter::repeat_n("ts <= ?".to_string(), vector.len()));
                params.extend(vector);
                continue;
            }
            _ => {
                return Err(McpError::new(
                    "MCP_FILTER_UNSUPPORTED",
                    serde_json::json!({"filter": k, "supported": ["tenant","event_type","event_id","object_type","object_id","actor_id","actor_role","token_id","surface","ts_after","ts_before"]}),
                ));
            }
        };
        clauses.push(clause);
        params.extend(vector);
    }
    Ok((clauses, params))
}

/// 事件来自哪个面：`admin`（管理面 `/admin/*` 与平台启动期）或 `mcp`（`/mcp`、内置端点、stdio）。
///
/// 由 actor 类型派生，而不是给每个调用点加参数：管理面主体只有 `ActorRef::admin` 与
/// `jumcp-server` 的 `admin_actor` 两条构造路径，派生点唯一；20 多处 store 写审计的位置
/// 无需逐一改签名（漏改一个就会让平台操作混进统计）。
///
/// surface **不参与哈希**（[`Envelope::without_hash`] 的键集合保持不变）：把它并入哈希材料
/// 会让所有历史链校验失败。列本身仍受 append-only 触发器保护，不能 UPDATE/DELETE。
pub fn surface_of(actor: &ActorRef) -> &'static str {
    if actor.actor_type == "admin" {
        "admin"
    } else {
        "mcp"
    }
}

/// 旧库补列：`surface` 列在 2026-11 新增，历史行保持空串（不重写、不重算哈希）。
fn ensure_surface_column(conn: &Connection) -> Result<(), McpError> {
    let mut stmt = conn
        .prepare("PRAGMA table_info(audit_events)")
        .map_err(|e| McpError::audit(e.to_string()))?;
    let columns: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(|e| McpError::audit(e.to_string()))?
        .filter_map(Result::ok)
        .collect();
    if columns.iter().any(|c| c == "surface") {
        return Ok(());
    }
    conn.execute(
        "ALTER TABLE audit_events ADD COLUMN surface TEXT NOT NULL DEFAULT ''",
        [],
    )
    .map_err(|e| McpError::audit(e.to_string()))?;
    Ok(())
}

fn row_to_envelope(row: &Row) -> rusqlite::Result<Envelope> {
    let actor_json: String = row.get(3)?;
    let payload_json: String = row.get(6)?;
    let actor = serde_json::from_str(&actor_json).map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "actor json decode failed",
            )),
        )
    })?;
    let payload = serde_json::from_str(&payload_json).map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            6,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "payload json decode failed",
            )),
        )
    })?;
    Ok(Envelope {
        event_id: row.get(0)?,
        ts: row.get(1)?,
        tenant: row.get(2)?,
        actor,
        session_id: row.get(4)?,
        event_type: row.get(5)?,
        payload,
        prev_hash: row.get(7)?,
        hash: row.get(8)?,
        surface: row.get(9)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ev() -> AuditEvent {
        AuditEvent {
            tenant: "t1".into(),
            actor: ActorRef {
                actor_type: "agent".into(),
                id: "a-1".into(),
                role: "analyst".into(),
                token_id: None,
                source: None,
            },
            event_type: "tool_call".into(),
            payload: json!({"tool":"db_query_readonly"}),
            session_id: "s-1".into(),
            ts: "2026-11-05T00:00:00+00:00".into(),
            event_id: "e1".into(),
        }
    }
    #[test]
    fn chain_and_trigger() {
        let log = AuditLog::open(":memory:").unwrap();
        let e1 = log.append(ev()).unwrap();
        assert_eq!(e1.prev_hash, "");
        assert_eq!(e1.hash.len(), 64);
        assert_eq!(log.verify_chain().unwrap(), ChainVerdict::Valid);
        let conn = log.conn.lock().unwrap();
        assert!(
            conn.execute("UPDATE audit_events SET tenant='x'", [])
                .is_err()
        );
    }
    #[test]
    fn admin_requires_subject_action_object_outcome() {
        let log = AuditLog::open(":memory:").unwrap();
        let mut e = ev();
        e.event_type = "admin_change".into();
        e.payload = json!({"object_type":"capability","object_id":"c1","action":"create"});
        assert!(log.append(e).is_err(), "outcome 必填");
        let mut ok = ev();
        ok.event_type = "admin_change".into();
        ok.payload = Value::Object(payload("capability", "c1", "create", "ok"));
        let env = log.append(ok).unwrap();
        assert_eq!(env.payload["schema_version"], json!(1));
        assert!(
            env.payload.get("version").is_none(),
            "顶层 version 已改名为 schema_version，不留别名"
        );
        assert_eq!(env.payload["data"], json!({}));
    }
    #[test]
    fn object_id_filter_uses_payload_and_default_query_is_asc() {
        let log = AuditLog::open(":memory:").unwrap();
        for (id, obj) in [("e1", "cap.a"), ("e2", "cap.b")] {
            let mut event = ev();
            event.event_id = id.into();
            event.payload = json!({
                "object_type":"capability","object_id":obj,"action":"create","outcome":"ok"
            });
            log.append(event).unwrap();
        }
        let filter = json!({"object_id":"cap.b"});
        let (rows, total) = log.query_page(Some(&filter), 10, 0, Order::Desc).unwrap();
        assert_eq!(total, 1);
        assert_eq!(rows[0].event_id, "e2");
        let (all, _) = log.query_page(None, 10, 0, Order::Desc).unwrap();
        assert_eq!(all[0].event_id, "e2", "desc 默认最新优先");
        let (asc, _) = log.query_page(None, 10, 0, Order::Asc).unwrap();
        assert_eq!(asc[0].event_id, "e1");
    }
    #[test]
    fn unknown_event_rejected() {
        let log = AuditLog::open(":memory:").unwrap();
        let mut e = ev();
        e.event_type = "bogus".into();
        assert!(log.append(e).is_err());
    }
    #[test]
    fn verify_chain_empty_is_valid() {
        let log = AuditLog::open(":memory:").unwrap();
        assert_eq!(log.verify_chain().unwrap(), ChainVerdict::Valid);
    }
    #[test]
    fn db_failure_returns_err_not_false_or_empty() {
        let log = AuditLog::open(":memory:").unwrap();
        {
            let conn = log.conn.lock().unwrap();
            drop(conn);
        }
        // 人为破坏表模拟真实 DB 失败：execute 建表副本场景见下方 tampered 测试；这里用删表触发 prepare 失败。
        let conn = log.conn.lock().unwrap();
        let _ = conn.execute("DROP TABLE audit_events", []);
        drop(conn);
        assert!(log.verify_chain().is_err());
        assert!(log.append(ev()).is_err());
    }
    #[test]
    fn tampered_row_returns_tampered_with_event_id() {
        // trigger 会阻止 UPDATE，测试用只读副本表复现篡改场景（删 trigger 的等价效果）。
        let log = AuditLog::open(":memory:").unwrap();
        let mut e2 = ev();
        e2.event_id = "e2".into();
        e2.payload = json!({"tool":"db_query_readonly","i":2});
        e2.ts = "2026-11-05T00:00:01+00:00".into();
        log.append(ev()).unwrap();
        log.append(e2).unwrap();
        assert_eq!(log.verify_chain().unwrap(), ChainVerdict::Valid);
        {
            let conn = log.conn.lock().unwrap();
            let _ = conn.execute("DROP TRIGGER trg_audit_no_update", []);
            let _ = conn.execute("DROP TRIGGER trg_audit_no_delete", []);
            conn.execute(
                "UPDATE audit_events SET payload=? WHERE event_id='e2'",
                rusqlite::params![r#"{"tool":"forged"}"#],
            )
            .unwrap();
        }
        assert_eq!(
            log.verify_chain().unwrap(),
            ChainVerdict::Tampered {
                event_id: "e2".into()
            }
        );
    }

    /// 时间键是区间：多个取值时占位符数量必须与绑定值一一对应，否则查询直接报错。
    #[test]
    fn time_range_filter_accepts_multiple_bounds() {
        let log = AuditLog::open(":memory:").unwrap();
        for (id, ts) in [
            ("e1", "2026-01-01T00:00:00+00:00"),
            ("e2", "2026-02-01T00:00:00+00:00"),
            ("e3", "2026-03-01T00:00:00+00:00"),
        ] {
            let mut event = ev();
            event.event_id = id.into();
            event.ts = ts.into();
            log.append(event).unwrap();
        }
        let filter = json!({"ts_after": [
            "2026-01-01T00:00:00+00:00",
            "2026-02-01T00:00:00+00:00"
        ]});
        let (rows, total) = log.query_page(Some(&filter), 10, 0, Order::Asc).unwrap();
        assert_eq!(total, 2);
        assert_eq!(rows[0].event_id, "e2");
        assert_eq!(rows[1].event_id, "e3");
        let filter = json!({
            "ts_after": "2026-02-01T00:00:00+00:00",
            "ts_before": "2026-02-01T00:00:00+00:00"
        });
        let (rows, total) = log.query_page(Some(&filter), 10, 0, Order::Asc).unwrap();
        assert_eq!(total, 1);
        assert_eq!(rows[0].event_id, "e2");
    }

    /// prev_hash 读不出来时必须上抛，不能退回空种子让链分叉。
    #[test]
    fn broken_chain_read_is_fatal() {
        let log = AuditLog::open(":memory:").unwrap();
        log.append(ev()).unwrap();
        {
            let conn = log.conn.lock().unwrap();
            conn.execute("DROP TABLE audit_events", []).unwrap();
        }
        assert!(log.append(ev()).is_err());
    }

    /// surface 由主体类型派生：agent 事件落 mcp，管理面事件落 admin。
    #[test]
    fn surface_is_derived_from_actor_type() {
        let log = AuditLog::open(":memory:").unwrap();
        let mut agent = ev();
        agent.event_id = "e1".into();
        assert_eq!(log.append(agent).unwrap().surface, "mcp");
        let mut admin = ev();
        admin.event_id = "e2".into();
        admin.actor = ActorRef::admin("alice");
        admin.event_type = "admin_change".into();
        admin.payload = Value::Object(payload("capability", "c1", "create", "ok"));
        assert_eq!(log.append(admin).unwrap().surface, "admin");
        let (rows, total) = log
            .query_page(Some(&json!({"surface": "admin"})), 10, 0, Order::Asc)
            .unwrap();
        assert_eq!(total, 1);
        assert_eq!(rows[0].event_id, "e2");
        let (rows, _) = log
            .query_page(Some(&json!({"surface_not": "admin"})), 10, 0, Order::Asc)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].event_id, "e1");
        // surface 不参与哈希：老链（无 surface 概念）仍校验通过。
        assert_eq!(log.verify_chain().unwrap(), ChainVerdict::Valid);
    }

    /// 旧库（无 surface 列、已有哈希链）打开时必须补列且不破坏历史链。
    #[test]
    fn old_database_gets_surface_column_without_breaking_chain() {
        let dir = std::env::temp_dir().join(format!("jumcp-audit-mig-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("audit.sqlite3");
        let path_str = path.to_str().unwrap().to_string();
        {
            // 旧 schema：没有 surface 列的等价建表语句 + 一行历史事件（哈希按当前算法生成）。
            let log = AuditLog::open(&path_str).unwrap();
            log.append(ev()).unwrap();
            let conn = log.conn.lock().unwrap();
            conn.execute("DROP INDEX IF EXISTS idx_audit_surface_ts", [])
                .unwrap();
            // SQLite 3.35+ 支持 DROP COLUMN；这里直接重建不含 surface 的表。
            conn.execute_batch(
                r#"CREATE TABLE legacy_audit_events AS SELECT event_id, ts, tenant, actor_json, session_id, event_type, payload, prev_hash, hash FROM audit_events;
DROP TABLE audit_events;
ALTER TABLE legacy_audit_events RENAME TO audit_events;"#,
            )
            .unwrap();
        }
        let log = AuditLog::open(&path_str).unwrap();
        let (rows, total) = log.query_page(None, 10, 0, Order::Asc).unwrap();
        assert_eq!(total, 1, "历史行不能被迁移丢掉");
        assert_eq!(rows[0].surface, "", "历史行 surface 为空串（未重写）");
        assert_eq!(log.verify_chain().unwrap(), ChainVerdict::Valid);
        let mut next = ev();
        next.event_id = "e2".into();
        next.ts = "2026-11-05T00:00:01+00:00".into();
        assert_eq!(log.append(next).unwrap().surface, "mcp");
        assert_eq!(log.verify_chain().unwrap(), ChainVerdict::Valid);
        std::fs::remove_dir_all(&dir).ok();
    }
}
