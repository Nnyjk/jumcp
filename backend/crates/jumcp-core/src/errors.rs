//! 统一错误码：与 Python errors.py 的 ERROR_CODES 保持同名稳定契约。

use serde_json::Value;
use thiserror::Error;

pub fn default_message(code: &str) -> &str {
    match code {
        "MCP_SQL_REJECTED" => "SQL 未通过只读校验",
        "MCP_MULTI_STATEMENT" => "只允许执行单条语句",
        "MCP_SCHEMA_OUT_OF_SCOPE" => "SQL 引用的 schema 不在允许范围内",
        "MCP_GROUP_FORBIDDEN" => "请求的 tool 组不在角色允许范围内",
        "MCP_BUDGET_EXCEEDED" => "上下文预算不足",
        "MCP_CONTEXT_BUDGET_EXHAUSTED" => "会话上下文预算耗尽",
        "MCP_CAPABILITY_NOT_FOUND" => "能力不存在或不可见",
        "MCP_VERSION_CONFLICT" => "能力版本与会话绑定版本不一致",
        "MCP_NOT_IMPLEMENTED" => "能力尚未实现",
        "MCP_APPROVAL_REQUIRED" => "审批缺失（缺少 approval_id）",
        "MCP_IMMUTABLE_PUBLISHED" => "已发布定义不可原地修改",
        "MCP_BUILTIN_IMMUTABLE" => "内置工具由代码维护，不接受注册表写入",
        "MCP_STATE_TRANSITION_INVALID" => "状态迁移非法",
        "MCP_STORE_ERROR" => "存储层错误",
        "MCP_AUDIT_ERROR" => "审计层错误",
        "MCP_CONFIG_ERROR" => "配置错误",
        "MCP_VALIDATION_ERROR" => "入参校验失败",
        "MCP_PARAM_STYLE_UNSUPPORTED" => "只接受位置参数（$1..$n）；命名占位符须经模板层改写",
        "MCP_PARAM_COUNT_MISMATCH" => "参数个数与 SQL 占位符不匹配",
        "MCP_UNSUPPORTED_COLUMN_TYPE" => "结果集含不支持的列类型",
        "MCP_EXEC_TIMEOUT" => "执行超时，结果未知",
        "MCP_NULL_PARAM_NEEDS_CAST" => "null 参数需要显式类型转换",
        "MCP_TEMPLATE_INVALID" => "模板定义未通过校验门禁",
        "MCP_FILTER_UNSUPPORTED" => "不支持的过滤键",
        "MCP_UNAUTHORIZED" => "鉴权失败：缺少或无效的 Bearer token",
        "MCP_BAD_REQUEST" => "请求体不合法",
        "MCP_PREFLIGHT_REQUIRED" => "发布前必须完成 EXPLAIN 预检",
        "MCP_GROUP_CONFLICT" => "组已存在或由配置维护，管理面不可重复创建",
        "MCP_GROUP_NOT_FOUND" => "组不存在",
        "MCP_GROUP_RESERVED" => "保留组名（builtin/admin）不可创建、修改或删除",
        "MCP_GROUP_IN_USE" => "组仍被能力或令牌引用，不能删除",
        "MCP_TOKEN_NOT_FOUND" => "令牌不存在",
        "MCP_TOKEN_ALREADY_REVOKED" => "令牌已撤销，不能重复撤销",
        "MCP_UPSTREAM_NOT_FOUND" => "上游 MCP server 不存在",
        "MCP_UPSTREAM_CONFLICT" => "上游 id 或工具名冲突",
        "MCP_UPSTREAM_INVALID" => "上游配置不合法",
        "MCP_UPSTREAM_WRITE_FORBIDDEN" => "上游声明可写需要 JUMCP_MCP_ALLOW_WRITE_UPSTREAM=1",
        "MCP_UPSTREAM_STDIO_DISABLED" => "stdio 上游默认关闭（需 JUMCP_MCP_UPSTREAM_STDIO=1）",
        "MCP_UPSTREAM_UNREACHABLE" => "上游 MCP server 不可达",
        "MCP_UPSTREAM_ERROR" => "上游 MCP server 返回错误",
        _ => "MCP error",
    }
}

#[derive(Debug, Error, Clone)]
#[error("[{code}] {message}")]
pub struct McpError {
    pub code: String,
    pub message: String,
    pub data: Value,
}

impl McpError {
    pub fn new(code: &str, data: Value) -> Self {
        Self {
            code: code.to_string(),
            message: default_message(code).to_string(),
            data,
        }
    }
    pub fn with_message(code: &str, message: String, data: Value) -> Self {
        Self {
            code: code.to_string(),
            message,
            data,
        }
    }
    /// 数据库锁中毒是进程内状态损坏，必须显式报错，不得返回空/假值。
    pub fn lock() -> Self {
        Self::new(
            "MCP_STORE_ERROR",
            serde_json::json!({"error": "db lock poisoned"}),
        )
    }
    /// 真正的存储/连接错误（解码失败、连接中断等）；超时用 `timeout`，两者不共用错误码。
    /// 错误码稳定、文案带定位信息：启动失败时运维只看到 message，必须能据此定位数据源。
    fn detailed(code: &str, error: String) -> Self {
        Self::with_message(code, error.clone(), serde_json::json!({"error": error}))
    }
    pub fn store(error: String) -> Self {
        Self::detailed("MCP_STORE_ERROR", error)
    }
    pub fn audit(error: String) -> Self {
        Self::detailed("MCP_AUDIT_ERROR", error)
    }
    pub fn config(error: String) -> Self {
        Self::detailed("MCP_CONFIG_ERROR", error)
    }
    pub fn validation(error: String) -> Self {
        Self::detailed("MCP_VALIDATION_ERROR", error)
    }
    /// 超时/取消：结果未知，调用方不得重试或重放（§6.3）。
    pub fn timeout(detail: String) -> Self {
        Self::with_message(
            "MCP_EXEC_TIMEOUT",
            detail.clone(),
            serde_json::json!({"error": detail, "outcome": "unknown"}),
        )
    }
}

impl From<rusqlite::Error> for McpError {
    fn from(e: rusqlite::Error) -> Self {
        Self::store(e.to_string())
    }
}

impl From<serde_json::Error> for McpError {
    fn from(e: serde_json::Error) -> Self {
        Self::store(format!("json decode failed: {e}"))
    }
}
