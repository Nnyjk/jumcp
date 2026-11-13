//! 方言定义：决定解析器行为、默认 schema 与 schema 越界判定方式。

use std::str::FromStr;

use sqlparser::dialect::{
    ClickHouseDialect, DuckDbDialect, MySqlDialect, PostgreSqlDialect, RedshiftSqlDialect,
    SQLiteDialect,
};

/// 只读 SQL 的方言。
///
/// 同一段 SQL 在不同方言下语法差异显著（占位符、注释、函数名），
/// 因此方言既是解析参数，也是默认 schema 与越界判定规则的输入。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Dialect {
    /// PostgreSQL（默认）。
    #[default]
    Postgres,
    /// Amazon Redshift（PostgreSQL 派生）。
    Redshift,
    /// DuckDB。
    DuckDb,
    /// SQLite。
    Sqlite,
    /// MySQL。
    MySql,
    /// MariaDB（MySQL 语法派生）。
    MariaDb,
    /// ClickHouse。
    ClickHouse,
}

impl Dialect {
    /// 无前缀表名所落地的默认 schema。
    ///
    /// 返回 `None` 表示默认 schema 由连接决定（MySQL/MariaDB 的当前库、
    /// ClickHouse 的 default 库），无法静态确定。
    pub fn default_schema(self) -> Option<&'static str> {
        match self {
            Dialect::Postgres | Dialect::Redshift => Some("public"),
            Dialect::DuckDb | Dialect::Sqlite => Some("main"),
            Dialect::MySql | Dialect::MariaDb | Dialect::ClickHouse => None,
        }
    }

    /// 默认 schema 由连接决定的方言。
    ///
    /// 这类方言的无前缀表名必然落在连接当前库，不可能跨库，
    /// 因此不做静态越界判定（由数据源侧只读账号与库级授权兜底）；
    /// 带前缀的表名仍逐个校验显式 schema。
    pub fn is_connection_scoped(self) -> bool {
        matches!(
            self,
            Dialect::MySql | Dialect::MariaDb | Dialect::ClickHouse
        )
    }

    /// 该方言是否原生把 `?` 当绑定占位符。
    ///
    /// PostgreSQL 系把 `?` 用作 jsonb 存在性运算符，
    /// 这类方言的 `?` 需要在解析前改写为 `$n` 才能作为占位符处理。
    pub fn supports_question_placeholder(self) -> bool {
        !matches!(self, Dialect::Postgres | Dialect::Redshift)
    }

    /// 构造 sqlparser 方言实例。
    pub(crate) fn sqlparser(self) -> Box<dyn sqlparser::dialect::Dialect> {
        match self {
            Dialect::Postgres => Box::new(PostgreSqlDialect {}),
            Dialect::Redshift => Box::new(RedshiftSqlDialect {}),
            Dialect::DuckDb => Box::new(DuckDbDialect {}),
            Dialect::Sqlite => Box::new(SQLiteDialect {}),
            Dialect::MySql | Dialect::MariaDb => Box::new(MySqlDialect {}),
            Dialect::ClickHouse => Box::new(ClickHouseDialect {}),
        }
    }
}

/// 方言名无法识别。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("未知 SQL 方言：{0}")]
pub struct UnknownDialect(pub String);

impl FromStr for Dialect {
    type Err = UnknownDialect;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "postgres" | "postgresql" | "pg" => Ok(Dialect::Postgres),
            "redshift" => Ok(Dialect::Redshift),
            "duckdb" => Ok(Dialect::DuckDb),
            "sqlite" => Ok(Dialect::Sqlite),
            "mysql" => Ok(Dialect::MySql),
            "mariadb" => Ok(Dialect::MariaDb),
            "clickhouse" => Ok(Dialect::ClickHouse),
            other => Err(UnknownDialect(other.to_string())),
        }
    }
}

impl std::fmt::Display for Dialect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Dialect::Postgres => "postgres",
            Dialect::Redshift => "redshift",
            Dialect::DuckDb => "duckdb",
            Dialect::Sqlite => "sqlite",
            Dialect::MySql => "mysql",
            Dialect::MariaDb => "mariadb",
            Dialect::ClickHouse => "clickhouse",
        };
        f.write_str(name)
    }
}
