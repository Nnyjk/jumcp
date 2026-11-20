//! 定义结构与注册期字段校验：非法定义在注册期一次性拒绝，运行期不重复校验。

use super::template_error;
use crate::canonical::definition_hash;
use crate::errors::McpError;
use jumcp_sqlguard::Dialect;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::HashSet;

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

/// 模板参数声明。
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
    /// 数组元素上限：字段保留以固定定义 schema，但数组参数在注册期即被拒绝，故当前不可达。
    #[serde(default)]
    pub array_max_items: Option<usize>,
}

impl ParamSpec {
    pub(super) fn is_column_ref(&self) -> bool {
        self.param_type.trim().eq_ignore_ascii_case("column_ref")
    }
    pub(super) fn is_array(&self) -> bool {
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

/// 模板执行目标：渲染后的 SQL 交给哪台上游 MCP 的哪个工具。
///
/// 路由层只编排、不直连数据库，因此模板必须声明目标；缺省没有合理默认值
/// （猜一个上游等于把用户 SQL 发到别处），注册期即拒。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TemplateTarget {
    pub upstream: String,
    pub tool: String,
    #[serde(default)]
    pub database: Option<String>,
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
    /// 执行目标：模板不再由本进程直连数据库，必须声明经由哪台上游的哪个工具执行。
    pub target: TemplateTarget,
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
    /// 0 表示不缓存；上限 300。
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
    /// 编译/校验所用方言：注册期与运行期必须是同一方言，否则占位符与默认 schema 会漂移。
    /// 不进入入库 JSON：`to_definition` 只存编译产物，读取时不重新编译。
    #[serde(skip)]
    pub dialect: Dialect,
}

/// 预检回执：把"预检过的定义"和"要发布的定义"绑定起来，防止预检 A 发布 B。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightReceipt {
    pub template_id: String,
    pub base_sql_hash: String,
}

impl TemplateDef {
    /// 严格解析 + 语义校验（非法定义在注册期一次性拒绝，运行期不重复校验）。
    pub fn parse(value: &Value) -> Result<Self, McpError> {
        // 缺 target 的定义来自旧版本（直连数据库时代），报错要点明迁移方向，
        // 不能只留 serde 的 missing field 文本。
        if value.get("target").is_none_or(Value::is_null) {
            return Err(target_error(
                "target_required",
                "模板必须声明 target.upstream/target.tool：执行走已接入的上游 MCP 工具，不再直连数据库",
                json!({}),
            ));
        }
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
        // 只读不变量：工具不得改变接入数据源；注册期即拒绝，运行期无写分支。
        if self.risk != "read" {
            return Err(template_error(
                "risk_not_readonly",
                "工具对数据源只读：只允许注册 risk=read 的能力",
                json!({"risk": self.risk}),
            ));
        }
        if self.sql.trim().is_empty() {
            return Err(template_error("sql_required", "sql 不能为空", json!({})));
        }
        self.target.validate()?;
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

impl TemplateTarget {
    /// 目标在注册期一次性校验：运行期 `resolve` 找不到上游属于部署漂移，不是定义错误。
    fn validate(&self) -> Result<(), McpError> {
        if !crate::registry::is_valid_upstream_id(&self.upstream)
            || crate::registry::RESERVED_UPSTREAM_IDS.contains(&self.upstream.as_str())
            || crate::registry::is_builtin_id(&self.upstream)
        {
            return Err(target_error(
                "upstream_invalid",
                &format!(
                    "target.upstream 必须是已接入的自定义上游 id（小写字母开头、2..=32 位、不含 `__`、非保留 id）：{}",
                    self.upstream
                ),
                json!({"upstream": self.upstream}),
            ));
        }
        let tool_len = self.tool.len();
        if tool_len == 0
            || tool_len > 64
            || !self
                .tool
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(target_error(
                "tool_invalid",
                &format!(
                    "target.tool 必须是上游暴露的工具名（1..=64 位 [A-Za-z0-9_-]）：{}",
                    self.tool
                ),
                json!({"tool": self.tool}),
            ));
        }
        // `admin_` 前缀留给内置管理面，模板不得借此触达管理工具。
        if self.tool.starts_with("admin_") {
            return Err(target_error(
                "tool_reserved",
                &format!(
                    "target.tool 不得以 admin_ 开头（管理工具不供模板调用）：{}",
                    self.tool
                ),
                json!({"tool": self.tool}),
            ));
        }
        if let Some(database) = &self.database {
            let len = database.len();
            if len == 0
                || len > 64
                || !database
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            {
                return Err(target_error(
                    "database_invalid",
                    &format!("target.database 必须为空或 1..=64 位 [A-Za-z0-9_]：{database}"),
                    json!({"database": database}),
                ));
            }
        }
        Ok(())
    }
}

/// 目标字段的错误统一带 `data.field="target"`：接入方按字段定位注册期报错。
fn target_error(rule: &str, message: &str, detail: Value) -> McpError {
    let mut data = serde_json::Map::new();
    data.insert("field".to_string(), json!("target"));
    data.insert("rule".to_string(), json!(rule));
    if let Value::Object(extra) = detail {
        for (k, v) in extra {
            data.insert(k, v);
        }
    }
    McpError::with_message(
        "MCP_VALIDATION_ERROR",
        message.to_string(),
        Value::Object(data),
    )
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
    if inner.is_some() {
        // 数组参数需要占位符数组绑定与展开，尚未实现：注册期就拒，
        // 不留"注册放行、调用必炸"的半成品。
        return Err(template_error(
            "array_param_unsupported",
            &format!("本轮未启用数组参数（数组占位符展开未实现）：{}", p.name),
            json!({"param": p.name, "type": p.param_type}),
        ));
    }
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

impl CompiledTemplate {
    /// 用定义 JSON 还原（定义入库时把 compiled 一起存，读取不重新编译）。
    ///
    /// 方言优先取入库时持久化的值；定义里没有该字段时退回注册表当前方言，
    /// 不能默默退回 PostgreSQL（ClickHouse 的 `?` 与 PG 的 `$n` 不兼容）。
    pub fn from_definition(value: &Value, fallback: Dialect) -> Result<Self, McpError> {
        let dialect = value
            .get("dialect")
            .and_then(Value::as_str)
            .and_then(|name| name.parse::<Dialect>().ok())
            .unwrap_or(fallback);
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
        let variants: Vec<CompiledVariant> = match value.get("variants") {
            // 缺省即"没有变体"：旧定义只存 base_sql，语义无歧义。
            None => Vec::new(),
            // 有值但反序列化失败必须显式报错。原先 `unwrap_or_default` 会静默吞成空向量，
            // 运行期于是回落到 base_sql——调用方以为选中的变体被丢掉，结果集可能完全不同。
            Some(raw) => serde_json::from_value(raw.clone()).map_err(|e| {
                template_error(
                    "variants_invalid",
                    &format!("variants 解析失败: {e}"),
                    json!({}),
                )
            })?,
        };
        Ok(Self {
            definition_hash: definition_hash(value),
            def,
            base_sql,
            bind_order,
            variants,
            dialect,
        })
    }

    /// 入库形态：定义 + 编译产物（definition_hash 覆盖两者）。
    pub fn to_definition(&self) -> Value {
        json!({
            "template": self.def,
            "base_sql": self.base_sql,
            "bind_order": self.bind_order,
            "variants": self.variants,
            "dialect": self.dialect.to_string()
        })
    }
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
