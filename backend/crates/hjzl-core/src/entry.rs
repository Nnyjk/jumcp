//! 平面 A 纯函数：三通道解析 → 越界判定 → EffectiveScope，严格按序（填充默认→越界→预算）。
//! 为什么顺序不可调：先交集使 strict 恒假，先截断掩盖越界（§4.1.3）。

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

#[derive(Debug, Clone)]
pub struct Actor {
    pub tenant: String,
    pub role: String,
    pub actor_type: String,
    pub actor_id: String,
}
impl Actor {
    pub fn new(tenant: &str, role: &str) -> Self {
        Self {
            tenant: tenant.into(),
            role: role.into(),
            actor_type: "agent".into(),
            actor_id: String::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct EffectiveScope {
    pub groups: Vec<String>,
    pub budget: i64,
    pub degraded: bool,
    pub source: String,
    pub mode: String,
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
    let mut requested = declared.unwrap_or_else(|| role_def.default_groups.clone());
    let allowlist = role_def.default_groups.clone();
    let registered = policy.group_names();
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
                    name: "db.read.core".into(),
                    priority: 10,
                    min_budget: 800,
                },
                GroupDef {
                    name: "db.read.analytics".into(),
                    priority: 8,
                    min_budget: 800,
                },
                GroupDef {
                    name: "db.admin.schema".into(),
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
}
