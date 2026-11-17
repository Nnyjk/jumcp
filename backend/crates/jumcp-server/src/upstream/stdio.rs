//! stdio 传输：直接 spawn 上游进程，剔除治理凭据环境变量，按行读写 JSON-RPC。

use super::UpstreamFailure;
use super::client::{next_request_id, rpc_result};
use jumcp_core::registry::McpUpstream;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex as AsyncMutex;

/// 把命令名解析成本机真实存在的可执行文件；找不到返回 `None`。
///
/// 查找顺序固定，因为两层都承载着语义：
/// 1. `JUMCP_MCP_BUNDLED_BIN_DIR`——随包分发目录的覆盖口，只给开发/测试与非常规部署用；
///    未设置时生产路径完全不经过它。
/// 2. `current_exe()` 同目录——"连接器跟着服务一起发"，用户不必改 PATH。
/// 3. PATH——保持既有部署方式（用户自己装的二进制）不变。
///
/// 带路径分隔符的命令被视为用户明确指定路径，**必须是已存在的文件**：不做名字查找、
/// 不补 `.exe`、不按 PATH 解析，文件不存在就返回 `None`。目录页据此显示
/// `binary_present=false`、安装校验回 400 `MCP_UPSTREAM_BINARY_MISSING`；但 spawn 侧对
/// `None` 会退回原样命令交给系统处理，因此"这里解析失败"不等于"一定起不来"。
/// 目录页与安装校验共用本函数：两处判断分叉会让用户看到"可安装"却在安装时被拒。
pub(crate) fn bundled_binary(name: &str) -> Option<PathBuf> {
    let path = Path::new(name);
    if path.components().count() > 1 {
        return path.is_file().then(|| path.to_path_buf());
    }
    if let Some(dir) = std::env::var_os("JUMCP_MCP_BUNDLED_BIN_DIR").filter(|dir| !dir.is_empty())
        && let Some(found) = lookup_in_dir(Path::new(&dir), name)
    {
        return Some(found);
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        && let Some(found) = lookup_in_dir(&dir, name)
    {
        return Some(found);
    }
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths).find_map(|dir| lookup_in_dir(&dir, name))
}

fn lookup_in_dir(dir: &Path, name: &str) -> Option<PathBuf> {
    let plain = dir.join(name);
    if plain.is_file() {
        return Some(plain);
    }
    // Windows 的裸可执行文件必须带 .exe，同名文件在目录里就是这么放的。
    if cfg!(windows) {
        let exe = dir.join(format!("{name}.exe"));
        if exe.is_file() {
            return Some(exe);
        }
    }
    None
}

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
        // 同一个解析函数也给目录页与安装校验用：显示"可安装"必须意味着真的能启动。
        let program = match bundled_binary(&def.command) {
            Some(path) => path.into_os_string(),
            None => def.command.clone().into(),
        };
        let mut command = Command::new(program);
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
