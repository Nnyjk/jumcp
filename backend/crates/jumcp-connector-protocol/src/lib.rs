//! Connector agent 线上协议（与具体数据库无关）。
//!
//! 只依赖 serde/serde_json：host 只引用本 crate，不会把任何数据库驱动链进服务进程。
//! 具体 agent 的错误类型、DSN 解析与执行逻辑留在各自 agent crate。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 协议版本：随 `initialize` 回执下发给对端；framing 层不做版本比对，对端必须自行拒绝不兼容版本。
pub const PROTOCOL_VERSION: u32 = 1;

pub const METHOD_INITIALIZE: &str = "initialize";
pub const METHOD_QUERY: &str = "query";
pub const METHOD_SHUTDOWN: &str = "shutdown";

/// JSON-RPC 标准码 + 本协议扩展码（扩展码落在实现自定义区间 -32000..=-32099）。
pub mod rpc_code {
    pub const PARSE_ERROR: i32 = -32700;
    pub const INVALID_REQUEST: i32 = -32600;
    pub const METHOD_NOT_FOUND: i32 = -32601;
    pub const INVALID_PARAMS: i32 = -32602;
    pub const INTERNAL_ERROR: i32 = -32603;
    /// 数据源明确返回的错误（语法、权限、未知表等）。
    pub const DB_ERROR: i32 = -32001;
    /// 客户端或服务端超时：结果未知，调用方不得重试。
    pub const EXEC_TIMEOUT: i32 = -32002;
    /// 连接配置/DNS/握手失败。
    pub const CONFIG_ERROR: i32 = -32003;
}

/// 与 host `McpError.code` 同名，保证跨进程错误码稳定。
pub mod mcp_code {
    pub const STORE_ERROR: &str = "MCP_STORE_ERROR";
    pub const EXEC_TIMEOUT: &str = "MCP_EXEC_TIMEOUT";
    pub const CONFIG_ERROR: &str = "MCP_CONFIG_ERROR";
    pub const VALIDATION_ERROR: &str = "MCP_VALIDATION_ERROR";
    pub const PARAM_STYLE_UNSUPPORTED: &str = "MCP_PARAM_STYLE_UNSUPPORTED";
}

/// 握手回执：协议版本 + agent/服务端版本 + 当前默认库。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeResult {
    pub protocol_version: u32,
    pub agent_version: String,
    pub server_version: String,
    pub database: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnDef {
    pub name: String,
    #[serde(rename = "type")]
    pub data_type: String,
}

/// `query` 入参。`params` 是标量位置数组，按 SQL 中 `?` 的顺序绑定。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryRequest {
    pub sql: String,
    #[serde(default)]
    pub params: Vec<Value>,
    #[serde(default = "default_max_rows")]
    pub max_rows: i64,
    #[serde(default)]
    pub database: Option<String>,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: i64,
}

fn default_max_rows() -> i64 {
    100
}

fn default_timeout_ms() -> i64 {
    15_000
}

/// `query` 出参：列（名/类型）+ 二维行 + 行数/截断/耗时。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResult {
    pub columns: Vec<ColumnDef>,
    pub rows: Vec<Vec<Value>>,
    pub row_count: i64,
    pub truncated: bool,
    pub elapsed_ms: i64,
}
