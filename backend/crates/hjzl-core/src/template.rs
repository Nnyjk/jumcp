//! 模板定义、校验门禁与注册表（W3.2 / §4.3.3–§4.3.6）。
//!
//! 设计约束：
//! - 只允许三类结构变体：可选过滤、`{{col:enum}}` 白名单标识符、显式 variants 矩阵。
//!   其它 `{{ }}`、Jinja 表达式一律注册即拒——模板不是模板引擎。
//! - 命名占位符 `:name` 在**注册时**一次性改写为 `$n`（按 params 声明顺序、只对实际用到的参数编号），
//!   运行期只做位置绑定；改写映射随定义入库，审计与错误定位据此还原参数名。
//! - 校验门禁与注销共用同一个 AST 校验器（hjzl-sqlguard）：渲染后必须仍是单条 SELECT/WITH。
//! - 生命周期/版本/四眼复用 `CapabilityStore`（kind=template），避免第二套治理实现分叉。

use crate::canonical::{canonical_json, definition_hash, sha256_hex};
use crate::errors::McpError;
use crate::registry::{CapabilityRecord, CapabilityStore};
use hjzl_sqlguard::{Dialect, GuardError};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const PARAM_TYPES: &[&str] = &[
    "string",
    "int",
    "float",
    "bool",
    "date",
    "timestamp",
    "enum",
    "column_ref",
];

fn default_risk() -> String {
    "read".to_string()
}
fn default_auto_limit() -> i64 {
    1000
}
fn default_max_result_tokens() -> i64 {
    1500
}
fn default_statement_timeout() -> i64 {
    15_000
}
fn default_cache_ttl() -> i64 {
    60
}
fn default_required() -> bool {
    false
}
fn default_nullable() -> bool {
    true
}
fn default_empty_string_as_null() -> bool {
    true
}

/// 模板参数声明（§4.3.3 参数类型系统）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ParamSpec {
    pub name: String,
    #[serde(rename = "type")]
    pub param_type: String,
    #[serde(default = "default_required")]
    pub required: bool,
    #[serde(default = "default_nullable")]
    pub nullable: bool,
    #[serde(default)]
    pub max_len: Option<usize>,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub pattern: Option<String>,
    #[serde(default)]
    pub r#enum: Vec<String>,
    #[serde(default)]
    pub timezone: Option<String>,
    #[serde(default = "default_empty_string_as_null")]
    pub empty_string_as_null: bool,
    #[serde(default)]
    pub array_max_items: Option<usize>,
}

impl ParamSpec {
    fn is_column_ref(&self) -> bool {
        self.param_type.trim().eq_ignore_ascii_case("column_ref")
    }
    fn is_array(&self) -> bool {
        self.param_type
            .trim()
            .to_ascii_lowercase()
            .starts_with("array")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Constraints {
    #[serde(default)]
    pub allowed_schemas: Vec<String>,
    #[serde(default = "default_auto_limit")]
    pub auto_limit: i64,
    #[serde(default = "default_max_result_tokens")]
    pub max_result_tokens: i64,
    #[serde(default = "default_statement_timeout")]
    pub statement_timeout_ms: i64,
    #[serde(default)]
    pub forbidden_columns: Vec<String>,
}
impl Default for Constraints {
    fn default() -> Self {
        Self {
            allowed_schemas: vec![],
            auto_limit: default_auto_limit(),
            max_result_tokens: default_max_result_tokens(),
            statement_timeout_ms: default_statement_timeout(),
            forbidden_columns: vec![],
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Preflight {
    #[serde(default)]
    pub max_scan_rows: Option<i64>,
    #[serde(default)]
    pub require_index: bool,
}

/// 显式变体：when 条件命中即用该 SQL（每个变体独立预审核/预检）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Variant {
    pub sql_id: String,
    pub sql: String,
    #[serde(default)]
    pub when: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TemplateDef {
    pub template_id: String,
    #[serde(default)]
    pub version: i64,
    pub group: String,
    #[serde(default = "default_risk")]
    pub risk: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub sql: String,
    #[serde(default)]
    pub params: Vec<ParamSpec>,
    #[serde(default)]
    pub constraints: Constraints,
    #[serde(default)]
    pub preflight: Preflight,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub discoverable: bool,
    #[serde(default)]
    pub callable: bool,
    #[serde(default)]
    pub owner: String,
    #[serde(default)]
    pub variants: Vec<Variant>,
    /// 0 表示不缓存；上限 300（§4.3.6）。
    #[serde(default = "default_cache_ttl")]
    pub result_cache_ttl_s: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CompiledVariant {
    pub sql_id: String,
    pub when: Map<String, Value>,
    /// 命名占位符已改写为位置占位符；`{{col:enum}}` 标记保留到运行期替换。
    pub sql: String,
    /// 位置 i → 参数名（只含该变体实际用到的参数）。
    pub bind_order: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CompiledTemplate {
    /// 原样保留定义（含原始 sql），供审计与 diff。
    pub def: TemplateDef,
    pub base_sql: String,
    pub bind_order: Vec<String>,
    pub variants: Vec<CompiledVariant>,
    pub definition_hash: String,
}

/// 预检回执：把"预检过的定义"和"要发布的定义"绑定起来，防止预检 A 发布 B。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightReceipt {
    pub template_id: String,
    pub base_sql_hash: String,
}

fn template_error(rule: &str, message: &str, detail: Value) -> McpError {
    let mut data = json!({"rule": rule, "message": message});
    if let (Some(obj), Some(extra)) = (data.as_object_mut(), detail.as_object()) {
        for (k, v) in extra {
            obj.insert(k.clone(), v.clone());
        }
    }
    McpError::with_message("MCP_TEMPLATE_INVALID", message.to_string(), data)
}

fn guard_to_mcp(e: GuardError) -> McpError {
    template_error(
        "guard_rejected",
        &e.message,
        json!({"guard_code": e.code, "guard_data": e.data}),
    )
}

impl TemplateDef {
    /// 严格解析 + 语义校验（非法定义在注册期一次性拒绝，运行期不再重复校验）。
    pub fn parse(value: &Value) -> Result<Self, McpError> {
        let def: TemplateDef = serde_json::from_value(value.clone())
            .map_err(|e| template_error("schema", &format!("模板定义不合法: {e}"), json!({})))?;
        def.validate()?;
        Ok(def)
    }

    fn validate(&self) -> Result<(), McpError> {
        if !is_template_id(&self.template_id) {
            return Err(template_error(
                "template_id_format",
                "template_id 必须是点号命名空间（例：order.daily_summary）",
                json!({"template_id": self.template_id}),
            ));
        }
        if self.group.trim().is_empty() {
            return Err(template_error(
                "group_required",
                "模板必须声明 group",
                json!({}),
            ));
        }
        // 本期只读：写类能力不注册（§1.3），注册期就拒绝而不是运行期再拒。
        if self.risk != "read" {
            return Err(template_error(
                "risk_not_readonly",
                "本期只注册 risk=read 的模板",
                json!({"risk": self.risk}),
            ));
        }
        if self.sql.trim().is_empty() {
            return Err(template_error("sql_required", "sql 不能为空", json!({})));
        }
        let mut seen: HashSet<&str> = HashSet::new();
        for p in &self.params {
            if !seen.insert(p.name.as_str()) {
                return Err(template_error(
                    "duplicate_param",
                    "参数名重复",
                    json!({"param": p.name}),
                ));
            }
            validate_param_spec(p)?;
        }
        if self.constraints.auto_limit <= 0 || self.constraints.auto_limit > 10_000 {
            return Err(template_error(
                "auto_limit_range",
                "auto_limit 必须在 1..=10000",
                json!({"auto_limit": self.constraints.auto_limit}),
            ));
        }
        if self.constraints.max_result_tokens <= 0 {
            return Err(template_error(
                "max_result_tokens_invalid",
                "max_result_tokens 必须为正",
                json!({}),
            ));
        }
        if self.constraints.statement_timeout_ms <= 0 {
            return Err(template_error(
                "statement_timeout_invalid",
                "statement_timeout_ms 必须为正",
                json!({}),
            ));
        }
        if self.result_cache_ttl_s < 0 || self.result_cache_ttl_s > 300 {
            return Err(template_error(
                "cache_ttl_range",
                "result_cache_ttl_s 必须在 0..=300",
                json!({"result_cache_ttl_s": self.result_cache_ttl_s}),
            ));
        }
        let mut ids: HashSet<&str> = HashSet::new();
        for v in &self.variants {
            if !ids.insert(v.sql_id.as_str()) {
                return Err(template_error(
                    "duplicate_variant",
                    "变体 sql_id 重复",
                    json!({"sql_id": v.sql_id}),
                ));
            }
            for key in v.when.keys() {
                if !seen.contains(key.as_str()) {
                    return Err(template_error(
                        "variant_unknown_param",
                        "变体 when 引用了未声明参数",
                        json!({"sql_id": v.sql_id, "param": key}),
                    ));
                }
            }
        }
        Ok(())
    }
}

fn validate_param_spec(p: &ParamSpec) -> Result<(), McpError> {
    if !is_lower_ident(&p.name) {
        return Err(template_error(
            "param_name_format",
            "参数名必须是小写下划线标识符",
            json!({"param": p.name}),
        ));
    }
    let ty = p.param_type.trim().to_ascii_lowercase();
    let inner = ty
        .strip_prefix("array<")
        .and_then(|rest| rest.strip_suffix('>'))
        .map(str::trim);
    let base = inner.unwrap_or(ty.as_str());
    if !PARAM_TYPES.contains(&base) {
        return Err(template_error(
            "param_type_unknown",
            "未知参数类型",
            json!({"param": p.name, "type": p.param_type}),
        ));
    }
    if inner.is_some() && base == "column_ref" {
        return Err(template_error(
            "param_type_array_column_ref",
            "column_ref 不能是数组",
            json!({"param": p.name}),
        ));
    }
    if base == "enum" && p.r#enum.is_empty() {
        return Err(template_error(
            "enum_values_required",
            "enum 类型必须给出白名单",
            json!({"param": p.name}),
        ));
    }
    if base == "column_ref" && p.r#enum.is_empty() {
        return Err(template_error(
            "column_ref_enum_required",
            "column_ref 必须给出标识符白名单（不做值插值）",
            json!({"param": p.name}),
        ));
    }
    if base == "column_ref" {
        for value in &p.r#enum {
            if !is_ident(value) {
                return Err(template_error(
                    "column_ref_identifier_invalid",
                    "column_ref 白名单只允许合法标识符",
                    json!({"param": p.name, "value": value}),
                ));
            }
        }
    }
    if let Some(pattern) = &p.pattern {
        Regex::new(pattern).map_err(|e| {
            template_error(
                "pattern_invalid",
                "pattern 不是合法正则",
                json!({"param": p.name, "error": e.to_string()}),
            )
        })?;
    }
    Ok(())
}

/// 注册期门禁：结构变体白名单 → 命名占位符改写 → 渲染 → sqlguard 复核。
pub fn compile(def: TemplateDef) -> Result<CompiledTemplate, McpError> {
    let (base_sql, bind_order) = rewrite(&def.sql, &def.params)?;
    let mut variants = Vec::with_capacity(def.variants.len());
    for v in &def.variants {
        let (sql, order) = rewrite(&v.sql, &def.params)?;
        variants.push(CompiledVariant {
            sql_id: v.sql_id.clone(),
            when: v.when.clone(),
            sql,
            bind_order: order,
        });
    }
    let compiled =
        CompiledTemplate {
            definition_hash: definition_hash(&serde_json::to_value(&def).map_err(|e| {
                template_error("schema", &format!("定义序列化失败: {e}"), json!({}))
            })?),
            def,
            base_sql,
            bind_order,
            variants,
        };
    compiled.gate()?;
    Ok(compiled)
}

impl CompiledTemplate {
    /// 用定义 JSON 还原（定义入库时把 compiled 一起存，读取不再重新编译）。
    pub fn from_definition(value: &Value) -> Result<Self, McpError> {
        let def_value = value.get("template").ok_or_else(|| {
            template_error("definition_missing", "定义缺少 template 字段", json!({}))
        })?;
        let def = TemplateDef::parse(def_value)?;
        let base_sql = value
            .get("base_sql")
            .and_then(|v| v.as_str())
            .ok_or_else(|| template_error("definition_missing", "定义缺少 base_sql", json!({})))?
            .to_string();
        let bind_order: Vec<String> = value
            .get("bind_order")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let variants: Vec<CompiledVariant> = value
            .get("variants")
            .cloned()
            .map(|v| serde_json::from_value(v).unwrap_or_default())
            .unwrap_or_default();
        Ok(Self {
            definition_hash: definition_hash(value),
            def,
            base_sql,
            bind_order,
            variants,
        })
    }

    /// 入库形态：定义 + 编译产物（definition_hash 覆盖两者）。
    pub fn to_definition(&self) -> Value {
        json!({
            "template": self.def,
            "base_sql": self.base_sql,
            "bind_order": self.bind_order,
            "variants": self.variants
        })
    }

    /// 校验门禁：对 base 与每个变体、以及 column_ref 的每个白名单值，都必须通过 sqlguard。
    fn gate(&self) -> Result<(), McpError> {
        let schemas = &self.def.constraints.allowed_schemas;
        self.check_sql(&self.base_sql, schemas)?;
        for v in &self.variants {
            self.check_sql(&v.sql, schemas)?;
        }
        Ok(())
    }

    fn check_sql(&self, sql: &str, schemas: &[String]) -> Result<(), McpError> {
        // column_ref 每取一个白名单值都必须仍是合法单条 SELECT：一次校验全部取值。
        let column_refs: Vec<&ParamSpec> = self
            .def
            .params
            .iter()
            .filter(|p| p.is_column_ref())
            .collect();
        if column_refs.is_empty() {
            validate_rendered(sql, schemas)?;
            self.check_forbidden_columns(sql)?;
            return Ok(());
        }
        let mut combinations: Vec<Vec<(usize, &str)>> = vec![Vec::new()];
        for (idx, spec) in column_refs.iter().enumerate() {
            let mut next = Vec::new();
            for combo in &combinations {
                for value in &spec.r#enum {
                    let mut c = combo.clone();
                    c.push((idx, value.as_str()));
                    next.push(c);
                }
            }
            combinations = next;
        }
        for combo in combinations {
            let mut rendered = sql.to_string();
            for (idx, value) in combo {
                let spec = column_refs[idx];
                rendered =
                    rendered.replace(&format!("{{{{{}:enum}}}}", spec.name), &quote_ident(value));
            }
            validate_rendered(&rendered, schemas)?;
            self.check_forbidden_columns(&rendered)?;
        }
        Ok(())
    }

    /// 列黑名单是词法级（sqlguard 只产出表名，不产出列清单）：命中即拒。
    fn check_forbidden_columns(&self, sql: &str) -> Result<(), McpError> {
        if self.def.constraints.forbidden_columns.is_empty() {
            return Ok(());
        }
        let hits = find_forbidden_columns(sql, &self.def.constraints.forbidden_columns);
        if hits.is_empty() {
            return Ok(());
        }
        Err(template_error(
            "forbidden_column",
            "模板引用了黑名单列",
            json!({"columns": hits}),
        ))
    }

    /// 运行期渲染：选变体 → 参数校验 → column_ref 引用替换 → 位置值序列。
    pub fn render(&self, params: &Value) -> Result<PreparedQuery, McpError> {
        let provided = match params {
            Value::Null => Map::new(),
            Value::Object(m) => m.clone(),
            other => {
                return Err(McpError::new(
                    "MCP_VALIDATION_ERROR",
                    json!({"fields": [{"field": "params", "rule": "object_required", "message": format!("params 必须是对象，实际是 {other}")}]}),
                ));
            }
        };
        let mut fields: Vec<Value> = Vec::new();
        let mut resolved: HashMap<&str, Value> = HashMap::new();
        for spec in &self.def.params {
            match validate_param_value(spec, provided.get(&spec.name)) {
                Ok(v) => {
                    resolved.insert(spec.name.as_str(), v);
                }
                Err((rule, message)) => fields.push(json!({
                    "field": spec.name, "rule": rule, "message": message
                })),
            }
        }
        // 未声明的参数名一律拒绝：拼写错误若被忽略会变成 NULL，静默改变查询语义。
        let known: HashSet<&str> = self.def.params.iter().map(|p| p.name.as_str()).collect();
        for key in provided.keys() {
            if !known.contains(key.as_str()) {
                fields.push(json!({
                    "field": key, "rule": "unknown_param", "message": "未声明的参数"
                }));
            }
        }
        if !fields.is_empty() {
            return Err(McpError::new(
                "MCP_VALIDATION_ERROR",
                json!({"template_id": self.def.template_id, "fields": fields}),
            ));
        }

        let (chosen_sql, order, variant_id) = match self.select_variant(&resolved) {
            Some(v) => (v.sql.clone(), v.bind_order.clone(), Some(v.sql_id.clone())),
            None => (self.base_sql.clone(), self.bind_order.clone(), None),
        };

        let mut sql = chosen_sql;
        for spec in self.def.params.iter().filter(|p| p.is_column_ref()) {
            let marker = format!("{{{{{}:enum}}}}", spec.name);
            if !sql.contains(&marker) {
                continue;
            }
            let value = resolved
                .get(spec.name.as_str())
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    McpError::new(
                        "MCP_VALIDATION_ERROR",
                        json!({"fields": [{"field": spec.name, "rule": "column_ref_required", "message": "column_ref 必须提供白名单内的标识符"}]}),
                    )
                })?;
            sql = sql.replace(&marker, &quote_ident(value));
        }
        if sql.contains("{{") || sql.contains("}}") {
            return Err(template_error(
                "template_marker_unresolved",
                "渲染后仍存在未替换的模板标记",
                json!({}),
            ));
        }

        let mut values: Vec<Value> = Vec::with_capacity(order.len());
        let mut positional: Map<String, Value> = Map::new();
        for name in &order {
            let spec = self
                .def
                .params
                .iter()
                .find(|p| &p.name == name)
                .ok_or_else(|| {
                    template_error(
                        "bind_order_unknown",
                        "绑定顺序引用了未知参数",
                        json!({"param": name}),
                    )
                })?;
            if spec.is_array() {
                let empty = resolved.get(name.as_str()).is_none_or(Value::is_null);
                if !empty {
                    return Err(McpError::new(
                        "MCP_PARAM_STYLE_UNSUPPORTED",
                        json!({"field": name, "hint": "数组参数需要 ANY 展开，本阶段未启用绑定"}),
                    ));
                }
            }
            let value = resolved.get(name.as_str()).cloned().unwrap_or(Value::Null);
            positional.insert(name.clone(), value.clone());
            values.push(value);
        }
        let params_hash = sha256_hex(&canonical_json(&Value::Object(positional)));

        Ok(PreparedQuery {
            sql,
            values,
            params_hash,
            variant_id,
        })
    }

    fn select_variant(&self, resolved: &HashMap<&str, Value>) -> Option<&CompiledVariant> {
        self.variants.iter().find(|v| {
            v.when
                .iter()
                .all(|(k, expected)| matches_condition(resolved.get(k.as_str()), expected))
        })
    }
}

fn matches_condition(value: Option<&Value>, expected: &Value) -> bool {
    match expected.as_str() {
        Some("not_null") => value.is_some_and(|v| !v.is_null()),
        Some("null") => value.is_none_or(Value::is_null),
        _ => value == Some(expected),
    }
}

/// 渲染完成、可直接交给执行器的一次调用。
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedQuery {
    pub sql: String,
    pub values: Vec<Value>,
    pub params_hash: String,
    pub variant_id: Option<String>,
}

/// 标识符引用：只对白名单里的合法标识符加双引号，绝不拼值。
fn quote_ident(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn validate_rendered(sql: &str, schemas: &[String]) -> Result<(), McpError> {
    hjzl_sqlguard::validate(sql, Dialect::Postgres, schemas).map_err(guard_to_mcp)?;
    Ok(())
}

/// 词法级列黑名单命中（跳过字符串字面量）。
fn find_forbidden_columns(sql: &str, forbidden: &[String]) -> Vec<String> {
    let banned: Vec<String> = forbidden.iter().map(|f| f.to_ascii_lowercase()).collect();
    let mut hits: Vec<String> = Vec::new();
    let bytes = sql.as_bytes();
    let mut i = 0;
    let mut in_string = false;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if in_string {
            if c == '\'' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if c == '\'' {
            in_string = true;
            i += 1;
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < bytes.len()
                && ((bytes[i] as char).is_ascii_alphanumeric() || bytes[i] == b'_')
            {
                i += 1;
            }
            let word = sql[start..i].to_ascii_lowercase();
            if banned.contains(&word) && !hits.contains(&word) {
                hits.push(word);
            }
            continue;
        }
        i += 1;
    }
    hits
}

/// 结构变体白名单 + 命名占位符改写。
///
/// 返回 (改写后的 SQL, 位置 i → 参数名)。编号按 params 声明顺序、只对实际出现的参数编号，
/// 保证与执行器的 `$1..$n` 连续性检查一致。
fn rewrite(sql: &str, params: &[ParamSpec]) -> Result<(String, Vec<String>), McpError> {
    check_template_syntax(sql, params)?;
    let used = collect_used_params(sql, params)?;
    let order: Vec<String> = params
        .iter()
        .filter(|p| used.contains(p.name.as_str()))
        .map(|p| p.name.clone())
        .collect();
    let positions: HashMap<&str, usize> = order
        .iter()
        .enumerate()
        .map(|(i, name)| (name.as_str(), i + 1))
        .collect();

    let bytes = sql.as_bytes();
    let mut out = String::with_capacity(sql.len() + 16);
    let mut i = 0;
    let mut in_string = false;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if in_string {
            out.push(c);
            if c == '\'' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match c {
            '\'' => {
                in_string = true;
                out.push(c);
                i += 1;
            }
            '{' if sql[i..].starts_with("{{") => {
                // 标记原样保留（结构检查已确认只有 {{name:enum}} 形式）。
                let end = sql[i..].find("}}").map(|off| i + off + 2).ok_or_else(|| {
                    template_error(
                        "template_marker_unclosed",
                        "`{{` 未闭合",
                        json!({"offset": i}),
                    )
                })?;
                out.push_str(&sql[i..end]);
                i = end;
            }
            ':' => {
                let prev_colon = i > 0 && bytes[i - 1] == b':';
                let next_colon = i + 1 < bytes.len() && bytes[i + 1] == b':';
                if prev_colon || next_colon {
                    out.push(c);
                    i += 1;
                    continue;
                }
                if i + 1 < bytes.len()
                    && ((bytes[i + 1] as char).is_ascii_alphabetic() || bytes[i + 1] == b'_')
                {
                    let mut j = i + 2;
                    while j < bytes.len()
                        && ((bytes[j] as char).is_ascii_alphanumeric() || bytes[j] == b'_')
                    {
                        j += 1;
                    }
                    let name = &sql[i + 1..j];
                    let pos = positions.get(name).ok_or_else(|| {
                        template_error(
                            "unknown_placeholder",
                            "SQL 使用了未声明的参数",
                            json!({"param": name}),
                        )
                    })?;
                    out.push('$');
                    out.push_str(&pos.to_string());
                    i = j;
                    continue;
                }
                out.push(c);
                i += 1;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    Ok((out, order))
}

/// 结构白名单检查：只允许 `{{name:enum}}`；拒绝 Jinja、`{{ param }}` 值插值、未闭合标记。
fn check_template_syntax(sql: &str, params: &[ParamSpec]) -> Result<(), McpError> {
    if sql.contains("{%") || sql.contains("%}") {
        return Err(template_error(
            "jinja_forbidden",
            "禁止 Jinja 表达式/循环/过滤器",
            json!({"marker": "{% ... %}"}),
        ));
    }
    if sql.contains("{#") || sql.contains("#}") {
        return Err(template_error(
            "jinja_forbidden",
            "禁止 Jinja 注释块",
            json!({"marker": "{# ... #}"}),
        ));
    }
    let mut rest = sql;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let end = after.find("}}").ok_or_else(|| {
            template_error(
                "template_marker_unclosed",
                "`{{` 未闭合",
                json!({"offset": start}),
            )
        })?;
        let inner = after[..end].trim();
        let (name, kind) = inner.split_once(':').ok_or_else(|| {
            template_error(
                "template_marker_invalid",
                "只允许 `{{参数名:enum}}` 形式的白名单标识符标记",
                json!({"marker": format!("{{{{{}}}}}", inner)}),
            )
        })?;
        if kind.trim() != "enum" {
            return Err(template_error(
                "template_marker_invalid",
                "标记类型只允许 enum（column_ref 白名单）",
                json!({"marker": format!("{{{{{}}}}}", inner), "kind": kind}),
            ));
        }
        let spec = params
            .iter()
            .find(|p| p.name == name.trim())
            .ok_or_else(|| {
                template_error(
                    "template_marker_unknown_param",
                    "标记引用了未声明参数",
                    json!({"param": name.trim()}),
                )
            })?;
        if !spec.is_column_ref() {
            return Err(template_error(
                "template_marker_not_column_ref",
                "`{{...:enum}}` 只能用于 column_ref 参数（其余参数走位置绑定，禁止值插值）",
                json!({"param": name.trim(), "type": spec.param_type}),
            ));
        }
        rest = &after[end + 2..];
    }
    if let Some(pos) = sql.find("}}") {
        // 没有对应 `{{` 的 `}}` 也是非法标记。
        let mut paired = 0usize;
        let mut idx = 0usize;
        let b = sql.as_bytes();
        while idx + 1 < b.len() {
            if &b[idx..idx + 2] == b"{{" {
                paired += 1;
            } else if &b[idx..idx + 2] == b"}}" {
                if paired == 0 {
                    return Err(template_error(
                        "template_marker_unclosed",
                        "`}}` 缺少对应的 `{{`",
                        json!({"offset": idx}),
                    ));
                }
                paired -= 1;
                idx += 1;
            }
            idx += 1;
        }
        let _ = pos;
    }
    Ok(())
}

fn collect_used_params(sql: &str, params: &[ParamSpec]) -> Result<HashSet<String>, McpError> {
    let known: HashSet<&str> = params.iter().map(|p| p.name.as_str()).collect();
    let mut used = HashSet::new();
    let bytes = sql.as_bytes();
    let mut i = 0;
    let mut in_string = false;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if in_string {
            if c == '\'' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match c {
            '\'' => {
                in_string = true;
                i += 1;
            }
            '{' if sql[i..].starts_with("{{") => {
                i += sql[i..].find("}}").map_or(2, |off| off + 2);
            }
            ':' => {
                let prev_colon = i > 0 && bytes[i - 1] == b':';
                let next_colon = i + 1 < bytes.len() && bytes[i + 1] == b':';
                if prev_colon || next_colon {
                    i += 1;
                    continue;
                }
                if i + 1 < bytes.len()
                    && ((bytes[i + 1] as char).is_ascii_alphabetic() || bytes[i + 1] == b'_')
                {
                    let mut j = i + 2;
                    while j < bytes.len()
                        && ((bytes[j] as char).is_ascii_alphanumeric() || bytes[j] == b'_')
                    {
                        j += 1;
                    }
                    let name = &sql[i + 1..j];
                    if !known.contains(name) {
                        return Err(template_error(
                            "unknown_placeholder",
                            "SQL 使用了未声明的参数",
                            json!({"param": name}),
                        ));
                    }
                    used.insert(name.to_string());
                    i = j;
                    continue;
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    Ok(used)
}

/// 单参数校验：返回规范化后的绑定值（None/null 表示 SQL NULL）。
fn validate_param_value(
    spec: &ParamSpec,
    raw: Option<&Value>,
) -> Result<Value, (&'static str, String)> {
    let ty = spec.param_type.trim().to_ascii_lowercase();
    let base = ty
        .strip_prefix("array<")
        .and_then(|rest| rest.strip_suffix('>'))
        .map(str::trim)
        .unwrap_or(ty.as_str());
    let is_array = ty.starts_with("array<");

    let Some(raw) = raw else {
        if spec.required {
            return Err(("required", "缺少必填参数".to_string()));
        }
        return Ok(Value::Null);
    };
    if raw.is_null() {
        if !spec.nullable {
            return Err(("not_nullable", "参数不允许为 null".to_string()));
        }
        return Ok(Value::Null);
    }
    if is_array {
        let items = raw.as_array().ok_or(("type", "期望数组".to_string()))?;
        if let Some(limit) = spec.array_max_items
            && items.len() > limit
        {
            return Err((
                "array_max_items",
                format!("数组元素数 {} 超过上限 {limit}", items.len()),
            ));
        }
        for item in items {
            validate_scalar(base, spec, item)?;
        }
        return Ok(raw.clone());
    }
    if let Some(text) = raw.as_str()
        && text.is_empty()
        && spec.empty_string_as_null
    {
        if !spec.nullable {
            return Err((
                "not_nullable",
                "空串按 null 处理，但参数不允许 null".to_string(),
            ));
        }
        return Ok(Value::Null);
    }
    validate_scalar(base, spec, raw)
}

fn validate_scalar(
    base: &str,
    spec: &ParamSpec,
    raw: &Value,
) -> Result<Value, (&'static str, String)> {
    match base {
        "string" | "date" | "timestamp" => {
            let text = raw.as_str().ok_or(("type", "期望字符串".to_string()))?;
            if let Some(max_len) = spec.max_len
                && text.chars().count() > max_len
            {
                return Err(("max_len", format!("长度超过 {max_len}")));
            }
            if base == "date" && !is_date(text) {
                return Err(("type", "日期必须是 YYYY-MM-DD".to_string()));
            }
            if base == "timestamp" && !is_timestamp(text) {
                return Err(("type", "时间戳必须是 ISO8601".to_string()));
            }
            if let Some(pattern) = &spec.pattern {
                // 注册期已校验可编译；此处仍按失败处理而不是 panic（模板定义可能来自旧版本数据）。
                let re =
                    Regex::new(pattern).map_err(|_| ("pattern", "pattern 无法编译".to_string()))?;
                if !re.is_match(text) {
                    return Err(("pattern", "不匹配 pattern".to_string()));
                }
            }
            Ok(Value::String(text.to_string()))
        }
        "int" => {
            let value = raw.as_i64().ok_or(("type", "期望整数".to_string()))?;
            check_range(spec, value as f64)?;
            Ok(json!(value))
        }
        "float" => {
            let value = raw.as_f64().ok_or(("type", "期望浮点数".to_string()))?;
            check_range(spec, value)?;
            Ok(json!(value))
        }
        "bool" => raw
            .as_bool()
            .map(Value::Bool)
            .ok_or(("type", "期望布尔值".to_string())),
        "enum" => {
            let text = raw.as_str().ok_or(("type", "期望枚举字符串".to_string()))?;
            if !spec.r#enum.iter().any(|v| v == text) {
                return Err(("enum", format!("不在白名单 {text:?}")));
            }
            Ok(Value::String(text.to_string()))
        }
        "column_ref" => {
            let text = raw
                .as_str()
                .ok_or(("type", "期望标识符字符串".to_string()))?;
            if !spec.r#enum.iter().any(|v| v == text) {
                return Err(("column_ref", "不在白名单标识符内".to_string()));
            }
            Ok(Value::String(text.to_string()))
        }
        _ => Err(("type", "未知类型".to_string())),
    }
}

fn check_range(spec: &ParamSpec, value: f64) -> Result<(), (&'static str, String)> {
    if let Some(min) = spec.min
        && value < min
    {
        return Err(("min", format!("小于最小值 {min}")));
    }
    if let Some(max) = spec.max
        && value > max
    {
        return Err(("max", format!("大于最大值 {max}")));
    }
    Ok(())
}

/// 运行期可见的模板记录（已编译）。
#[derive(Debug, Clone)]
pub struct TemplateRecord {
    pub template_id: String,
    pub version: i64,
    pub group: String,
    pub status: String,
    pub discoverable: bool,
    pub callable: bool,
    pub successor_id: String,
    pub compiled: CompiledTemplate,
}

impl TemplateRecord {
    fn from_capability(rec: &CapabilityRecord) -> Result<Self, McpError> {
        let compiled = CompiledTemplate::from_definition(&rec.definition)?;
        Ok(Self {
            template_id: rec.capability_id.clone(),
            version: rec.version,
            group: rec.group.clone(),
            status: rec.status.clone(),
            discoverable: rec.discoverable,
            callable: rec.callable,
            successor_id: rec.successor_id.clone(),
            compiled,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    template_id: String,
    version: i64,
    params_hash: String,
}

#[derive(Debug, Clone)]
struct CacheEntry {
    value: Value,
    stored_at: Instant,
    ttl: Duration,
}

/// 模板注册表：生命周期复用 CapabilityStore（kind=template），本层只加编译、版本钉住与结果缓存。
pub struct TemplateRegistry {
    store: Arc<CapabilityStore>,
    pins: Mutex<HashMap<(String, String), i64>>,
    cache: Mutex<HashMap<CacheKey, CacheEntry>>,
}

impl TemplateRegistry {
    pub fn new(store: Arc<CapabilityStore>) -> Self {
        Self {
            store,
            pins: Mutex::new(HashMap::new()),
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// 注册草案（draft）：门禁不过即拒，不落库。
    pub fn register(&self, value: &Value) -> Result<CompiledTemplate, McpError> {
        let compiled = compile(TemplateDef::parse(value)?)?;
        self.store.register(
            &compiled.def.template_id,
            "template",
            &compiled.def.group,
            &compiled.def.risk,
            compiled.to_definition(),
        )?;
        Ok(compiled)
    }

    /// 预检回执：编译通过后给出"这份定义的指纹"，发布时必须出示同一指纹。
    pub fn preflight(&self, value: &Value) -> Result<PreflightReceipt, McpError> {
        let compiled = compile(TemplateDef::parse(value)?)?;
        Ok(PreflightReceipt {
            template_id: compiled.def.template_id.clone(),
            base_sql_hash: sha256_hex(&compiled.base_sql),
        })
    }

    /// 发布新版本：必须先预检（回执指纹必须与待发布定义一致），四眼与审批复用 registry。
    pub fn publish_version(
        &self,
        id: &str,
        value: &Value,
        approval_id: &str,
        submitted_by: &str,
        approved_by: &str,
        receipt: &PreflightReceipt,
    ) -> Result<CompiledTemplate, McpError> {
        let compiled = compile(TemplateDef::parse(value)?)?;
        if compiled.def.template_id != id || receipt.template_id != id {
            return Err(template_error(
                "template_id_mismatch",
                "定义与目标 template_id 不一致",
                json!({"expected": id, "actual": compiled.def.template_id}),
            ));
        }
        if sha256_hex(&compiled.base_sql) != receipt.base_sql_hash {
            return Err(McpError::new(
                "MCP_PREFLIGHT_REQUIRED",
                json!({"template_id": id, "hint": "预检回执与待发布定义不一致，必须重新预检"}),
            ));
        }
        self.store.publish_version(
            id,
            compiled.to_definition(),
            approval_id,
            submitted_by,
            approved_by,
        )?;
        Ok(compiled)
    }

    pub fn submit_for_review(&self, id: &str) -> Result<(), McpError> {
        self.store.submit_for_review(id).map(|_| ())
    }

    pub fn publish(
        &self,
        id: &str,
        approval_id: &str,
        submitted_by: &str,
        approved_by: &str,
    ) -> Result<(), McpError> {
        self.store
            .promote_to_published(id, approval_id, submitted_by, approved_by, "")
            .map(|_| ())
    }

    pub fn deprecate(&self, id: &str, successor_id: &str) -> Result<(), McpError> {
        self.store.deprecate(id, successor_id).map(|_| ())
    }

    pub fn set_visibility(
        &self,
        id: &str,
        discoverable: Option<bool>,
        callable: Option<bool>,
        approval_id: &str,
    ) -> Result<(), McpError> {
        self.store
            .set_visibility(id, discoverable, callable, approval_id)
            .map(|_| ())
    }

    /// 当前版本；不存在 → NOT_FOUND，deprecated → NOT_FOUND + successor_id（§6.3 404 语义）。
    pub fn current(&self, id: &str) -> Result<TemplateRecord, McpError> {
        let record = self.store.get(id)?.ok_or_else(|| not_found(id, None))?;
        if record.kind != "template" {
            return Err(not_found(id, None));
        }
        if record.status == "deprecated" {
            return Err(not_found(id, Some(record.successor_id.clone())));
        }
        TemplateRecord::from_capability(&record)
    }

    /// 指定历史版本（会话钉住时用；旧版本保留可查）。
    pub fn at_version(&self, id: &str, version: i64) -> Result<TemplateRecord, McpError> {
        let record = self
            .store
            .get_version(id, version)?
            .ok_or_else(|| not_found(id, None))?;
        if record.kind != "template" {
            return Err(not_found(id, None));
        }
        TemplateRecord::from_capability(&record)
    }

    /// 会话版本钉住：首调建立绑定；显式换版本即冲突（§4.3.5）。
    pub fn pin(
        &self,
        session_id: &str,
        template_id: &str,
        requested: Option<i64>,
        current: i64,
    ) -> Result<i64, McpError> {
        let mut pins = self.pins.lock().map_err(|_| McpError::lock())?;
        let key = (session_id.to_string(), template_id.to_string());
        if let Some(pinned) = pins.get(&key) {
            if requested.is_some_and(|v| v != *pinned) {
                return Err(McpError::new(
                    "MCP_VERSION_CONFLICT",
                    json!({"template_id": template_id, "pinned": pinned, "requested": requested}),
                ));
            }
            return Ok(*pinned);
        }
        let chosen = requested.unwrap_or(current);
        pins.insert(key, chosen);
        Ok(chosen)
    }

    pub fn cache_lookup(
        &self,
        template_id: &str,
        version: i64,
        params_hash: &str,
    ) -> Option<Value> {
        let cache = self.cache.lock().ok()?;
        let key = CacheKey {
            template_id: template_id.to_string(),
            version,
            params_hash: params_hash.to_string(),
        };
        let entry = cache.get(&key)?;
        if entry.stored_at.elapsed() >= entry.ttl {
            return None;
        }
        Some(entry.value.clone())
    }

    pub fn cache_store(
        &self,
        template_id: &str,
        version: i64,
        params_hash: &str,
        value: Value,
        ttl_s: i64,
    ) {
        if ttl_s <= 0 {
            return;
        }
        let Ok(mut cache) = self.cache.lock() else {
            return;
        };
        cache.insert(
            CacheKey {
                template_id: template_id.to_string(),
                version,
                params_hash: params_hash.to_string(),
            },
            CacheEntry {
                value,
                stored_at: Instant::now(),
                ttl: Duration::from_secs(ttl_s.min(300) as u64),
            },
        );
    }
}

fn not_found(id: &str, successor: Option<String>) -> McpError {
    let mut data = json!({"template_id": id, "hint": "模板不存在或不可见"});
    if let Some(s) = successor
        && !s.is_empty()
        && let Some(obj) = data.as_object_mut()
    {
        obj.insert("successor_id".into(), json!(s));
    }
    McpError::new("MCP_CAPABILITY_NOT_FOUND", data)
}

/// `domain.name` 形式的模板标识（只允许小写字母数字与下划线分段）。
fn is_template_id(value: &str) -> bool {
    let mut segments = 0usize;
    for part in value.split('.') {
        if part.is_empty()
            || !part
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        {
            return false;
        }
        segments += 1;
    }
    segments >= 2
}

/// 参数名与 SQL 绑定顺序里的键：小写字母/数字/下划线，首字符非数字。
fn is_lower_ident(value: &str) -> bool {
    let mut bytes = value.bytes();
    match bytes.next() {
        Some(b) if b.is_ascii_lowercase() || b == b'_' => {}
        _ => return false,
    }
    bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// 列引用白名单标识符（大小写皆可，首字符非数字）。
fn is_ident(value: &str) -> bool {
    let mut bytes = value.bytes();
    match bytes.next() {
        Some(b) if b.is_ascii_alphabetic() || b == b'_' => {}
        _ => return false,
    }
    bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn is_date(value: &str) -> bool {
    let b = value.as_bytes();
    b.len() == 10
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-'
        && b[8..].iter().all(u8::is_ascii_digit)
}

/// ISO8601 宽松校验：日期 + 可选时间 + 可选时区（不做日历合法性判定，交给数据源）。
fn is_timestamp(value: &str) -> bool {
    let Some((date, rest)) = value.split_at_checked(10) else {
        return false;
    };
    if !is_date(date) {
        return false;
    }
    if rest.is_empty() {
        return true;
    }
    let Some(time) = rest.strip_prefix('T').or_else(|| rest.strip_prefix(' ')) else {
        return false;
    };
    let (clock, zone) = match time.find(['Z', '+']) {
        Some(idx) => (&time[..idx], &time[idx..]),
        None => match time.rfind('-') {
            Some(idx) if idx > 0 => (&time[..idx], &time[idx..]),
            _ => (time, ""),
        },
    };
    let mut parts = clock.split(':');
    let (Some(hh), Some(mm)) = (parts.next(), parts.next()) else {
        return false;
    };
    if hh.len() != 2 || mm.len() != 2 {
        return false;
    }
    if !hh.bytes().all(|b| b.is_ascii_digit()) || !mm.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    if let Some(ss) = parts.next()
        && !ss
            .split('.')
            .next()
            .is_some_and(|s| s.len() == 2 && s.bytes().all(|b| b.is_ascii_digit()))
    {
        return false;
    }
    if zone.is_empty() || zone == "Z" {
        return true;
    }
    let zone_body = zone.trim_start_matches(['+', '-']).replace(':', "");
    zone_body.len() == 4 && zone_body.bytes().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def_value(sql: &str, params: Value, variants: Value) -> Value {
        json!({
            "template_id": "order.daily_summary",
            "group": "db.read.analytics",
            "risk": "read",
            "description": "按 day 汇总",
            "sql": sql,
            "params": params,
            "variants": variants,
            "constraints": {"allowed_schemas": ["public"]},
            "result_cache_ttl_s": 60
        })
    }

    fn simple_params() -> Value {
        json!([
            {"name":"start_date","type":"timestamp","required":true,"nullable":false},
            {"name":"end_date","type":"timestamp","required":true,"nullable":false}
        ])
    }

    #[test]
    fn named_placeholders_rewritten_in_declaration_order() {
        let v = def_value(
            "SELECT day FROM public.orders WHERE created_at >= :start_date AND created_at < :end_date",
            simple_params(),
            json!([]),
        );
        let compiled = compile(TemplateDef::parse(&v).unwrap()).unwrap();
        assert_eq!(
            compiled.base_sql,
            "SELECT day FROM public.orders WHERE created_at >= $1 AND created_at < $2"
        );
        assert_eq!(compiled.bind_order, vec!["start_date", "end_date"]);
    }

    #[test]
    fn unknown_placeholder_rejected() {
        let v = def_value(
            "SELECT 1 FROM public.orders WHERE a = :nope",
            simple_params(),
            json!([]),
        );
        let e = TemplateDef::parse(&v).and_then(compile).unwrap_err();
        assert_eq!(e.code, "MCP_TEMPLATE_INVALID");
        assert_eq!(e.data["rule"], json!("unknown_placeholder"));
    }

    #[test]
    fn jinja_rejected() {
        let v = def_value(
            "SELECT 1 FROM public.orders {% if x %} WHERE 1=1 {% endif %}",
            simple_params(),
            json!([]),
        );
        let compiled = TemplateDef::parse(&v).and_then(compile).unwrap_err();
        assert_eq!(compiled.data["rule"], json!("jinja_forbidden"));
    }

    #[test]
    fn value_interpolation_marker_rejected() {
        let v = def_value(
            "SELECT 1 FROM public.orders WHERE a = '{{ start_date }}'",
            simple_params(),
            json!([]),
        );
        let e = TemplateDef::parse(&v).and_then(compile).unwrap_err();
        assert_eq!(e.data["rule"], json!("template_marker_invalid"));
    }

    #[test]
    fn unclosed_marker_rejected() {
        let v = def_value(
            "SELECT 1 FROM public.orders WHERE a = '{{ start_date'",
            simple_params(),
            json!([]),
        );
        let e = TemplateDef::parse(&v).and_then(compile).unwrap_err();
        assert_eq!(e.data["rule"], json!("template_marker_unclosed"));
    }

    #[test]
    fn column_ref_requires_enum_and_quotes_identifier() {
        let params = json!([
            {"name":"col","type":"column_ref","required":true,"nullable":false,"enum":["channel","region"]}
        ]);
        let v = def_value(
            "SELECT {{col:enum}} FROM public.orders GROUP BY {{col:enum}}",
            params,
            json!([]),
        );
        let compiled = compile(TemplateDef::parse(&v).unwrap()).unwrap();
        let prepared = compiled.render(&json!({"col": "channel"})).unwrap();
        assert!(prepared.sql.contains("\"channel\""));
        assert!(!prepared.sql.contains("{{"));
        assert!(prepared.values.is_empty());
    }

    #[test]
    fn column_ref_outside_whitelist_rejected() {
        let params = json!([
            {"name":"col","type":"column_ref","required":true,"nullable":false,"enum":["channel"]}
        ]);
        let v = def_value("SELECT {{col:enum}} FROM public.orders", params, json!([]));
        let compiled = compile(TemplateDef::parse(&v).unwrap()).unwrap();
        let e = compiled
            .render(&json!({"col": "amount; DROP TABLE x"}))
            .unwrap_err();
        assert_eq!(e.code, "MCP_VALIDATION_ERROR");
        assert_eq!(e.data["fields"][0]["rule"], json!("column_ref"));
    }

    #[test]
    fn marker_on_non_column_ref_rejected() {
        let v = def_value(
            "SELECT {{start_date:enum}} FROM public.orders",
            simple_params(),
            json!([]),
        );
        let e = TemplateDef::parse(&v).and_then(compile).unwrap_err();
        assert_eq!(e.data["rule"], json!("template_marker_not_column_ref"));
    }

    #[test]
    fn write_sql_rejected_by_gate() {
        let v = def_value("DELETE FROM public.orders", simple_params(), json!([]));
        let e = TemplateDef::parse(&v).and_then(compile).unwrap_err();
        assert_eq!(e.code, "MCP_TEMPLATE_INVALID");
        assert_eq!(e.data["rule"], json!("guard_rejected"));
    }

    #[test]
    fn forbidden_column_rejected() {
        let mut v = def_value(
            "SELECT user_phone FROM public.orders",
            simple_params(),
            json!([]),
        );
        v["constraints"]["forbidden_columns"] = json!(["user_phone"]);
        let e = TemplateDef::parse(&v).and_then(compile).unwrap_err();
        assert_eq!(e.data["rule"], json!("forbidden_column"));
    }

    #[test]
    fn optional_filter_variant_passes() {
        let v = def_value(
            "SELECT day FROM public.orders WHERE (:start_date IS NULL OR created_at >= :start_date)",
            json!([{"name":"start_date","type":"timestamp"}]),
            json!([]),
        );
        let compiled = compile(TemplateDef::parse(&v).unwrap()).unwrap();
        assert_eq!(compiled.bind_order, vec!["start_date"]);
    }

    #[test]
    fn explicit_variant_matrix_selects_by_condition() {
        let v = def_value(
            "SELECT day FROM public.orders",
            json!([{"name":"channel","type":"enum","enum":["web","app"]}]),
            json!([{
                "sql_id": "with_channel",
                "when": {"channel": "not_null"},
                "sql": "SELECT day FROM public.orders WHERE channel = :channel"
            }]),
        );
        let compiled = compile(TemplateDef::parse(&v).unwrap()).unwrap();
        let with = compiled.render(&json!({"channel":"web"})).unwrap();
        assert_eq!(with.variant_id.as_deref(), Some("with_channel"));
        assert_eq!(with.values, vec![json!("web")]);
        let without = compiled.render(&json!({})).unwrap();
        assert!(without.variant_id.is_none());
    }

    #[test]
    fn param_validation_field_errors() {
        let v = def_value(
            "SELECT 1 FROM public.orders WHERE created_at >= :start_date AND created_at < :end_date",
            simple_params(),
            json!([]),
        );
        let compiled = compile(TemplateDef::parse(&v).unwrap()).unwrap();
        let e = compiled
            .render(&json!({"start_date": "not-a-date"}))
            .unwrap_err();
        assert_eq!(e.code, "MCP_VALIDATION_ERROR");
        let fields = e.data["fields"].as_array().unwrap();
        assert!(fields.iter().any(|f| f["field"] == json!("start_date")));
        assert!(
            fields
                .iter()
                .any(|f| f["field"] == json!("end_date") && f["rule"] == json!("required"))
        );
    }

    #[test]
    fn empty_string_becomes_null_by_default() {
        let v = def_value(
            "SELECT 1 FROM public.orders WHERE (:a IS NULL OR channel = :a)",
            json!([{"name":"a","type":"string"}]),
            json!([]),
        );
        let compiled = compile(TemplateDef::parse(&v).unwrap()).unwrap();
        let out = compiled.render(&json!({"a": ""})).unwrap();
        assert_eq!(out.values, vec![Value::Null]);
    }

    #[test]
    fn cache_ttl_zero_disables_cache() {
        let store = Arc::new(CapabilityStore::open(":memory:", None).unwrap());
        let reg = TemplateRegistry::new(store);
        reg.cache_store("t", 1, "h", json!({"a":1}), 0);
        assert!(reg.cache_lookup("t", 1, "h").is_none());
        reg.cache_store("t", 1, "h", json!({"a":1}), 60);
        assert_eq!(reg.cache_lookup("t", 1, "h"), Some(json!({"a":1})));
    }

    #[test]
    fn pin_version_conflict() {
        let store = Arc::new(CapabilityStore::open(":memory:", None).unwrap());
        let reg = TemplateRegistry::new(store);
        assert_eq!(reg.pin("s1", "t", None, 3).unwrap(), 3);
        assert_eq!(reg.pin("s1", "t", Some(3), 4).unwrap(), 3);
        let e = reg.pin("s1", "t", Some(2), 4).unwrap_err();
        assert_eq!(e.code, "MCP_VERSION_CONFLICT");
    }
}
