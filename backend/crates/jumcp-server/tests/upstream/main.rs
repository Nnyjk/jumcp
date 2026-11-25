//! router 上游契约：管理面 CRUD/校验/脱敏/审计 + 动态插入后 tools/list 与 tools/call 的真实转发。
//!
//! 全程走真实 axum 链路（admin router + `/mcp`）与真实 SQLite 注册表，执行器换成 fake；
//! 上游侧用本进程内起的 axum fake MCP server（JSON 与 SSE 两种响应），不依赖外部网络。
//!
//! token 授权面只剩组：`groups` 是唯一硬上限，可见性由组内暴露矩阵决定；
//! 测试令牌必须显式带 `basic` 才看得见常驻元工具（不做隐式授权假设）。

#[path = "../support/mod.rs"]
mod support;

mod crud;
mod exposure;
mod fixtures;
mod packages;
mod preflight;
mod scope;
mod token_options;
mod tools;
mod transports;
