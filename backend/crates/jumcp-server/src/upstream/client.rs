//! 传输抽象：连接池里的一条上游连接（http / stdio 二选一）与配置引用解析。

use super::http::HttpTransport;
use super::stdio::StdioTransport;
use super::{MCP_PROTOCOL_VERSION, UpstreamFailure};
use jumcp_core::registry::{McpUpstream, UpstreamTool};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// `tools/list` 翻页上限（防御上游返回自引用 cursor）。
const MAX_TOOL_PAGES: usize = 20;

pub(super) struct UpstreamClient {
    transport: Transport,
}

enum Transport {
    // stdio 侧持有的进程句柄/管道明显更大，装箱避免每次 match 都搬动整个枚举。
    Http(Box<HttpTransport>),
    Stdio(Box<StdioTransport>),
}

impl UpstreamClient {
    pub(super) async fn connect(
        http: reqwest::Client,
        def: &McpUpstream,
    ) -> Result<Self, UpstreamFailure> {
        // env 的 `env:NAME` 引用对两种传输都在建连时解析：http 不使用 env，但写错的引用
        // 仍然要显式失败（静默忽略会让配置错误在切换传输后才暴露）。
        let env = resolve_refs(&def.id, "env", &def.env)?;
        let mut transport = match def.transport.as_str() {
            "http" => Transport::Http(Box::new(HttpTransport::new(http, def)?)),
            _ => Transport::Stdio(Box::new(StdioTransport::spawn(def, env).await?)),
        };
        transport.initialize(def).await?;
        Ok(Self { transport })
    }

    pub(super) async fn list_tools(&self, id: &str) -> Result<Vec<UpstreamTool>, UpstreamFailure> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_TOOL_PAGES {
            let params = cursor
                .as_ref()
                .map_or_else(|| json!({}), |c| json!({"cursor": c}));
            let result = self.request("tools/list", params).await?;
            for item in result
                .get("tools")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(name) = item.get("name").and_then(Value::as_str) else {
                    continue;
                };
                out.push(UpstreamTool {
                    name: name.to_string(),
                    description: item
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    input_schema: item
                        .get("inputSchema")
                        .filter(|v| v.is_object())
                        .cloned()
                        .unwrap_or_else(|| json!({"type":"object","properties":{}})),
                    annotations: item.get("annotations").filter(|v| v.is_object()).cloned(),
                });
            }
            cursor = result
                .get("nextCursor")
                .and_then(Value::as_str)
                .filter(|c| !c.is_empty())
                .map(str::to_string);
            if cursor.is_none() {
                return Ok(out);
            }
        }
        Err(UpstreamFailure::error(
            id,
            "MCP_UPSTREAM_ERROR",
            "tools/list 翻页超过上限（疑似自引用 cursor）",
        ))
    }

    pub(super) async fn call(
        &self,
        id: &str,
        tool: &str,
        arguments: &Value,
    ) -> Result<Value, UpstreamFailure> {
        self.request("tools/call", json!({"name": tool, "arguments": arguments}))
            .await
            .map_err(|mut e| {
                e.upstream_id = id.to_string();
                e
            })
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, UpstreamFailure> {
        self.transport.request(method, params).await
    }
}

impl Transport {
    async fn initialize(&mut self, def: &McpUpstream) -> Result<(), UpstreamFailure> {
        self.request(
            "initialize",
            json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "jumcp-mcp-router", "version": env!("CARGO_PKG_VERSION")}
            }),
        )
        .await?;
        // `notifications/initialized` 没有 id，也没有响应体；失败按建连失败处理。
        self.notify("notifications/initialized", json!({}), def)
            .await
    }

    async fn notify(
        &self,
        method: &str,
        params: Value,
        def: &McpUpstream,
    ) -> Result<(), UpstreamFailure> {
        match self {
            Transport::Http(http) => http.notify(method, params).await,
            Transport::Stdio(stdio) => stdio.notify(method, params).await.map_err(|mut e| {
                e.upstream_id = def.id.clone();
                e
            }),
        }
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, UpstreamFailure> {
        match self {
            Transport::Http(http) => http.request(method, params).await,
            Transport::Stdio(stdio) => stdio.request(method, params).await,
        }
    }
}

/// `env:NAME` / 字面量解析：建连时一次性完成，缺失的环境变量显式失败（不静默空串）。
pub(super) fn resolve_refs(
    upstream_id: &str,
    field: &str,
    map: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, UpstreamFailure> {
    let mut out = BTreeMap::new();
    for (key, value) in map {
        let resolved = match value.strip_prefix("env:") {
            Some(name) => std::env::var(name).map_err(|_| {
                UpstreamFailure::error(
                    upstream_id,
                    "MCP_UPSTREAM_INVALID",
                    format!("{field}.{key} 引用的环境变量 {name} 未设置"),
                )
            })?,
            None => value.clone(),
        };
        out.insert(key.clone(), resolved);
    }
    Ok(out)
}

pub(super) fn rpc_result(
    upstream_id: &str,
    method: &str,
    envelope: Value,
) -> Result<Value, UpstreamFailure> {
    if let Some(error) = envelope.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("upstream error");
        let code = error
            .get("code")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        return Err(UpstreamFailure::error(
            upstream_id,
            "MCP_UPSTREAM_ERROR",
            format!("{method} 失败: [{code}] {message}"),
        ));
    }
    match envelope.get("result") {
        Some(result) => Ok(result.clone()),
        None => Err(UpstreamFailure::error(
            upstream_id,
            "MCP_UPSTREAM_ERROR",
            format!("{method} 响应缺少 result"),
        )),
    }
}

pub(super) fn next_request_id() -> i64 {
    use std::sync::atomic::{AtomicI64, Ordering};
    static NEXT: AtomicI64 = AtomicI64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}
