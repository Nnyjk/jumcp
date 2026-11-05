// 能力抽象与内存 catalog：可见性 published ∧ discoverable ∧ group ∈ scope。
// 为什么硬过滤在内部：防能力探测（§2.6），不可见与不存在同构。

use hjzl_core::budget::estimate_tokens;
use hjzl_core::entry::EffectiveScope;
use serde_json::{Value, json};

#[derive(Debug, Clone)]
pub struct Capability {
    pub id: String,
    pub kind: String,
    pub group: String,
    pub risk: String,
    pub status: String,
    pub discoverable: bool,
    pub callable: bool,
    pub one_line: String,
    pub description: String,
    pub params_schema: Value,
}
impl Capability {
    pub fn new(id: &str, kind: &str, group: &str) -> Self {
        Self {
            id: id.into(),
            kind: kind.into(),
            group: group.into(),
            risk: "read".into(),
            status: "published".into(),
            discoverable: true,
            callable: true,
            one_line: String::new(),
            description: String::new(),
            params_schema: json!({"type":"object","properties":{}}),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CapabilityHit {
    pub id: String,
    pub kind: String,
    pub group: String,
    pub one_line: String,
    pub param_summary: String,
    pub token_cost: i64,
}

#[derive(Debug, Default)]
pub struct Catalog {
    pub capabilities: Vec<Capability>,
}
impl Catalog {
    pub fn register(&mut self, cap: Capability) {
        self.capabilities.push(cap);
    }
    pub fn visible(&self, scope: &EffectiveScope) -> Vec<&Capability> {
        self.capabilities
            .iter()
            .filter(|c| {
                c.status == "published" && c.discoverable && scope.groups.contains(&c.group)
            })
            .collect()
    }
    pub fn search(
        &self,
        scope: &EffectiveScope,
        query: &str,
        kind: Option<&str>,
        group: Option<&str>,
        limit: usize,
    ) -> Vec<CapabilityHit> {
        let q = query.trim().to_lowercase();
        let mut hits: Vec<&Capability> = self
            .visible(scope)
            .into_iter()
            .filter(|c| {
                (kind.is_none() || kind == Some(c.kind.as_str()))
                    && (group.is_none() || group == Some(c.group.as_str()))
                    && (q.is_empty()
                        || c.id.to_lowercase().contains(&q)
                        || c.one_line.to_lowercase().contains(&q)
                        || c.description.to_lowercase().contains(&q))
            })
            .collect();
        hits.sort_by(|a, b| a.id.cmp(&b.id));
        hits.truncate(limit);
        hits.into_iter()
            .map(|c| CapabilityHit {
                id: c.id.clone(),
                kind: c.kind.clone(),
                group: c.group.clone(),
                one_line: c.one_line.clone(),
                param_summary: schema_summary(&c.params_schema),
                token_cost: estimate_tokens(&c.params_schema),
            })
            .collect()
    }
    pub fn describe(&self, scope: &EffectiveScope, id: &str) -> Option<&Capability> {
        self.visible(scope).into_iter().find(|c| c.id == id)
    }
    pub fn group_counts(&self, scope: &EffectiveScope) -> Vec<Value> {
        use std::collections::BTreeMap;
        let mut counts: BTreeMap<String, i64> = BTreeMap::new();
        for c in self.visible(scope) {
            *counts.entry(c.group.clone()).or_default() += 1;
        }
        counts
            .into_iter()
            .map(|(g, n)| json!({"group": g, "capability_count": n}))
            .collect()
    }
}

fn schema_summary(schema: &Value) -> String {
    if let Some(props) = schema.get("properties").and_then(|p| p.as_object()) {
        if props.is_empty() {
            return String::new();
        }
        let mut names: Vec<&str> = props.keys().map(|s| s.as_str()).collect();
        names.sort();
        return format!("params: {}", names.join(", "));
    }
    String::new()
}

pub fn token_cost_of(schema: &Value) -> i64 {
    estimate_tokens(schema)
}
