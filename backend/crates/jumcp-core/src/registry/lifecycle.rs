//! 生命周期：注册、审核、发布、下架与可见性；状态迁移只在事务里推进版本指针。

use super::record::{
    AdminChange, CapabilityRecord, CapabilityVersion, change, definition_changes, row_to_record,
};
use super::{CALLABLE_WHERE, CapabilityStore, KINDS, RISKS, VISIBLE_WHERE, is_builtin_id, now_iso};
use crate::audit::{ActorRef, AuditEvent, IntoAuditOrigin};
use crate::canonical::{canonical_json, definition_hash};
use crate::errors::McpError;
use serde_json::{Value, json};

impl CapabilityStore {
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

    /// 版本历史（升序）。空字符串按"未知"处理，由 API 层输出 null。
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
    /// 与 `visible` 是两条不同的谓词（`discoverable=false, callable=true` 是合法的
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

    /// 状态迁移的 UPDATE 必须带"源状态"条件：读状态与写状态分属两把锁，
    /// 条件不命中说明记录已被并发迁移，按冲突返回而不是覆盖。
    fn expect_transition(
        conn: &rusqlite::Connection,
        id: &str,
        expected_from: &str,
        to: &str,
        changed: usize,
    ) -> Result<(), McpError> {
        if changed > 0 {
            return Ok(());
        }
        let actual: String = conn
            .query_row(
                "SELECT status FROM capabilities WHERE capability_id=?1",
                [id],
                |r| r.get(0),
            )
            .unwrap_or_else(|_| "unknown".to_string());
        Err(McpError::new(
            "MCP_STATE_TRANSITION_INVALID",
            json!({
                "capability_id": id,
                "from": actual,
                "expected_from": expected_from,
                "to": to
            }),
        ))
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
            let changed = conn
                .execute(
                    "UPDATE capabilities SET status='published', updated_at=? WHERE capability_id=? AND status=?",
                    rusqlite::params![now, id, record.status.as_str()],
                )
                .map_err(|e| McpError::store(e.to_string()))?;
            Self::expect_transition(&conn, id, &record.status, "published", changed)?;
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
            let changed = conn
                .execute(
                    "UPDATE capabilities SET status='review', submitted_by=?, submitted_at=?, updated_at=? WHERE capability_id=? AND status='draft'",
                    rusqlite::params![submitted_by, now, now, id],
                )
                .map_err(|e| McpError::store(e.to_string()))?;
            Self::expect_transition(&conn, id, "draft", "review", changed)?;
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
            let changed = conn
                .execute(
                    "UPDATE capabilities SET status='draft', submitted_by='', submitted_at='', updated_at=? WHERE capability_id=? AND status='review'",
                    rusqlite::params![now_iso(), id],
                )
                .map_err(|e| McpError::store(e.to_string()))?;
            Self::expect_transition(&conn, id, "review", "draft", changed)?;
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
            let changed = tx
                .execute(
                    "UPDATE capabilities SET version=?1, definition_json=?2, definition_hash=?3, updated_at=?4 WHERE capability_id=?5 AND status=?6",
                    rusqlite::params![new_version, canon, dhash, now, id, record.status.as_str()],
                )
                .map_err(|e| McpError::store(e.to_string()))?;
            Self::expect_transition(&tx, id, &record.status, &record.status, changed)?;
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
    /// （草案还没有可批准的定义）；`approval_id` 只作为可选外部票号留痕，不自动生成。
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
            let changed = tx
                .execute(
                    "UPDATE capabilities SET version=?1, definition_json=?2, definition_hash=?3, updated_at=?4 WHERE capability_id=?5 AND status='draft'",
                    rusqlite::params![new_version, canon, dhash, now, id],
                )
                .map_err(|e| McpError::store(e.to_string()))?;
            Self::expect_transition(&tx, id, "draft", "draft", changed)?;
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
