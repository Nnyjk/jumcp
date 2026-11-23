//! 注册期门禁与运行期渲染：结构变体白名单 → 命名占位符改写 → sqlguard 复核 → 参数绑定。

use super::definition::{CompiledTemplate, CompiledVariant, ParamSpec, TemplateDef};
use super::template_error;
use crate::canonical::{canonical_json, definition_hash, sha256_hex};
use crate::errors::McpError;
use jumcp_sqlguard::{Dialect, GuardError};
use regex::Regex;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};

/// 注册期门禁：结构变体白名单 → 命名占位符改写 → 渲染 → sqlguard 复核。
///
/// 保留默认入口（PostgreSQL）供既有调用方/测试使用；生产入口必须按**上游 SQL 方言**
/// 调用 [`compile_with_dialect`]，否则 ClickHouse 会拿到 `$n` 占位符。
///
/// 方言不是"本服务的数据库方言"——本进程不持有任何连接；它只决定占位符改写与渲染期
/// sqlguard 复核按哪套词法走，因此必须与模板 `target` 指向的上游一致（当前生产固定
/// ClickHouse，见 `docs/mcp-router.md` 的 `db_run_template` 小节）。
pub fn compile(def: TemplateDef) -> Result<CompiledTemplate, McpError> {
    compile_with_dialect(def, Dialect::Postgres)
}

pub fn compile_with_dialect(
    def: TemplateDef,
    dialect: Dialect,
) -> Result<CompiledTemplate, McpError> {
    let (base_sql, bind_order) = rewrite(&def.sql, &def.params, dialect)?;
    let mut variants = Vec::with_capacity(def.variants.len());
    for v in &def.variants {
        let (sql, order) = rewrite(&v.sql, &def.params, dialect)?;
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
            dialect,
        };
    compiled.gate()?;
    Ok(compiled)
}

impl CompiledTemplate {
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
            validate_rendered(sql, schemas, self.dialect)?;
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
            validate_rendered(&rendered, schemas, self.dialect)?;
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

    /// 内联渲染：把位置占位符换成转义后的字面量，得到可直接发给上游 MCP 的一段 SQL。
    ///
    /// 上游 `query` 工具只收 SQL 文本（无参数数组），绑定值必须在本地落地；
    /// 字面量由 `render` 校验过的参数派生，落地后再过一遍 sqlguard，防拼接引入的新结构。
    pub fn render_inline(&self, params: &Value) -> Result<RenderedSql, McpError> {
        let prepared = self.render(params)?;
        let sql = inline_values(&prepared.sql, &prepared.values)?;
        validate_rendered(&sql, &self.def.constraints.allowed_schemas, self.dialect)?;
        self.check_forbidden_columns(&sql)?;
        Ok(RenderedSql {
            sql,
            params_hash: prepared.params_hash,
            variant_id: prepared.variant_id,
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

/// 内联渲染产物：`sql` 已不含占位符，只带上游需要的文本与缓存键。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedSql {
    pub sql: String,
    pub params_hash: String,
    /// 命中的变体 id：响应里要能说明"这次用的是哪条变体"。
    pub variant_id: Option<String>,
}

/// 把 `?` 占位符按序替换为转义后的字面量。
///
/// `??` 是字面 `?` 的转义（与 `check_placeholders` 同一约定），原样保留；
/// 数量不符即报错——多一个或少一个都说明绑定序列与 SQL 已经错位。
fn inline_values(sql: &str, values: &[Value]) -> Result<String, McpError> {
    let mut out = String::with_capacity(sql.len() + values.len() * 8);
    let mut next = values.iter();
    let mut used = 0usize;
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '?' {
            out.push(c);
            continue;
        }
        if chars.peek() == Some(&'?') {
            chars.next();
            out.push('?');
            continue;
        }
        let value = next.next().ok_or_else(|| {
            McpError::new(
                "MCP_PARAM_COUNT_MISMATCH",
                json!({"expected": values.len(), "provided": used + 1}),
            )
        })?;
        out.push_str(&literal(value)?);
        used += 1;
    }
    if used != values.len() {
        return Err(McpError::new(
            "MCP_PARAM_COUNT_MISMATCH",
            json!({"expected": values.len(), "provided": used}),
        ));
    }
    Ok(out)
}

/// 单个绑定值的 SQL 字面量。类型闭集来自 `validate_scalar`：字符串/数值/布尔/NULL。
fn literal(value: &Value) -> Result<String, McpError> {
    match value {
        Value::String(s) => Ok(format!(
            "'{}'",
            s.replace('\\', "\\\\").replace('\'', "\\'")
        )),
        Value::Number(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(if *b { "TRUE" } else { "FALSE" }.to_string()),
        Value::Null => Ok("NULL".to_string()),
        other => Err(McpError::new(
            "MCP_VALIDATION_ERROR",
            json!({"fields": [{"field": "params", "rule": "scalar_required", "message": format!("绑定值必须标量，实际是 {other}")}]}),
        )),
    }
}

/// 标识符引用：只对白名单里的合法标识符加双引号，绝不拼值。
fn quote_ident(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn validate_rendered(sql: &str, schemas: &[String], dialect: Dialect) -> Result<(), McpError> {
    jumcp_sqlguard::validate(sql, dialect, schemas).map_err(guard_to_mcp)?;
    Ok(())
}

fn guard_to_mcp(e: GuardError) -> McpError {
    template_error(
        "guard_rejected",
        &e.message,
        json!({"guard_code": e.code, "guard_data": e.data}),
    )
}

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
fn rewrite(
    sql: &str,
    params: &[ParamSpec],
    dialect: Dialect,
) -> Result<(String, Vec<String>), McpError> {
    check_template_syntax(sql, params)?;
    let used = collect_used_params(sql, params)?;
    let declaration_order: Vec<String> = params
        .iter()
        .filter(|p| used.contains(p.name.as_str()))
        .map(|p| p.name.clone())
        .collect();
    let positions: HashMap<&str, usize> = declaration_order
        .iter()
        .enumerate()
        .map(|(i, name)| (name.as_str(), i + 1))
        .collect();

    let question_style = dialect.supports_question_placeholder();
    if question_style {
        // 运行时 `?` 由编译期生成；模板里出现原生 `?` 会让绑定顺序与 SQL 不一致。
        let bytes = sql.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'?' {
                if bytes.get(i + 1) == Some(&b'?') {
                    i += 2;
                    continue;
                }
                return Err(template_error(
                    "template_placeholder_style",
                    "模板只允许 `:name` 命名占位符；`?` 由编译期生成",
                    json!({"offset": i}),
                ));
            }
            i += 1;
        }
    }
    let mut order: Vec<String> = Vec::new();

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
                    if !used.contains(name) {
                        return Err(template_error(
                            "unknown_placeholder",
                            "SQL 使用了未声明的参数",
                            json!({"param": name}),
                        ));
                    }
                    if question_style {
                        out.push('?');
                        order.push(name.to_string());
                    } else {
                        let pos = positions.get(name).ok_or_else(|| {
                            template_error(
                                "unknown_placeholder",
                                "SQL 使用了未声明的参数",
                                json!({"param": name}),
                            )
                        })?;
                        out.push('$');
                        out.push_str(&pos.to_string());
                    }
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
    Ok((
        out,
        if question_style {
            order
        } else {
            declaration_order
        },
    ))
}

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
