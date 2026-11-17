//! 二进制级用例：缺 DSN 必须非 0 退出；带 DSN 时 initialize 能在不连库的情况下完成。
//!
//! DSN 指向不可达端口，且 server 启动阶段不做数据库握手——这同时验证了「启动不依赖库可达」。

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};

/// 指向本机未监听端口的假 DSN；密码用哨兵值，便于断言 stderr 没有泄露。
const FAKE_DSN: &str = "http://reader:hunter2@127.0.0.1:18123";

fn binary() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_jumcp-mcp-clickhouse"));
    command.env_remove("JUMCP_CLICKHOUSE_DSN");
    command.env_remove("JUMCP_CLICKHOUSE_ALLOWED_SCHEMAS");
    command.env_remove("JUMCP_CLICKHOUSE_TIMEOUT_MS");
    command
}

#[test]
fn missing_dsn_exits_nonzero_with_variable_name_on_stderr() {
    let output = binary().stdin(Stdio::null()).output().expect("启动二进制");
    assert!(!output.status.success(), "缺 DSN 必须失败退出");
    assert_eq!(output.status.code(), Some(2), "约定用退出码 2 表示配置错误");
    assert!(output.stdout.is_empty(), "失败时 stdout 不得有任何输出");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("JUMCP_CLICKHOUSE_DSN"), "{stderr}");
    assert!(
        stderr.contains("拒绝启动"),
        "stderr 应说明拒绝启动的原因：{stderr}"
    );
}

#[test]
fn binary_answers_initialize_over_stdio_and_never_echoes_dsn() {
    let mut child = binary()
        .env("JUMCP_CLICKHOUSE_DSN", FAKE_DSN)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("启动二进制");

    let mut stdin = child.stdin.take().expect("stdin 管道");
    stdin
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n")
        .expect("写 initialize");
    stdin.flush().expect("刷新 stdin");

    // 读 stdout 放进独立线程，主线程用 recv_timeout 兜底，避免协议回归时测试永久挂起。
    let stdout = child.stdout.take().expect("stdout 管道");
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let read = BufReader::new(stdout).read_line(&mut line);
        let _ = sender.send((read, line));
    });
    let (read, line) = match receiver.recv_timeout(Duration::from_secs(10)) {
        Ok(value) => value,
        Err(_) => {
            let _ = child.kill();
            panic!("二进制 10 秒内没有响应 initialize");
        }
    };
    assert!(read.expect("读 stdout") > 0, "stdout 不应为空");
    let response: Value = serde_json::from_str(&line).expect("stdout 每行都必须是一条 JSON-RPC");
    assert_eq!(response["jsonrpc"], json!("2.0"));
    assert_eq!(response["id"], json!(1));
    assert_eq!(response["result"]["protocolVersion"], json!("2025-03-26"));
    assert_eq!(
        response["result"]["serverInfo"]["name"],
        json!("jumcp-mcp-clickhouse")
    );
    assert_eq!(
        response["result"]["serverInfo"]["version"],
        json!(env!("CARGO_PKG_VERSION"))
    );

    // 关闭 stdin：协议循环读到 EOF 后应正常退出。
    drop(stdin);
    let status = child.wait().expect("等待子进程退出");
    assert!(status.success(), "EOF 后应正常退出：{status:?}");

    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("stderr 管道")
        .read_to_string(&mut stderr)
        .expect("读 stderr");
    assert!(
        !stderr.contains("hunter2"),
        "stderr 泄露了 DSN 密码：{stderr}"
    );
    assert!(!stderr.contains("18123"), "stderr 泄露了 DSN：{stderr}");
}
