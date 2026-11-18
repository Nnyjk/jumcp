// jumcp-server：rmcp API 漂移故手写 JSON-RPC（serde_json + tokio + axum），双传输共用 async 链。
pub mod admin;
pub mod app;
pub mod builtin;
pub mod catalog;
pub mod connector;
pub mod discovery;
pub mod executor;
pub mod mcp;
pub mod service;
pub mod templates_boot;
pub mod upstream;
pub mod web;

pub use catalog::{Capability, Catalog};
pub use connector::{ConnectorConfig, ConnectorExecutor};
pub use executor::{DefaultQueryExecutor, QueryExecutor, QueryParams};
pub use service::{Service, health, health_report};
pub use upstream::{UpstreamManager, exposed_tool_name};
