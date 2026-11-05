// 只读执行委托：默认 MCP_NOT_IMPLEMENTED，绝不假执行（§2.10 错误必须显式）。
// db_query_readonly 参数见 spec §4.3.2：sql/params/max_rows/max_tokens/cell_char_limit/cell_char_offset/allowed_schemas。

use hjzl_core::entry::EffectiveScope;
use hjzl_core::errors::McpError;
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;

/// 异步执行面：生产实现为 PgExecutor（L1–L3），测试注入 FakeExecutor。
/// 用 BoxFuture 而非 async-trait：仓库不引入宏 crate，签名仍保持零成本静态分发。
pub type ExecFuture<'a> = Pin<Box<dyn Future<Output = Result<Value, McpError>> + Send + 'a>>;

/// db_query_readonly 入参（已过 schema 收窄校验与占位符风格校验）。
#[derive(Debug, Clone)]
pub struct QueryParams {
    pub sql: String,
    /// 位置参数数组（按 `$1..$n` 顺序绑定），非数组一律拒绝。
    pub params: Vec<Value>,
    pub max_rows: i64,
    pub max_tokens: Option<i64>,
    pub cell_char_limit: i64,
    pub cell_char_offset: i64,
    pub allowed_schemas: Vec<String>,
    pub statement_timeout_ms: i64,
}

impl QueryParams {
    /// §4.3.2 默认/上限：max_rows 默认 100 上限 1000；cell_char_limit 默认 200 上限 4000。
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
                        "hint": "params 只接受位置参数数组（按 $1..$n 顺序绑定）；命名参数需经模板层改写",
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
        })
    }
}

impl QueryParams {
    /// 模板路径：SQL 已由注册期门禁改写为位置占位符，值按 `$1..$n` 顺序给出。
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

/// 可注入执行面：生产实现为 PgExecutor（L1–L3），测试注入 FakeExecutor。
pub trait QueryExecutor: Send + Sync {
    fn execute<'a>(&'a self, scope: &'a EffectiveScope, q: &'a QueryParams) -> ExecFuture<'a>;
}

/// 默认实现：抛 MCP_NOT_IMPLEMENTED——未接执行链时不假执行，避免治理面被绕过。
pub struct DefaultQueryExecutor;

impl QueryExecutor for DefaultQueryExecutor {
    fn execute<'a>(&'a self, _scope: &'a EffectiveScope, _q: &'a QueryParams) -> ExecFuture<'a> {
        Box::pin(async {
            Err(McpError::new(
                "MCP_NOT_IMPLEMENTED",
                json!({"hint": "未注入 QueryExecutor（sqlguard 链 / 数据源未就绪）"}),
            ))
        })
    }
}

/// 桥接 GuardError → McpError：保 code 与 data 原样。
pub fn guard_to_mcp(e: hjzl_sqlguard::GuardError) -> McpError {
    McpError::with_message(e.code, e.message, e.data)
}

/// 只读校验（L4）：单语句 + 根节点为查询 + 黑名单 + schema 范围。
pub fn validate_sql(q: &QueryParams) -> Result<hjzl_sqlguard::Validated, McpError> {
    hjzl_sqlguard::validate(&q.sql, hjzl_sqlguard::Dialect::Postgres, &q.allowed_schemas)
        .map_err(guard_to_mcp)
}

/// 参数风格与个数校验：只接受 `$1..$n` 位置参数；命名占位符留给模板层改写，不在此做字符串替换。
///
/// 扫描时跳过字符串字面量与 `::` 类型转换，避免把 `'a:b'`、`x::int` 误判成命名占位符。
pub fn check_placeholders(sql: &str, param_count: usize) -> Result<(), McpError> {
    let bytes = sql.as_bytes();
    let mut i = 0;
    let mut max_index = 0usize;
    let mut indices: Vec<usize> = Vec::new();
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
            '$' => {
                let mut j = i + 1;
                let mut digits = String::new();
                while j < bytes.len() && (bytes[j] as char).is_ascii_digit() {
                    digits.push(bytes[j] as char);
                    j += 1;
                }
                if !digits.is_empty() {
                    let n: usize = digits.parse().map_err(|_| {
                        McpError::new(
                            "MCP_PARAM_COUNT_MISMATCH",
                            json!({"hint": "占位符编号非法", "placeholder": digits}),
                        )
                    })?;
                    max_index = max_index.max(n);
                    indices.push(n);
                    i = j;
                } else {
                    i += 1;
                }
            }
            ':' => {
                // `::` 是类型转换；`:=` 是赋值运算符；两者都不是命名占位符。
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
                    return Err(McpError::new(
                        "MCP_PARAM_STYLE_UNSUPPORTED",
                        json!({
                            "hint": "命名占位符需经模板层改写为位置参数；db_query_readonly 只接受 $1..$n",
                            "placeholder": format!(":{name}")
                        }),
                    ));
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    if max_index != param_count {
        return Err(McpError::new(
            "MCP_PARAM_COUNT_MISMATCH",
            json!({"expected": max_index, "provided": param_count, "placeholders": indices}),
        ));
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
        }
    }

    #[tokio::test]
    async fn default_is_not_implemented() {
        let q = QueryParams::new("SELECT 1".into(), &["public".into()], 1000, &json!({})).unwrap();
        let e = DefaultQueryExecutor
            .execute(&scope(), &q)
            .await
            .unwrap_err();
        assert_eq!(e.code, "MCP_NOT_IMPLEMENTED");
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
    fn object_params_rejected_as_named_style() {
        let e = QueryParams::new("SELECT 1".into(), &[], 1000, &json!({"params": {"a": 1}}))
            .unwrap_err();
        assert_eq!(e.code, "MCP_PARAM_STYLE_UNSUPPORTED");
    }

    #[test]
    fn placeholder_count_matches() {
        assert!(check_placeholders("SELECT $1, $2", 2).is_ok());
        assert!(check_placeholders("SELECT 1", 0).is_ok());
        let e = check_placeholders("SELECT $1", 0).unwrap_err();
        assert_eq!(e.code, "MCP_PARAM_COUNT_MISMATCH");
    }

    #[test]
    fn named_placeholder_rejected_but_casts_and_literals_allowed() {
        let e = check_placeholders("SELECT * FROM t WHERE a = :name", 0).unwrap_err();
        assert_eq!(e.code, "MCP_PARAM_STYLE_UNSUPPORTED");
        assert!(check_placeholders("SELECT a::int FROM t WHERE b = 'x:y'", 0).is_ok());
    }
}
