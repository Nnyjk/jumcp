//! ClickHouse 只读查询的内置 MCP 插件（MCP `2025-03-26`）。
//!
//! 定位：内置包 `clickhouse-basic` 的插件实现。server 侧把安装产物登记成一条 HTTP 上游，
//! 请求经 `/internal/mcp/clickhouse-basic/<upstream_id>` 回到本插件的 [`Server`]；
//! router 对外把工具暴露成 `<server_id>__<tool>`，所以这里只用短名
//! `query` / `list_databases` / `list_tables`。
//!
//! 只读语义三层都在，任何一层都不因本 crate 而削弱：
//!
//! - L1：连接账号权限（部署侧只给只读账号）；
//! - L2：connector agent 固定 `readonly=1`、`max_execution_time`、`max_result_rows`、
//!   `result_overflow_mode=break`、`max_threads=1`；
//! - L4：[`jumcp_sqlguard::validate`] 的单语句 / 根查询 / 函数黑名单 / schema 范围校验。
//!
//! 边界：配置只经 [`BuiltinConfig`](jumcp_core::builtin::BuiltinConfig) 按字段 key 传入，
//! 密码不进入任何日志或错误文案；工具级失败回 `isError:true` 结果（`content[0].text`
//! 内含稳定 `MCP_*` 码），不是协议错误。

mod executor;
mod plugin;
mod tools;

pub use executor::{ExecFuture, Executor, ToolError};
pub use plugin::{ClickHousePlugin, plugin};
pub use tools::{
    CELL_CHAR_LIMIT_CEILING, CELL_CHAR_LIMIT_DEFAULT, MAX_ROWS_CEILING, MAX_ROWS_DEFAULT,
    TOOL_LIST_DATABASES, TOOL_LIST_TABLES, TOOL_NAMES, TOOL_QUERY,
};

use std::sync::Arc;

use serde_json::{Value, json};

/// 工具执行的缺省超时（毫秒）。单次调用可用 `timeout_ms` 覆盖，上限见 [`TIMEOUT_CEILING_MS`]。
pub const DEFAULT_TIMEOUT_MS: i64 = 15_000;
/// 单次调用允许的最大超时（毫秒）。
pub const TIMEOUT_CEILING_MS: i64 = tools::TIMEOUT_CEILING_MS;

/// 工具面：执行器 + 只读范围 + 缺省超时。
pub struct Server {
    executor: Arc<dyn Executor>,
    allowed_schemas: Vec<String>,
    timeout_ms: i64,
}

impl Server {
    /// 构造服务端。
    pub fn new(executor: Arc<dyn Executor>, allowed_schemas: Vec<String>, timeout_ms: i64) -> Self {
        Self {
            executor,
            allowed_schemas,
            timeout_ms,
        }
    }

    /// 执行器。
    pub fn executor(&self) -> &dyn Executor {
        self.executor.as_ref()
    }

    /// 允许的 schema/库（空表示不限制）。
    pub fn allowed_schemas(&self) -> &[String] {
        &self.allowed_schemas
    }

    /// 查询缺省超时（毫秒）。
    pub fn timeout_ms(&self) -> i64 {
        self.timeout_ms
    }

    /// 执行 `tools/call`，返回 MCP 结果对象（成功含 content+structuredContent，失败含 isError）。
    ///
    /// 未知工具与参数反序列化失败也走工具级失败：调用方只会从 `tools/list` 拿到工具名，
    /// 这两种情况属于调用方错误而不是协议损坏，用稳定的 `MCP_VALIDATION_ERROR` 更好排障。
    pub async fn call_tool(&self, tool: &str, arguments: &Value) -> Value {
        let arguments = match arguments {
            Value::Null => json!({}),
            value @ Value::Object(_) => value.clone(),
            _ => {
                return failure_result(&invalid_tool(tool, "tools/call 的 arguments 必须是对象"));
            }
        };
        let outcome = match tool {
            tools::TOOL_QUERY => match serde_json::from_value::<tools::QueryArgs>(arguments) {
                Ok(args) => self.call_query(args).await,
                Err(error) => {
                    return failure_result(&invalid_tool(
                        tool,
                        format!("query 参数不合法: {error}"),
                    ));
                }
            },
            tools::TOOL_LIST_DATABASES => self.call_list_databases().await,
            tools::TOOL_LIST_TABLES => {
                match serde_json::from_value::<tools::ListTablesArgs>(arguments) {
                    Ok(args) => self.call_list_tables(args).await,
                    Err(error) => {
                        return failure_result(&invalid_tool(
                            tool,
                            format!("list_tables 参数不合法: {error}"),
                        ));
                    }
                }
            }
            other => {
                return failure_result(&invalid_tool(other, format!("未知工具: {other}")));
            }
        };
        match outcome {
            Ok(payload) => success_result(payload),
            Err(error) => failure_result(&error),
        }
    }
}

fn invalid_tool(tool: &str, message: impl Into<String>) -> ToolError {
    ToolError::new(
        "MCP_VALIDATION_ERROR",
        message,
        json!({"tool": tool, "known_tools": tools::TOOL_NAMES}),
    )
}

/// 成功结果：`content` 文本与 `structuredContent` 是同一份 JSON。
fn success_result(payload: Value) -> Value {
    json!({
        "content": [{"type": "text", "text": compact_json(&payload)}],
        "structuredContent": payload,
        "isError": false
    })
}

/// 失败结果：`isError:true` + 含稳定 `code` 的文本（不伪装成协议错误）。
fn failure_result(error: &ToolError) -> Value {
    json!({
        "content": [{"type": "text", "text": error.to_json()}],
        "isError": true
    })
}

/// 紧凑 JSON（行间不换行，省 token）。
fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| error_text("MCP_STORE_ERROR", "结果序列化失败"))
}

/// 序列化兜底文案：`serde_json::Value` 的序列化实际不会失败，保留仅为不 panic。
fn error_text(code: &str, message: &str) -> String {
    format!("{{\"code\":\"{code}\",\"message\":\"{message}\"}}")
}
