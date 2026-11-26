//! 组暴露矩阵：权限组内对上游 MCP 工具的两层控制。
//!
//! 第一层是「组 → 整台上游」（`mcp_group_upstreams`）：授予后该上游的**全部**工具对该组可见。
//! 第二层是「组 × 上游 × 工具」的三态规则（`mcp_group_tool_rules`）：`include` 把单个工具
//! 强制纳入（上游未授予时也能单独分享一个工具），`exclude` 把单个工具从该组移除。
//! 没有规则行 = 继承第一层，因此"只授予一个工具"和"授予整台但关掉几个"都不需要复制工具清单。
//!
//! 判定按组独立求值，互不影响：同一台工具在 A 组被排除不影响它在 B 组可见。
//! 工具对一次请求可见 ⇔ 该请求 scope 的**至少一个组**判定可见——没有隐式公共通道。

use super::record::AdminChange;
use super::{CapabilityStore, now_iso};
use crate::audit::IntoAuditOrigin;
use crate::errors::McpError;
use rusqlite::Row;
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap, HashSet};

/// 单个 (组, 上游) 组合的逐工具规则条数上限（与 args/env/headers 的条目上限同量级）。
pub const GROUP_TOOL_RULE_MAX: usize = 128;

/// 逐工具规则的三态中的两种显式取值；`None`（无行）表示继承第一层。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolRuleState {
    /// 强制纳入：即使上游未授予本组，该工具也对本组可见。
    Include,
    /// 强制排除：即使上游已授予本组，该工具也不对本组可见。
    Exclude,
}

impl ToolRuleState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Include => "include",
            Self::Exclude => "exclude",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "include" => Some(Self::Include),
            "exclude" => Some(Self::Exclude),
            _ => None,
        }
    }
}

/// 一行逐工具规则（管理面读写用；运行期索引见 [`GroupExposure`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupToolRule {
    pub group_code: String,
    pub upstream_id: String,
    pub tool_name: String,
    pub state: ToolRuleState,
}

/// 单条逐工具变更：工具名与新状态（`None` = 删除规则、回到继承第一层）。
pub type ToolRuleChange = (String, Option<ToolRuleState>);

/// 一次提交里某台上游的全部逐工具变更：`(upstream_id, changes)`。
pub type UpstreamToolRuleChange = (String, Vec<ToolRuleChange>);

/// 组暴露矩阵的运行期索引：`grants[group] = {upstream}`、`rules[(group,upstream,tool)] = state`。
///
/// 索引化而非逐次查库：一次请求内的可见性判定会被 `tools/list` 与 `tools/call` 反复调用，
/// 规则总量与上游数同量级（每个组每台几十条），整份读进内存远比每次查询便宜。
#[derive(Debug, Clone, Default)]
pub struct GroupExposure {
    grants: HashMap<String, HashSet<String>>,
    rules: HashMap<(String, String, String), ToolRuleState>,
}

impl GroupExposure {
    /// 某组是否被授予整台上游（第一层）。
    pub fn granted(&self, group: &str, upstream: &str) -> bool {
        self.grants
            .get(group)
            .is_some_and(|set| set.contains(upstream))
    }

    /// 显式规则；`None` = 继承第一层。
    pub fn rule(&self, group: &str, upstream: &str, tool: &str) -> Option<ToolRuleState> {
        self.rules
            .get(&(group.to_string(), upstream.to_string(), tool.to_string()))
            .copied()
    }

    /// 单个组内的可见性：规则优先于第一层授予。
    pub fn visible(&self, group: &str, upstream: &str, tool: &str) -> bool {
        match self.rule(group, upstream, tool) {
            Some(ToolRuleState::Include) => true,
            Some(ToolRuleState::Exclude) => false,
            None => self.granted(group, upstream),
        }
    }

    /// 任一给定组内可见：一次请求的可见性判定（scope 的组集合里至少一个组放行）。
    pub fn visible_any(&self, groups: &[String], upstream: &str, tool: &str) -> bool {
        groups.iter().any(|g| self.visible(g, upstream, tool))
    }

    /// 该工具在给定组集合里可见的组（升序，输出稳定）。
    pub fn visible_groups(&self, groups: &[String], upstream: &str, tool: &str) -> Vec<String> {
        let mut out: Vec<String> = groups
            .iter()
            .filter(|g| self.visible(g, upstream, tool))
            .cloned()
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// 发现面投影用的单一生效组：多组可见时取字典序最小的那个（确定性输出）。
    ///
    /// `Capability.group` 是单值字段：它只需满足"落在 scope 内"，同时让不同请求对同一工具
    /// 得到稳定的展示组。完整的可见组集合见管理面 `GET /admin/groups/{code}/exposure`。
    pub fn visible_group<'a>(
        &self,
        groups: &'a [String],
        upstream: &str,
        tool: &str,
    ) -> Option<&'a str> {
        groups
            .iter()
            .filter(|g| self.visible(g, upstream, tool))
            .map(String::as_str)
            .min()
    }

    /// 任一给定组是否被授予整台上游（前缀回退的准入判断）。
    pub fn granted_any(&self, groups: &[String], upstream: &str) -> bool {
        groups.iter().any(|g| self.granted(g, upstream))
    }

    /// 这些组"碰得到"的上游 id：授予的 ∪ 有规则行的。
    ///
    /// 投影用它跳过与本次 scope 无关的上游——未被任何组触碰的上游不该被建连拉工具。
    pub fn touched_upstreams(&self, groups: &[String]) -> HashSet<String> {
        let mut out: HashSet<String> = HashSet::new();
        for group in groups {
            if let Some(set) = self.grants.get(group) {
                out.extend(set.iter().cloned());
            }
        }
        for (group, upstream, _) in self.rules.keys() {
            if groups.iter().any(|g| g == group) {
                out.insert(upstream.clone());
            }
        }
        out
    }

    /// 授予整台上游的组（升序）；管理面读视图用。
    pub fn groups_granting(&self, upstream: &str) -> Vec<String> {
        let mut out: Vec<String> = self
            .grants
            .iter()
            .filter(|(_, set)| set.contains(upstream))
            .map(|(group, _)| group.clone())
            .collect();
        out.sort();
        out
    }

    /// 某组某台上游的全部显式规则（按工具名升序）。
    pub fn rules_of(&self, group: &str, upstream: &str) -> Vec<(&str, ToolRuleState)> {
        let mut out: Vec<(&str, ToolRuleState)> = self
            .rules
            .iter()
            .filter(|((g, u, _), _)| g == group && u == upstream)
            .map(|((_, _, tool), state)| (tool.as_str(), *state))
            .collect();
        out.sort_by(|a, b| a.0.cmp(b.0));
        out
    }
}

impl CapabilityStore {
    /// 整份暴露矩阵（两张表各一次查询）。
    pub fn group_exposure(&self) -> Result<GroupExposure, McpError> {
        let conn = self.conn.lock().map_err(|_| McpError::lock())?;
        let mut exposure = GroupExposure::default();
        {
            let mut stmt = conn
                .prepare("SELECT group_code, upstream_id FROM mcp_group_upstreams")
                .map_err(|e| McpError::store(e.to_string()))?;
            let mapped = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map_err(|e| McpError::store(e.to_string()))?;
            for row in mapped {
                let (group, upstream) = row.map_err(|e| McpError::store(e.to_string()))?;
                exposure.grants.entry(group).or_default().insert(upstream);
            }
        }
        {
            let mut stmt = conn
                .prepare(
                    "SELECT group_code, upstream_id, tool_name, state FROM mcp_group_tool_rules",
                )
                .map_err(|e| McpError::store(e.to_string()))?;
            let mapped = stmt
                .query_map([], row_to_rule)
                .map_err(|e| McpError::store(e.to_string()))?;
            for row in mapped {
                let rule = row.map_err(|e| McpError::store(e.to_string()))?;
                exposure.rules.insert(
                    (rule.group_code, rule.upstream_id, rule.tool_name),
                    rule.state,
                );
            }
        }
        Ok(exposure)
    }

    /// 某组授予的上游 id（升序）。
    pub fn group_upstreams_of(&self, group: &str) -> Result<Vec<String>, McpError> {
        let conn = self.conn.lock().map_err(|_| McpError::lock())?;
        let mut stmt = conn
            .prepare("SELECT upstream_id FROM mcp_group_upstreams WHERE group_code=? ORDER BY upstream_id")
            .map_err(|e| McpError::store(e.to_string()))?;
        let mapped = stmt
            .query_map([group], |r| r.get::<_, String>(0))
            .map_err(|e| McpError::store(e.to_string()))?;
        let mut out = Vec::new();
        for row in mapped {
            out.push(row.map_err(|e| McpError::store(e.to_string()))?);
        }
        Ok(out)
    }

    /// 某组的全部逐工具规则（按 `upstream_id, tool_name` 升序）。
    pub fn group_tool_rules_of(&self, group: &str) -> Result<Vec<GroupToolRule>, McpError> {
        let conn = self.conn.lock().map_err(|_| McpError::lock())?;
        let mut stmt = conn
            .prepare(
                "SELECT group_code, upstream_id, tool_name, state FROM mcp_group_tool_rules WHERE group_code=? ORDER BY upstream_id, tool_name",
            )
            .map_err(|e| McpError::store(e.to_string()))?;
        let mapped = stmt
            .query_map([group], row_to_rule)
            .map_err(|e| McpError::store(e.to_string()))?;
        let mut out = Vec::new();
        for row in mapped {
            out.push(row.map_err(|e| McpError::store(e.to_string()))?);
        }
        Ok(out)
    }

    /// 合并式写入第一层：`grants` 里 `true` = 授予，`false` = 撤销。
    ///
    /// 撤销整台上游时同组同上游的 `exclude` 规则一并删除：撤销后它们已不表达任何意图，
    /// 留着只会在下次重新授予时静默复现旧配置。`include` 规则保留（单工具分享）。
    /// 幂等：无真实变化时不写审计，返回空 changes。
    pub fn apply_group_upstreams(
        &self,
        group: &str,
        grants: &[(String, bool)],
        actor: impl IntoAuditOrigin,
    ) -> Result<Vec<Value>, McpError> {
        let origin = actor.into_audit_origin();
        let before: HashSet<String> = self.group_upstreams_of(group)?.into_iter().collect();
        let mut after: HashSet<String> = before.clone();
        for (upstream, granted) in grants {
            if *granted {
                after.insert(upstream.clone());
            } else {
                after.remove(upstream);
            }
        }
        if after == before {
            return Ok(Vec::new());
        }
        {
            let conn = self.conn.lock().map_err(|_| McpError::lock())?;
            let tx = conn
                .unchecked_transaction()
                .map_err(|e| McpError::store(e.to_string()))?;
            let now = now_iso();
            {
                let mut insert = tx
                    .prepare(
                        "INSERT INTO mcp_group_upstreams (group_code, upstream_id, updated_at) VALUES (?1,?2,?3) ON CONFLICT(group_code, upstream_id) DO UPDATE SET updated_at=excluded.updated_at",
                    )
                    .map_err(|e| McpError::store(e.to_string()))?;
                for upstream in after.difference(&before) {
                    insert
                        .execute(rusqlite::params![group, upstream, &now])
                        .map_err(|e| McpError::store(e.to_string()))?;
                }
            }
            {
                let mut revoke = tx
                    .prepare(
                        "DELETE FROM mcp_group_upstreams WHERE group_code=?1 AND upstream_id=?2",
                    )
                    .map_err(|e| McpError::store(e.to_string()))?;
                let mut cleanup = tx
                    .prepare(
                        "DELETE FROM mcp_group_tool_rules WHERE group_code=?1 AND upstream_id=?2 AND state='exclude'",
                    )
                    .map_err(|e| McpError::store(e.to_string()))?;
                for upstream in before.difference(&after) {
                    revoke
                        .execute(rusqlite::params![group, upstream])
                        .map_err(|e| McpError::store(e.to_string()))?;
                    cleanup
                        .execute(rusqlite::params![group, upstream])
                        .map_err(|e| McpError::store(e.to_string()))?;
                }
            }
            tx.commit().map_err(|e| McpError::store(e.to_string()))?;
        }
        let mut changes: Vec<Value> = Vec::new();
        for upstream in after.difference(&before) {
            changes.push(
                json!({"field": "upstream", "upstream": upstream, "before": Value::Null, "after": true}),
            );
        }
        for upstream in before.difference(&after) {
            changes.push(
                json!({"field": "upstream", "upstream": upstream, "before": true, "after": Value::Null}),
            );
        }
        changes.sort_by(|a, b| a["upstream"].as_str().cmp(&b["upstream"].as_str()));
        self.write_admin_audit(
            AdminChange::new("group", group, "group_upstreams")
                .with_origin(origin)
                .with_changes(changes.clone())
                .with_data(json!({"group": group, "upstream_count": after.len()})),
        )?;
        Ok(changes)
    }

    /// 合并式写入第二层：`changes` 里 `Some(state)` 建/改行，`None` 删行（回到继承）。
    ///
    /// 上限 [`GROUP_TOOL_RULE_MAX`] 在事务内检查，避免两个并发 PUT 各自看到"未超限"。
    pub fn apply_group_tool_rules(
        &self,
        group: &str,
        upstream_id: &str,
        changes: &[ToolRuleChange],
        actor: impl IntoAuditOrigin,
    ) -> Result<Vec<Value>, McpError> {
        let origin = actor.into_audit_origin();
        let before: HashMap<String, ToolRuleState> = self
            .group_tool_rules_of(group)?
            .into_iter()
            .filter(|r| r.upstream_id == upstream_id)
            .map(|r| (r.tool_name, r.state))
            .collect();
        let mut after = before.clone();
        for (tool, state) in changes {
            match state {
                Some(state) => {
                    after.insert(tool.clone(), *state);
                }
                None => {
                    after.remove(tool);
                }
            }
        }
        let changed: Vec<(&String, Option<&ToolRuleState>, Option<&ToolRuleState>)> = after
            .keys()
            .chain(before.keys())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter(|tool| before.get(*tool) != after.get(*tool))
            .map(|tool| (tool, before.get(tool), after.get(tool)))
            .collect();
        if changed.is_empty() {
            return Ok(Vec::new());
        }
        if after.len() > GROUP_TOOL_RULE_MAX {
            return Err(McpError::validation(format!(
                "组 {group} 对上游 {upstream_id} 的工具规则超过上限 {GROUP_TOOL_RULE_MAX}（{} 条）",
                after.len()
            )));
        }
        {
            let conn = self.conn.lock().map_err(|_| McpError::lock())?;
            let tx = conn
                .unchecked_transaction()
                .map_err(|e| McpError::store(e.to_string()))?;
            let now = now_iso();
            {
                let mut upsert = tx
                    .prepare(
                        "INSERT INTO mcp_group_tool_rules (group_code, upstream_id, tool_name, state, updated_at) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(group_code, upstream_id, tool_name) DO UPDATE SET state=excluded.state, updated_at=excluded.updated_at",
                    )
                    .map_err(|e| McpError::store(e.to_string()))?;
                let mut delete = tx
                    .prepare(
                        "DELETE FROM mcp_group_tool_rules WHERE group_code=?1 AND upstream_id=?2 AND tool_name=?3",
                    )
                    .map_err(|e| McpError::store(e.to_string()))?;
                for (tool, _, next) in &changed {
                    match next {
                        Some(state) => upsert
                            .execute(rusqlite::params![
                                group,
                                upstream_id,
                                tool,
                                state.as_str(),
                                &now
                            ])
                            .map_err(|e| McpError::store(e.to_string()))?,
                        None => delete
                            .execute(rusqlite::params![group, upstream_id, tool])
                            .map_err(|e| McpError::store(e.to_string()))?,
                    };
                }
            }
            tx.commit().map_err(|e| McpError::store(e.to_string()))?;
        }
        let changes: Vec<Value> = changed
            .iter()
            .map(|(tool, before, after)| {
                json!({
                    "field": "tool_rule",
                    "tool": tool,
                    "before": before.map(|s| s.as_str()),
                    "after": after.map(|s| s.as_str()),
                })
            })
            .collect();
        self.write_admin_audit(
            AdminChange::new("group", group, "group_tool_rules")
                .with_origin(origin)
                .with_changes(changes.clone())
                .with_data(json!({
                    "group": group,
                    "upstream_id": upstream_id,
                    "rule_count": after.len()
                })),
        )?;
        Ok(changes)
    }

    /// 组被暴露矩阵引用的条数（第一层授予数, 第二层规则数）；组删除门禁用。
    pub(crate) fn group_exposure_refs(
        &self,
        conn: &rusqlite::Connection,
        group: &str,
    ) -> Result<(i64, i64), McpError> {
        let grants: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM mcp_group_upstreams WHERE group_code=?",
                [group],
                |r| r.get(0),
            )
            .map_err(|e| McpError::store(e.to_string()))?;
        let rules: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM mcp_group_tool_rules WHERE group_code=?",
                [group],
                |r| r.get(0),
            )
            .map_err(|e| McpError::store(e.to_string()))?;
        Ok((grants, rules))
    }
}

fn row_to_rule(row: &Row) -> rusqlite::Result<GroupToolRule> {
    let raw: String = row.get("state")?;
    let state = ToolRuleState::parse(&raw).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "group tool rule state 非法",
            )),
        )
    })?;
    Ok(GroupToolRule {
        group_code: row.get("group_code")?,
        upstream_id: row.get("upstream_id")?,
        tool_name: row.get("tool_name")?,
        state,
    })
}
