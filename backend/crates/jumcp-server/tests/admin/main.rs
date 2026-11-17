//! Admin HTTP API 契约测试：axum oneshot，不需要真实 PG，也不起监听端口。
//!
//! 覆盖：鉴权（无/错 token）、审核队列状态面、非法状态转移 409、同人发布成功、
//! 可见性成功写 admin_change、审计过滤与分页、limit 夹紧、health 的链状态。

#[path = "../support/mod.rs"]
mod support;

mod audit;
mod auth;
mod capabilities;
mod fixtures;
mod groups;
mod review;
mod routes;
mod stats;
mod writes;
