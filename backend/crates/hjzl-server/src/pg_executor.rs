// 真实执行器 PgExecutor（阶段一 L1/L2/L3）。
// 为什么只实现 PG：阶段一唯一承接的数据源是 PG 只读账号；其余方言走同一 QueryExecutor 扩展点。

use crate::executor::{ExecFuture, QueryExecutor, QueryParams, check_placeholders};
use base64::Engine;
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use hjzl_core::budget::estimate_tokens;
use hjzl_core::entry::EffectiveScope;
use hjzl_core::errors::McpError;
use serde_json::{Value, json};
use sqlx::postgres::{PgArguments, PgPool, PgPoolOptions, PgRow};
use sqlx::{Arguments, Column, Row, TypeInfo};
use std::time::Instant;

/// 连接选项只读会话设置：为什么写在 DSN 而非每次 SET——运行期 `SET ... off` 可被
/// `COMMIT; SET ...=off; DROP ...` 链式绕过，且 PgBouncer 下会污染共享 socket（§4.3.1）。
pub const READONLY_OPTIONS: &str = "options=-c default_transaction_read_only=on";

/// 给 DSN 追加只读会话选项（重复出现时 PG 取最后一个，故直接追加）。
pub fn readonly_dsn(base: &str) -> String {
    let sep = if base.contains('?') { '&' } else { '?' };
    format!("{base}{sep}{READONLY_OPTIONS}")
}

#[derive(Debug, Clone)]
pub struct PgExecutorConfig {
    pub datasource_name: String,
    pub dsn: String,
    pub statement_timeout_ms: i64,
    pub allowed_schemas: Vec<String>,
    pub max_connections: u32,
}

impl PgExecutorConfig {
    /// 启动期纯校验：不连库也能发现配置错误（fail-closed 在连接前就拒绝）。
    pub fn validate(&self) -> Result<(), McpError> {
        if self.dsn.trim().is_empty() {
            return Err(McpError::config(format!(
                "数据源 {} 的 DSN 为空",
                self.datasource_name
            )));
        }
        if !(self.dsn.starts_with("postgres://") || self.dsn.starts_with("postgresql://")) {
            return Err(McpError::config(format!(
                "数据源 {} 的 DSN 不是 postgres:// 连接串",
                self.datasource_name
            )));
        }
        if self.statement_timeout_ms <= 0 {
            return Err(McpError::config(format!(
                "数据源 {} 的 statement_timeout_ms 非法",
                self.datasource_name
            )));
        }
        Ok(())
    }
}

pub struct PgExecutor {
    pool: PgPool,
    statement_timeout_ms: i64,
    pub datasource_name: String,
    pub allowed_schemas: Vec<String>,
}

impl PgExecutor {
    /// 启动时解析 DSN 并自检（fail-closed）：只读开关必须为 on，写操作必须失败。
    pub async fn connect(cfg: PgExecutorConfig) -> Result<Self, McpError> {
        cfg.validate()?;
        let pool = PgPoolOptions::new()
            .max_connections(cfg.max_connections.max(1))
            .connect(&readonly_dsn(&cfg.dsn))
            .await
            .map_err(|e| {
                McpError::store(format!("数据源 {} 连接失败: {e}", cfg.datasource_name))
            })?;
        let executor = Self {
            pool,
            statement_timeout_ms: cfg.statement_timeout_ms,
            datasource_name: cfg.datasource_name,
            allowed_schemas: cfg.allowed_schemas,
        };
        executor.self_check().await?;
        Ok(executor)
    }

    /// 启动自检（W1.4）：任一不满足即 Err，调用方据此非零退出。
    pub async fn self_check(&self) -> Result<(), McpError> {
        let read_only: String = sqlx::query_scalar("SHOW default_transaction_read_only")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| {
                McpError::store(format!(
                    "数据源 {} 自检失败(SHOW): {e}",
                    self.datasource_name
                ))
            })?;
        if read_only != "on" {
            return Err(McpError::config(format!(
                "数据源 {} 自检失败：default_transaction_read_only={read_only}，期望 on",
                self.datasource_name
            )));
        }
        if sqlx::query("CREATE TEMP TABLE __hjzl_probe(x int)")
            .execute(&self.pool)
            .await
            .is_ok()
        {
            return Err(McpError::config(format!(
                "数据源 {} 自检失败：只读会话下写语句未被拒绝",
                self.datasource_name
            )));
        }
        Ok(())
    }

    /// 单事务执行：SET LOCAL 级超时 + 位置参数绑定 + 解码；永远 rollback（只读面无可提交内容）。
    async fn run(&self, q: &QueryParams) -> Result<(Vec<Vec<Value>>, i64), McpError> {
        check_placeholders(&q.sql, q.params.len())?;
        let started = Instant::now();
        let timeout_ms = if q.statement_timeout_ms > 0 {
            q.statement_timeout_ms
        } else {
            self.statement_timeout_ms
        };
        let mut tx = self.pool.begin().await.map_err(|e| {
            McpError::store(format!("数据源 {} 开启事务失败: {e}", self.datasource_name))
        })?;
        // 选"事务级 SET"而非连接选项：超时按模板/调用变化，连接选项只能给固定值。
        // `SET LOCAL statement_timeout = $1` 不接受绑定参数（utility 语句无参数位），
        // 故用 set_config(..., is_local=true) 等价实现：事务结束（本处恒为 rollback）即失效，
        // 不会泄漏到 PgBouncer 的共享 socket。
        sqlx::query("SELECT set_config('statement_timeout', $1, true)")
            .bind(format!("{timeout_ms}ms"))
            .execute(&mut *tx)
            .await
            .map_err(|e| McpError::store(format!("设置 statement_timeout 失败: {e}")))?;

        let mut args = PgArguments::default();
        for p in &q.params {
            bind_json(&mut args, p)?;
        }
        let query = sqlx::query_with::<sqlx::Postgres, _>(sqlx::AssertSqlSafe(q.sql.clone()), args);
        let fetched = query.fetch_all(&mut *tx).await;
        match fetched {
            Ok(rows) => {
                let decoded = decode_rows(&rows)?;
                // 只读面不存在需要提交的事务；rollback 同时回收 SET LOCAL 与快照。
                let _ = tx.rollback().await;
                Ok((decoded, started.elapsed().as_millis() as i64))
            }
            Err(e) => {
                let _ = tx.rollback().await;
                let message = e.to_string();
                if message.contains("statement timeout") || message.contains("canceling statement")
                {
                    return Err(McpError::timeout(message));
                }
                Err(McpError::store(message))
            }
        }
    }
}

impl PgExecutor {
    /// 发布期预检：只读事务里跑一次 EXPLAIN（全 NULL 样本值），失败即拒绝发布（fail-closed）。
    pub async fn explain(&self, sql: &str, value_count: usize) -> Result<(), McpError> {
        let q = QueryParams::from_parts(
            format!("EXPLAIN {sql}"),
            vec![Value::Null; value_count],
            100,
            200,
            0,
            self.allowed_schemas.clone(),
            self.statement_timeout_ms,
        );
        self.run(&q).await.map(|_| ())
    }
}

impl QueryExecutor for PgExecutor {
    fn execute<'a>(&'a self, _scope: &'a EffectiveScope, q: &'a QueryParams) -> ExecFuture<'a> {
        Box::pin(async move {
            let (rows, elapsed_ms) = self.run(q).await?;
            let shaped = hjzl_sqlguard::shape_rows(
                &rows,
                q.max_rows as usize,
                q.cell_char_limit as usize,
                q.cell_char_offset as usize,
            );
            let tokens = estimate_tokens(&json!({ "rows": shaped.rows }));
            let next_offsets: Vec<Value> = shaped
                .next_offsets
                .iter()
                .map(|(r, c, o)| json!({"row": r, "column": c, "next_offset": o}))
                .collect();
            Ok(json!({
                "rows": shaped.rows,
                "row_count": shaped.row_count,
                "truncated": shaped.truncated,
                "effective_max_rows": shaped.effective_max_rows,
                "next_offsets": next_offsets,
                "elapsed_ms": elapsed_ms,
                "tokens": tokens
            }))
        })
    }
}

/// JSON → 位置参数绑定。null 绑定为 text NULL（类型不可知的显式选择，避免静默丢弃参数）。
fn bind_json(args: &mut PgArguments, value: &Value) -> Result<(), McpError> {
    match value {
        Value::Bool(b) => args.add(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                args.add(i)
            } else if let Some(f) = n.as_f64() {
                args.add(f)
            } else {
                return Err(McpError::validation(format!("不支持的数值参数: {n}")));
            }
        }
        Value::String(s) => args.add(s.clone()),
        Value::Null => args.add(Option::<String>::None),
        other => {
            return Err(McpError::new(
                "MCP_PARAM_STYLE_UNSUPPORTED",
                json!({"hint": "位置参数只支持标量（bool/number/string/null）", "received": other}),
            ));
        }
    }
    .map_err(|e| McpError::validation(format!("参数绑定失败: {e}")))?;
    Ok(())
}

fn decode_rows(rows: &[PgRow]) -> Result<Vec<Vec<Value>>, McpError> {
    let mut out: Vec<Vec<Value>> = Vec::with_capacity(rows.len());
    for row in rows {
        let mut cells: Vec<Value> = Vec::with_capacity(row.columns().len());
        for (idx, col) in row.columns().iter().enumerate() {
            cells.push(decode_cell(row, idx, col.name(), col.type_info().name())?);
        }
        out.push(cells);
    }
    Ok(out)
}

/// 单格解码：类型不在支持集内即显式报错（带列名与类型名），绝不把解码失败吞成 NULL。
fn decode_cell(row: &PgRow, idx: usize, name: &str, type_name: &str) -> Result<Value, McpError> {
    let unsupported = || {
        McpError::new(
            "MCP_UNSUPPORTED_COLUMN_TYPE",
            json!({"column": name, "type": type_name}),
        )
    };
    let decode_err =
        |e: sqlx::Error| McpError::store(format!("列 {name}（{type_name}）解码失败: {e}"));
    let value = match type_name {
        "BOOL" => opt(row.try_get::<Option<bool>, _>(idx))
            .map_err(decode_err)?
            .map(|v| json!(v)),
        "INT2" => opt(row.try_get::<Option<i16>, _>(idx))
            .map_err(decode_err)?
            .map(|v| json!(v)),
        "INT4" => opt(row.try_get::<Option<i32>, _>(idx))
            .map_err(decode_err)?
            .map(|v| json!(v)),
        "INT8" => opt(row.try_get::<Option<i64>, _>(idx))
            .map_err(decode_err)?
            .map(|v| json!(v)),
        "FLOAT4" => opt(row.try_get::<Option<f32>, _>(idx))
            .map_err(decode_err)?
            .map(|v| json!(v)),
        "FLOAT8" => opt(row.try_get::<Option<f64>, _>(idx))
            .map_err(decode_err)?
            .map(|v| json!(v)),
        "NUMERIC" => opt(row.try_get::<Option<sqlx::types::BigDecimal>, _>(idx))
            .map_err(decode_err)?
            .map(|v| json!(v.to_string())),
        "TEXT" | "VARCHAR" | "BPCHAR" | "NAME" | "UNKNOWN" => {
            opt(row.try_get::<Option<String>, _>(idx))
                .map_err(decode_err)?
                .map(|v| json!(v))
        }
        "DATE" => opt(row.try_get::<Option<NaiveDate>, _>(idx))
            .map_err(decode_err)?
            .map(|v| json!(v.format("%Y-%m-%d").to_string())),
        "TIME" => opt(row.try_get::<Option<NaiveTime>, _>(idx))
            .map_err(decode_err)?
            .map(|v| json!(v.format("%H:%M:%S%.f").to_string())),
        "TIMESTAMP" => opt(row.try_get::<Option<NaiveDateTime>, _>(idx))
            .map_err(decode_err)?
            .map(|v| json!(v.format("%Y-%m-%dT%H:%M:%S%.f").to_string())),
        "TIMESTAMPTZ" => opt(row.try_get::<Option<DateTime<Utc>>, _>(idx))
            .map_err(decode_err)?
            .map(|v| json!(v.to_rfc3339())),
        "UUID" => opt(row.try_get::<Option<uuid::Uuid>, _>(idx))
            .map_err(decode_err)?
            .map(|v| json!(v.to_string())),
        "JSON" | "JSONB" => opt(row.try_get::<Option<Value>, _>(idx)).map_err(decode_err)?,
        "BYTEA" => opt(row.try_get::<Option<Vec<u8>>, _>(idx))
            .map_err(decode_err)?
            .map(|v| json!(base64::engine::general_purpose::STANDARD.encode(v))),
        _ => return Err(unsupported()),
    };
    Ok(value.unwrap_or(Value::Null))
}

/// `try_get::<Option<T>>` 的 Err 上抛，Ok(None) 与 Ok(Some) 交给调用方区分 NULL 与值。
fn opt<T>(r: Result<Option<T>, sqlx::Error>) -> Result<Option<T>, sqlx::Error> {
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readonly_dsn_appends_options() {
        let d = readonly_dsn("postgres://u@localhost/db");
        assert!(d.ends_with(READONLY_OPTIONS));
        assert!(d.contains("?options="));
        let d2 = readonly_dsn("postgres://u@localhost/db?sslmode=require");
        assert!(d2.contains("?sslmode=require&options="));
    }

    #[test]
    fn config_validation_rejects_bad_dsn_with_datasource_name() {
        let cfg = PgExecutorConfig {
            datasource_name: "main-pg".into(),
            dsn: String::new(),
            statement_timeout_ms: 1000,
            allowed_schemas: vec![],
            max_connections: 1,
        };
        let e = cfg.validate().unwrap_err();
        assert_eq!(e.code, "MCP_CONFIG_ERROR");
        assert!(e.message.contains("main-pg"));

        let bad = PgExecutorConfig {
            dsn: "mysql://u@h/db".into(),
            ..cfg
        };
        assert_eq!(bad.validate().unwrap_err().code, "MCP_CONFIG_ERROR");
    }

    #[test]
    fn bind_rejects_non_scalar() {
        let mut args = PgArguments::default();
        let e = bind_json(&mut args, &json!({"a": 1})).unwrap_err();
        assert_eq!(e.code, "MCP_PARAM_STYLE_UNSUPPORTED");
    }

    #[tokio::test]
    #[ignore = "需要真实 PG：HJZL_TEST_PG_DSN"]
    async fn pg_real_self_check() {
        let Ok(dsn) = std::env::var("HJZL_TEST_PG_DSN") else {
            return;
        };
        let cfg = PgExecutorConfig {
            datasource_name: "test-pg".into(),
            dsn,
            statement_timeout_ms: 5000,
            allowed_schemas: vec!["public".into()],
            max_connections: 1,
        };
        assert!(PgExecutor::connect(cfg).await.is_ok());
    }
}
