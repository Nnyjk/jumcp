//! 平面 A 纯函数：三通道解析 → 越界判定 → EffectiveScope，严格按序（填充默认→越界→预算）。
//! 为什么顺序不可调：先交集使 strict 恒假，先截断掩盖越界。

use crate::config::TenantPolicy;
use crate::errors::McpError;
use serde_json::{Value, json};

#[derive(Debug, Clone, Default)]
pub struct Profile {
    pub groups: Vec<String>,
    pub budget: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct GroupRequest {
    pub header_groups: Option<String>,
    pub meta: Value,
    pub profile: Option<Profile>,
    pub header_budget: Option<i64>,
    pub meta_budget: Option<i64>,
    pub profile_budget: Option<i64>,
}
impl Default for GroupRequest {
    fn default() -> Self {
        Self {
            header_groups: None,
            meta: json!({}),
            profile: None,
            header_budget: None,
            meta_budget: None,
            profile_budget: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Actor {
    pub tenant: String,
    pub role: String,
    pub actor_type: String,
    pub actor_id: String,
    /// 派生身份的数据库 token id；stdio / 管理面静态 token 为空（审计 JSON 不落该键）。
    pub token_id: String,
    /// 调用来源：MCP token 取 token 配置的 `source`，管理面固定 `admin-api`，stdio 为空。
    pub source: String,
    /// token 配置的组集合 = 本次请求的**硬上限与默认组**；`None` 表示沿用角色默认组。
    pub allowed_groups: Option<Vec<String>>,
    /// token 配置的内置工具精确 allowlist；`None` 表示不限制（stdio / 管理面）。
    pub allowed_builtin_tools: Option<Vec<String>>,
}
impl Actor {
    pub fn new(tenant: &str, role: &str) -> Self {
        Self {
            tenant: tenant.into(),
            role: role.into(),
            actor_type: "agent".into(),
            actor_id: String::new(),
            ..Default::default()
        }
    }
    /// token 派生的 agent 身份：`actor_id` 即 agent_id（审计据此追责，不接受请求体自述）。
    pub fn agent(tenant: &str, agent_id: &str, role: &str) -> Self {
        Self {
            tenant: tenant.into(),
            role: role.into(),
            actor_type: "agent".into(),
            actor_id: agent_id.into(),
            ..Default::default()
        }
    }
    /// 数据库 access token 派生的身份。
    ///
    /// `groups` 同时是该 token 的默认组与硬上限：请求头 / `_meta` 只能在其中收窄，
    /// 绝不可能借角色默认组扩展到 token 未被授予的组（见 `resolve_scope`）。
    pub fn access_token(
        tenant: &str,
        caller: &str,
        role: &str,
        token_id: &str,
        source: &str,
        groups: Vec<String>,
    ) -> Self {
        Self {
            tenant: tenant.into(),
            role: role.into(),
            actor_type: "agent".into(),
            actor_id: caller.into(),
            token_id: token_id.into(),
            source: source.into(),
            allowed_groups: Some(groups),
            allowed_builtin_tools: None,
        }
    }
    /// 附加内置工具 allowlist（token 创建时由 `builtin_tools` 配置填充）。
    pub fn with_builtin_tools(mut self, tools: Vec<String>) -> Self {
        self.allowed_builtin_tools = Some(tools);
        self
    }
}

#[derive(Debug, Clone)]
pub struct EffectiveScope {
    pub groups: Vec<String>,
    pub budget: i64,
    pub degraded: bool,
    pub source: String,
    pub mode: String,
    /// 从 `Actor` 透传的内置工具 allowlist；`None` = 不限制。
    pub allowed_builtin_tools: Option<Vec<String>>,
}

pub type DegradedSink = Box<dyn Fn(&[String], &[String]) + Send>;

fn split_header(value: &str) -> Vec<String> {
    // 显式空串 = 空集；去空白去重保留序（与 Python dict.fromkeys 一致）。
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for part in value.split(',') {
        let g = part.trim();
        if !g.is_empty() && seen.insert(g.to_string()) {
            out.push(g.to_string());
        }
    }
    out
}

fn meta_groups(meta: &Value) -> Option<Vec<String>> {
    let node = meta.get("mcp")?;
    let value = node.get("groups")?;
    if let Some(s) = value.as_str() {
        return Some(split_header(s));
    }
    if let Some(a) = value.as_array() {
        return Some(
            a.iter()
                .filter_map(|g| g.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
        );
    }
    None
}

pub fn parse_groups_request(req: &GroupRequest) -> (Option<Vec<String>>, String, Option<i64>) {
    if let Some(h) = &req.header_groups {
        return (
            Some(split_header(h)),
            "header".to_string(),
            req.header_budget,
        );
    }
    if let Some(groups) = meta_groups(&req.meta) {
        let budget = req
            .meta
            .get("mcp")
            .and_then(|n| n.get("toolBudget"))
            .and_then(|b| b.as_i64())
            .or(req.meta_budget);
        return (Some(groups), "meta".to_string(), budget);
    }
    if let Some(p) = &req.profile {
        return (
            Some(p.groups.clone()),
            "profile".to_string(),
            p.budget.or(req.profile_budget),
        );
    }
    (None, "default".to_string(), None)
}

pub fn resolve_scope(
    actor: &Actor,
    req: &GroupRequest,
    policy: &TenantPolicy,
    mode: &str,
    on_degraded: Option<&DegradedSink>,
) -> Result<EffectiveScope, McpError> {
    let role_def = policy
        .role(&actor.role)
        .map_err(|e| McpError::with_message("MCP_GROUP_FORBIDDEN", e, json!({})))?;
    let (declared, source, requested_budget) = parse_groups_request(req);
    // token 派生身份的组集合是硬上限与默认组；无 token（stdio/管理面）沿用角色默认组。
    let base = actor
        .allowed_groups
        .clone()
        .unwrap_or_else(|| role_def.default_groups.clone());
    let mut requested = declared.unwrap_or_else(|| base.clone());
    let allowlist = base;
    let registered = policy.group_codes();
    let offending: Vec<String> = requested
        .iter()
        .filter(|g| !allowlist.contains(g) || !registered.contains(*g))
        .cloned()
        .collect();
    let mut degraded = false;
    if !offending.is_empty() {
        if mode == "intersect" && policy.allow_intersect {
            let kept: Vec<String> = requested
                .iter()
                .filter(|g| allowlist.contains(g) && registered.contains(*g))
                .cloned()
                .collect();
            if let Some(sink) = on_degraded {
                sink(&offending, &kept);
            }
            requested = kept;
            degraded = true;
        } else {
            return Err(McpError::new(
                "MCP_GROUP_FORBIDDEN",
                json!({"offending_groups": offending, "hint": "请求角色不可见该组；请使用角色默认组或联系平台管理员"}),
            ));
        }
    }
    let candidates = [
        requested_budget,
        Some(role_def.default_budget),
        Some(policy.tenant_budget_cap),
    ];
    let budget = candidates
        .iter()
        .filter_map(|x| *x)
        .min()
        .unwrap_or(role_def.default_budget);
    if budget < 800 {
        return Err(McpError::new(
            "MCP_BUDGET_EXCEEDED",
            json!({"required": 800, "available": budget, "dropped_candidates": allowlist}),
        ));
    }
    Ok(EffectiveScope {
        groups: requested,
        budget,
        degraded,
        source,
        mode: mode.to_string(),
        allowed_builtin_tools: actor.allowed_builtin_tools.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{GroupDef, RoleDef};
    use std::collections::HashMap;
    fn policy(allow: bool) -> TenantPolicy {
        let mut roles = HashMap::new();
        roles.insert(
            "analyst".into(),
            RoleDef {
                name: "analyst".into(),
                default_groups: vec!["db.read.core".into(), "db.read.analytics".into()],
                default_budget: 2000,
            },
        );
        TenantPolicy {
            groups: vec![
                GroupDef {
                    code: "db.read.core".into(),
                    name: "核心数据只读".into(),
                    priority: 10,
                    min_budget: 800,
                },
                GroupDef {
                    code: "db.read.analytics".into(),
                    name: "分析数据只读".into(),
                    priority: 8,
                    min_budget: 800,
                },
                GroupDef {
                    code: "db.admin.schema".into(),
                    name: "库表管理".into(),
                    priority: 1,
                    min_budget: 800,
                },
            ],
            roles,
            tenant_budget_cap: 2000,
            allow_intersect: allow,
        }
    }
    #[test]
    fn header_wins() {
        let p = policy(false);
        let a = Actor::new("t1", "analyst");
        let req = GroupRequest {
            header_groups: Some("db.read.analytics".into()),
            ..Default::default()
        };
        assert_eq!(
            resolve_scope(&a, &req, &p, "strict", None).unwrap().source,
            "header"
        );
    }
    #[test]
    fn strict_rejects_with_offending() {
        let p = policy(false);
        let a = Actor::new("t1", "analyst");
        let req = GroupRequest {
            header_groups: Some("db.admin.schema".into()),
            ..Default::default()
        };
        let e = resolve_scope(&a, &req, &p, "strict", None).unwrap_err();
        assert_eq!(e.code, "MCP_GROUP_FORBIDDEN");
        assert_eq!(e.data["offending_groups"], json!(["db.admin.schema"]));
    }
    #[test]
    fn display_name_is_not_a_group_identity() {
        // 中文展示名不能当组用：scope 只认 code，否则"改个显示名"就能改权限。
        let p = policy(false);
        let a = Actor::new("t1", "analyst");
        let req = GroupRequest {
            header_groups: Some("核心数据只读".into()),
            ..Default::default()
        };
        let e = resolve_scope(&a, &req, &p, "strict", None).unwrap_err();
        assert_eq!(e.code, "MCP_GROUP_FORBIDDEN");
        assert_eq!(e.data["offending_groups"], json!(["核心数据只读"]));
        // 角色默认组仍是 code。
        assert_eq!(
            resolve_scope(&a, &GroupRequest::default(), &p, "strict", None)
                .unwrap()
                .groups,
            vec!["db.read.core", "db.read.analytics"]
        );
    }
    #[test]
    fn intersect_gate() {
        let p = policy(true);
        let a = Actor::new("t1", "analyst");
        let req = GroupRequest {
            header_groups: Some("db.read.core,db.admin.schema".into()),
            ..Default::default()
        };
        use std::sync::{Arc, Mutex};
        let calls = Arc::new(Mutex::new(Vec::new()));
        let calls2 = calls.clone();
        let sink: DegradedSink = Box::new(move |o: &[String], k: &[String]| {
            calls2.lock().unwrap().push((o.to_vec(), k.to_vec()))
        });
        let s = resolve_scope(&a, &req, &p, "intersect", Some(&sink)).unwrap();
        assert!(s.degraded);
        assert_eq!(s.groups, vec!["db.read.core"]);
    }
    #[test]
    fn budget_min_and_floor() {
        let p = policy(false);
        let a = Actor::new("t1", "analyst");
        let req = GroupRequest {
            header_groups: Some("db.read.core".into()),
            header_budget: Some(1200),
            ..Default::default()
        };
        assert_eq!(
            resolve_scope(&a, &req, &p, "strict", None).unwrap().budget,
            1200
        );
        let req2 = GroupRequest {
            header_groups: Some("db.read.core".into()),
            header_budget: Some(799),
            ..Default::default()
        };
        assert_eq!(
            resolve_scope(&a, &req2, &p, "strict", None)
                .unwrap_err()
                .code,
            "MCP_BUDGET_EXCEEDED"
        );
    }
    #[test]
    fn empty_header_is_empty_set() {
        let p = policy(false);
        let a = Actor::new("t1", "analyst");
        let req = GroupRequest {
            header_groups: Some("".into()),
            ..Default::default()
        };
        assert!(
            resolve_scope(&a, &req, &p, "strict", None)
                .unwrap()
                .groups
                .is_empty()
        );
    }

    /// token 的 groups 是硬上限与默认组：header 越界拒绝，角色默认组不会把它扩回去。
    #[test]
    fn token_groups_are_hard_cap() {
        let p = policy(false);
        let token = Actor::access_token(
            "t1",
            "agent-1",
            "analyst",
            "tok_1",
            "agent",
            vec!["db.read.core".into()],
        );
        // 默认组 = token 组（不是角色 default_groups）。
        let scope = resolve_scope(&token, &GroupRequest::default(), &p, "strict", None).unwrap();
        assert_eq!(scope.groups, vec!["db.read.core"]);
        // 角色默认组里的 analytics 也不在 token 内 → 越界。
        let req = GroupRequest {
            header_groups: Some("db.read.analytics".into()),
            ..Default::default()
        };
        let e = resolve_scope(&token, &req, &p, "strict", None).unwrap_err();
        assert_eq!(e.code, "MCP_GROUP_FORBIDDEN");
        // 角色默认组不含 admin，token 却被授予 admin → 可用（token 配置优先于角色默认）。
        let admin_token = Actor::access_token(
            "t1",
            "agent-1",
            "analyst",
            "tok_1",
            "agent",
            vec!["db.read.core".into(), "db.admin.schema".into()],
        );
        let req = GroupRequest {
            header_groups: Some("db.admin.schema".into()),
            ..Default::default()
        };
        assert_eq!(
            resolve_scope(&admin_token, &req, &p, "strict", None)
                .unwrap()
                .groups,
            vec!["db.admin.schema"]
        );
    }

    /// 内置工具 allowlist 原样进入 scope；无 token 的 actor 保持 `None`（不限制）。
    #[test]
    fn builtin_allowlist_propagates_into_scope() {
        let p = policy(false);
        let token = Actor::access_token(
            "t1",
            "agent-1",
            "analyst",
            "tok_1",
            "agent",
            vec!["db.read.core".into()],
        )
        .with_builtin_tools(vec!["search_capabilities".into()]);
        let scope = resolve_scope(&token, &GroupRequest::default(), &p, "strict", None).unwrap();
        assert_eq!(
            scope.allowed_builtin_tools,
            Some(vec!["search_capabilities".to_string()])
        );
        let plain = resolve_scope(
            &Actor::new("t1", "analyst"),
            &GroupRequest::default(),
            &p,
            "strict",
            None,
        )
        .unwrap();
        assert_eq!(plain.allowed_builtin_tools, None);
    }
}
