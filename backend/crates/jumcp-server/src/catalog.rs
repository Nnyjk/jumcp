// 能力抽象与内存目录：可见性 published ∧ discoverable ∧ group ∈ scope。
// 为什么硬过滤在内部：防能力探测（§2.6），不可见与不存在同构。
//
// 本模块只描述"一条能力长什么样"以及如何在一个已确定可见的能力集合上检索；
// 可见集合怎么拼（builtin ∪ store ∪ 内存目录 ∪ 上游）只有 `discovery` 一处。

use jumcp_core::budget::estimate_tokens;
use jumcp_core::entry::EffectiveScope;
use serde_json::{Value, json};

/// 内置常驻工具：定义来自代码，注册表不可写。
pub const SOURCE_BUILTIN: &str = "builtin";
/// 治理面维护的能力（注册表行 / 内存注入的能力）。
pub const SOURCE_MANAGED: &str = "managed";
/// 动态插入的上游 MCP server 工具（定义来自上游 `tools/list`，不落本服务注册表）。
pub const SOURCE_UPSTREAM: &str = "upstream";

/// 分类标签（与授权解耦：basic/analysis 不是 `scope.groups` 的授权组）。
pub const CATEGORY_BASIC: &str = "basic";
pub const CATEGORY_ANALYSIS: &str = "analysis";
pub const CATEGORY_ADMIN: &str = "admin";
pub const CATEGORY_CUSTOM: &str = "custom";

/// 上游工具的来源坐标：`tools/list` 与审计据此从一个合成名回到上游原始工具。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamSource {
    pub server_id: String,
    pub tool: String,
}

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
    /// `builtin` | `managed` | `upstream`：客户端据此区分定义来源。
    pub source: String,
    /// 分类标签：`basic|analysis|admin|custom`，只用于展示与检索过滤，不参与授权。
    pub category: String,
    /// 上游声明的 MCP annotations；内置/托管为 `None`（输出时按 `risk` 保守生成：
    /// `read` → 只读注解，否则 → 协议缺省 destructive 注解）。
    pub annotations: Option<Value>,
    /// 上游来源坐标；非上游能力为 `None`。
    pub upstream: Option<UpstreamSource>,
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
            source: SOURCE_MANAGED.into(),
            category: CATEGORY_CUSTOM.into(),
            annotations: None,
            upstream: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CapabilityHit {
    pub id: String,
    pub kind: String,
    pub group: String,
    pub category: String,
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
    /// 内存目录内的可见子集（同一谓词）；发现面的并集在 `discovery`。
    pub fn visible(&self, scope: &EffectiveScope) -> Vec<&Capability> {
        self.capabilities
            .iter()
            .filter(|c| is_visible(c, scope))
            .collect()
    }
}

/// 可见谓词：已发布 ∧ 可发现 ∧ 组在 scope 内。
pub fn is_visible(cap: &Capability, scope: &EffectiveScope) -> bool {
    cap.status == "published" && cap.discoverable && scope.groups.contains(&cap.group)
}

/// 调用谓词：已发布 ∧ 可调用 ∧ 组在 scope 内（与 `is_visible` 是两条不同谓词，§4.4.1）。
pub fn is_callable(cap: &Capability, scope: &EffectiveScope) -> bool {
    cap.status == "published" && cap.callable && scope.groups.contains(&cap.group)
}

/// 在一个已确定的可见集合上检索（不重复做可见性判断）。
pub fn search(
    capabilities: &[Capability],
    query: &str,
    kind: Option<&str>,
    group: Option<&str>,
    category: Option<&str>,
    limit: usize,
) -> Vec<CapabilityHit> {
    let q = query.trim().to_lowercase();
    let mut hits: Vec<&Capability> = capabilities
        .iter()
        .filter(|c| {
            (kind.is_none() || kind == Some(c.kind.as_str()))
                && (group.is_none() || group == Some(c.group.as_str()))
                && (category.is_none() || category == Some(c.category.as_str()))
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
            category: c.category.clone(),
            one_line: c.one_line.clone(),
            param_summary: schema_summary(&c.params_schema),
            token_cost: estimate_tokens(&c.params_schema),
        })
        .collect()
}

pub fn find<'a>(capabilities: &'a [Capability], id: &str) -> Option<&'a Capability> {
    capabilities.iter().find(|c| c.id == id)
}

/// 业务组索引：内置工具不是业务组（§4.2），上游工具按它注册时的组计数。
pub fn group_counts(capabilities: &[Capability]) -> Vec<Value> {
    use std::collections::BTreeMap;
    let mut counts: BTreeMap<String, i64> = BTreeMap::new();
    for c in capabilities {
        // 常驻工具不是业务组：`list_groups` 是业务组索引（§4.2），不能让调用方以为存在
        // 一个叫 builtin 的组。
        if c.source == SOURCE_BUILTIN {
            continue;
        }
        *counts.entry(c.group.clone()).or_default() += 1;
    }
    counts
        .into_iter()
        .map(|(g, n)| json!({"group": g, "capability_count": n}))
        .collect()
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
