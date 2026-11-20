//! router 上游契约：管理面 CRUD/校验/脱敏/审计 + 动态插入后 tools/list 与 tools/call 的真实转发。
//!
//! 全程走真实 axum 链路（admin router + `/mcp`）与真实 SQLite 注册表，执行器换成 fake；
//! 上游侧用本进程内起的 axum fake MCP server（JSON 与 SSE 两种响应），不依赖外部网络。
//!
//! token 一律经 `POST /admin/tokens` 现场签发：内置与上游工具共用同一份 allowlist，
//! 测试必须显式列出它要看的名字（不做"引导令牌默认全量"的假设）。

#[path = "../support/mod.rs"]
mod support;

mod basic;
mod crud;
mod fixtures;
mod packages;
mod preflight;
mod scope;
mod token_options;
mod tools;
mod transports;
