//! 管理组：业务组的 CRUD 与引用计数；组 code 是唯一权限标识，展示名不参与判定。

use super::record::{AdminChange, field_changes};
use super::{CapabilityStore, now_iso};
use crate::audit::IntoAuditOrigin;
use crate::config::{is_reserved_group_name, validate_group_name};
use crate::errors::McpError;
use rusqlite::Row;
use serde_json::{Value, json};
use std::collections::HashMap;

/// 组变更审计的字段集：只写真实变化的字段。
const GROUP_CHANGE_FIELDS: &[&str] = &["name", "priority", "min_budget", "roles"];

/// 管理面新增的业务组：与配置里的 `GroupDef` 同语义，另带"授权给哪些角色"。
///
/// `code` 是唯一权限标识（`groups.name` 列存 code），`name` 是展示名
/// （`groups.display_name` 列；空则回退 code）。角色列表是"该组对哪些角色可见"
/// 的唯一授权来源：`Service::effective_policy` 会把组 code 追加进这些角色的
/// `default_groups`，下一次 scope 解析即可用上。
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
    // 展示名未设置时回退 code（列可能为空串）。
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
