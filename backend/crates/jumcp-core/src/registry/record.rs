//! 记录类型与审计投影：记录/版本/管理变更的结构，以及定义 diff（只写真实变化的字段）。

use rusqlite::Row;
use serde_json::{Value, json};

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

    pub(super) fn into_payload(self) -> Value {
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

pub(super) fn row_to_record(row: &Row) -> rusqlite::Result<CapabilityRecord> {
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
