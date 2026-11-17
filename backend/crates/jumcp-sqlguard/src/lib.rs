//! 只读 SQL 校验（L4 纵深防御）与结果载荷整形。
//!
//! 本 crate 是 MCP 服务「通用只读入口」的执行前置闸门：
//!
//! - [`validate`]：单条只读 SQL 的语法级校验——单语句、根节点为查询、
//!   函数黑名单、schema 不越界。
//! - [`shape_rows`]：结果载荷整形——行数与单格字符超限一律夹紧并回显，
//!   不做 token 预算判定。
//!
//! 校验只做语法级判定：能否读到数据由数据源账号权限（L1）保证，
//! 本 crate 无权也无从保证。

mod dialect;
mod error;
mod lex;
mod shape;
mod validate;

pub use dialect::{Dialect, UnknownDialect};
pub use error::{CODE_MULTI_STATEMENT, CODE_SCHEMA_OUT_OF_SCOPE, CODE_SQL_REJECTED, GuardError};
pub use shape::{ShapedResult, shape_rows};
pub use validate::{Kind, Validated, validate};
