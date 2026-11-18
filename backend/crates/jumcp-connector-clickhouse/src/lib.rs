//! ClickHouse connector agent：进程内只做「连接 / 绑定 / 执行 / 取数」，治理、校验、整形、
//! 预算与审计全部留在 host。协议是 stdin/stdout 上按行分隔的 JSON-RPC 2.0。
//!
//! 边界：
//! - 不接收任意连接串：DSN 只从进程环境 `JUMCP_CLICKHOUSE_DSN` 读取，永不回显。
//! - 不接受写语义：每条查询都带 `readonly=1`，并设置超时与结果块上限。
//! - 不打印 SQL、参数值或密码：stderr 只允许无敏感信息的进程级消息。
//! - 错误用 JSON-RPC error + 稳定的 `data.code`（`MCP_*`），网络/超时标记 `outcome=unknown`。

use clickhouse::Client;
use jumcp_connector_protocol::{
    ColumnDef, InitializeResult, METHOD_INITIALIZE, METHOD_QUERY, METHOD_SHUTDOWN,
    PROTOCOL_VERSION, QueryRequest, QueryResult, mcp_code, rpc_code,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use url::Url;

/// agent 侧错误：`rpc_code` 决定 JSON-RPC 数字码，`mcp_code` 保持 host 语义。
#[derive(Debug, Clone)]
pub struct AgentError {
    pub rpc_code: i32,
    pub mcp_code: &'static str,
    pub message: String,
    pub detail: Value,
    pub outcome_unknown: bool,
}

impl AgentError {
    pub fn db(message: String) -> Self {
        Self {
            rpc_code: rpc_code::DB_ERROR,
            mcp_code: mcp_code::STORE_ERROR,
            message,
            detail: json!({"code": mcp_code::STORE_ERROR}),
            outcome_unknown: false,
        }
    }

    pub fn timeout(message: String) -> Self {
        Self {
            rpc_code: rpc_code::EXEC_TIMEOUT,
            mcp_code: mcp_code::EXEC_TIMEOUT,
            message,
            detail: json!({"code": mcp_code::EXEC_TIMEOUT, "outcome": "unknown"}),
            outcome_unknown: true,
        }
    }

    pub fn config(message: String) -> Self {
        Self {
            rpc_code: rpc_code::CONFIG_ERROR,
            mcp_code: mcp_code::CONFIG_ERROR,
            message,
            detail: json!({"code": mcp_code::CONFIG_ERROR}),
            outcome_unknown: false,
        }
    }

    pub fn invalid_params(message: String) -> Self {
        Self {
            rpc_code: rpc_code::INVALID_PARAMS,
            mcp_code: mcp_code::VALIDATION_ERROR,
            message,
            detail: json!({"code": mcp_code::VALIDATION_ERROR}),
            outcome_unknown: false,
        }
    }

    /// 序列化为 JSON-RPC error 对象（`data` 内含稳定的 `code` 与可选 `outcome`）。
    pub fn to_jsonrpc(&self) -> Value {
        json!({
            "code": self.rpc_code,
            "message": self.message,
            "data": self.detail,
        })
    }
}

/// DSN 拆解结果。`password` 只用于构造 HTTP 认证头，不参与任何日志/错误文案。
#[derive(Debug, Clone)]
pub struct Dsn {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub database: Option<String>,
}

/// ClickHouse HTTP 端点的网络部分（不含凭据与库名）。
///
/// 无端口时按 ClickHouse 惯例取 http=8123 / https=8443；不接受 URL 内嵌 userinfo，
/// 凭据只经独立字段传入，避免密码进入会被日志记录的 URL。
#[derive(Debug, Clone)]
pub struct Endpoint {
    pub scheme: String,
    pub host: String,
    pub port: u16,
}

impl Endpoint {
    pub fn parse(raw: &str) -> Result<Self, String> {
        let url = Url::parse(raw).map_err(|_| "URL 不是合法地址".to_string())?;
        let scheme = url.scheme().to_ascii_lowercase();
        if scheme != "http" && scheme != "https" {
            return Err("URL 必须使用 http:// 或 https://".to_string());
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err("URL 不能包含用户名或密码；凭据请填在独立字段".to_string());
        }
        let host = url
            .host_str()
            .filter(|h| !h.is_empty())
            .ok_or_else(|| "URL 缺少主机".to_string())?
            .to_string();
        let port = url
            .port()
            .unwrap_or(if scheme == "https" { 8443 } else { 8123 });
        Ok(Self { scheme, host, port })
    }

    /// 不含 userinfo 的 `scheme://host:port`。
    pub fn base(&self) -> String {
        format!(
            "{}://{}:{}",
            self.scheme,
            host_for_url(&self.host),
            self.port
        )
    }
}

impl Dsn {
    /// 按已拆解的部分直接构造（不拼回字符串再解析）。
    pub fn from_parts(
        endpoint: &Endpoint,
        user: &str,
        password: &str,
        database: Option<&str>,
    ) -> Self {
        Self {
            scheme: endpoint.scheme.clone(),
            host: endpoint.host.clone(),
            port: endpoint.port,
            user: user.to_string(),
            password: password.to_string(),
            database: database.filter(|db| !db.is_empty()).map(str::to_string),
        }
    }

    /// 解析 `http://user:pass@host:port[/database]`。
    ///
    /// 错误信息**不含**原始 DSN：解析失败时不能把密码带进日志或错误响应。
    pub fn parse(raw: &str) -> Result<Self, String> {
        let url = Url::parse(raw).map_err(|_| "DSN 不是合法 URL".to_string())?;
        let scheme = url.scheme().to_ascii_lowercase();
        if scheme != "http" && scheme != "https" {
            return Err("DSN 必须使用 http:// 或 https://".to_string());
        }
        let host = url
            .host_str()
            .filter(|h| !h.is_empty())
            .ok_or_else(|| "DSN 缺少主机".to_string())?
            .to_string();
        let port = url
            .port_or_known_default()
            .ok_or_else(|| "DSN 缺少端口".to_string())?;
        let user = decode(url.username());
        if user.is_empty() {
            return Err("DSN 缺少用户名".to_string());
        }
        let password = url.password().map(decode).unwrap_or_default();
        let path = url.path().trim_start_matches('/');
        let database = if path.is_empty() {
            None
        } else if path.contains('/') {
            return Err("DSN 路径只能是单个 database 名".to_string());
        } else {
            Some(decode(path))
        };
        Ok(Self {
            scheme,
            host,
            port,
            user,
            password,
            database,
        })
    }

    /// 不含 userinfo 的 HTTP endpoint：凭据走请求头，不放进 URL。
    pub fn endpoint(&self) -> String {
        format!(
            "{}://{}:{}",
            self.scheme,
            host_for_url(&self.host),
            self.port
        )
    }
}

fn host_for_url(host: &str) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{}]", host)
    } else {
        host.to_string()
    }
}

fn decode(value: &str) -> String {
    percent_encoding::percent_decode_str(value)
        .decode_utf8_lossy()
        .into_owned()
}

#[derive(Debug, Deserialize)]
struct JsonCompact {
    #[serde(default)]
    meta: Vec<ColumnDef>,
    #[serde(default)]
    data: Vec<Vec<Value>>,
    /// `default_format=JSON*` 时 ClickHouse 把错误也放在 200 响应体里，必须显式识别，
    /// 否则 DDL/权限错误会被当成「0 行成功」静默放过。
    #[serde(default)]
    exception: Option<String>,
}

/// 已建立的 ClickHouse 客户端。构造只做 DSN 解析，真正的连接在 `initialize` 时验证。
pub struct ClickHouseAgent {
    client: Client,
    default_database: String,
}

impl ClickHouseAgent {
    pub fn from_dsn(dsn: &str) -> Result<Self, AgentError> {
        let dsn = Dsn::parse(dsn).map_err(AgentError::config)?;
        Self::from_parts(&dsn)
    }

    /// 从已拆解的 DSN 构造；配置已由管理面按字段校验，这里不再回拼字符串。
    pub fn from_parts(dsn: &Dsn) -> Result<Self, AgentError> {
        let default_database = dsn
            .database
            .clone()
            .unwrap_or_else(|| "default".to_string());
        let mut client = Client::default()
            .with_url(dsn.endpoint())
            .with_user(dsn.user.clone());
        if !dsn.password.is_empty() {
            client = client.with_password(dsn.password.clone());
        }
        if let Some(database) = &dsn.database {
            client = client.with_database(database);
        }
        Ok(Self {
            client,
            default_database,
        })
    }

    /// 握手/健康检查：真连一次 ClickHouse，拿到版本与当前库；失败即拒绝。
    pub async fn initialize(&self) -> Result<InitializeResult, AgentError> {
        let result = self
            .run("SELECT version(), currentDatabase()", &[], 1, None, 10_000)
            .await?;
        let server_version = result
            .rows
            .first()
            .and_then(|row| row.first())
            .and_then(Value::as_str)
            .ok_or_else(|| AgentError::db("initialize 未返回服务端版本".to_string()))?
            .to_string();
        let database = result
            .rows
            .first()
            .and_then(|row| row.get(1))
            .and_then(Value::as_str)
            .unwrap_or(&self.default_database)
            .to_string();
        Ok(InitializeResult {
            protocol_version: PROTOCOL_VERSION,
            agent_version: env!("CARGO_PKG_VERSION").to_string(),
            server_version,
            database,
        })
    }

    /// 执行一条只读查询。`sql` 中的 `?` 由 clickhouse crate 按 `params` 顺序绑定。
    pub async fn query(&self, req: &QueryRequest) -> Result<QueryResult, AgentError> {
        if req.sql.trim().is_empty() {
            return Err(AgentError::invalid_params("sql 不能为空".to_string()));
        }
        let max_rows = req.max_rows.clamp(1, 1000);
        let timeout_ms = if req.timeout_ms > 0 {
            req.timeout_ms.min(300_000)
        } else {
            15_000
        };
        if let Some(database) = &req.database
            && database.trim().is_empty()
        {
            return Err(AgentError::invalid_params(
                "database 不能为空字符串".to_string(),
            ));
        }
        self.run(
            &req.sql,
            &req.params,
            max_rows,
            req.database.as_deref(),
            timeout_ms,
        )
        .await
    }

    async fn run(
        &self,
        sql: &str,
        params: &[Value],
        max_rows: i64,
        database: Option<&str>,
        timeout_ms: i64,
    ) -> Result<QueryResult, AgentError> {
        let mut client = self.client.clone();
        if let Some(database) = database {
            client = client.with_database(database);
        }

        let mut query = client.query(sql);
        for value in params {
            query = bind_value(query, value)?;
        }
        // 只读与防爆设置都放在同一个请求的 URL 参数里（服务端在解析该请求时生效）；
        // max_block_size + max_result_rows 一起用，才能把 "break" 的溢出限制在约一个块内。
        let cap = max_rows.saturating_add(1).min(65_536);
        let query = query
            .with_setting("readonly", "1")
            .with_setting(
                "max_execution_time",
                format!("{:.3}", timeout_ms as f64 / 1000.0),
            )
            .with_setting("max_result_rows", cap.to_string())
            .with_setting("result_overflow_mode", "break")
            .with_setting("max_block_size", cap.to_string())
            .with_setting("max_threads", "1");

        let started = Instant::now();
        let mut cursor = query.fetch_bytes("JSONCompact").map_err(classify)?;
        // 客户端超时是权威边界：max_execution_time 是块级软限制，函数内阻塞时不会及时中断。
        let bytes = tokio::time::timeout(
            Duration::from_millis(timeout_ms as u64 + 1_000),
            cursor.collect(),
        )
        .await
        .map_err(|_| AgentError::timeout(format!("ClickHouse 查询超过 {timeout_ms}ms")))?
        .map_err(classify)?;

        let raw: JsonCompact = serde_json::from_slice(&bytes).map_err(|e| {
            AgentError::db(format!(
                "结果不是 JSONCompact（SQL 里可能自带 FORMAT）：{e}"
            ))
        })?;
        if let Some(exception) = raw.exception {
            return Err(classify_message(&exception));
        }
        let mut rows = raw.data;
        let truncated = rows.len() > max_rows as usize;
        rows.truncate(max_rows as usize);
        Ok(QueryResult {
            columns: raw.meta,
            row_count: rows.len() as i64,
            rows,
            truncated,
            elapsed_ms: started.elapsed().as_millis() as i64,
        })
    }
}

fn bind_value(
    query: clickhouse::query::Query,
    value: &Value,
) -> Result<clickhouse::query::Query, AgentError> {
    Ok(match value {
        Value::Null => query.bind(Option::<String>::None),
        Value::Bool(v) => query.bind(*v),
        Value::Number(n) => {
            if let Some(v) = n.as_i64() {
                query.bind(v)
            } else if let Some(v) = n.as_u64() {
                query.bind(v)
            } else if let Some(v) = n.as_f64() {
                query.bind(v)
            } else {
                return Err(AgentError::invalid_params(format!("不支持的数值参数: {n}")));
            }
        }
        Value::String(s) => query.bind(s.as_str()),
        Value::Array(_) | Value::Object(_) => {
            return Err(AgentError {
                rpc_code: rpc_code::INVALID_PARAMS,
                mcp_code: mcp_code::PARAM_STYLE_UNSUPPORTED,
                message: "位置参数只支持标量（bool/number/string/null）".to_string(),
                detail: json!({
                    "code": mcp_code::PARAM_STYLE_UNSUPPORTED,
                    "hint": "数组/对象参数需要显式类型与展开，当前不支持"
                }),
                outcome_unknown: false,
            });
        }
    })
}

/// ClickHouse 异常文本 → 稳定错误码。`Code: N` 是服务端异常码，159 表示超时。
fn classify(err: clickhouse::error::Error) -> AgentError {
    match &err {
        clickhouse::error::Error::TimedOut => {
            AgentError::timeout("ClickHouse 客户端超时".to_string())
        }
        clickhouse::error::Error::BadResponse(message) => classify_message(message),
        other => AgentError::db(format!("ClickHouse 请求失败: {other}")),
    }
}

fn classify_message(message: &str) -> AgentError {
    if message.contains("Timeout exceeded") || message.contains("TIMEOUT_EXCEEDED") {
        return AgentError::timeout(message.to_string());
    }
    if message.contains("READONLY") {
        let mut error = AgentError::db(message.to_string());
        error.detail = json!({"code": mcp_code::STORE_ERROR, "readonly": true});
        return error;
    }
    AgentError::db(message.to_string())
}

/// 运行按行 JSON-RPC 循环。stdin 只承载协议帧，stdout 只承载响应；stderr 留给进程级诊断。
pub async fn serve<R, W>(
    mut reader: R,
    mut writer: W,
    agent: ClickHouseAgent,
) -> std::io::Result<()>
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
        let request: Value = match serde_json::from_str(trimmed) {
            Ok(value) => value,
            Err(e) => {
                write_response(
                    &mut writer,
                    &json!({
                        "jsonrpc": "2.0",
                        "id": Value::Null,
                        "error": {
                            "code": rpc_code::PARSE_ERROR,
                            "message": format!("请求不是合法 JSON: {e}"),
                            "data": {"code": mcp_code::VALIDATION_ERROR}
                        }
                    }),
                )
                .await?;
                continue;
            }
        };
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        let params = request.get("params").cloned().unwrap_or_else(|| json!({}));

        let response = match method {
            METHOD_INITIALIZE => match agent.initialize().await {
                Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                Err(e) => error_response(id, &e),
            },
            METHOD_QUERY => match serde_json::from_value::<QueryRequest>(params) {
                Ok(req) => match agent.query(&req).await {
                    Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                    Err(e) => error_response(id, &e),
                },
                Err(e) => error_response(
                    id,
                    &AgentError::invalid_params(format!("query 入参不合法: {e}")),
                ),
            },
            METHOD_SHUTDOWN => {
                let response = json!({"jsonrpc": "2.0", "id": id, "result": {"ok": true}});
                write_response(&mut writer, &response).await?;
                return Ok(());
            }
            "" => error_response(id, &AgentError::invalid_params("缺少 method".to_string())),
            other => {
                let mut e = AgentError::invalid_params(format!("未知方法: {other}"));
                e.rpc_code = rpc_code::METHOD_NOT_FOUND;
                error_response(id, &e)
            }
        };
        write_response(&mut writer, &response).await?;
    }
}

fn error_response(id: Value, error: &AgentError) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": error.to_jsonrpc()})
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dsn_parses_credentials_and_database() {
        let dsn = Dsn::parse("http://user:p%40ss@ch.example:18123/analytics").unwrap();
        assert_eq!(dsn.endpoint(), "http://ch.example:18123");
        assert_eq!(dsn.user, "user");
        assert_eq!(dsn.password, "p@ss");
        assert_eq!(dsn.database.as_deref(), Some("analytics"));
    }

    #[test]
    fn dsn_errors_never_echo_credentials() {
        let e = Dsn::parse("postgres://user:secret@host:5432/db").unwrap_err();
        assert!(!e.contains("secret"), "错误文案不得包含密码: {e}");
        let e = Dsn::parse("http://user:secret@host:1/db/extra").unwrap_err();
        assert!(!e.contains("secret"), "错误文案不得包含密码: {e}");
    }

    #[test]
    fn dsn_requires_user_and_host() {
        assert!(Dsn::parse("http://host:8123").is_err());
        assert!(Dsn::parse("http://user:pw@:8123").is_err());
    }

    #[test]
    fn ipv6_endpoint_is_bracketed() {
        let dsn = Dsn::parse("http://u:p@[::1]:8123").unwrap();
        assert_eq!(dsn.endpoint(), "http://[::1]:8123");
    }

    #[test]
    fn error_payload_carries_stable_code_and_outcome() {
        let timeout = AgentError::timeout("x".into()).to_jsonrpc();
        assert_eq!(timeout["code"], json!(rpc_code::EXEC_TIMEOUT));
        assert_eq!(timeout["data"]["code"], json!(mcp_code::EXEC_TIMEOUT));
        assert_eq!(timeout["data"]["outcome"], json!("unknown"));

        let invalid = AgentError::invalid_params("x".into()).to_jsonrpc();
        assert_eq!(invalid["code"], json!(rpc_code::INVALID_PARAMS));
        assert_eq!(invalid["data"]["code"], json!(mcp_code::VALIDATION_ERROR));
    }
}
