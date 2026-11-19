// jumcp-core：平面 A/D/G 语义移植（Python 参考只读）。
// 哈希/normalize 必须与 Python 逐字节一致：生成固定向量见 tests/hash_vectors.rs。

pub mod access;
pub mod audit;
pub mod budget;
pub mod builtin;
pub mod canonical;
pub mod config;
pub mod entry;
pub mod errors;
pub mod password;
pub mod registry;
pub mod schemas;
pub mod secret;
pub mod template;

pub use canonical::{canonical_json, definition_hash, hash_concat, sha256_hex};
pub use errors::McpError;
