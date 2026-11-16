//! 连接器桥阶层：把只读执行委托给独立子进程 `jumcp-connector-clickhouse`。
//!
//! 分工边界（与计划 §4.3.1 对齐）：
//! - host 负责 L4（`jumcp_sqlguard`，ClickHouse 方言）、database 白名单、行数夹紧、单格整形、
//!   token 预算、`sql_hash` 与审计；本模块只把已校验的 SQL 交给 agent。
//! - agent 只负责连接、参数绑定、执行与取数；它不理解治理语义。
//! - 传输是 stdin/stdout 上按行分隔的 JSON-RPC 2.0。agent 使用独立管道，
//!   即使 host 自身走 stdio 传输也不会与 MCP 帧混淆。
//! - 超时/断链 = 结果未知：杀死进程、标记 `outcome=unknown`，**不重试**同一请求。
//!   下一次调用会重新拉起 agent（无跨调用会话状态）。

use crate::executor::{
    ExecFuture, QueryExecutor, QueryParams, check_placeholders, check_scalar_params, validate_sql,
};
use jumcp_connector_protocol::{
    InitializeResult, METHOD_INITIALIZE, METHOD_QUERY, PROTOCOL_VERSION, QueryRequest, QueryResult,
};
use jumcp_core::budget::estimate_tokens;
use jumcp_core::entry::EffectiveScope;
use jumcp_core::errors::McpError;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::time::timeout;

/// agent 握手与进程管理的配置。`dsn` 只从部署环境读出，永不落日志/配置。
#[derive(Debug, Clone)]
pub struct ConnectorConfig {
    pub datasource_name: String,
    pub dsn: String,
    pub statement_timeout_ms: i64,
    pub allowed_schemas: Vec<String>,
}

impl ConnectorConfig {
    /// 启动期纯校验：只检查非空/超时/白名单形态；DSN 语义与默认库由 agent 握手返回，
    /// host 不做数据库方言解析（也绝不回显 DSN 内容）。
    fn validate(&self) -> Result<(), McpError> {
        if self.datasource_name.trim().is_empty() {
            return Err(McpError::config("数据源 name 不能为空".to_string()));
        }
        if self.dsn.trim().is_empty() {
            return Err(McpError::config(format!(
                "数据源 {} 的 DSN 为空",
                self.datasource_name
            )));
        }
        if self.statement_timeout_ms <= 0 {
            return Err(McpError::config(format!(
                "数据源 {} 的 statement_timeout_ms 必须为正",
                self.datasource_name
            )));
        }
        if self
            .allowed_schemas
            .iter()
            .any(|schema| schema.trim().is_empty())
        {
            return Err(McpError::config(format!(
                "数据源 {} 的 allowed_schemas 不能包含空字符串",
                self.datasource_name
            )));
        }
        Ok(())
    }
}

/// 默认库必须落在连接声明的白名单内；默认库来自 `initialize`，不再由 host 解析 DSN 得到。
fn ensure_database_allowed(
    allowed_schemas: &[String],
    database: &str,
    datasource: &str,
) -> Result<(), McpError> {
    if !allowed_schemas.is_empty() && !allowed_schemas.iter().any(|schema| schema == database) {
        return Err(McpError::config(format!(
            "数据源 {datasource} 的默认库 {database} 不在 allowed_schemas 白名单内: {}",
            allowed_schemas.join(",")
        )));
    }
    Ok(())
}

/// 已运行的 agent 子进程与它的管道/诊断尾。
struct ConnectorAgent {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    next_id: u64,
}

/// 一次请求失败：`fatal` 表示进程/管道已不可继续使用，必须杀进程等下一次重拉。
///
/// `error` 用 `Box` 装箱：`McpError` 内含 `serde_json::Value`（`preserve_order` 下 Value 较大），
/// 直接内联会让 `Result<_, AgentFailure>` 的 Err 变体超过 clippy `result_large_err` 阈值。
struct AgentFailure {
    error: Box<McpError>,
    fatal: bool,
}

impl ConnectorAgent {
    fn spawn(dsn: &str, binary: &Path) -> Result<Self, McpError> {
        let mut command = Command::new(binary);
        command
            .env("JUMCP_CLICKHOUSE_DSN", dsn)
            // 子进程不需要 host 的管理面/MCP 凭证；显式剔除，避免环境泄漏。
            .env_remove("JUMCP_ADMIN_TOKENS")
            .env_remove("JUMCP_AGENT_TOKENS")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|e| {
            McpError::config(format!(
                "启动 connector agent 失败（{}）: {e}",
                binary.display()
            ))
        })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::store("connector agent stdin 不可用".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::store("connector agent stdout 不可用".to_string()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| McpError::store("connector agent stderr 不可用".to_string()))?;
        let stderr_tail = Arc::new(Mutex::new(VecDeque::with_capacity(32)));
        let tail = stderr_tail.clone();
        // agent 不打印 SQL/参数/密码；这里再脱敏一次 URL 形式的凭据并保留有限行数排障。
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let redacted = redact(&line);
                if let Ok(mut tail) = tail.lock() {
                    if tail.len() == 32 {
                        tail.pop_front();
                    }
                    tail.push_back(redacted);
                }
            }
        });
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            stderr_tail,
            next_id: 1,
        })
    }

    fn kill(&mut self) {
        let _ = self.child.start_kill();
    }

    async fn request(
        &mut self,
        method: &str,
        params: Value,
        deadline: Duration,
    ) -> Result<Value, AgentFailure> {
        let id = self.next_id;
        self.next_id += 1;
        let mut encoded = serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .map_err(|e| AgentFailure {
            error: Box::new(McpError::store(format!("编码 agent 请求失败: {e}"))),
            fatal: false,
        })?;
        encoded.push('\n');

        let response_line = timeout(deadline, async {
            self.stdin.write_all(encoded.as_bytes()).await?;
            self.stdin.flush().await?;
            let mut line = String::new();
            if self.stdout.read_line(&mut line).await? == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "agent stdout 已关闭",
                ));
            }
            Ok::<String, std::io::Error>(line)
        })
        .await;

        let line = match response_line {
            Err(_) => {
                return Err(AgentFailure {
                    error: Box::new(McpError::timeout(format!(
                        "connector agent 请求 {method} 超时（{}ms）",
                        deadline.as_millis()
                    ))),
                    fatal: true,
                });
            }
            Ok(Err(e)) => {
                return Err(AgentFailure {
                    error: Box::new(abnormal(format!("connector agent 通信失败: {e}"))),
                    fatal: true,
                });
            }
            Ok(Ok(line)) => line,
        };

        let response: Value = serde_json::from_str(line.trim()).map_err(|e| AgentFailure {
            error: Box::new(abnormal(format!("connector agent 返回非法 JSON: {e}"))),
            fatal: true,
        })?;
        if response.get("id").and_then(Value::as_u64) != Some(id) {
            return Err(AgentFailure {
                error: Box::new(abnormal("connector agent 响应 id 不匹配".to_string())),
                fatal: true,
            });
        }
        if let Some(error) = response.get("error") {
            return Err(AgentFailure {
                error: Box::new(map_rpc_error(error)),
                // JSON-RPC error 是 agent 明确给出的失败，进程仍可继续服务。
                fatal: false,
            });
        }
        response.get("result").cloned().ok_or_else(|| AgentFailure {
            error: Box::new(abnormal("connector agent 响应缺少 result".to_string())),
            fatal: true,
        })
    }
}

fn abnormal(message: String) -> McpError {
    McpError::with_message(
        "MCP_STORE_ERROR",
        message.clone(),
        json!({"error": message, "outcome": "unknown"}),
    )
}

fn map_rpc_error(error: &Value) -> McpError {
    let rpc_code = error.get("code").and_then(Value::as_i64).unwrap_or(-32603);
    let data = error.get("data").cloned().unwrap_or(Value::Null);
    let mcp_code = data
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or("MCP_STORE_ERROR");
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("connector agent error");
    let mut detail = json!({
        "connector": "clickhouse",
        "rpc_code": rpc_code,
        "detail": data,
    });
    if data.get("outcome").and_then(Value::as_str) == Some("unknown") {
        detail["outcome"] = json!("unknown");
    }
    McpError::with_message(mcp_code, format!("connector agent: {message}"), detail)
}

/// 输出脱敏：把 URL authority 里的 `user:password@` 变成 `***@`，并限制单行长度。
fn redact(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(index) = rest.find("://") {
        let (head, tail) = rest.split_at(index + 3);
        out.push_str(head);
        if let Some(at) = tail.find('@') {
            let authority = &tail[..at];
            if authority.contains(':') {
                out.push_str("***");
            } else {
                out.push_str(authority);
            }
            out.push('@');
            rest = &tail[at + 1..];
        } else {
            out.push_str(tail);
            rest = "";
        }
    }
    out.push_str(rest);
    out.chars().take(500).collect()
}

/// 生产执行器：拉起 agent、握手、把校验后的请求转发过去并整形结果。
pub struct ConnectorExecutor {
    config: ConnectorConfig,
    default_database: Option<String>,
    agent: tokio::sync::Mutex<Option<ConnectorAgent>>,
}

impl ConnectorExecutor {
    /// 启动即拉起 agent 并完成 `initialize` 握手；默认库取自握手回执，不在 host 解析 DSN。
    pub async fn connect(config: ConnectorConfig) -> Result<Self, McpError> {
        config.validate()?;
        let mut executor = Self {
            default_database: None,
            config,
            agent: tokio::sync::Mutex::new(None),
        };
        let (agent, init) = executor.spawn_and_initialize().await?;
        executor.default_database = Some(init.database);
        *executor.agent.lock().await = Some(agent);
        Ok(executor)
    }

    async fn spawn_and_initialize(&self) -> Result<(ConnectorAgent, InitializeResult), McpError> {
        let binary = resolve_agent_bin();
        let mut agent = ConnectorAgent::spawn(&self.config.dsn, &binary)?;
        let result = agent
            .request(
                METHOD_INITIALIZE,
                json!({"protocol_version": PROTOCOL_VERSION}),
                Duration::from_secs(10),
            )
            .await;
        match result {
            Ok(value) => {
                let init: InitializeResult = serde_json::from_value(value).map_err(|e| {
                    agent.kill();
                    McpError::config(format!("connector agent 握手回执非法: {e}"))
                })?;
                if init.protocol_version != PROTOCOL_VERSION {
                    agent.kill();
                    return Err(McpError::config(format!(
                        "connector agent 协议版本不匹配: host={PROTOCOL_VERSION} agent={}",
                        init.protocol_version
                    )));
                }
                if let Err(error) = ensure_database_allowed(
                    &self.config.allowed_schemas,
                    &init.database,
                    &self.config.datasource_name,
                ) {
                    agent.kill();
                    return Err(error);
                }
                tracing::info!(
                    datasource = %self.config.datasource_name,
                    server_version = %init.server_version,
                    database = %init.database,
                    "connector agent 已就绪"
                );
                Ok((agent, init))
            }
            Err(failure) => {
                let error = with_tail(*failure.error, &agent.stderr_tail);
                agent.kill();
                Err(error)
            }
        }
    }

    /// 串行化 agent 请求（单进程单管道）。断链/超时后杀死进程，下一次调用重新拉起。
    async fn call(
        &self,
        method: &str,
        params: Value,
        deadline: Duration,
    ) -> Result<Value, McpError> {
        let mut guard = self.agent.lock().await;
        if guard.is_none() {
            let (mut agent, init) = self.spawn_and_initialize().await?;
            if let Some(expected) = &self.default_database
                && &init.database != expected
            {
                agent.kill();
                return Err(McpError::config(format!(
                    "connector agent 重启后默认库变化: 期望 {expected}，实际 {}",
                    init.database
                )));
            }
            *guard = Some(agent);
        }
        let result = {
            let agent = guard.as_mut().expect("agent 已初始化");
            agent.request(method, params, deadline).await
        };
        match result {
            Ok(value) => Ok(value),
            Err(failure) => {
                let error = match guard.as_ref() {
                    Some(agent) => with_tail(*failure.error, &agent.stderr_tail),
                    None => *failure.error,
                };
                if failure.fatal
                    && let Some(mut agent) = guard.take()
                {
                    agent.kill();
                }
                Err(error)
            }
        }
    }

    /// 执行前校验：L4 + 占位符个数 + 标量参数 + database 白名单（含默认库）。
    fn check(&self, q: &QueryParams) -> Result<(), McpError> {
        validate_sql(q)?;
        check_placeholders(&q.sql, q.params.len())?;
        check_scalar_params(&q.params)?;
        crate::executor::check_database(q, self.default_database.as_deref())
    }
}

impl QueryExecutor for ConnectorExecutor {
    fn validate(&self, q: &QueryParams) -> Result<(), McpError> {
        self.check(q)
    }

    fn execute<'a>(&'a self, _scope: &'a EffectiveScope, q: &'a QueryParams) -> ExecFuture<'a> {
        Box::pin(async move {
            // 模板路径不经过 `validate`（注册期已验证），因此执行前仍要检查占位符、参数形状与 database 白名单。
            check_placeholders(&q.sql, q.params.len())?;
            check_scalar_params(&q.params)?;
            crate::executor::check_database(q, self.default_database.as_deref())?;

            let statement_timeout_ms = if q.statement_timeout_ms > 0 {
                q.statement_timeout_ms
            } else {
                self.config.statement_timeout_ms
            };
            let request = QueryRequest {
                sql: q.sql.clone(),
                params: q.params.clone(),
                max_rows: q.max_rows.clamp(1, 1000),
                database: q.database.clone().or_else(|| self.default_database.clone()),
                timeout_ms: statement_timeout_ms,
            };
            let started = Instant::now();
            let read_deadline = Duration::from_millis(
                statement_timeout_ms.saturating_add(5_000).min(310_000) as u64,
            );
            let value = self
                .call(
                    METHOD_QUERY,
                    serde_json::to_value(request)
                        .map_err(|e| McpError::store(format!("编码查询请求失败: {e}")))?,
                    read_deadline,
                )
                .await?;
            let reply: QueryResult = serde_json::from_value(value)
                .map_err(|e| McpError::store(format!("connector agent 查询回执非法: {e}")))?;

            let shaped = jumcp_sqlguard::shape_rows(
                &reply.rows,
                q.max_rows as usize,
                q.cell_char_limit as usize,
                q.cell_char_offset as usize,
            );
            let next_offsets: Vec<Value> = shaped
                .next_offsets
                .iter()
                .map(|(r, c, o)| json!({"row": r, "column": c, "next_offset": o}))
                .collect();
            let tokens = estimate_tokens(&json!({
                "columns": reply.columns,
                "rows": shaped.rows
            }));
            Ok(json!({
                "columns": reply.columns,
                "rows": shaped.rows,
                "row_count": shaped.row_count,
                "truncated": shaped.truncated || reply.truncated,
                "effective_max_rows": shaped.effective_max_rows,
                "next_offsets": next_offsets,
                "elapsed_ms": started.elapsed().as_millis() as i64,
                "tokens": tokens
            }))
        })
    }
}

fn with_tail(mut error: McpError, tail: &Arc<Mutex<VecDeque<String>>>) -> McpError {
    if let Ok(tail) = tail.lock()
        && !tail.is_empty()
    {
        error.data["stderr_tail"] = json!(tail.iter().collect::<Vec<_>>());
    }
    error
}

/// agent 二进制解析顺序：`JUMCP_CLICKHOUSE_AGENT_BIN` → 可执行文件同目录 →
/// `target/debug` 兄弟目录（`cargo test` 的 exe 位于 `deps/`）→ `PATH`。
fn resolve_agent_bin() -> PathBuf {
    if let Ok(value) = std::env::var("JUMCP_CLICKHOUSE_AGENT_BIN")
        && !value.trim().is_empty()
    {
        return PathBuf::from(value.trim());
    }
    let name = if cfg!(windows) {
        "jumcp-connector-clickhouse.exe"
    } else {
        "jumcp-connector-clickhouse"
    };
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sibling = dir.join(name);
        if sibling.is_file() {
            return sibling;
        }
        if let Some(parent) = dir.parent() {
            let sibling = parent.join(name);
            if sibling.is_file() {
                return sibling;
            }
        }
    }
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jumcp_connector_protocol::rpc_code;

    fn config(dsn: &str, allowed: &[&str]) -> ConnectorConfig {
        ConnectorConfig {
            datasource_name: "test-ch".into(),
            dsn: dsn.into(),
            statement_timeout_ms: 5_000,
            allowed_schemas: allowed.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn config_rejects_empty_dsn_and_bad_timeout() {
        let e = config("", &[]).validate().unwrap_err();
        assert_eq!(e.code, "MCP_CONFIG_ERROR");
        assert!(e.message.contains("test-ch"));
        assert!(e.message.contains("DSN"));

        let mut bad_timeout = config("http://u:p@h:18123", &[]);
        bad_timeout.statement_timeout_ms = 0;
        let e = bad_timeout.validate().unwrap_err();
        assert_eq!(e.code, "MCP_CONFIG_ERROR");
        assert!(e.message.contains("statement_timeout_ms"));

        let e = config("http://u:p@h:18123", &[""]).validate().unwrap_err();
        assert_eq!(e.code, "MCP_CONFIG_ERROR");
        assert!(e.message.contains("allowed_schemas"));
    }

    #[test]
    fn default_database_must_be_in_whitelist() {
        let env_air = vec!["env_air".to_string()];
        assert!(ensure_database_allowed(&[], "default", "test-ch").is_ok());
        assert!(ensure_database_allowed(&env_air, "env_air", "test-ch").is_ok());
        let e = ensure_database_allowed(&env_air, "default", "test-ch").unwrap_err();
        assert_eq!(e.code, "MCP_CONFIG_ERROR");
        assert!(e.message.contains("test-ch"));
        assert!(e.message.contains("default"));
        assert!(e.message.contains("env_air"));
    }

    #[test]
    fn redact_removes_url_credentials() {
        assert_eq!(
            redact("connect http://default:secret@192.0.2.1:18123 failed"),
            "connect http://***@192.0.2.1:18123 failed"
        );
        assert_eq!(redact("plain line"), "plain line");
    }

    #[test]
    fn map_rpc_error_preserves_unknown_outcome() {
        let error = map_rpc_error(&json!({
            "code": rpc_code::EXEC_TIMEOUT,
            "message": "timeout",
            "data": {"code": "MCP_EXEC_TIMEOUT", "outcome": "unknown"}
        }));
        assert_eq!(error.code, "MCP_EXEC_TIMEOUT");
        assert_eq!(error.data["outcome"], json!("unknown"));
        assert_eq!(error.data["connector"], json!("clickhouse"));
    }

    #[tokio::test]
    #[ignore = "需要真实 ClickHouse：JUMCP_TEST_CLICKHOUSE_DSN"]
    async fn connector_real_version_query() {
        let Ok(dsn) = std::env::var("JUMCP_TEST_CLICKHOUSE_DSN") else {
            return;
        };
        let executor = ConnectorExecutor::connect(config(&dsn, &[]))
            .await
            .expect("connect");
        let q = QueryParams::new("SELECT version()".into(), &[], 5_000, &json!({})).unwrap();
        executor.validate(&q).expect("validate");
        let result = executor
            .execute(&scope(), &q)
            .await
            .expect("execute version");
        assert_eq!(result["row_count"], json!(1));
        assert!(result["rows"][0][0].as_str().unwrap().contains("25."));
        assert_eq!(result["columns"][0]["name"], json!("version()"));
    }

    #[tokio::test]
    #[ignore = "需要真实 ClickHouse：JUMCP_TEST_CLICKHOUSE_DSN"]
    async fn connector_real_max_rows_truncates() {
        let Ok(dsn) = std::env::var("JUMCP_TEST_CLICKHOUSE_DSN") else {
            return;
        };
        let executor = ConnectorExecutor::connect(config(&dsn, &[]))
            .await
            .expect("connect");
        let q = QueryParams::new(
            "SELECT number FROM system.numbers LIMIT 10".into(),
            &[],
            5_000,
            &json!({"max_rows": 3}),
        )
        .unwrap();
        let result = executor.execute(&scope(), &q).await.expect("execute");
        assert_eq!(result["row_count"], json!(3));
        assert_eq!(result["truncated"], json!(true));
        assert_eq!(result["rows"].as_array().unwrap().len(), 3);
    }

    #[tokio::test]
    #[ignore = "需要真实 ClickHouse：JUMCP_TEST_CLICKHOUSE_DSN"]
    async fn connector_real_positional_binding() {
        let Ok(dsn) = std::env::var("JUMCP_TEST_CLICKHOUSE_DSN") else {
            return;
        };
        let executor = ConnectorExecutor::connect(config(&dsn, &[]))
            .await
            .expect("connect");
        let q =
            QueryParams::new("SELECT ? + 1".into(), &[], 5_000, &json!({"params": [41]})).unwrap();
        let result = executor.execute(&scope(), &q).await.expect("execute");
        assert_eq!(result["rows"][0][0], json!(42));
    }

    #[tokio::test]
    #[ignore = "需要真实 ClickHouse：JUMCP_TEST_CLICKHOUSE_DSN"]
    async fn connector_real_ddl_rejected_at_l4() {
        let Ok(dsn) = std::env::var("JUMCP_TEST_CLICKHOUSE_DSN") else {
            return;
        };
        let executor = ConnectorExecutor::connect(config(&dsn, &[]))
            .await
            .expect("connect");
        let q = QueryParams::new(
            "DROP TABLE IF EXISTS default.__jumcp_connector_probe".into(),
            &[],
            5_000,
            &json!({}),
        )
        .unwrap();
        let e = executor.validate(&q).unwrap_err();
        assert!(e.code == "MCP_SQL_REJECTED" || e.code == "MCP_MULTI_STATEMENT");
    }

    #[tokio::test]
    #[ignore = "需要真实 ClickHouse：JUMCP_TEST_CLICKHOUSE_DSN"]
    async fn connector_real_agent_readonly_rejects_ddl() {
        let Ok(dsn) = std::env::var("JUMCP_TEST_CLICKHOUSE_DSN") else {
            return;
        };
        let executor = ConnectorExecutor::connect(config(&dsn, &[]))
            .await
            .expect("connect");
        // 绕过 host L4 直发 agent：证明 agent 请求侧的 readonly=1 生效。
        // `SET` 是安全探针：即使只读未生效，也只影响本次请求的设置，不产生数据/结构副作用。
        let q = QueryParams::new("SET max_threads=2".into(), &[], 5_000, &json!({})).unwrap();
        let e = executor.execute(&scope(), &q).await.unwrap_err();
        assert_eq!(e.code, "MCP_STORE_ERROR");
        assert!(
            e.message.contains("READONLY") || e.message.contains("readonly"),
            "期望 ClickHouse 只读拒绝，实际: {}",
            e.message
        );
    }

    #[tokio::test]
    #[ignore = "需要真实 ClickHouse：JUMCP_TEST_CLICKHOUSE_DSN"]
    async fn connector_real_template_draft_review_publish_and_execute() {
        use jumcp_core::registry::CapabilityStore;
        use jumcp_core::template::TemplateRegistry;

        let Ok(dsn) = std::env::var("JUMCP_TEST_CLICKHOUSE_DSN") else {
            return;
        };
        let executor = ConnectorExecutor::connect(config(&dsn, &[]))
            .await
            .expect("connect");

        let store = Arc::new(CapabilityStore::open(":memory:", None).unwrap());
        let registry = TemplateRegistry::with_dialect(store, jumcp_sqlguard::Dialect::ClickHouse);
        let value = json!({
            "template_id": "ch.params_probe",
            "group": "db.read.core",
            "risk": "read",
            "description": "参数绑定探针",
            "sql": "SELECT :x + 1",
            "params": [{"name":"x","type":"int","required":true,"nullable":false}],
            "constraints": {"allowed_schemas": []},
            "discoverable": true,
            "callable": true
        });
        registry.register(&value).expect("register");
        registry
            .submit_for_review("ch.params_probe", "alice")
            .expect("review");
        registry
            .publish("ch.params_probe", "ap-1", "bob")
            .expect("publish");
        registry
            .set_visibility("ch.params_probe", Some(true), Some(true), "alice")
            .expect("visibility");

        let record = registry.current("ch.params_probe").expect("current");
        assert_eq!(record.compiled.dialect, jumcp_sqlguard::Dialect::ClickHouse);
        assert_eq!(record.compiled.base_sql, "SELECT ? + 1");
        let prepared = record.compiled.render(&json!({"x": 41})).expect("render");
        let q = QueryParams::from_parts(prepared.sql, prepared.values, 100, 200, 0, vec![], 5_000);
        let result = executor.execute(&scope(), &q).await.expect("execute");
        assert_eq!(result["rows"][0][0], json!(42));

        // `database=env_air`：用 DSN 默认库覆盖，确认模板在真实库上可执行。
        let dsn_env_air = format!("{dsn}/env_air");
        let cfg = ConnectorConfig {
            datasource_name: "test-ch-env-air".into(),
            dsn: dsn_env_air,
            statement_timeout_ms: 5_000,
            allowed_schemas: vec!["env_air".into()],
        };
        let executor = ConnectorExecutor::connect(cfg)
            .await
            .expect("connect env_air");
        let exists = executor
            .execute(
                &scope(),
                &QueryParams::new(
                    "SELECT count() FROM system.tables WHERE database = 'env_air' AND name = 'air_orig_h'"
                        .into(),
                    &[],
                    5_000,
                    &json!({}),
                )
                .unwrap(),
            )
            .await
            .expect("probe env_air");
        let present = exists["rows"][0][0]
            .as_str()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        if present == 0 {
            return;
        }

        let store = Arc::new(CapabilityStore::open(":memory:", None).unwrap());
        let registry = TemplateRegistry::with_dialect(store, jumcp_sqlguard::Dialect::ClickHouse);
        let value = json!({
            "template_id": "ch.env_air_count",
            "group": "db.read.core",
            "risk": "read",
            "description": "env_air 行数探针",
            "sql": "SELECT count() FROM air_orig_h",
            "params": [],
            "constraints": {"allowed_schemas": ["env_air"]},
            "discoverable": true,
            "callable": true
        });
        registry.register(&value).expect("register env_air");
        registry
            .submit_for_review("ch.env_air_count", "alice")
            .expect("review env_air");
        registry
            .publish("ch.env_air_count", "ap-2", "bob")
            .expect("publish env_air");
        let record = registry
            .current("ch.env_air_count")
            .expect("current env_air");
        let prepared = record.compiled.render(&json!({})).expect("render env_air");
        let q = QueryParams::from_parts(
            prepared.sql,
            prepared.values,
            10,
            200,
            0,
            vec!["env_air".into()],
            5_000,
        );
        let result = executor
            .execute(&scope(), &q)
            .await
            .expect("execute env_air");
        let count = result["rows"][0][0]
            .as_str()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        assert!(count > 0, "env_air.air_orig_h 应有数据，实际: {count}");
    }

    fn scope() -> EffectiveScope {
        EffectiveScope {
            groups: vec![],
            budget: 2_000,
            degraded: false,
            source: "test".into(),
            mode: "strict".into(),
            allowed_builtin_tools: None,
        }
    }
}
