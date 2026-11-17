//! 平台生成的 MCP access token：密文持久化、按 hash 鉴权、撤销与组引用计数。
//!
//! 为什么落 registry 同一个 SQLite：token 与组/能力必须同库同事务视图，
//! 否则"删组前检查 token 引用"会出现跨库竞态。
//!
//! 明文只在创建或重新签发时返回一次（`jumcp_<64 hex>`），库里只存 sha256；审计只写前缀，
//! 永远不写明文或 hash。角色/组/工具白名单的**成员资格**属于合并策略（`config`）的职责，
//! 本模块只做字段级门禁与持久化。

mod token;

pub use token::{
    AccessTokenRecord, BOOTSTRAP_TOKEN_NAME, IssuedAccessToken, NewAccessToken, TOKEN_FIELD_MAX,
    TOKEN_PLAINTEXT_PREFIX, TOKEN_STATUS_ACTIVE, TOKEN_STATUS_EXPIRED, TOKEN_STATUS_REVOKED,
};
pub(crate) use token::{ensure_schema, group_token_count_locked};

#[cfg(test)]
mod tests;
