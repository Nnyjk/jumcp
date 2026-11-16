//! 平台 access token 契约测试：创建 / 列表 / 撤销 / options / 组引用闭环，
//! 以及"token 配置 → /mcp tools/list 与 tools/call 同口径"的端到端验证。
//!
//! 全部走真实 axum 链路（admin router + /mcp），执行器替换为 fake，其余是生产实现。

#[path = "../support/mod.rs"]
mod support;

mod audit;
mod auth;
mod crud;
mod fixtures;
mod groups;
mod reissue;
