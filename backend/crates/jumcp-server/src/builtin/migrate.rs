//! 旧 stdio 安装行迁移：`clickhouse-basic` 的 DSN 拆成结构化字段并改走 HTTP 内嵌端点。
//!
//! 只在 HTTP 模式、构造上游管理器之前运行一次。DSN 解析失败时保留原行并告警：
//! 删行或停用会让"只是 DSN 里有个笔误"变成"安装消失了"，运营者没法自证原配置。

use std::collections::BTreeMap;

use jumcp_connector_clickhouse::Dsn;
use jumcp_core::errors::McpError;
use jumcp_core::registry::{CapabilityStore, McpUpstream};
use jumcp_core::secret::SecretKey;

use super::BuiltinEndpoint;

/// 迁移本包所有 `transport=stdio` 的历史安装，返回改写的行数。
pub fn migrate_legacy_packages(
    store: &CapabilityStore,
    key: &SecretKey,
    endpoint: &BuiltinEndpoint,
) -> Result<usize, McpError> {
    let mut migrated = 0;
    for definition in store.upstreams()? {
        if definition.catalog_id.as_deref() != Some("clickhouse-basic")
            || definition.transport != "stdio"
        {
            continue;
        }
        let Some(raw_dsn) = definition
            .env
            .get("JUMCP_CLICKHOUSE_DSN")
            .map(String::as_str)
            .filter(|value| !value.trim().is_empty())
        else {
            tracing::warn!(
                upstream = %definition.id,
                "clickhouse-basic 旧安装没有 DSN，保留 stdio 原行（需在管理台重装或改配置）"
            );
            continue;
        };
        let dsn = match Dsn::parse(raw_dsn.trim()) {
            Ok(dsn) => dsn,
            Err(error) => {
                tracing::warn!(
                    upstream = %definition.id,
                    error = %error,
                    "clickhouse-basic 旧安装的 DSN 无法解析，保留 stdio 原行"
                );
                continue;
            }
        };

        let mut env = BTreeMap::new();
        env.insert("JUMCP_CLICKHOUSE_URL".to_string(), dsn.endpoint());
        env.insert("JUMCP_CLICKHOUSE_USER".to_string(), dsn.user.clone());
        if !dsn.password.is_empty() {
            let encrypted = key
                .encrypt(&dsn.password)
                .map_err(|error| McpError::config(format!("密码加密失败: {error}")))?;
            env.insert("JUMCP_CLICKHOUSE_PASSWORD".to_string(), encrypted);
        }
        if let Some(database) = dsn.database.as_deref().filter(|value| !value.is_empty()) {
            env.insert(
                "JUMCP_CLICKHOUSE_DATABASE".to_string(),
                database.to_string(),
            );
        }
        if let Some(schemas) = definition
            .env
            .get("JUMCP_CLICKHOUSE_ALLOWED_SCHEMAS")
            .filter(|value| !value.is_empty())
        {
            env.insert(
                "JUMCP_CLICKHOUSE_ALLOWED_SCHEMAS".to_string(),
                schemas.clone(),
            );
        }

        let updated = McpUpstream {
            transport: "http".to_string(),
            url: endpoint.url_for("clickhouse-basic", &definition.id),
            command: String::new(),
            args: Vec::new(),
            env,
            ..definition
        };
        store.update_upstream(&updated, &[], "migration")?;
        migrated += 1;
        tracing::info!(upstream = %updated.id, "clickhouse-basic 旧安装已迁移到 HTTP 内嵌端点");
    }
    Ok(migrated)
}
