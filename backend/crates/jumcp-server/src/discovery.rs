//! 发现面的唯一可见集：`builtin 常驻工具 ∪ 注册表可见行 ∪ 内存目录可见条目`。
//!
//! 为什么只允许一个生产函数：`search_capabilities` / `describe_capability` / `list_groups` /
//! `tools/list` 的平铺 / 模板与上游工具的可见性判定必须同一口径，否则会出现
//! "搜得到但调不了"或"调得了但搜不到"的双套口径漂移。
//!
//! 为什么不缓存：每次请求按 scope 从 SQLite 现查（本地读，代价可接受），
//! 于是管理面 publish / 发布新版本 / 改可见性 / 下架后，**下一个** MCP 请求即可见变化。

use crate::catalog::{
    CATEGORY_ANALYSIS, Capability, Catalog, SOURCE_BUILTIN, SOURCE_MANAGED, SOURCE_UPSTREAM,
    UpstreamSource,
};
use crate::mcp::{admin_tool_definitions, builtin_category, tool_definitions};
use crate::service::{ADMIN_TOOL_NAMES, RESIDENT_TOOL_NAMES};
use jumcp_core::entry::EffectiveScope;
use jumcp_core::errors::McpError;
use jumcp_core::registry::{CapabilityRecord, CapabilityStore, McpUpstream, UpstreamTool};
use serde_json::{Map, Value, json};
use std::collections::HashSet;

/// `basic` 组：常驻内置工具与"逐工具开放的上游工具"的公共基础通道。
///
/// 它不是可授权组：`scope.groups` 里永远不需要（也不应该）出现 `basic`，
/// 成员恒可见，只受 token 的 `builtin_tools` allowlist 与上游自身开关约束。
pub const BASIC_GROUP: &str = jumcp_core::config::BASIC_GROUP_NAME;

/// 维护通道的组名：内置 admin 工具只在 `scope.groups` 含它时可见可调。
///
/// 组是部署配置（`config/policy.toml` 的 `[[groups]]` + 角色的 `default_groups`），
/// 因此"谁是 admin"由策略决定，不由请求自述决定。
pub const ADMIN_GROUP: &str = "admin";

/// 维护通道开关的唯一判定：发现面投影、`tools/list` 与调用前授权共用这一处。
pub fn admin_enabled(scope: &EffectiveScope) -> bool {
    scope.groups.iter().any(|g| g == ADMIN_GROUP)
}

/// 工具 allowlist 的唯一判定：`None` = 不限制（stdio / 管理面），`Some` = 精确白名单。
///
/// 名字同时覆盖内置 id 与上游暴露名（`<server_id>__<tool>`）：`tools/list`、发现面投影与
/// `tools/call` 三处共用这一处实现，避免"列表里没有、调用却放行"。
/// 与 `admin_enabled` 是**双重判定**：admin_* 既要 allowlist 命中，也要 admin 组在 scope 内；
/// 两者都不能由请求头/`_meta` 扩大（allowlist 来自 token 配置，header 只能收窄组）。
pub fn tool_allowed(scope: &EffectiveScope, id: &str) -> bool {
    match &scope.allowed_builtin_tools {
        None => true,
        Some(allow) => allow.iter().any(|allowed| allowed == id),
    }
}

fn empty_schema() -> Value {
    json!({"type": "object", "properties": {}})
}

/// 单条内置定义 → 发现面能力（定义只在代码里，`source=builtin`）。
fn project_builtin(def: &Value, group: &str, category: &str) -> Option<Capability> {
    let id = def.get("name")?.as_str()?.to_string();
    let text = def
        .get("description")
        .and_then(|d| d.as_str())
        .unwrap_or_default()
        .to_string();
    Some(Capability {
        id,
        kind: "tool".into(),
        group: group.into(),
        risk: "read".into(),
        status: "published".into(),
        discoverable: true,
        callable: true,
        one_line: text.clone(),
        description: text,
        params_schema: def.get("inputSchema").cloned().unwrap_or_else(empty_schema),
        source: SOURCE_BUILTIN.into(),
        category: category.into(),
        annotations: None,
        upstream: None,
    })
}

/// builtin 能力投影：直接复用 `mcp::tool_definitions()` / `mcp::admin_tool_definitions()`
/// （`tools/list` 的同一份定义），不在这里再写一份 schema，否则同一工具会有两个定义源。
///
/// 分组规则（唯一实现）：`group == BASIC_GROUP` 的常驻工具恒在；维护工具属于
/// `ADMIN_GROUP`，只有 `scope.groups` 含 admin 时才进可见集/调用集。
pub fn builtin_capabilities(scope: &EffectiveScope) -> Vec<Capability> {
    let mut out: Vec<Capability> = tool_definitions()
        .iter()
        .filter(|def| {
            def.get("name")
                .and_then(|n| n.as_str())
                .is_some_and(|id| RESIDENT_TOOL_NAMES.contains(&id) && tool_allowed(scope, id))
        })
        .filter_map(|def| {
            let id = def.get("name").and_then(|n| n.as_str())?;
            project_builtin(def, BASIC_GROUP, builtin_category(id))
        })
        .collect();
    if admin_enabled(scope) {
        out.extend(
            admin_tool_definitions()
                .iter()
                .filter(|def| {
                    def.get("name")
                        .and_then(|n| n.as_str())
                        .is_some_and(|id| ADMIN_TOOL_NAMES.contains(&id) && tool_allowed(scope, id))
                })
                .filter_map(|def| {
                    let id = def.get("name").and_then(|n| n.as_str())?;
                    project_builtin(def, ADMIN_GROUP, builtin_category(id))
                }),
        );
    }
    out
}

/// 注册表行 → 发现面能力。
///
/// `kind=template` 由 `definition.params` 生成 JSON Schema；`kind=endpoint|tool` 优先复用
/// 定义里已有的 `params_schema` / `inputSchema`，两者都没有时退化为空对象 schema（不 panic）。
pub fn project_record(rec: &CapabilityRecord) -> Capability {
    let text = describe_text(&rec.definition);
    let one_line = text.clone().unwrap_or_else(|| rec.capability_id.clone());
    let params_schema = if rec.kind == "template" {
        template_params_schema(&rec.definition)
    } else {
        declared_schema(&rec.definition)
    };
    Capability {
        id: rec.capability_id.clone(),
        kind: rec.kind.clone(),
        group: rec.group.clone(),
        risk: rec.risk.clone(),
        status: rec.status.clone(),
        discoverable: rec.discoverable,
        callable: rec.callable,
        one_line,
        description: text.unwrap_or_default(),
        params_schema,
        source: SOURCE_MANAGED.into(),
        // 治理面发布的能力都是本服务的数据入口（模板/端点/工具），归入 analysis。
        category: CATEGORY_ANALYSIS.into(),
        annotations: None,
        upstream: None,
    }
}

/// 上游 `tools/list` 的一条工具 → 发现面能力。
///
/// 名由 manager 先按 `<server_id>__<tool>` 规则规范化并查重；这里只做投影。
/// `group` 是**生效组**：默认是上游注册时的 `group`，被逐工具覆盖到 `basic` 时由调用方传入。
pub fn project_upstream(
    upstream: &McpUpstream,
    tool: &UpstreamTool,
    exposed: &str,
    group: &str,
) -> Capability {
    let text = tool.description.clone();
    Capability {
        id: exposed.to_string(),
        kind: "tool".into(),
        group: group.to_string(),
        // 非只读上游是人工审批放行的高风险能力，沿用闭集里最保守的标签。
        risk: if upstream.read_only {
            "read"
        } else {
            "sensitive"
        }
        .into(),
        status: "published".into(),
        discoverable: true,
        callable: true,
        one_line: if text.is_empty() {
            tool.name.clone()
        } else {
            text.clone()
        },
        description: text,
        params_schema: if tool.input_schema.is_object() {
            tool.input_schema.clone()
        } else {
            empty_schema()
        },
        source: SOURCE_UPSTREAM.into(),
        category: upstream.category.clone(),
        annotations: tool.annotations.clone(),
        upstream: Some(UpstreamSource {
            server_id: upstream.id.clone(),
            tool: tool.name.clone(),
        }),
    }
}

/// 说明文案：`definition.description` → `definition.template.description` → 无。
fn describe_text(definition: &Value) -> Option<String> {
    let direct = definition
        .get("description")
        .and_then(|d| d.as_str())
        .filter(|s| !s.is_empty());
    let nested = definition
        .get("template")
        .and_then(|t| t.get("description"))
        .and_then(|d| d.as_str())
        .filter(|s| !s.is_empty());
    direct.or(nested).map(String::from)
}

/// endpoint / tool：定义自带的 schema 直接用；没有就是空对象 schema。
fn declared_schema(definition: &Value) -> Value {
    for key in ["params_schema", "inputSchema"] {
        if let Some(schema) = definition.get(key)
            && schema.is_object()
        {
            return schema.clone();
        }
    }
    empty_schema()
}

/// template：`definition.params`（或 `definition.template.params`）→ JSON Schema。
fn template_params_schema(definition: &Value) -> Value {
    let params = definition
        .get("params")
        .or_else(|| definition.get("template").and_then(|t| t.get("params")))
        .and_then(|p| p.as_array());
    let mut properties = Map::new();
    let mut required: Vec<Value> = Vec::new();
    for spec in params.into_iter().flatten() {
        let Some(name) = spec.get("name").and_then(|n| n.as_str()) else {
            continue;
        };
        properties.insert(name.to_string(), param_schema(spec));
        if spec
            .get("required")
            .and_then(|r| r.as_bool())
            .unwrap_or(false)
        {
            required.push(json!(name));
        }
    }
    let mut schema = json!({"type": "object", "properties": properties});
    if !required.is_empty() {
        schema["required"] = Value::Array(required);
    }
    schema
}

/// 单个参数声明 → property schema：类型、说明、枚举、长度与数值约束能映射就映射。
fn param_schema(spec: &Value) -> Value {
    let declared = spec
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or("string")
        .trim()
        .to_ascii_lowercase();
    let (json_type, format) = match declared.as_str() {
        "int" | "integer" => ("integer", None),
        "float" | "number" | "double" => ("number", None),
        "bool" | "boolean" => ("boolean", None),
        "date" => ("string", Some("date")),
        "timestamp" | "datetime" => ("string", Some("date-time")),
        // string / enum / column_ref / 未知类型：统一按字符串暴露（enum 值另置于 enum 字段）。
        _ => ("string", None),
    };
    let mut out = Map::new();
    out.insert("type".into(), json!(json_type));
    if let Some(fmt) = format {
        out.insert("format".into(), json!(fmt));
    }
    if let Some(desc) = spec.get("description").and_then(|d| d.as_str()) {
        out.insert("description".into(), json!(desc));
    }
    if let Some(values) = spec.get("enum").and_then(|e| e.as_array())
        && !values.is_empty()
    {
        out.insert("enum".into(), Value::Array(values.clone()));
    }
    if let Some(max_len) = spec.get("max_len").and_then(|v| v.as_u64()) {
        out.insert("maxLength".into(), json!(max_len));
    }
    if let Some(min) = spec.get("min").and_then(|v| v.as_f64()) {
        out.insert("minimum".into(), json!(min));
    }
    if let Some(max) = spec.get("max").and_then(|v| v.as_f64()) {
        out.insert("maximum".into(), json!(max));
    }
    if let Some(pattern) = spec.get("pattern").and_then(|p| p.as_str()) {
        out.insert("pattern".into(), json!(pattern));
    }
    Value::Object(out)
}

/// 唯一可见集生产函数：builtin 恒在，随后注册表可见行（每次请求现查），最后内存目录。
///
/// 去重按 id，先到先得：builtin > 注册表 > 内存目录——内置定义不允许被注册表影子覆盖。
pub fn visible_capabilities(
    catalog: &Catalog,
    store: Option<&CapabilityStore>,
    scope: &EffectiveScope,
) -> Result<Vec<Capability>, McpError> {
    let store_rows = match store {
        Some(store) => store
            .visible(&scope.groups)?
            .iter()
            .map(project_record)
            .collect(),
        None => Vec::new(),
    };
    Ok(union(store_rows, catalog.visible(scope), scope))
}

/// 调用面集合：`published ∧ callable ∧ group ∈ scope`。
///
/// 与可见集**不是**同一个集合（`discoverable=false, callable=true` 是合法的
/// "系统直调"状态）。两条谓词共用 `CapabilityStore::visible` / `CapabilityStore::callable`
/// 背后的同一处 SQL 拼装，因此不会各写一份 WHERE。
pub fn callable_capabilities(
    catalog: &Catalog,
    store: Option<&CapabilityStore>,
    scope: &EffectiveScope,
) -> Result<Vec<Capability>, McpError> {
    let store_rows = match store {
        Some(store) => store
            .callable(&scope.groups)?
            .iter()
            .map(project_record)
            .collect(),
        None => Vec::new(),
    };
    let catalog_rows: Vec<&Capability> = catalog
        .capabilities
        .iter()
        .filter(|c| crate::catalog::is_callable(c, scope))
        .collect();
    Ok(union(store_rows, catalog_rows, scope))
}

/// 两路结果的并集：builtin 恒在（admin 组的内置工具按 scope 过滤），去重按 id 先到先得。
fn union(
    store_rows: Vec<Capability>,
    catalog_rows: Vec<&Capability>,
    scope: &EffectiveScope,
) -> Vec<Capability> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<Capability> = Vec::new();
    for cap in builtin_capabilities(scope) {
        if seen.insert(cap.id.clone()) {
            out.push(cap);
        }
    }
    for cap in store_rows {
        if seen.insert(cap.id.clone()) {
            out.push(cap);
        }
    }
    for cap in catalog_rows {
        if seen.insert(cap.id.clone()) {
            out.push(cap.clone());
        }
    }
    out
}

/// 把上游投影并入已确定的可见集。
///
/// 去重按 id 先到先得：内置 > 注册表 > 内存目录 > 上游。上游与内置/其他上游的命名冲突
/// 在注册/刷新期已拒绝（不静默加后缀）；这里只防"同一 id 两条投影"。
pub fn merge_exposed(mut base: Vec<Capability>, extra: Vec<Capability>) -> Vec<Capability> {
    let mut seen: HashSet<String> = base.iter().map(|c| c.id.clone()).collect();
    for cap in extra {
        if seen.insert(cap.id.clone()) {
            base.push(cap);
        }
    }
    base
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(kind: &str, definition: Value) -> CapabilityRecord {
        CapabilityRecord {
            capability_id: "cap.test".into(),
            kind: kind.into(),
            group: "db.read.analytics".into(),
            risk: "read".into(),
            status: "published".into(),
            discoverable: true,
            callable: true,
            version: 1,
            definition,
            definition_hash: "x".into(),
            successor_id: String::new(),
            updated_at: String::new(),
            submitted_by: "alice".into(),
            submitted_at: String::new(),
        }
    }

    /// 模板参数 → JSON Schema：类型、枚举、长度/数值约束、required 都要能映射。
    #[test]
    fn template_definition_projects_param_constraints_into_json_schema() {
        let definition = json!({
            "template": {
                "description": "按渠道汇总",
                "params": [
                    {"name":"channel","type":"enum","enum":["app","web"],"required":true,"description":"渠道"},
                    {"name":"amount","type":"float","min":0.0,"max":100.0},
                    {"name":"day","type":"date","required":true},
                    {"name":"note","type":"string","max_len":32,"pattern":"^[a-z]+$"},
                    {"name":"flag","type":"bool"}
                ]
            }
        });
        let cap = project_record(&record("template", definition));
        assert_eq!(cap.source, SOURCE_MANAGED);
        assert_eq!(cap.kind, "template");
        assert_eq!(cap.one_line, "按渠道汇总");
        let schema = &cap.params_schema;
        assert_eq!(schema["type"], json!("object"));
        assert_eq!(schema["properties"]["channel"]["type"], json!("string"));
        assert_eq!(
            schema["properties"]["channel"]["enum"],
            json!(["app", "web"])
        );
        assert_eq!(
            schema["properties"]["channel"]["description"],
            json!("渠道")
        );
        assert_eq!(schema["properties"]["amount"]["type"], json!("number"));
        assert_eq!(schema["properties"]["amount"]["minimum"], json!(0.0));
        assert_eq!(schema["properties"]["amount"]["maximum"], json!(100.0));
        assert_eq!(schema["properties"]["day"]["format"], json!("date"));
        assert_eq!(schema["properties"]["note"]["maxLength"], json!(32));
        assert_eq!(schema["properties"]["note"]["pattern"], json!("^[a-z]+$"));
        assert_eq!(schema["properties"]["flag"]["type"], json!("boolean"));
        assert_eq!(schema["required"], json!(["channel", "day"]));
    }

    #[test]
    fn template_definition_supports_top_level_params_and_id_fallback() {
        let definition = json!({"params": [{"name":"id","type":"int"}]});
        let cap = project_record(&record("template", definition));
        assert_eq!(cap.one_line, "cap.test", "没有说明文案时回落到 id");
        assert_eq!(
            cap.params_schema["properties"]["id"]["type"],
            json!("integer")
        );
        assert!(
            cap.params_schema.get("required").is_none(),
            "没有必填参数时不写 required 键"
        );
    }

    fn scope(groups: &[&str]) -> EffectiveScope {
        EffectiveScope {
            groups: groups.iter().map(|g| (*g).to_string()).collect(),
            budget: 2000,
            degraded: false,
            source: "default".into(),
            mode: "strict".into(),
            allowed_builtin_tools: None,
        }
    }

    /// admin 组的内置工具必须受 scope.groups 过滤：非 admin scope 下它们既不可见也不可调。
    #[test]
    fn builtin_projection_gates_admin_tools_by_scope() {
        let catalog = Catalog::default();
        let analyst = visible_capabilities(&catalog, None, &scope(&["db.read.core"])).unwrap();
        assert_eq!(
            analyst.len(),
            crate::service::RESIDENT_TOOL_NAMES.len(),
            "非 admin scope 只应看到常驻工具"
        );
        assert!(
            analyst.iter().all(|c| c.group == BASIC_GROUP),
            "非 admin scope 不得出现 admin 组能力"
        );
        assert!(
            analyst
                .iter()
                .all(|c| !crate::service::ADMIN_TOOL_NAMES.contains(&c.id.as_str()))
        );

        let admin = visible_capabilities(&catalog, None, &scope(&["admin"])).unwrap();
        assert_eq!(
            admin.len(),
            crate::service::RESIDENT_TOOL_NAMES.len() + crate::service::ADMIN_TOOL_NAMES.len()
        );
        for id in crate::service::ADMIN_TOOL_NAMES {
            let cap = admin
                .iter()
                .find(|c| c.id == *id)
                .expect("admin 工具必须可见");
            assert_eq!(cap.group, ADMIN_GROUP);
            assert_eq!(cap.source, SOURCE_BUILTIN);
        }
        // 调用面与发现面同口径：非 admin 的调用集里没有 admin 工具。
        let analyst_callable =
            callable_capabilities(&catalog, None, &scope(&["db.read.core"])).unwrap();
        assert!(
            analyst_callable
                .iter()
                .all(|c| !crate::service::ADMIN_TOOL_NAMES.contains(&c.id.as_str()))
        );
    }

    #[test]
    fn non_template_definition_prefers_declared_schema_then_empty_object() {
        let bare = project_record(&record("endpoint", json!({"description":"裸端点"})));
        assert_eq!(
            bare.params_schema,
            json!({"type": "object", "properties": {}}),
            "没有任何 schema 的 endpoint 不得 panic，退化为空对象"
        );
        let declared = project_record(&record(
            "tool",
            json!({"params_schema":{"type":"object","properties":{"q":{"type":"string"}}}}),
        ));
        assert_eq!(
            declared.params_schema["properties"]["q"]["type"],
            json!("string")
        );
        let input = project_record(&record(
            "tool",
            json!({"inputSchema":{"type":"object","properties":{"k":{"type":"integer"}}}}),
        ));
        assert_eq!(
            input.params_schema["properties"]["k"]["type"],
            json!("integer")
        );
    }
}
