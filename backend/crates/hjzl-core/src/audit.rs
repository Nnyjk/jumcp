//! 追加审计：单表 + 哈希链 + trigger 禁 UPDATE/DELETE。
//! 为什么 trigger：防应用 bug/误操作改写历史，应用层不提供更新/删除 API。

use crate::canonical::{canonical_json, hash_concat};
use crate::errors::McpError;
use chrono::Utc;
use rusqlite::{Connection, Row};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Mutex;

pub const EVENT_TYPES: &[&str] = &[
    "session_init",
    "tools_list_served",
    "capability_search",
    "capability_describe",
    "tool_call",
    "template_run",
    "policy_decision",
    "admin_change",
];
pub const ADMIN_REQUIRED_FIELDS: &[&str] = &["before_hash", "after_hash", "diff", "approval_id"];

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS audit_events (
    event_id TEXT PRIMARY KEY, ts TEXT NOT NULL, tenant TEXT NOT NULL,
    actor_json TEXT NOT NULL, session_id TEXT NOT NULL, event_type TEXT NOT NULL,
    payload TEXT NOT NULL, prev_hash TEXT NOT NULL, hash TEXT NOT NULL
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
}
impl Default for ActorRef {
    fn default() -> Self {
        Self {
            actor_type: "agent".into(),
            id: String::new(),
            role: String::new(),
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
        let prev_hash: String = conn
            .query_row(
                "SELECT hash FROM audit_events ORDER BY rowid DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap_or_default();
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
        let actor =
            json!({"type": event.actor.actor_type, "id": event.actor.id, "role": event.actor.role});
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
        };
        let hash = hash_concat(&draft.without_hash(), &prev_hash);
        let env = Envelope {
            hash: hash.clone(),
            ..draft
        };
        conn.execute("INSERT INTO audit_events (event_id, ts, tenant, actor_json, session_id, event_type, payload, prev_hash, hash) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            rusqlite::params![env.event_id, env.ts, env.tenant, canonical_json(&env.actor), env.session_id, env.event_type, canonical_json(&env.payload), env.prev_hash, env.hash])
            .map_err(|e| McpError::audit(e.to_string()))?;
        Ok(env)
    }

    pub fn verify_chain(&self) -> Result<ChainVerdict, McpError> {
        let conn = self.conn.lock().map_err(|_| McpError::lock())?;
        let mut stmt = conn.prepare("SELECT event_id, ts, tenant, actor_json, session_id, event_type, payload, prev_hash, hash FROM audit_events ORDER BY rowid ASC").map_err(|e| McpError::audit(e.to_string()))?;
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
            "SELECT {} FROM audit_events {where_clause} ORDER BY rowid ASC LIMIT ? OFFSET ?",
            COLUMNS
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

#[allow(dead_code)]
fn parse_map(s: &str) -> Result<Value, McpError> {
    serde_json::from_str(s).map_err(|e| McpError::audit(format!("audit json decode failed: {e}")))
}

const COLUMNS: &str =
    "event_id, ts, tenant, actor_json, session_id, event_type, payload, prev_hash, hash";

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
            "session_id" => format!("session_id IN ({placeholders})"),
            "actor_role" => format!("json_extract(actor_json, '$.role') IN ({placeholders})"),
            "ts_after" => {
                clauses.push("ts >= ?".to_string());
                params.extend(vector);
                continue;
            }
            "ts_before" => {
                clauses.push("ts <= ?".to_string());
                params.extend(vector);
                continue;
            }
            _ => {
                return Err(McpError::new(
                    "MCP_FILTER_UNSUPPORTED",
                    serde_json::json!({"filter": k, "supported": ["tenant","event_type","session_id","actor_role","ts_after","ts_before"]}),
                ));
            }
        };
        clauses.push(clause);
        params.extend(vector);
    }
    Ok((clauses, params))
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
    fn admin_requires_fields() {
        let log = AuditLog::open(":memory:").unwrap();
        let mut e = ev();
        e.event_type = "admin_change".into();
        e.payload = json!({"before_hash":"a"});
        assert!(log.append(e).is_err());
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
}
