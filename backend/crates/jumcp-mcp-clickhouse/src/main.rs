//! `jumcp-mcp-clickhouse` 二进制：stdio 上的 MCP server（按行分隔 JSON-RPC 2.0）。
//!
//! 只从环境变量读配置（DSN 不进命令行参数，避免出现在进程列表里）：
//!
//! - `JUMCP_CLICKHOUSE_DSN`（必需）；
//! - `JUMCP_CLICKHOUSE_ALLOWED_SCHEMAS`（逗号分隔，可空）；
//! - `JUMCP_CLICKHOUSE_TIMEOUT_MS`（缺省 15000）。
//!
//! 启动不做数据库握手：MCP `initialize` 不依赖数据库可达，工具调用时才暴露连接问题，
//! 这样「数据源健康」由 router 的工具探测体现，而不是让进程反复重启。

use jumcp_connector_clickhouse::ClickHouseAgent;
use jumcp_mcp_clickhouse::{Config, SERVER_NAME, Server, serve};
use std::sync::Arc;
use tokio::io::{BufReader, stdin, stdout};

#[tokio::main]
async fn main() {
    let config = match Config::from_env() {
        Ok(config) => config,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    let agent = match ClickHouseAgent::from_dsn(&config.dsn) {
        Ok(agent) => agent,
        Err(error) => {
            // 文案由 connector 保证不含 DSN 明文。
            eprintln!("{SERVER_NAME} 配置非法: {}", error.message);
            std::process::exit(2);
        }
    };
    let server = Server::new(Arc::new(agent), config.allowed_schemas, config.timeout_ms);
    if let Err(error) = serve(BufReader::new(stdin()), stdout(), server).await {
        eprintln!("{SERVER_NAME} 协议循环失败: {error}");
        std::process::exit(1);
    }
}
