//! ClickHouse 只读查询的 MCP stdio server（MCP `2025-03-26`）。
//!
//! 定位：可安装包 `clickhouse-basic` 的可执行体。上游 router 以 stdio 子进程拉起本服务，
//! 对外把工具暴露成 `<server_id>__<tool>`，因此本服务的工具只用短名
//! `query` / `list_databases` / `list_tables`。
//!
//! 只读语义三层都在，任何一层都不因本 crate 而削弱：
//!
//! - L1：DSN 账号权限（部署侧只给只读账号）；
//! - L2：connector agent 固定 `readonly=1`、`max_execution_time`、`max_result_rows`、
//!   `result_overflow_mode=break`、`max_threads=1`；
//! - L4：[`jumcp_sqlguard::validate`] 的单语句 / 根查询 / 函数黑名单 / schema 范围校验。
//!
//! 边界：
//!
//! - DSN 只从环境变量 `JUMCP_CLICKHOUSE_DSN` 读，永不回显（错误文案，由 connector 保证）。
//! - stdout 只承载协议行（一行一个 JSON-RPC 2.0 消息），诊断只走 stderr。
//! - 工具级失败回 `isError:true` 结果（`content[0].text` 内含稳定 `MCP_*` 码），
//!   只有协议级错误才用 JSON-RPC `error`。

mod executor;
mod tools;

pub use executor::{ExecFuture, Executor, ToolError};
pub use tools::{
    CELL_CHAR_LIMIT_CEILING, CELL_CHAR_LIMIT_DEFAULT, MAX_ROWS_CEILING, MAX_ROWS_DEFAULT,
    TOOL_LIST_DATABASES, TOOL_LIST_TABLES, TOOL_NAMES, TOOL_QUERY,
};

use std::sync::Arc;

use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

/// 对外声明的 MCP 协议版本。
pub const MCP_PROTOCOL_VERSION: &str = "2025-03-26";
/// `serverInfo.name`。
pub const SERVER_NAME: &str = "jumcp-mcp-clickhouse";
/// 查询超时环境变量的缺省值（毫秒）。
pub const DEFAULT_TIMEOUT_MS: i64 = 15_000;
/// 查询超时上限（毫秒）。
pub const TIMEOUT_CEILING_MS: i64 = tools::TIMEOUT_CEILING_MS;

/// JSON-RPC 标准错误码（本服务只用到协议级的那几个）。
pub mod rpc_code {
    /// 请求不是合法 JSON。
    pub const PARSE_ERROR: i32 = -32700;
    /// 请求不是 JSON-RPC 对象。
    pub const INVALID_REQUEST: i32 = -32600;
    /// 方法不存在。
    pub const METHOD_NOT_FOUND: i32 = -32601;
    /// 参数不合法（含未知工具名）。
    pub const INVALID_PARAMS: i32 = -32602;
}

/// 进程级配置：全部来自环境变量。
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// ClickHouse DSN（只读账号）。永不打印。
    pub dsn: String,
    /// 允许的 schema/库；空表示不限制。
    pub allowed_schemas: Vec<String>,
    /// 查询缺省超时（毫秒）。
    pub timeout_ms: i64,
}

impl Config {
    /// 从进程环境读取配置。
    pub fn from_env() -> Result<Self, String> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// 从任意「环境变量查询函数」读取配置：单测据此覆盖缺失/非法取值。
    ///
    /// 错误文案只提到变量名，不含取值——DSN 里的密码绝不能进 stderr。
    pub fn from_lookup<F>(lookup: F) -> Result<Self, String>
    where
        F: Fn(&str) -> Option<String>,
    {
        let dsn = lookup("JUMCP_CLICKHOUSE_DSN")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                format!("JUMCP_CLICKHOUSE_DSN 未设置：{SERVER_NAME} 拒绝启动（需要只读账号的 DSN）")
            })?;
        // 大小写不敏感去重：sqlguard 与工具层都按大小写不敏感比较，重复项只会让钉库结果
        // 取决于运营者的手滑。保留首现原串，`with_database` 需要真实的库名拼写。
        let mut allowed_schemas: Vec<String> = Vec::new();
        for schema in lookup("JUMCP_CLICKHOUSE_ALLOWED_SCHEMAS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|schema| !schema.is_empty())
        {
            if !allowed_schemas
                .iter()
                .any(|seen| seen.eq_ignore_ascii_case(schema))
            {
                allowed_schemas.push(schema.to_string());
            }
        }
        let timeout_ms = match lookup("JUMCP_CLICKHOUSE_TIMEOUT_MS")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
        {
            None => DEFAULT_TIMEOUT_MS,
            Some(raw) => raw
                .parse::<i64>()
                .ok()
                .filter(|value| *value > 0)
                .ok_or_else(|| "JUMCP_CLICKHOUSE_TIMEOUT_MS 必须是正整数（毫秒）".to_string())?
                .min(TIMEOUT_CEILING_MS),
        };
        Ok(Self {
            dsn,
            allowed_schemas,
            timeout_ms,
        })
    }
}

/// MCP 服务端状态：执行器 + 只读范围。
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

    /// 处理一条已解析的请求；返回 `None` 表示这是通知（不回包）。
    pub async fn handle_request(&self, request: &Value) -> Option<Value> {
        let method = request.get("method").and_then(Value::as_str);
        // 通知（无 id 或 notifications/*）一律不回包：JSON-RPC 的通知不能有响应。
        if matches!(method, Some(method) if method.starts_with("notifications/")) {
            return None;
        }
        let id = request.get("id").cloned()?;
        let Some(method) = method else {
            return Some(error_response(
                id,
                rpc_code::INVALID_REQUEST,
                "请求缺少 method",
                json!({}),
            ));
        };
        let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
        match method {
            "initialize" => Some(ok_response(
                id,
                json!({
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": {"tools": {}},
                    "serverInfo": {
                        "name": SERVER_NAME,
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }),
            )),
            "ping" => Some(ok_response(id, json!({}))),
            "tools/list" => Some(ok_response(id, json!({"tools": tools::definitions()}))),
            "tools/call" => Some(self.tools_call(id, &params).await),
            other => Some(error_response(
                id,
                rpc_code::METHOD_NOT_FOUND,
                format!("未知方法: {other}"),
                json!({"method": other}),
            )),
        }
    }

    /// `tools/call` 分发。
    ///
    /// 未知工具名、缺 `name`、`arguments` 类型错误 → JSON-RPC `-32602`（协议级）。
    /// 工具自身的失败 → `isError:true` 结果。
    async fn tools_call(&self, id: Value, params: &Value) -> Value {
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return error_response(
                id,
                rpc_code::INVALID_PARAMS,
                "tools/call 缺少 name",
                json!({}),
            );
        };
        let arguments = match params.get("arguments") {
            None | Some(Value::Null) => json!({}),
            Some(value @ Value::Object(_)) => value.clone(),
            Some(_) => {
                return error_response(
                    id,
                    rpc_code::INVALID_PARAMS,
                    "tools/call 的 arguments 必须是对象",
                    json!({"tool": name}),
                );
            }
        };
        let outcome = match name {
            tools::TOOL_QUERY => match serde_json::from_value::<tools::QueryArgs>(arguments) {
                Ok(args) => self.call_query(args).await,
                Err(error) => {
                    return error_response(
                        id,
                        rpc_code::INVALID_PARAMS,
                        format!("query 参数不合法: {error}"),
                        json!({"tool": name}),
                    );
                }
            },
            tools::TOOL_LIST_DATABASES => self.call_list_databases().await,
            tools::TOOL_LIST_TABLES => {
                match serde_json::from_value::<tools::ListTablesArgs>(arguments) {
                    Ok(args) => self.call_list_tables(args).await,
                    Err(error) => {
                        return error_response(
                            id,
                            rpc_code::INVALID_PARAMS,
                            format!("list_tables 参数不合法: {error}"),
                            json!({"tool": name}),
                        );
                    }
                }
            }
            other => {
                return error_response(
                    id,
                    rpc_code::INVALID_PARAMS,
                    format!("未知工具: {other}"),
                    json!({"tool": other, "known_tools": tools::TOOL_NAMES}),
                );
            }
        };
        match outcome {
            Ok(payload) => ok_response(id, success_result(payload)),
            Err(error) => ok_response(id, failure_result(&error)),
        }
    }
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

fn ok_response(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error_response(id: Value, code: i32, message: impl Into<String>, data: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message.into(), "data": data}
    })
}

/// 按行分隔 JSON-RPC 2.0 的协议循环：读到 EOF 即正常退出。
///
/// stdout 只写协议行；任何诊断都必须走调用方的 stderr，不能混进这里。
pub async fn serve<R, W>(mut reader: R, mut writer: W, server: Server) -> std::io::Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            return Ok(());
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(trimmed) {
            Ok(value) if value.is_object() => server.handle_request(&value).await,
            Ok(_) => Some(error_response(
                Value::Null,
                rpc_code::INVALID_REQUEST,
                "请求必须是 JSON 对象",
                json!({}),
            )),
            Err(error) => Some(error_response(
                Value::Null,
                rpc_code::PARSE_ERROR,
                format!("请求不是合法 JSON: {error}"),
                json!({}),
            )),
        };
        if let Some(response) = response {
            write_response(&mut writer, &response).await?;
        }
    }
}

async fn write_response<W: AsyncWrite + Unpin>(
    writer: &mut W,
    response: &Value,
) -> std::io::Result<()> {
    let mut encoded = serde_json::to_string(response).map_err(std::io::Error::other)?;
    encoded.push('\n');
    writer.write_all(encoded.as_bytes()).await?;
    writer.flush().await
}
