//! stdio 传输：直接 spawn 上游进程，剔除治理凭据环境变量，按行读写 JSON-RPC。

use super::UpstreamFailure;
use super::client::{next_request_id, rpc_result};
use jumcp_core::registry::McpUpstream;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex as AsyncMutex;

pub(super) struct StdioTransport {
    io: AsyncMutex<StdioIo>,
    upstream_id: String,
    /// 进程句柄只用于生命周期：`kill_on_drop(true)` 保证随连接池回收一起退出。
    _child: Child,
    timeout: Duration,
}

struct StdioIo {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl StdioTransport {
    pub(super) async fn spawn(
        def: &McpUpstream,
        env: BTreeMap<String, String>,
    ) -> Result<Self, UpstreamFailure> {
        let mut command = Command::new(&def.command);
        command
            .args(&def.args)
            .envs(&env)
            // 上游进程不得继承本服务的治理凭据：谁拉起进程，谁负责收窄环境。
            .env_remove("JUMCP_ADMIN_TOKENS")
            .env_remove("JUMCP_AGENT_TOKENS")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            // stderr 丢弃：上游可能把 env 里的凭据打到 stderr，status_message 会进 GET 响应。
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|e| {
            UpstreamFailure::error(
                &def.id,
                "MCP_UPSTREAM_UNREACHABLE",
                format!("stdio 上游启动失败: {e}"),
            )
        })?;
        let stdin = child.stdin.take().ok_or_else(|| {
            UpstreamFailure::error(&def.id, "MCP_UPSTREAM_ERROR", "stdio 上游缺少 stdin")
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            UpstreamFailure::error(&def.id, "MCP_UPSTREAM_ERROR", "stdio 上游缺少 stdout")
        })?;
        Ok(Self {
            io: AsyncMutex::new(StdioIo {
                stdin,
                stdout: BufReader::new(stdout),
            }),
            upstream_id: def.id.clone(),
            _child: child,
            timeout: Duration::from_millis(def.timeout_ms as u64),
        })
    }

    pub(super) async fn notify(&self, method: &str, params: Value) -> Result<(), UpstreamFailure> {
        let line = serde_json::to_string(&json!({"jsonrpc":"2.0","method":method,"params":params}))
            .map_err(|e| {
                UpstreamFailure::error(
                    &self.upstream_id,
                    "MCP_UPSTREAM_ERROR",
                    format!("请求序列化失败: {e}"),
                )
            })?;
        let mut io = self.io.lock().await;
        io.stdin
            .write_all(format!("{line}\n").as_bytes())
            .await
            .map_err(|e| self.io_failure(&e))?;
        io.stdin.flush().await.map_err(|e| self.io_failure(&e))
    }

    pub(super) async fn request(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Value, UpstreamFailure> {
        let id = next_request_id();
        let line = serde_json::to_string(
            &json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
        )
        .map_err(|e| {
            UpstreamFailure::error(
                &self.upstream_id,
                "MCP_UPSTREAM_ERROR",
                format!("请求序列化失败: {e}"),
            )
        })?;
        let mut io = self.io.lock().await;
        io.stdin
            .write_all(format!("{line}\n").as_bytes())
            .await
            .map_err(|e| self.io_failure(&e))?;
        io.stdin.flush().await.map_err(|e| self.io_failure(&e))?;
        let mut line = String::new();
        loop {
            line.clear();
            let read = tokio::time::timeout(self.timeout, io.stdout.read_line(&mut line)).await;
            let read = match read {
                Ok(result) => result.map_err(|e| self.io_failure(&e))?,
                Err(_) => {
                    return Err(UpstreamFailure::unknown(
                        &self.upstream_id,
                        "MCP_EXEC_TIMEOUT",
                        format!("stdio 上游 {method} 超时"),
                    ));
                }
            };
            if read == 0 {
                return Err(UpstreamFailure::error(
                    &self.upstream_id,
                    "MCP_UPSTREAM_UNREACHABLE",
                    "stdio 上游已退出（stdout EOF）",
                ));
            }
            let Ok(envelope) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            if envelope.get("id").and_then(Value::as_i64) != Some(id) {
                // 上游主动推送的通知/其他响应：router 不消费通知。
                continue;
            }
            return rpc_result(&self.upstream_id, method, envelope);
        }
    }

    fn io_failure(&self, error: &std::io::Error) -> UpstreamFailure {
        UpstreamFailure::error(
            &self.upstream_id,
            "MCP_UPSTREAM_UNREACHABLE",
            format!("stdio 上游 IO 失败: {error}"),
        )
    }
}
