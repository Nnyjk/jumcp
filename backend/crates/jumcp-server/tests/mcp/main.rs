//! MCP 面内置工具的端到端用例：`/mcp` 真实 axum 链路 + Bearer 令牌 + 审计链路。
//!
//! 与 `tests/upstream/` 的差别：这里断言的是**内置**工具本身（模板执行臂），
//! 上游转发不在本用例范围。

#[path = "../support/mod.rs"]
mod support;

mod template_exec;
