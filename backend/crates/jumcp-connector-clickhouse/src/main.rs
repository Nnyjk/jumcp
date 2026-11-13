//! `jumcp-connector-clickhouse` 二进制：从 stdin 读取按行 JSON-RPC，向 stdout 写响应。
//!
//! 只从 `JUMCP_CLICKHOUSE_DSN` 读取连接串；不解析命令行参数，避免凭据进入进程列表。

use jumcp_connector_clickhouse::{ClickHouseAgent, serve};
use tokio::io::{BufReader, stdin, stdout};

#[tokio::main]
async fn main() {
    let dsn = match std::env::var("JUMCP_CLICKHOUSE_DSN") {
        Ok(value) if !value.trim().is_empty() => value,
        _ => {
            eprintln!("JUMCP_CLICKHOUSE_DSN 未设置：connector agent 拒绝启动");
            std::process::exit(2);
        }
    };
    let agent = match ClickHouseAgent::from_dsn(&dsn) {
        Ok(agent) => agent,
        Err(error) => {
            // 错误文案由 lib 保证不含 DSN 明文。
            eprintln!("connector agent 配置非法: {}", error.message);
            std::process::exit(2);
        }
    };
    if let Err(error) = serve(BufReader::new(stdin()), stdout(), agent).await {
        eprintln!("connector agent 协议循环失败: {error}");
        std::process::exit(1);
    }
}
