//! 执行器抽象与工具级错误。
//!
//! 拆分目的：把「校验 + 整形 + 协议」与「真正打 ClickHouse」解耦，协议与工具语义可以在
//! 不连真库的前提下用 fake 执行器测试。生产实现就是 [`ClickHouseAgent`] 本体，
//! 它内部固定 `readonly=1`、`max_execution_time`、`max_result_rows` 与
//! `result_overflow_mode=break`，本 crate 不重复也不放宽这些设置。

use std::future::Future;
use std::pin::Pin;

use jumcp_connector_clickhouse::{AgentError, ClickHouseAgent};
use jumcp_connector_protocol::{QueryRequest, QueryResult, mcp_code};
use jumcp_sqlguard::GuardError;
use serde_json::{Value, json};

/// 执行一次只读查询的 future。用装箱而不是泛型，是为了 trait object 化（测试注入）。
pub type ExecFuture<'a> =
    Pin<Box<dyn Future<Output = Result<QueryResult, AgentError>> + Send + 'a>>;

/// 只读查询执行器。
pub trait Executor: Send + Sync {
    /// 执行一条已通过 L4 校验的只读查询。
    fn query<'a>(&'a self, request: &'a QueryRequest) -> ExecFuture<'a>;
}

impl Executor for ClickHouseAgent {
    fn query<'a>(&'a self, request: &'a QueryRequest) -> ExecFuture<'a> {
        // 显式走固有方法：`ClickHouseAgent::query` 与本 trait 方法同名。
        Box::pin(async move { ClickHouseAgent::query(self, request).await })
    }
}

/// 工具级失败：`code` 是稳定错误码，`message` 面向调用方，`data` 为结构化细节。
///
/// 这类失败回 MCP 结果 `isError:true`，**不是** JSON-RPC error：调用方据此看到
/// 「工具执行失败」，而不是「协议/方法不存在」。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolError {
    /// 稳定错误码（`MCP_*`）。
    pub code: &'static str,
    /// 面向调用方的描述文案。
    pub message: String,
    /// 结构化附加信息。
    pub data: Value,
}

impl ToolError {
    /// 构造工具级失败。
    pub fn new(code: &'static str, message: impl Into<String>, data: Value) -> Self {
        Self {
            code,
            message: message.into(),
            data,
        }
    }

    /// L4 校验失败：错误码由 `jumcp-sqlguard` 决定，原样透传（客户端按码分支）。
    pub fn from_guard(error: GuardError) -> Self {
        Self::new(error.code, error.message, error.data)
    }

    /// connector agent 失败：把 agent 侧 `MCP_*` 映射到本服务的工具级错误码。
    ///
    /// `MCP_VALIDATION_ERROR`（空 SQL、非标量参数等）在本服务语义上属于「SQL/参数被拒」，
    /// 统一落 `MCP_SQL_REJECTED`，原始 agent 码保留在 `data.agent_code` 里便于排查。
    pub fn from_agent(error: AgentError) -> Self {
        let code = match error.mcp_code {
            mcp_code::STORE_ERROR => "MCP_STORE_ERROR",
            mcp_code::EXEC_TIMEOUT => "MCP_EXEC_TIMEOUT",
            mcp_code::CONFIG_ERROR => "MCP_CONFIG_ERROR",
            _ => "MCP_SQL_REJECTED",
        };
        let mut data = error.detail;
        if let Some(object) = data.as_object_mut() {
            object.insert("agent_code".into(), json!(error.mcp_code));
            // outcome 明确区分「可能已执行」（unknown）与「确定失败」（failed），调用方据此决定能否重试。
            object.insert(
                "outcome".into(),
                json!(if error.outcome_unknown {
                    "unknown"
                } else {
                    "failed"
                }),
            );
        }
        Self::new(code, error.message, data)
    }

    /// schema/库越界：`allowed_schemas` 白名单不接受该库。
    pub fn schema_out_of_scope(database: &str, allowed: &[String]) -> Self {
        Self::new(
            "MCP_SCHEMA_OUT_OF_SCOPE",
            format!("database {database} 不在 allowed_schemas 白名单内"),
            json!({"database": database, "allowed_schemas": allowed}),
        )
    }

    /// 紧凑 JSON 载荷：工具失败时直接作为 `content[0].text`。
    pub fn to_json(&self) -> String {
        serde_json::to_string(&json!({
            "code": self.code,
            "message": self.message,
            "data": self.data,
        }))
        .unwrap_or_else(|_| format!("{{\"code\":\"{}\"}}", self.code))
    }
}
