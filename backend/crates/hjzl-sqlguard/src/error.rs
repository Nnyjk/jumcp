//! 校验失败的错误类型。
//!
//! 错误码是服务端与客户端之间的稳定契约：同一语义的问题必须落到同一错误码，
//! 客户端才能在不解析文案的前提下做出分支。

use serde_json::Value;

/// 解析出多条语句。
pub const CODE_MULTI_STATEMENT: &str = "MCP_MULTI_STATEMENT";
/// SQL 未通过只读校验。
pub const CODE_SQL_REJECTED: &str = "MCP_SQL_REJECTED";
/// SQL 引用的 schema 不在允许范围内。
pub const CODE_SCHEMA_OUT_OF_SCOPE: &str = "MCP_SCHEMA_OUT_OF_SCOPE";

/// 校验失败。
///
/// `code` 是稳定错误码，`message` 是面向调用方的描述文案，
/// `data` 是结构化附加信息（如 offending_tables），随错误响应回传。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("[{code}] {message}")]
pub struct GuardError {
    /// 稳定错误码。
    pub code: &'static str,
    /// 面向调用方的描述文案。
    pub message: String,
    /// 结构化附加信息。
    pub data: Value,
}

impl GuardError {
    /// 构造校验失败错误。
    pub fn new(code: &'static str, message: impl Into<String>, data: Value) -> Self {
        Self {
            code,
            message: message.into(),
            data,
        }
    }
}
