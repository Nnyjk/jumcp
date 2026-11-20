//! 第三方工具调用的前置拦截：在转发上游之前按入参形状判定的统一闸门。
//!
//! 上游 MCP 是第三方实现，收到什么参数、会不会自己校验，本服务既不能假设也不能依赖；
//! 本服务是这些调用链上唯一由我们控制的收口点，因此三类可能的数据获取通道都要拦：
//!
//! | family | 判定依据 | 规则表 |
//! |---|---|---|
//! | `sql` | 入参含 `sql`/`query`/`statement` | [`sql::check`] |
//! | `file` | 入参含路径键（`path`/`file`/`dir`…） | [`file::check`] |
//! | `http` | 入参含 URL 键或分页/时间范围键 | [`http::check`] |
//!
//! 判定顺序即优先级：一次调用被识别为 SQL 后不再套用文件/接口规则（SQL 的库白名单与
//! LIMIT 上限是更强、更准确的约束）。三族共用[`PreflightConfig`]，拒绝结果共用
//! [`Rejection`]，错误码按 family 区分，审计按 `surface` + `rule` 聚合。

use serde_json::Value;

mod file;
mod http;
pub mod sql;

pub use sql::sql_argument;
/// 从 SQL 里抽取表引用：候选评分（`admin/scoring`）按表名聚合调用热点。
pub use sql::table_refs;

/// 拦截族：决定错误码与审计 `surface`，是管理台/告警的聚合维度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Sql,
    File,
    Http,
}

impl Family {
    /// 审计 `data.detail.surface`：与事件类型（policy_decision）一起定位拦截层。
    pub fn surface(self) -> &'static str {
        match self {
            Self::Sql => "mcp/sql-preflight",
            Self::File => "mcp/file-preflight",
            Self::Http => "mcp/http-preflight",
        }
    }

    /// 面向调用方的错误码：族之间不混用，客户端可据此分支。
    pub fn error_code(self) -> &'static str {
        match self {
            Self::Sql => "MCP_SQL_BLOCKED",
            Self::File => "MCP_FILE_BLOCKED",
            Self::Http => "MCP_HTTP_BLOCKED",
        }
    }
}

/// 拦截结果。`rule` 是稳定标识：管理台/告警按它聚合，不解析文案。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    pub family: Family,
    /// 命中的规则名（见各族 `check` 的规则表）。
    pub rule: &'static str,
    /// 面向调用方的一句话原因。
    pub message: String,
    /// 可执行的改法；agent 拿到它就能自己修正调用。
    pub hint: String,
}

pub(super) fn reject(
    family: Family,
    rule: &'static str,
    message: impl Into<String>,
    hint: impl Into<String>,
) -> Rejection {
    Rejection {
        family,
        rule,
        message: message.into(),
        hint: hint.into(),
    }
}

/// 前置拦截配置。
///
/// 只有"部署侧确实会因环境不同"的项开放环境变量；上限类常量（SQL `max_limit`、
/// 分页上限、时间跨度上限）钉死在代码里：它们是"一次调用能带走多少数据"的安全承诺，
/// 可配置就等于把它降级成运维便利项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightConfig {
    /// 允许访问的库；空表示不做库范围限定（`schema_out_of_scope` 规则整体跳过）。
    pub allowed_schemas: Vec<String>,
    /// 单次查询允许的最大 LIMIT；无 LIMIT 的宽查询按 `unbounded` 拒绝。
    pub max_limit: i64,
    /// 允许访问的文件路径前缀；空表示不做根目录限定（敏感路径与穿越仍然拒绝）。
    pub allowed_roots: Vec<String>,
    /// 是否允许接口调用指向内网地址（RFC1918 / 唯一本地 IPv6）；回环、链路本地与云元数据
    /// 地址永远拒绝。
    pub allow_private_hosts: bool,
    /// 单次接口调用的时间范围上限（天）。
    pub max_time_range_days: i64,
    /// 单次接口调用的分页大小上限。
    pub max_page_size: i64,
}

impl Default for PreflightConfig {
    fn default() -> Self {
        Self {
            allowed_schemas: Vec::new(),
            max_limit: 1000,
            allowed_roots: Vec::new(),
            allow_private_hosts: false,
            max_time_range_days: 366,
            max_page_size: 1000,
        }
    }
}

impl PreflightConfig {
    /// 部署侧配置：
    /// - `JUMCP_SQL_ALLOWED_SCHEMAS`：SQL 库白名单（逗号分隔，空 = 不做库范围限定）；
    /// - `JUMCP_FILE_ALLOWED_ROOTS`：文件路径白名单（逗号分隔的绝对路径前缀，空 = 不限定）；
    /// - `JUMCP_HTTP_ALLOW_PRIVATE_HOSTS`：`1`/`true` 时允许内网地址（回环/元数据仍拒绝）。
    pub fn from_env() -> Self {
        Self {
            allowed_schemas: parse_list(
                &std::env::var("JUMCP_SQL_ALLOWED_SCHEMAS").unwrap_or_default(),
            ),
            allowed_roots: parse_list(
                &std::env::var("JUMCP_FILE_ALLOWED_ROOTS").unwrap_or_default(),
            ),
            allow_private_hosts: matches!(
                std::env::var("JUMCP_HTTP_ALLOW_PRIVATE_HOSTS")
                    .unwrap_or_default()
                    .trim()
                    .to_ascii_lowercase()
                    .as_str(),
                "1" | "true"
            ),
            ..Self::default()
        }
    }
}

/// 逗号分隔列表：去空白、丢空项、去重（大小写不敏感；判定时统一小写比较）。
fn parse_list(raw: &str) -> Vec<String> {
    let mut kept: Vec<String> = Vec::new();
    for entry in raw.split(',') {
        let value = entry.trim();
        if value.is_empty() || kept.iter().any(|k| k.eq_ignore_ascii_case(value)) {
            continue;
        }
        kept.push(value.to_string());
    }
    kept
}

/// 一次上游调用的唯一入口：按入参形状选择族并返回首个命中。
///
/// 返回 `None` 表示放行；各族 `check` 的规则顺序即优先级（先命中先返回）。
pub fn check_call(arguments: &Value, cfg: &PreflightConfig) -> Option<Rejection> {
    if let Some(sql) = sql::sql_argument(arguments) {
        return sql::check(sql, cfg).err();
    }
    file::check(arguments, cfg)
        .err()
        .or_else(|| http::check(arguments, cfg).err())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 三族按入参形状分派：同一份配置下，SQL 调用只走 SQL 规则，路径/URL 调用不被 SQL 规则误伤。
    #[test]
    fn dispatch_picks_the_family_by_argument_shape() {
        let cfg = PreflightConfig::default();
        let blocked = check_call(&json!({"sql": "SELECT * FROM t"}), &cfg).expect("应拦截");
        assert_eq!(blocked.family, Family::Sql);
        assert_eq!(blocked.rule, "unbounded");

        let blocked = check_call(&json!({"path": "../../etc/passwd"}), &cfg).expect("应拦截");
        assert_eq!(blocked.family, Family::File);
        assert_eq!(blocked.rule, "file_traversal");

        let blocked = check_call(
            &json!({"url": "http://169.254.169.254/latest/meta-data/"}),
            &cfg,
        )
        .expect("应拦截");
        assert_eq!(blocked.family, Family::Http);
        assert_eq!(blocked.rule, "http_private_target");

        assert!(check_call(&json!({"msg": "hi"}), &cfg).is_none());
    }

    /// 列表配置去空白、丢空项、大小写不敏感去重。
    #[test]
    fn list_config_is_normalized() {
        assert_eq!(
            parse_list(" env_air , ,ENV_AIR, other "),
            vec!["env_air".to_string(), "other".to_string()]
        );
    }
}
