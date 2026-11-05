//! capability 存储：双布尔 + 版本不可原地改 + definition_hash + 四眼审批 + deprecated 带 successor。
//! 为什么已有发布定义走新 version：编辑即新版本（§4.4.4），capability_versions append-only。

use crate::audit::{ActorRef, AuditEvent, AuditLog};
use crate::canonical::{canonical_json, definition_hash};
use crate::errors::McpError;
use rusqlite::{Connection, Row};
use serde_json::{Value, json};
use std::sync::Mutex;

pub const STATUSES: &[&str] = &["draft", "review", "published", "deprecated"];
pub const KINDS: &[&str] = &["tool", "template", "endpoint"];
pub const RISKS: &[&str] = &["read", "write", "sensitive", "admin"];

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
    /// 提审人身份（四眼判定的基准）：由鉴权派生，不来自请求头自述。
    pub submitted_by: String,
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
    updated_at TEXT NOT NULL DEFAULT '', submitted_by TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_cap_status_group ON capabilities(status, group_name);
CREATE TABLE IF NOT EXISTS capability_versions (
    capability_id TEXT NOT NULL, version INTEGER NOT NULL, definition_json TEXT NOT NULL,
    definition_hash TEXT NOT NULL, approval_id TEXT NOT NULL DEFAULT '', PRIMARY KEY (capability_id, version)
);
CREATE TRIGGER IF NOT EXISTS trg_ver_no_update BEFORE UPDATE ON capability_versions BEGIN SELECT RAISE(ABORT, 'capability_versions is append-only'); END;
CREATE TRIGGER IF NOT EXISTS trg_ver_no_delete BEFORE DELETE ON capability_versions BEGIN SELECT RAISE(ABORT, 'capability_versions is append-only'); END;
"#).map_err(|e| McpError::store(e.to_string()))?;
        // 旧库补列：CREATE TABLE IF NOT EXISTS 不会为已存在的表加列。
        let has_updated = column_exists(&conn, "updated_at")?;
        if !has_updated {
            conn.execute(
                "ALTER TABLE capabilities ADD COLUMN updated_at TEXT NOT NULL DEFAULT ''",
                [],
            )
            .map_err(|e| McpError::store(e.to_string()))?;
        }
        if !column_exists(&conn, "submitted_by")? {
            conn.execute(
                "ALTER TABLE capabilities ADD COLUMN submitted_by TEXT NOT NULL DEFAULT ''",
                [],
            )
            .map_err(|e| McpError::store(e.to_string()))?;
        }
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

    /// 可见性：published ∧ discoverable ∧ group ∈ scope。
    pub fn visible_ids(&self, groups: &[String]) -> Result<Vec<String>, McpError> {
        if groups.is_empty() {
            return Ok(vec![]);
        }
        let placeholders = groups.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT capability_id FROM capabilities WHERE status='published' AND discoverable=1 AND group_name IN ({placeholders})"
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
            .query_map(refs.as_slice(), |r| r.get(0))
            .map_err(|e| McpError::store(e.to_string()))?;
        let mut rows: Vec<String> = Vec::new();
        for row in mapped {
            let id = row.map_err(|e| McpError::store(e.to_string()))?;
            rows.push(id);
        }
        Ok(rows)
    }

    pub fn register(
        &self,
        id: &str,
        kind: &str,
        group: &str,
        risk: &str,
        definition: Value,
    ) -> Result<CapabilityRecord, McpError> {
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
            tx.execute("INSERT INTO capabilities (capability_id, kind, group_name, risk, status, discoverable, callable, version, definition_json, definition_hash, updated_at) VALUES (?1,?2,?3,?4,'draft',0,0,1,?5,?6,?7)",
                rusqlite::params![id, kind, group, risk, canon, dhash, now_iso()]).map_err(|e| McpError::store(e.to_string()))?;
            tx.execute("INSERT INTO capability_versions (capability_id, version, definition_json, definition_hash, approval_id) VALUES (?1,1,?2,?3,'')",
                rusqlite::params![id, canon, dhash]).map_err(|e| McpError::store(e.to_string()))?;
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

    fn transition(
        &self,
        id: &str,
        to: &str,
        approval_id: &str,
        submitted_by: &str,
        approved_by: &str,
        diff: &str,
    ) -> Result<CapabilityRecord, McpError> {
        if approval_id.is_empty() {
            return Err(McpError::new(
                "MCP_APPROVAL_REQUIRED",
                json!({"capability_id": id, "missing": ["approval_id"]}),
            ));
        }
        if !submitted_by.is_empty() && submitted_by == approved_by {
            return Err(McpError::new(
                "MCP_APPROVAL_REQUIRED",
                json!({"capability_id": id, "hint": "提交者与批准者必须不同（四眼）"}),
            ));
        }
        let record = self.require(id)?;
        if record.status == "deprecated" {
            return Err(McpError::new(
                "MCP_STATE_TRANSITION_INVALID",
                json!({"capability_id": id, "from": record.status, "to": to}),
            ));
        }
        {
            let conn = self.conn.lock().map_err(|_| McpError::lock())?;
            conn.execute(
                "UPDATE capabilities SET status=?, updated_at=? WHERE capability_id=?",
                rusqlite::params![to, now_iso(), id],
            )
            .map_err(|e| McpError::store(e.to_string()))?;
        }
        let diff_str = if diff.is_empty() {
            format!("{}->{}", record.status, to)
        } else {
            diff.to_string()
        };
        // 审计主体用本次操作者（批准人＝鉴权派生 id），不用固定 "system" 掩盖真实操作者。
        let audit_actor = if approved_by.is_empty() {
            submitted_by
        } else {
            approved_by
        };
        self.write_admin_audit(
            id,
            approval_id,
            &record.definition_hash,
            &record.definition_hash,
            &diff_str,
            audit_actor,
        )?;
        let rec = self.get(id)?;
        match rec {
            Some(r) => Ok(r),
            None => Err(McpError::store(format!("transition 后记录缺失: {id}"))),
        }
    }

    /// draft → review。`submitted_by` 必须是鉴权派生身份，四眼判定以它为准。
    pub fn submit_for_review(
        &self,
        id: &str,
        submitted_by: &str,
    ) -> Result<CapabilityRecord, McpError> {
        let record = self.require(id)?;
        if record.status != "draft" {
            return Err(McpError::new(
                "MCP_STATE_TRANSITION_INVALID",
                json!({"capability_id": id, "from": record.status, "to": "review"}),
            ));
        }
        {
            let conn = self.conn.lock().map_err(|_| McpError::lock())?;
            conn.execute(
                "UPDATE capabilities SET status='review', submitted_by=?, updated_at=? WHERE capability_id=?",
                rusqlite::params![submitted_by, now_iso(), id],
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

    /// review → published。四眼比较的是「记录的提审人」与「本次批准人」，
    /// 两者都来自鉴权派生身份，请求体无法自述。
    pub fn promote_to_published(
        &self,
        id: &str,
        approval_id: &str,
        approved_by: &str,
        diff: &str,
    ) -> Result<CapabilityRecord, McpError> {
        let record = self.require(id)?;
        if record.status != "review" {
            return Err(McpError::new(
                "MCP_STATE_TRANSITION_INVALID",
                json!({"capability_id": id, "from": record.status, "to": "published"}),
            ));
        }
        self.transition(
            id,
            "published",
            approval_id,
            &record.submitted_by,
            approved_by,
            diff,
        )
    }

    /// 已发布定义只插入新 version；哈希变化不得沿用旧审批（须新 approval_id + 四眼）。单个事务内：校验状态→插入新 version 行→更新指针(status 保持不变，不经过 review 中间态)。
    pub fn publish_version(
        &self,
        id: &str,
        definition: Value,
        approval_id: &str,
        submitted_by: &str,
        approved_by: &str,
    ) -> Result<CapabilityRecord, McpError> {
        if approval_id.is_empty() || (!submitted_by.is_empty() && submitted_by == approved_by) {
            return Err(McpError::new(
                "MCP_APPROVAL_REQUIRED",
                json!({"capability_id": id, "hint": "定义变更必须带新的 approval_id 且提交/批准不同人"}),
            ));
        }
        let record = self.require(id)?;
        if record.status == "deprecated" {
            return Err(McpError::new(
                "MCP_STATE_TRANSITION_INVALID",
                json!({"capability_id": id, "from": record.status, "to": "publish_version"}),
            ));
        }
        let diff_value = definition_diff(&record.definition, &definition);
        let diff_str = canonical_json(&diff_value);
        let new_version = record.version + 1;
        let dhash = definition_hash(&definition);
        let canon = canonical_json(&definition);
        {
            let mut conn = self.conn.lock().map_err(|_| McpError::lock())?;
            let tx = conn
                .transaction()
                .map_err(|e| McpError::store(e.to_string()))?;
            tx.execute("INSERT INTO capability_versions (capability_id, version, definition_json, definition_hash, approval_id) VALUES (?1,?2,?3,?4,?5)",
                rusqlite::params![id, new_version, canon, dhash, approval_id]).map_err(|_| McpError::new("MCP_VERSION_CONFLICT", json!({"capability_id": id, "version": new_version})))?;
            tx.execute("UPDATE capabilities SET version=?1, definition_json=?2, definition_hash=?3, updated_at=?4 WHERE capability_id=?5",
                rusqlite::params![new_version, canon, dhash, now_iso(), id]).map_err(|e| McpError::store(e.to_string()))?;
            tx.commit().map_err(|e| McpError::store(e.to_string()))?;
        }
        self.write_admin_audit(
            id,
            approval_id,
            &record.definition_hash,
            &dhash,
            &diff_str,
            if submitted_by.is_empty() {
                approved_by
            } else {
                submitted_by
            },
        )?;
        let rec = self.get(id)?;
        match rec {
            Some(r) => Ok(r),
            None => Err(McpError::store(format!("publish_version 后记录缺失: {id}"))),
        }
    }

    pub fn deprecate(&self, id: &str, successor_id: &str) -> Result<CapabilityRecord, McpError> {
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

    pub fn set_visibility(
        &self,
        id: &str,
        discoverable: Option<bool>,
        callable: Option<bool>,
        approval_id: &str,
        actor: &str,
    ) -> Result<CapabilityRecord, McpError> {
        if approval_id.is_empty() {
            return Err(McpError::new(
                "MCP_APPROVAL_REQUIRED",
                json!({"capability_id": id, "missing": ["approval_id"]}),
            ));
        }
        let record = self.require(id)?;
        let before = record.definition_hash.clone();
        let mut changes = serde_json::Map::new();
        if let Some(d) = discoverable {
            changes.insert("discoverable".into(), json!(d));
        }
        if let Some(c) = callable {
            changes.insert("callable".into(), json!(c));
        }
        if !changes.is_empty() {
            let conn = self.conn.lock().map_err(|_| McpError::lock())?;
            let mut assigns = Vec::new();
            let mut vals: Vec<i64> = Vec::new();
            if let Some(d) = discoverable {
                assigns.push("discoverable=?".to_string());
                vals.push(i64::from(d));
            }
            if let Some(c) = callable {
                assigns.push("callable=?".to_string());
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
            bound.push(Box::new(now_iso()));
            bound.push(Box::new(id.to_string()));
            let refs: Vec<&dyn rusqlite::ToSql> = bound
                .iter()
                .map(|b| b.as_ref() as &dyn rusqlite::ToSql)
                .collect();
            stmt.execute(refs.as_slice())
                .map_err(|e| McpError::store(e.to_string()))?;
        }
        self.write_admin_audit(
            id,
            approval_id,
            &before,
            &before,
            &canonical_json(&Value::Object(changes)),
            actor,
        )?;
        let rec = self.get(id)?;
        match rec {
            Some(r) => Ok(r),
            None => Err(McpError::store(format!("set_visibility 后记录缺失: {id}"))),
        }
    }

    fn write_admin_audit(
        &self,
        capability_id: &str,
        approval_id: &str,
        before_hash: &str,
        after_hash: &str,
        diff: &str,
        actor: &str,
    ) -> Result<(), McpError> {
        if let Some(audit) = &self.audit {
            audit.append(AuditEvent { tenant: "default".into(), actor: ActorRef { actor_type: "admin".into(), id: actor.into(), role: "admin".into() }, event_type: "admin_change".into(), payload: json!({"capability_id": capability_id, "before_hash": before_hash, "after_hash": after_hash, "diff": diff, "approval_id": approval_id}), session_id: String::new(), ts: String::new(), event_id: String::new() }).map_err(|e| McpError::audit(e.to_string()))?;
        }
        Ok(())
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

fn column_exists(conn: &Connection, column: &str) -> Result<bool, McpError> {
    let mut stmt = conn
        .prepare("PRAGMA table_info(capabilities)")
        .map_err(|e| McpError::store(e.to_string()))?;
    let names: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(|e| McpError::store(e.to_string()))?
        .filter_map(Result::ok)
        .collect();
    Ok(names.iter().any(|n| n == column))
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// 字段级变更：新增/删除/修改的 key 列表（JSON patch 简化版，仅顶层 key）。
pub fn definition_diff(old: &Value, new: &Value) -> Value {
    use std::collections::HashSet;
    let old_obj = old.as_object();
    let new_obj = new.as_object();
    match (old_obj, new_obj) {
        (Some(o), Some(n)) => {
            let mut added: Vec<String> = Vec::new();
            let mut removed: Vec<String> = Vec::new();
            let mut changed: Vec<String> = Vec::new();
            let old_keys: HashSet<&String> = o.keys().collect();
            let new_keys: HashSet<&String> = n.keys().collect();
            for k in new_keys.difference(&old_keys) {
                added.push((*k).clone());
            }
            for k in old_keys.difference(&new_keys) {
                removed.push((*k).clone());
            }
            for k in old_keys.intersection(&new_keys) {
                let ov = o.get(*k);
                let nv = n.get(*k);
                if ov != nv {
                    changed.push((*k).clone());
                }
            }
            added.sort();
            removed.sort();
            changed.sort();
            json!({"added": added, "removed": removed, "changed": changed})
        }
        _ => json!({"changed": ["<root>"]}),
    }
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn store() -> CapabilityStore {
        CapabilityStore::open(":memory:", None).unwrap()
    }
    #[test]
    fn four_eyes_same_person_rejected() {
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
        let e = s
            .promote_to_published("cap1", "ap-1", "alice", "")
            .unwrap_err();
        assert_eq!(e.code, "MCP_APPROVAL_REQUIRED");
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
        s.promote_to_published("cap1", "ap-1", "bob", "").unwrap();
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
        // 审计 before/after 不同且 diff 为字段级
        let rows = {
            let conn = s.audit.as_ref().unwrap();
            conn.query(None).unwrap()
        };
        let admin: Vec<_> = rows
            .into_iter()
            .filter(|e| e.event_type == "admin_change")
            .collect();
        assert!(!admin.is_empty());
        let last = admin.last().unwrap();
        assert_eq!(last.payload["capability_id"], json!("cap1"));
        assert_eq!(last.payload["before_hash"], json!(before_hash));
        assert_eq!(last.payload["after_hash"], json!(r.definition_hash));
        assert_ne!(last.payload["before_hash"], last.payload["after_hash"]);
        let diff_str = last.payload["diff"].as_str().unwrap();
        assert!(diff_str.contains("new"), "diff 应含新增 key: {diff_str}");
        assert!(
            diff_str.contains("description"),
            "diff 应含修改 key: {diff_str}"
        );
        // capabilities 指针已指向新 version
        assert_eq!(s.get("cap1").unwrap().unwrap().version, 2);
    }
    #[test]
    fn publish_version_same_person_rejected() {
        let s = store();
        s.register(
            "cap1",
            "tool",
            "db.read.core",
            "read",
            json!({"description":"v1"}),
        )
        .unwrap();
        let e = s
            .publish_version(
                "cap1",
                json!({"description":"v2"}),
                "ap-1",
                "alice",
                "alice",
            )
            .unwrap_err();
        assert_eq!(e.code, "MCP_APPROVAL_REQUIRED");
        assert_eq!(s.get("cap1").unwrap().unwrap().version, 1);
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
        s.promote_to_published("cap1", "ap-1", "bob", "").unwrap();
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

    #[test]
    fn definition_diff_keys() {
        let d = definition_diff(&json!({"a":1,"b":2}), &json!({"b":3,"c":4}));
        assert_eq!(d["added"], json!(["c"]));
        assert_eq!(d["removed"], json!(["a"]));
        assert_eq!(d["changed"], json!(["b"]));
    }
}
