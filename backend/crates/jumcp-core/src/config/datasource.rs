//! 数据源引用：DSN 只从环境变量取，配置里永不出现明文连接串。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasourceRef {
    pub name: String,
    /// 存放 DSN 的环境变量名；配置里永不出现明文连接串。
    pub dsn_env: String,
    #[serde(default = "default_engine")]
    pub engine: String,
    #[serde(default = "default_true")]
    pub readonly: bool,
    /// 连接级 schema 白名单：请求的 allowed_schemas 只能是它的子集（只做收敛）。
    /// ClickHouse 下语义为允许的 database 名列表；为空表示不限制。
    #[serde(default)]
    pub allowed_schemas: Vec<String>,
    #[serde(default = "default_statement_timeout")]
    pub statement_timeout_ms: i64,
}
fn default_engine() -> String {
    "clickhouse".to_string()
}
fn default_true() -> bool {
    true
}
fn default_statement_timeout() -> i64 {
    15_000
}

impl DatasourceRef {
    /// 边界校验：明文连接串含 `=`/`://`/`/` 一律拒（凭证不落配置）。
    pub fn validate(&self) -> Result<(), String> {
        if self.name.is_empty() || self.dsn_env.is_empty() {
            return Err(format!("数据源 {} 的 name/dsn_env 不能为空", self.name));
        }
        if self.dsn_env.contains('=') || self.dsn_env.contains("://") || self.dsn_env.contains('/')
        {
            return Err(format!(
                "数据源 {} 的 dsn_env 只能是环境变量名，禁止保存明文连接串",
                self.name
            ));
        }
        if self.engine != "clickhouse" {
            return Err(format!(
                "数据源 {} 的 engine 不支持: {}（当前只接受 clickhouse connector agent）",
                self.name, self.engine
            ));
        }
        if self.statement_timeout_ms <= 0 {
            return Err(format!(
                "数据源 {} 的 statement_timeout_ms 必须为正",
                self.name
            ));
        }
        Ok(())
    }
    /// 读进程 env；缺失即显式 Err，不返回占位值。
    pub fn dsn(&self) -> Result<String, String> {
        std::env::var(&self.dsn_env).map_err(|_| {
            format!(
                "数据源 {} 引用的环境变量 {} 未设置",
                self.name, self.dsn_env
            )
        })
    }
}

/// datasources 与策略同文件时的顶层容器（与 `[[groups]]` 等策略键并存）。
#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct DatasourceFile {
    #[serde(default)]
    pub(super) datasources: Vec<DatasourceRef>,
}
