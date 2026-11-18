//! 内置 MCP：插件注册表、内嵌 HTTP 端点、旧数据迁移。
//!
//! 内置包不再以 stdio 子进程接入：安装产物是一条普通 HTTP 上游，URL 指向本进程的
//! `POST /internal/mcp/{package_id}/{upstream_id}`。端点只接受进程内随机 token，
//! 该 token 只保存在内存里，由 [`crate::upstream::UpstreamManager`] 在请求命中
//! [`BuiltinEndpoint::base`] 前缀时注入。
//!
//! 新增一个内置 MCP = 新 crate 实现 `jumcp_core::builtin::BuiltinMcp` +
//! 在 [`plugins`] 里加一行。

mod migrate;

pub use migrate::migrate_legacy_packages;

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, LazyLock};

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use jumcp_core::builtin::{BuiltinConfig, BuiltinError, BuiltinMcp, BuiltinPackage};
use jumcp_core::config::constant_time_eq;
use jumcp_core::errors::McpError;
use jumcp_core::registry::{CapabilityStore, McpUpstream};
use jumcp_core::secret::{SecretKey, is_encrypted, random_hex};
use serde_json::{Value, json};

use crate::upstream::MCP_PROTOCOL_VERSION;

/// 内置插件清单。顺序即目录顺序；包 id 必须全局唯一。
static PLUGINS: LazyLock<Vec<&'static dyn BuiltinMcp>> =
    LazyLock::new(|| vec![jumcp_mcp_clickhouse::plugin()]);

/// 全部内置插件（目录展示与安装校验用）。
pub fn plugins() -> &'static [&'static dyn BuiltinMcp] {
    &PLUGINS
}

/// 按包 id 取插件；未注册返回 `None`。
pub fn plugin(id: &str) -> Option<&'static dyn BuiltinMcp> {
    plugins()
        .iter()
        .copied()
        .find(|plugin| plugin.package().id == id)
}

/// 内嵌端点地址与进程内 token。
///
/// `base` 形如 `http://127.0.0.1:8000/internal/mcp`（无末尾斜杠）；
/// 安装时拼成 `{base}/{package_id}/{upstream_id}`。
#[derive(Clone)]
pub struct BuiltinEndpoint {
    pub base: String,
    pub token: String,
}

/// `token` 是注入给上游客户端的 Bearer 凭据：Debug 里一律 `***`，只留 base。
impl fmt::Debug for BuiltinEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BuiltinEndpoint")
            .field("base", &self.base)
            .field("token", &"***")
            .finish()
    }
}

impl BuiltinEndpoint {
    /// 显式指定 base 与 token（测试用；生产走 [`Self::local`]）。
    ///
    /// base 是"包端点根"：只给 origin（`http://127.0.0.1:8088`）时补上
    /// `/internal/mcp`；已带路径前缀则原样保留（网关挂了子路径时也能对齐）。
    pub fn new(base: impl Into<String>, token: impl Into<String>) -> Self {
        let base = base.into();
        let trimmed = base.trim_end_matches('/');
        let path = trimmed
            .split_once("://")
            .map_or(trimmed, |(_, rest)| rest)
            .split_once('/')
            .map(|(_, path)| path);
        let base = match path {
            Some(path) if !path.is_empty() => trimmed.to_string(),
            _ => format!("{trimmed}/internal/mcp"),
        };
        Self {
            base,
            token: token.into(),
        }
    }

    /// 按监听地址生成本机回环地址与一次性 token。
    ///
    /// 监听在通配地址时不能拿 `0.0.0.0` 建连，回环 `127.0.0.1` 才是本进程。
    pub fn local(host: &str, port: u16) -> Result<Self, McpError> {
        let host = match host {
            "0.0.0.0" | "::" | "[::]" | "" => "127.0.0.1",
            other => other,
        };
        let token = random_hex(32)
            .map_err(|error| McpError::config(format!("内置 MCP token 生成失败: {error}")))?;
        Ok(Self::new(format!("http://{host}:{port}"), token))
    }

    /// 某条安装产物的上游 URL。
    pub fn url_for(&self, package_id: &str, upstream_id: &str) -> String {
        format!("{}/{package_id}/{upstream_id}", self.base)
    }

    /// URL 是否指向本端点（要求 base 之后的下一字符是 `/`，避免前缀误命中）。
    pub fn matches(&self, url: &str) -> bool {
        url.strip_prefix(&self.base)
            .is_some_and(|rest| rest.starts_with('/'))
    }
}

/// 端点运行时状态：存储、解密密钥、Bearer token。
#[derive(Clone)]
pub struct BuiltinState {
    store: Arc<CapabilityStore>,
    key: Arc<SecretKey>,
    token: Arc<str>,
}

impl BuiltinState {
    pub fn new(
        store: Arc<CapabilityStore>,
        key: Arc<SecretKey>,
        endpoint: &BuiltinEndpoint,
    ) -> Self {
        Self {
            store,
            key,
            token: Arc::from(endpoint.token.as_str()),
        }
    }
}

/// 内嵌 MCP 路由，挂在 `/internal` 下（该前缀不对外暴露静态资源，见 `web::API_PREFIXES`）。
pub fn router(state: BuiltinState) -> Router {
    Router::new()
        .route("/internal/mcp/{package_id}/{upstream_id}", post(handle))
        .with_state(state)
}

async fn handle(
    State(state): State<BuiltinState>,
    Path((package_id, upstream_id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !authorized(&state, &headers) {
        return error_response(
            StatusCode::UNAUTHORIZED,
            McpError::new(
                "MCP_UNAUTHORIZED",
                json!({"hint": "内置 MCP 端点只接受进程内 Bearer token"}),
            ),
        );
    }
    let request: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(error) => {
            return rpc_error(
                StatusCode::BAD_REQUEST,
                Value::Null,
                -32700,
                format!("请求不是合法 JSON: {error}"),
            );
        }
    };
    let Some(object) = request.as_object() else {
        return rpc_error(
            StatusCode::BAD_REQUEST,
            Value::Null,
            -32600,
            "请求必须是单个 JSON-RPC 2.0 对象".to_string(),
        );
    };
    let params = object.get("params").cloned().unwrap_or_else(|| json!({}));
    let id = object.get("id").cloned();
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        return rpc_error(
            StatusCode::BAD_REQUEST,
            id.unwrap_or(Value::Null),
            -32600,
            "请求缺少 method".to_string(),
        );
    };
    let Some(plugin) = plugin(&package_id) else {
        return error_response(
            StatusCode::NOT_FOUND,
            McpError::new("MCP_CATALOG_NOT_FOUND", json!({"id": package_id})),
        );
    };
    let package = plugin.package();
    let definition = match ensure_installation(&state, package, &upstream_id) {
        Ok(definition) => definition,
        Err(response) => return *response,
    };
    // 通知（无 id）不回包，但只有通过包/安装行校验后才静默成功；客户端只依赖 initialize 的响应。
    let Some(id) = id else {
        return StatusCode::ACCEPTED.into_response();
    };

    match method {
        "initialize" => rpc_result(
            id,
            json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": package.id, "version": package.version},
            }),
        ),
        "ping" => rpc_result(id, json!({})),
        "tools/list" => rpc_result(
            id,
            json!({
                "tools": plugin.tools().iter().map(|tool| json!({
                    "name": tool.name,
                    "description": tool.description,
                    "inputSchema": tool.input_schema,
                })).collect::<Vec<_>>(),
            }),
        ),
        "tools/call" => {
            let Some(tool) = params.get("name").and_then(Value::as_str) else {
                return rpc_error(
                    StatusCode::OK,
                    id,
                    -32602,
                    "tools/call 缺少 name".to_string(),
                );
            };
            let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
            let config = match build_config(&definition, package, &state.key) {
                Ok(config) => config,
                Err(error) => return rpc_result(id, tool_failure(error)),
            };
            match plugin.call(&config, tool, &arguments).await {
                Ok(result) => rpc_result(id, result),
                Err(error) => rpc_result(id, tool_failure(error)),
            }
        }
        other => rpc_error(StatusCode::OK, id, -32601, format!("未知方法: {other}")),
    }
}

/// 校验安装行属于本包、走 HTTP、且启用。
///
/// 错误直接是响应，装箱只为让 `Result` 不被一个冷路径的大 `Err` 撑大。
fn ensure_installation(
    state: &BuiltinState,
    package: &BuiltinPackage,
    upstream_id: &str,
) -> Result<McpUpstream, Box<Response>> {
    let definition = match state.store.upstream(upstream_id) {
        Ok(Some(definition)) => definition,
        Ok(None) => {
            return Err(Box::new(error_response(
                StatusCode::NOT_FOUND,
                McpError::new(
                    "MCP_UPSTREAM_NOT_FOUND",
                    json!({"id": upstream_id, "package": package.id}),
                ),
            )));
        }
        Err(error) => {
            return Err(Box::new(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                error,
            )));
        }
    };
    if definition.catalog_id.as_deref() != Some(package.id) {
        return Err(Box::new(error_response(
            StatusCode::NOT_FOUND,
            McpError::new(
                "MCP_UPSTREAM_NOT_FOUND",
                json!({"id": upstream_id, "package": package.id, "hint": "该上游不是这个包安装的"}),
            ),
        )));
    }
    if definition.transport != "http" {
        return Err(Box::new(error_response(
            StatusCode::FORBIDDEN,
            McpError::new(
                "MCP_BUILTIN_UNAVAILABLE",
                json!({"id": upstream_id, "package": package.id, "transport": definition.transport}),
            ),
        )));
    }
    if !definition.enabled {
        return Err(Box::new(error_response(
            StatusCode::FORBIDDEN,
            McpError::new(
                "MCP_UPSTREAM_DISABLED",
                json!({"id": upstream_id, "package": package.id}),
            ),
        )));
    }
    Ok(definition)
}

/// 把落库 env 还原成插件配置：只取包声明的字段，secret 解密失败即显式失败。
fn build_config(
    definition: &McpUpstream,
    package: &BuiltinPackage,
    key: &SecretKey,
) -> Result<BuiltinConfig, BuiltinError> {
    let mut values = BTreeMap::new();
    for field in package.fields {
        let Some(raw) = definition.env.get(field.env) else {
            continue;
        };
        if raw.is_empty() {
            continue;
        }
        let value = if field.secret && is_encrypted(raw) {
            key.decrypt(raw).map_err(|error| {
                BuiltinError::new(
                    "MCP_CONFIG_ERROR",
                    format!("{} 解密失败：{error}", field.label),
                    json!({"field": field.key}),
                )
            })?
        } else {
            raw.clone()
        };
        values.insert(field.key.to_string(), value);
    }
    Ok(BuiltinConfig::new(values))
}

fn authorized(state: &BuiltinState, headers: &HeaderMap) -> bool {
    let Some(presented) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    else {
        return false;
    };
    constant_time_eq(&state.token, presented.trim())
}

fn tool_failure(error: BuiltinError) -> Value {
    let text = serde_json::to_string(&json!({
        "code": error.code,
        "message": error.message,
        "data": error.data,
    }))
    .expect("错误对象可序列化");
    json!({"content": [{"type": "text", "text": text}], "isError": true})
}

fn rpc_result(id: Value, result: Value) -> Response {
    Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response()
}

fn rpc_error(status: StatusCode, id: Value, code: i64, message: String) -> Response {
    (
        status,
        Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": code, "message": message},
        })),
    )
        .into_response()
}

fn error_response(status: StatusCode, error: McpError) -> Response {
    (
        status,
        Json(json!({"error": {
            "code": error.code,
            "message": error.message,
            "data": error.data,
        }})),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn endpoint_urls_are_joined_and_matched_precisely() {
        let endpoint = BuiltinEndpoint::new("http://127.0.0.1:8000/internal/mcp/", "t");
        assert_eq!(
            endpoint.url_for("clickhouse-basic", "ch"),
            "http://127.0.0.1:8000/internal/mcp/clickhouse-basic/ch"
        );
        assert!(endpoint.matches("http://127.0.0.1:8000/internal/mcp/clickhouse-basic/ch"));
        assert!(!endpoint.matches("http://127.0.0.1:8000/internal/mcpX/clickhouse-basic/ch"));
        assert!(!endpoint.matches("http://127.0.0.1:8001/internal/mcp/clickhouse-basic/ch"));
    }

    #[test]
    fn plugin_registry_exposes_clickhouse() {
        let package = plugin("clickhouse-basic")
            .expect("内置插件已注册")
            .package();
        assert_eq!(package.id, "clickhouse-basic");
        assert!(plugin("no-such-package").is_none());
        assert_eq!(
            package
                .fields
                .iter()
                .map(|field| field.key)
                .collect::<Vec<_>>(),
            ["url", "username", "password", "database", "allowed_schemas"]
        );
    }

    const TEST_TOKEN: &str = "tok_internal_test";

    fn test_state(
        dir: &tempfile::TempDir,
    ) -> (BuiltinState, BuiltinEndpoint, Arc<CapabilityStore>) {
        let path = dir.path().join("registry.sqlite3");
        let store = Arc::new(
            CapabilityStore::open(path.to_str().expect("临时路径是 UTF-8"), None)
                .expect("打开测试库"),
        );
        let key = Arc::new(SecretKey::parse(&"11".repeat(32)).expect("固定密钥"));
        let endpoint = BuiltinEndpoint::new("http://127.0.0.1:8000", TEST_TOKEN);
        (
            BuiltinState::new(store.clone(), key, &endpoint),
            endpoint,
            store,
        )
    }

    fn install(store: &CapabilityStore, endpoint: &BuiltinEndpoint) {
        let definition = McpUpstream {
            id: "ch".to_string(),
            name: "ClickHouse 基础查询".to_string(),
            transport: "http".to_string(),
            url: endpoint.url_for("clickhouse-basic", "ch"),
            command: String::new(),
            args: Vec::new(),
            env: BTreeMap::new(),
            headers: BTreeMap::new(),
            group: "db.read.core".to_string(),
            category: "custom".to_string(),
            read_only: true,
            enabled: true,
            timeout_ms: 30_000,
            catalog_id: Some("clickhouse-basic".to_string()),
            catalog_version: Some("0.1.0".to_string()),
            created_at: String::new(),
            updated_at: String::new(),
        };
        store
            .insert_upstream(&definition, &[], "test")
            .expect("写入安装行");
    }

    async fn notify(state: &BuiltinState, package: &str, upstream: &str) -> StatusCode {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer tok_internal_test"),
        );
        handle(
            State(state.clone()),
            Path((package.to_string(), upstream.to_string())),
            headers,
            Bytes::from_static(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#),
        )
        .await
        .status()
    }

    /// 通知也不豁免包/安装行校验：未知包或未知上游仍要 404，只有合法安装行才静默 202。
    #[tokio::test]
    async fn notifications_still_require_a_registered_installation() {
        let dir = tempfile::tempdir().expect("临时目录");
        let (state, endpoint, store) = test_state(&dir);
        assert_eq!(
            notify(&state, "no-such-package", "ch").await,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            notify(&state, "clickhouse-basic", "no-such-upstream").await,
            StatusCode::NOT_FOUND
        );
        install(&store, &endpoint);
        assert_eq!(
            notify(&state, "clickhouse-basic", "ch").await,
            StatusCode::ACCEPTED
        );
    }
}
