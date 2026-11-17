// 只读执行委托：默认报「未配置数据源」，绝不假执行。
// 入参形状沿用自由查询契约：sql/params/max_rows/max_tokens/cell_char_limit/cell_char_offset/allowed_schemas。

use jumcp_core::entry::EffectiveScope;
use jumcp_core::errors::McpError;
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;

/// 异步执行面：生产实现为 ConnectorExecutor（ClickHouse agent 桥接），测试注入 FakeExecutor。
/// 用 BoxFuture 而非 async-trait：仓库不引入宏 crate，签名仍保持零成本静态分发。
pub type ExecFuture<'a> = Pin<Box<dyn Future<Output = Result<Value, McpError>> + Send + 'a>>;

/// 自由查询入参（已过 schema 收窄校验与占位符风格校验）。
///
/// 内置 basic 组不暴露直连查询工具，这里保留供连接器/数据源健康检查与执行器契约使用。
#[derive(Debug, Clone)]
pub struct QueryParams {
    pub sql: String,
    /// 位置参数数组（按 SQL 中 `?` 的顺序绑定），非数组一律拒绝。
    pub params: Vec<Value>,
    pub max_rows: i64,
    pub max_tokens: Option<i64>,
    pub cell_char_limit: i64,
    pub cell_char_offset: i64,
    pub allowed_schemas: Vec<String>,
    pub statement_timeout_ms: i64,
    /// ClickHouse database：给出时必须与连接白名单一致；省略则用连接默认库。
    pub database: Option<String>,
}

impl QueryParams {
    /// 默认/上限：max_rows 默认 100 上限 1000；cell_char_limit 默认 200 上限 4000。
    /// allowed_schemas 只能收窄连接声明：请求超出连接声明即 schema 越界拒绝。
    pub fn new(
        sql: String,
        connection_allowed: &[String],
        statement_timeout_ms: i64,
        args: &Value,
    ) -> Result<Self, McpError> {
        let max_rows = args
            .get("max_rows")
            .and_then(|v| v.as_i64())
            .map_or(100, |v| v.clamp(1, 1000));
        let cell_char_limit = args
            .get("cell_char_limit")
            .and_then(|v| v.as_i64())
            .map_or(200, |v| v.clamp(1, 4000));
        let cell_char_offset = args
            .get("cell_char_offset")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            .max(0);
        let max_tokens = args.get("max_tokens").and_then(|v| v.as_i64());
        let requested: Vec<String> = match args.get("allowed_schemas") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(|s| s.as_str().map(|t| t.to_string()))
                .collect(),
            Some(other) => {
                return Err(McpError::new(
                    "MCP_SCHEMA_OUT_OF_SCOPE",
                    json!({"error": "allowed_schemas 必须是字符串数组", "received": other}),
                ));
            }
        };
        if !connection_allowed.is_empty() && !requested.is_empty() {
            let offending: Vec<&String> = requested
                .iter()
                .filter(|s| !connection_allowed.contains(*s))
                .collect();
            if !offending.is_empty() {
                return Err(McpError::new(
                    "MCP_SCHEMA_OUT_OF_SCOPE",
                    json!({"offending_schemas": offending, "allowed": connection_allowed}),
                ));
            }
        }
        let effective_schemas = if requested.is_empty() {
            connection_allowed.to_vec()
        } else {
            requested
        };
        let params = match args.get("params") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(a)) => a.clone(),
            Some(other) => {
                return Err(McpError::new(
                    "MCP_PARAM_STYLE_UNSUPPORTED",
                    json!({
                        "hint": "params 只接受位置参数数组（按 SQL 中 ? 的顺序绑定）；命名参数需经模板层改写",
                        "received_type": json_type_name(other)
                    }),
                ));
            }
        };
        // database 是 ClickHouse 默认库；此处只解析形状，白名单判定由执行器负责。
        let database = match args.get("database") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    return Err(McpError::new(
                        "MCP_VALIDATION_ERROR",
                        json!({"error": "database 不能为空字符串"}),
                    ));
                }
                Some(trimmed.to_string())
            }
            Some(other) => {
                return Err(McpError::new(
                    "MCP_VALIDATION_ERROR",
                    json!({
                        "error": "database 必须是字符串",
                        "received_type": json_type_name(other)
                    }),
                ));
            }
        };
        Ok(Self {
            sql,
            params,
            max_rows,
            max_tokens,
            cell_char_limit,
            cell_char_offset,
            allowed_schemas: effective_schemas,
            statement_timeout_ms,
            database,
        })
    }
}

impl QueryParams {
    /// 模板路径：SQL 已由注册期门禁改写为位置占位符，值按 SQL 中 `?` 的顺序给出。
    pub fn from_parts(
        sql: String,
        params: Vec<Value>,
        max_rows: i64,
        cell_char_limit: i64,
        cell_char_offset: i64,
        allowed_schemas: Vec<String>,
        statement_timeout_ms: i64,
    ) -> Self {
        Self {
            sql,
            params,
            max_rows,
            max_tokens: None,
            cell_char_limit,
            cell_char_offset,
            allowed_schemas,
            statement_timeout_ms,
            database: None,
        }
    }
}

fn json_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// 可注入执行面：生产实现为 ConnectorExecutor，测试注入 FakeExecutor。
pub trait QueryExecutor: Send + Sync {
    /// 执行前校验（同步，L4）：sqlguard（ClickHouse 方言）+ `?` 占位符个数 + 标量参数 +
    /// database 白名单。默认实现即当前唯一引擎；其它引擎接入时覆盖为自己的守卫。
    fn validate(&self, q: &QueryParams) -> Result<(), McpError> {
        validate_sql(q)?;
        check_placeholders(&q.sql, q.params.len())?;
        check_scalar_params(&q.params)?;
        check_database(q, None)?;
        Ok(())
    }

    fn execute<'a>(&'a self, scope: &'a EffectiveScope, q: &'a QueryParams) -> ExecFuture<'a>;
}

/// 默认实现：没有接入只读数据源时显式失败，不假执行。
pub struct DefaultQueryExecutor;

impl QueryExecutor for DefaultQueryExecutor {
    fn execute<'a>(&'a self, _scope: &'a EffectiveScope, _q: &'a QueryParams) -> ExecFuture<'a> {
        Box::pin(async {
            Err(McpError::new(
                "MCP_STORE_ERROR",
                json!({"hint": "未配置只读数据源：请在 policy 中声明 engine=\"clickhouse\" 的 datasource"}),
            ))
        })
    }
}

/// 桥接 GuardError → McpError：保 code 与 data 原样。
pub fn guard_to_mcp(e: jumcp_sqlguard::GuardError) -> McpError {
    McpError::with_message(e.code, e.message, e.data)
}

/// 只读校验（L4）：单语句 + 根节点为查询 + 黑名单 + schema 范围。当前唯一引擎是 ClickHouse。
pub fn validate_sql(q: &QueryParams) -> Result<jumcp_sqlguard::Validated, McpError> {
    jumcp_sqlguard::validate(
        &q.sql,
        jumcp_sqlguard::Dialect::ClickHouse,
        &q.allowed_schemas,
    )
    .map_err(guard_to_mcp)
}

/// `database` 收窄：请求只能落在连接声明的白名单内。
///
/// `default` 是 agent 未显式指定 database 时的服务端默认库；既然它是实际执行库，
/// 就必须一起参与判定，不能因为入参省略而绕过白名单。
pub fn check_database(q: &QueryParams, default_database: Option<&str>) -> Result<(), McpError> {
    if q.allowed_schemas.is_empty() {
        return Ok(());
    }
    let effective = q
        .database
        .as_deref()
        .or(default_database)
        .unwrap_or("default");
    if q.allowed_schemas.iter().any(|s| s == effective) {
        return Ok(());
    }
    Err(McpError::new(
        "MCP_SCHEMA_OUT_OF_SCOPE",
        json!({"database": effective, "allowed": q.allowed_schemas}),
    ))
}

/// 位置参数个数校验：ClickHouse 的 `?` 由 clickhouse crate 在客户端解析。
///
/// 为了与 crate 的解析完全一致，这里也**不跳过**字符串字面量里的 `?`：
/// crate 文档明确要求 SQL 中不能出现额外 `?`，字面量里的 `?` 也必须写成 `??`。
/// 多算或少算都会在绑定阶段变成难以定位的 `unbound query argument`。
pub fn check_placeholders(sql: &str, param_count: usize) -> Result<(), McpError> {
    let bytes = sql.as_bytes();
    let mut i = 0usize;
    let mut count = 0usize;
    let mut in_string = false;
    while i < bytes.len() {
        let byte = bytes[i];
        if byte == b'\'' {
            if in_string && bytes.get(i + 1) == Some(&b'\'') {
                // SQL 字符串里的 '' 是转义，不结束字面量。
                i += 2;
                continue;
            }
            in_string = !in_string;
            i += 1;
            continue;
        }
        if byte == b'?' {
            if bytes.get(i + 1) == Some(&b'?') {
                i += 2;
                continue;
            }
            if sql[i + 1..].starts_with("fields") {
                return Err(McpError::new(
                    "MCP_PARAM_STYLE_UNSUPPORTED",
                    json!({"hint": "`?fields` 只适用于 clickhouse crate 的 Row 派生，自由查询不支持"}),
                ));
            }
            count += 1;
            i += 1;
            continue;
        }
        // `:name` 只属于模板层（注册期改写为 `?`）；自由查询里出现即拒绝，避免把非法 SQL 透传给 agent。
        if !in_string && byte == b':' {
            let prev_colon = i > 0 && bytes[i - 1] == b':';
            let next_colon = bytes.get(i + 1) == Some(&b':');
            if !prev_colon
                && !next_colon
                && bytes
                    .get(i + 1)
                    .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
            {
                let mut j = i + 2;
                while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                    j += 1;
                }
                return Err(McpError::new(
                    "MCP_PARAM_STYLE_UNSUPPORTED",
                    json!({
                        "hint": "命名占位符只用于模板（注册期改写为 ?）；自由查询只接受 ? 位置占位符",
                        "placeholder": &sql[i..j]
                    }),
                ));
            }
        }
        i += 1;
    }
    if count != param_count {
        return Err(McpError::new(
            "MCP_PARAM_COUNT_MISMATCH",
            json!({"expected": count, "provided": param_count}),
        ));
    }
    Ok(())
}

/// 绑定参数只支持标量：数组/对象需要显式类型与展开，当前不做隐式字符串化。
pub fn check_scalar_params(params: &[Value]) -> Result<(), McpError> {
    for (index, value) in params.iter().enumerate() {
        if matches!(value, Value::Array(_) | Value::Object(_)) {
            return Err(McpError::new(
                "MCP_PARAM_STYLE_UNSUPPORTED",
                json!({
                    "position": index,
                    "hint": "位置参数只支持标量（bool/number/string/null）",
                    "received_type": json_type_name(value)
                }),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> EffectiveScope {
        EffectiveScope {
            groups: vec![],
            budget: 2000,
            degraded: false,
            source: "header".into(),
            mode: "strict".into(),
            allowed_builtin_tools: None,
        }
    }

    #[tokio::test]
    async fn default_reports_missing_datasource() {
        let q = QueryParams::new("SELECT 1".into(), &["public".into()], 1000, &json!({})).unwrap();
        let e = DefaultQueryExecutor
            .execute(&scope(), &q)
            .await
            .unwrap_err();
        assert_eq!(e.code, "MCP_STORE_ERROR");
        assert!(e.data["hint"].as_str().unwrap().contains("clickhouse"));
    }

    #[test]
    fn params_defaults_and_clamps() {
        let q = QueryParams::new(
            "SELECT 1".into(),
            &["public".into()],
            1000,
            &json!({"max_rows": 5000, "cell_char_limit": 99999}),
        )
        .unwrap();
        assert_eq!(q.max_rows, 1000);
        assert_eq!(q.cell_char_limit, 4000);
    }

    #[test]
    fn allowed_schemas_only_narrow() {
        let e = QueryParams::new(
            "SELECT 1".into(),
            &["public".into()],
            1000,
            &json!({"allowed_schemas": ["secret"]}),
        )
        .unwrap_err();
        assert_eq!(e.code, "MCP_SCHEMA_OUT_OF_SCOPE");
    }

    #[test]
    fn database_parses_only_non_empty_strings() {
        let q = QueryParams::new("SELECT 1".into(), &[], 1000, &json!({})).unwrap();
        assert_eq!(q.database, None);
        let q = QueryParams::new(
            "SELECT * FROM cpu".into(),
            &[],
            1000,
            &json!({"database": null}),
        )
        .unwrap();
        assert_eq!(q.database, None);

        let q = QueryParams::new(
            "SELECT * FROM cpu".into(),
            &[],
            1000,
            &json!({"database": "  env_air  "}),
        )
        .unwrap();
        assert_eq!(q.database.as_deref(), Some("env_air"));

        let e = QueryParams::new(
            "SELECT * FROM cpu".into(),
            &[],
            1000,
            &json!({"database": "  "}),
        )
        .unwrap_err();
        assert_eq!(e.code, "MCP_VALIDATION_ERROR");

        let e = QueryParams::new(
            "SELECT * FROM cpu".into(),
            &[],
            1000,
            &json!({"database": 1}),
        )
        .unwrap_err();
        assert_eq!(e.code, "MCP_VALIDATION_ERROR");
    }

    #[test]
    fn default_validate_checks_database_against_whitelist() {
        let q = QueryParams::new(
            "SELECT 1".into(),
            &["env_air".into()],
            1000,
            &json!({"database": "system"}),
        )
        .unwrap();
        let e = DefaultQueryExecutor.validate(&q).unwrap_err();
        assert_eq!(e.code, "MCP_SCHEMA_OUT_OF_SCOPE");
        assert_eq!(e.data["database"], json!("system"));

        let q = QueryParams::new(
            "SELECT 1".into(),
            &["env_air".into()],
            1000,
            &json!({"database": "env_air"}),
        )
        .unwrap();
        assert!(DefaultQueryExecutor.validate(&q).is_ok());

        // 不传 database 时实际执行库是 default，白名单不含它就必须拒绝。
        let q = QueryParams::new("SELECT 1".into(), &["env_air".into()], 1000, &json!({})).unwrap();
        let e = DefaultQueryExecutor.validate(&q).unwrap_err();
        assert_eq!(e.data["database"], json!("default"));
    }

    #[test]
    fn object_params_rejected_as_named_style() {
        let e = QueryParams::new("SELECT 1".into(), &[], 1000, &json!({"params": {"a": 1}}))
            .unwrap_err();
        assert_eq!(e.code, "MCP_PARAM_STYLE_UNSUPPORTED");
    }

    #[test]
    fn placeholder_count_matches() {
        assert!(check_placeholders("SELECT ?, ?", 2).is_ok());
        assert!(check_placeholders("SELECT 1", 0).is_ok());
        let e = check_placeholders("SELECT ?", 0).unwrap_err();
        assert_eq!(e.code, "MCP_PARAM_COUNT_MISMATCH");
        // `??` 是字面 `?`，不占参数位（与 clickhouse crate 的解析一致）。
        assert!(check_placeholders("SELECT '??' AS q", 0).is_ok());
        assert!(check_placeholders("SELECT ?, '??' AS q", 1).is_ok());
    }

    #[test]
    fn question_inside_literal_counts_like_crate() {
        // crate 的 SqlBuilder 会扫描所有 `?`，字面量里也不例外；这里必须同口径，
        // 否则绑定阶段才会以 "unbound query argument" 暴露。
        let e = check_placeholders("SELECT '?' AS q", 0).unwrap_err();
        assert_eq!(e.code, "MCP_PARAM_COUNT_MISMATCH");
    }

    #[test]
    fn unsupported_param_shapes_are_rejected() {
        assert!(check_scalar_params(&[json!(1), json!("x"), json!(null), json!(true)]).is_ok());
        let e = check_scalar_params(&[json!([1, 2])]).unwrap_err();
        assert_eq!(e.code, "MCP_PARAM_STYLE_UNSUPPORTED");
        assert_eq!(e.data["position"], json!(0));
        let e = check_scalar_params(&[json!({"a": 1})]).unwrap_err();
        assert_eq!(e.code, "MCP_PARAM_STYLE_UNSUPPORTED");
    }
}
