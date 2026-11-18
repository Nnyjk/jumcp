//! `clickhouse-basic` 插件：字段目录 + 配置解析 + agent 缓存。
//!
//! 缓存键是配置指纹（sha256），密码只参与哈希、不落缓存：同一条上游的
//! `tools/call` 复用同一个 `clickhouse::Client`（连接池），管理面改配置后指纹变化
//! 自然重建，不需要失效通知。

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use jumcp_connector_clickhouse::{ClickHouseAgent, Dsn, Endpoint};
use jumcp_core::builtin::{
    BuiltinConfig, BuiltinError, BuiltinField, BuiltinFuture, BuiltinMcp, BuiltinPackage,
    BuiltinTool, BuiltinToolDef, FieldKind,
};
use jumcp_core::{canonical_json, sha256_hex};
use serde_json::{Value, json};

use crate::{DEFAULT_TIMEOUT_MS, Server, tools};

/// 缓存上限：正常部署只有个位数上游；配置反复编辑会不断产生新指纹，超限整体清空
/// 以换取"不无限增长"，代价是下一次调用重建连接。
const MAX_CACHED_AGENTS: usize = 32;

const FIELDS: &[BuiltinField] = &[
    BuiltinField {
        key: "url",
        label: "ClickHouse 地址",
        kind: FieldKind::Text,
        required: true,
        secret: false,
        default: None,
        placeholder: Some("http://127.0.0.1:8123"),
        help: Some(
            "ClickHouse HTTP 端点（http/https）。不要写用户名密码；不写端口时 http 用 8123、https 用 8443。",
        ),
        env: "JUMCP_CLICKHOUSE_URL",
    },
    BuiltinField {
        key: "username",
        label: "用户名",
        kind: FieldKind::Text,
        required: true,
        secret: false,
        default: Some("default"),
        placeholder: Some("default"),
        help: Some("建议使用只读账号：连接固定 readonly=1，账号本身的权限仍是最后一道闸。"),
        env: "JUMCP_CLICKHOUSE_USER",
    },
    BuiltinField {
        key: "password",
        label: "密码",
        kind: FieldKind::Password,
        required: false,
        secret: true,
        default: None,
        placeholder: Some("留空表示无密码"),
        help: Some("加密存储；轮换 JUMCP_SECRET_KEY 后旧密文不可解，需要重填。"),
        env: "JUMCP_CLICKHOUSE_PASSWORD",
    },
    BuiltinField {
        key: "database",
        label: "默认库",
        kind: FieldKind::Text,
        required: false,
        secret: false,
        default: None,
        placeholder: Some("default"),
        help: Some("留空时连接使用 default 库。"),
        env: "JUMCP_CLICKHOUSE_DATABASE",
    },
    BuiltinField {
        key: "allowed_schemas",
        label: "允许的库（逗号分隔）",
        kind: FieldKind::Text,
        required: false,
        secret: false,
        default: None,
        placeholder: Some("analytics,ods"),
        help: Some(
            "留空表示不限制；带库名的表必须落在其中，未指定库的查询钉到列表首个库；库名按实际大小写填写，比较不区分大小写。",
        ),
        env: "JUMCP_CLICKHOUSE_ALLOWED_SCHEMAS",
    },
];

const TOOLS: &[BuiltinTool] = &[
    BuiltinTool {
        name: tools::TOOL_QUERY,
        summary: "执行单条只读 SQL，返回列与行。",
    },
    BuiltinTool {
        name: tools::TOOL_LIST_DATABASES,
        summary: "列出当前账号可见的数据库。",
    },
    BuiltinTool {
        name: tools::TOOL_LIST_TABLES,
        summary: "列出指定数据库下的表。",
    },
];

static PACKAGE: BuiltinPackage = BuiltinPackage {
    id: "clickhouse-basic",
    name: "ClickHouse 基础查询",
    summary: "只读 SQL 查询与库表探索；执行前做单语句、根查询与 schema 范围校验。",
    version: env!("CARGO_PKG_VERSION"),
    tags: &["database", "clickhouse"],
    default_upstream_id: "clickhouse",
    fields: FIELDS,
    tools: TOOLS,
};

static PLUGIN: ClickHousePlugin = ClickHousePlugin;

/// 进程内 agent 缓存（按配置指纹）。配置来自管理面、条数极少，进程级共享即可。
static AGENTS: LazyLock<Mutex<HashMap<String, Arc<ClickHouseAgent>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// 进程级插件实例（server 的插件注册表只持有这个静态引用）。
pub fn plugin() -> &'static dyn BuiltinMcp {
    &PLUGIN
}

/// 内置 MCP 插件：包目录、工具清单与工具调用。
pub struct ClickHousePlugin;

impl BuiltinMcp for ClickHousePlugin {
    fn package(&self) -> &'static BuiltinPackage {
        &PACKAGE
    }

    fn tools(&self) -> Vec<BuiltinToolDef> {
        tools::definitions()
            .into_iter()
            .map(|definition| BuiltinToolDef {
                name: definition["name"]
                    .as_str()
                    .expect("工具定义由本 crate 生成，必然含 name")
                    .to_string(),
                description: definition["description"]
                    .as_str()
                    .expect("工具定义由本 crate 生成，必然含 description")
                    .to_string(),
                input_schema: definition
                    .get("inputSchema")
                    .cloned()
                    .expect("工具定义由本 crate 生成，必然含 inputSchema"),
            })
            .collect()
    }

    fn call<'a>(
        &'a self,
        config: &'a BuiltinConfig,
        tool: &'a str,
        arguments: &'a Value,
    ) -> BuiltinFuture<'a, Result<Value, BuiltinError>> {
        Box::pin(async move {
            let config = PluginConfig::from_builtin(config)?;
            let agent = self.agent(&config)?;
            let server = Server::new(agent, config.allowed_schemas, DEFAULT_TIMEOUT_MS);
            Ok(server.call_tool(tool, arguments).await)
        })
    }
}

impl ClickHousePlugin {
    fn agent(&self, config: &PluginConfig) -> Result<Arc<ClickHouseAgent>, BuiltinError> {
        let key = config.fingerprint();
        if let Some(agent) = AGENTS
            .lock()
            .expect("agent 缓存锁未被持有期间 panic")
            .get(&key)
        {
            return Ok(agent.clone());
        }
        let agent = Arc::new(
            ClickHouseAgent::from_parts(&config.dsn)
                .map_err(|error| config_error("url", error.message))?,
        );
        let mut cache = AGENTS.lock().expect("agent 缓存锁未被持有期间 panic");
        if cache.len() >= MAX_CACHED_AGENTS {
            cache.clear();
        }
        cache.insert(key, agent.clone());
        Ok(agent)
    }
}

struct PluginConfig {
    dsn: Dsn,
    allowed_schemas: Vec<String>,
}

impl PluginConfig {
    /// 把解密后的字段值解析成连接参数；缺 `url` 或 URL 形态不合法是配置错误
    /// （`MCP_CONFIG_ERROR`），不是工具错误。
    fn from_builtin(config: &BuiltinConfig) -> Result<Self, BuiltinError> {
        let url = config
            .get("url")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| config_error("url", "缺少配置字段：url"))?;
        let endpoint = Endpoint::parse(url)
            .map_err(|error| config_error("url", format!("url 不合法：{error}")))?;
        let username = config
            .get("username")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("default");
        let password = config.get("password").unwrap_or("");
        let database = config
            .get("database")
            .map(str::trim)
            .filter(|value| !value.is_empty());
        Ok(Self {
            dsn: Dsn::from_parts(&endpoint, username, password, database),
            allowed_schemas: parse_schemas(config.get("allowed_schemas")),
        })
    }

    /// 配置指纹：包含会影响连接或只读范围的每一项；密码只在哈希里出现。
    fn fingerprint(&self) -> String {
        sha256_hex(&canonical_json(&json!({
            "endpoint": self.dsn.endpoint(),
            "user": self.dsn.user.as_str(),
            "password": self.dsn.password.as_str(),
            "database": self.dsn.database.as_deref(),
            "allowed_schemas": self.allowed_schemas,
        })))
    }
}

/// 逗号分隔的库名列表：trim、丢空项、大小写不敏感去重（保留首现原串）。
fn parse_schemas(raw: Option<&str>) -> Vec<String> {
    let mut schemas: Vec<String> = Vec::new();
    for item in raw.unwrap_or("").split(',') {
        let name = item.trim();
        if name.is_empty() {
            continue;
        }
        if schemas
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(name))
        {
            continue;
        }
        schemas.push(name.to_string());
    }
    schemas
}

fn config_error(field: &str, message: impl Into<String>) -> BuiltinError {
    BuiltinError::new("MCP_CONFIG_ERROR", message, json!({"field": field}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(pairs: &[(&str, &str)]) -> BuiltinConfig {
        BuiltinConfig::new(
            pairs
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect(),
        )
    }

    #[test]
    fn endpoint_defaults_port_and_rejects_userinfo() {
        let parsed = PluginConfig::from_builtin(&config(&[("url", "http://db.lan")])).unwrap();
        assert_eq!(parsed.dsn.endpoint(), "http://db.lan:8123");
        let parsed = PluginConfig::from_builtin(&config(&[("url", "https://db.lan")])).unwrap();
        assert_eq!(parsed.dsn.endpoint(), "https://db.lan:8443");
        let err = PluginConfig::from_builtin(&config(&[("url", "http://u:p@db.lan:8123")]))
            .err()
            .unwrap();
        assert_eq!(err.code, "MCP_CONFIG_ERROR");
        assert!(!err.message.contains("p@"));
    }

    #[test]
    fn username_defaults_and_schemas_dedupe() {
        let parsed = PluginConfig::from_builtin(&config(&[
            ("url", "http://db.lan:8123"),
            ("allowed_schemas", " Analytics , ,analytics,ods "),
        ]))
        .unwrap();
        assert_eq!(parsed.dsn.user, "default");
        assert_eq!(parsed.dsn.password, "");
        assert_eq!(parsed.allowed_schemas, vec!["Analytics", "ods"]);
    }

    #[test]
    fn fingerprint_changes_with_password_but_leaks_nothing() {
        let base = config(&[("url", "http://db.lan:8123"), ("password", "s3cret")]);
        let changed = config(&[("url", "http://db.lan:8123"), ("password", "s3cret2")]);
        let first = PluginConfig::from_builtin(&base).unwrap().fingerprint();
        let second = PluginConfig::from_builtin(&changed).unwrap().fingerprint();
        assert_ne!(first, second);
        assert_eq!(first.len(), 64);
        assert!(!first.contains("s3cret"));
    }
}
