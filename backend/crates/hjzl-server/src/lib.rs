// hjzl-server：rmcp API 漂移故手写 JSON-RPC（serde_json + tokio + axum），双传输共用 async 链。
pub mod admin;
pub mod catalog;
pub mod executor;
pub mod mcp;
pub mod pg_executor;
pub mod service;
pub mod templates_boot;

pub use catalog::{Capability, Catalog};
pub use executor::{DefaultQueryExecutor, QueryExecutor, QueryParams};
pub use pg_executor::{PgExecutor, PgExecutorConfig};
pub use service::{Service, health};
