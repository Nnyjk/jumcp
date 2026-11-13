//! 真实 ClickHouse 的协议级验收：直接拉起 agent 二进制，走 stdin/stdout JSON-RPC。
//!
//! 默认 `#[ignore]`；需要 `JUMCP_TEST_CLICKHOUSE_DSN`（只读账号）并先构建二进制。

use serde_json::{Value, json};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

async fn start() -> Option<(Child, ChildStdin, BufReader<ChildStdout>)> {
    let dsn = std::env::var("JUMCP_TEST_CLICKHOUSE_DSN").ok()?;
    let mut child = Command::new(env!("CARGO_BIN_EXE_jumcp-connector-clickhouse"))
        .env("JUMCP_CLICKHOUSE_DSN", dsn)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn agent");
    let stdin = child.stdin.take().unwrap();
    let stdout = BufReader::new(child.stdout.take().unwrap());
    Some((child, stdin, stdout))
}

async fn call(
    stdin: &mut ChildStdin,
    stdout: &mut BufReader<ChildStdout>,
    request: Value,
) -> Value {
    let mut line = serde_json::to_string(&request).unwrap();
    line.push('\n');
    stdin.write_all(line.as_bytes()).await.unwrap();
    stdin.flush().await.unwrap();
    let mut response = String::new();
    stdout.read_line(&mut response).await.unwrap();
    serde_json::from_str(&response).expect("agent 响应必须是 JSON")
}

#[tokio::test]
#[ignore = "需要真实 ClickHouse：JUMCP_TEST_CLICKHOUSE_DSN"]
async fn agent_protocol_initialize_query_and_shutdown() {
    let Some((mut child, mut stdin, mut stdout)) = start().await else {
        return;
    };

    let init = call(
        &mut stdin,
        &mut stdout,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocol_version":1}}),
    )
    .await;
    assert_eq!(init["id"], json!(1));
    assert_eq!(init["result"]["protocol_version"], json!(1));
    assert!(
        init["result"]["server_version"]
            .as_str()
            .unwrap()
            .contains("25.")
    );

    let rows = call(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc":"2.0","id":2,"method":"query",
            "params":{"sql":"SELECT number FROM system.numbers LIMIT 10","params":[],"max_rows":3,"timeout_ms":5000}
        }),
    )
    .await;
    assert_eq!(rows["id"], json!(2));
    assert_eq!(rows["result"]["row_count"], json!(3));
    assert_eq!(rows["result"]["truncated"], json!(true));
    assert_eq!(rows["result"]["rows"].as_array().unwrap().len(), 3);
    assert_eq!(rows["result"]["columns"][0]["name"], json!("number"));
    assert_eq!(rows["result"]["columns"][0]["type"], json!("UInt64"));

    let bound = call(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc":"2.0","id":3,"method":"query",
            "params":{"sql":"SELECT ? + 1","params":[41],"max_rows":10,"timeout_ms":5000}
        }),
    )
    .await;
    assert_eq!(bound["result"]["rows"][0][0], json!(42));

    let readonly = call(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc":"2.0","id":4,"method":"query",
            "params":{"sql":"SET max_threads=2","params":[],"max_rows":10,"timeout_ms":5000}
        }),
    )
    .await;
    assert_eq!(readonly["error"]["data"]["code"], json!("MCP_STORE_ERROR"));
    assert!(
        readonly["error"]["message"]
            .as_str()
            .unwrap()
            .contains("READONLY")
    );

    let shutdown = call(
        &mut stdin,
        &mut stdout,
        json!({"jsonrpc":"2.0","id":5,"method":"shutdown"}),
    )
    .await;
    assert_eq!(shutdown["result"]["ok"], json!(true));
    let status = child.wait().await.unwrap();
    assert!(status.success());
}

#[tokio::test]
#[ignore = "需要真实 ClickHouse：JUMCP_TEST_CLICKHOUSE_DSN"]
async fn agent_protocol_rejects_non_scalar_params() {
    let Some((mut child, mut stdin, mut stdout)) = start().await else {
        return;
    };
    let init = call(
        &mut stdin,
        &mut stdout,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
    )
    .await;
    assert!(init.get("result").is_some());

    let reply = call(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc":"2.0","id":2,"method":"query",
            "params":{"sql":"SELECT ?","params":[[1,2]],"max_rows":10,"timeout_ms":5000}
        }),
    )
    .await;
    assert_eq!(
        reply["error"]["data"]["code"],
        json!("MCP_PARAM_STYLE_UNSUPPORTED")
    );
    let _ = child.start_kill();
}
