//! Streamable HTTP 传输：POST + JSON/SSE 响应、会话头、超时整形。

use super::UpstreamFailure;
use super::client::{next_request_id, resolve_refs, rpc_result};
use jumcp_core::registry::McpUpstream;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::sync::Mutex as AsyncMutex;

fn http_failure(upstream_id: &str, error: &reqwest::Error) -> UpstreamFailure {
    if error.is_timeout() {
        return UpstreamFailure::unknown(
            upstream_id,
            "MCP_EXEC_TIMEOUT",
            format!("上游请求超时: {error}"),
        );
    }
    UpstreamFailure::error(
        upstream_id,
        "MCP_UPSTREAM_UNREACHABLE",
        format!("上游不可达: {error}"),
    )
}

pub(super) struct HttpTransport {
    client: reqwest::Client,
    upstream_id: String,
    url: reqwest::Url,
    headers: BTreeMap<String, String>,
    session: AsyncMutex<Option<String>>,
    timeout: Duration,
}

impl HttpTransport {
    pub(super) fn new(client: reqwest::Client, def: &McpUpstream) -> Result<Self, UpstreamFailure> {
        let url = reqwest::Url::parse(&def.url).map_err(|_| {
            UpstreamFailure::error(&def.id, "MCP_UPSTREAM_INVALID", "url 不是合法的绝对地址")
        })?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(UpstreamFailure::error(
                &def.id,
                "MCP_UPSTREAM_INVALID",
                "url 只允许 http/https",
            ));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(UpstreamFailure::error(
                &def.id,
                "MCP_UPSTREAM_INVALID",
                "url 不允许携带 userinfo",
            ));
        }
        Ok(Self {
            client,
            upstream_id: def.id.clone(),
            url,
            headers: resolve_refs(&def.id, "headers", &def.headers)?,
            session: AsyncMutex::new(None),
            timeout: Duration::from_millis(def.timeout_ms as u64),
        })
    }

    fn build(&self, body: &Value) -> reqwest::RequestBuilder {
        let mut req = self
            .client
            .post(self.url.clone())
            .timeout(self.timeout)
            .header(
                reqwest::header::ACCEPT,
                "application/json, text/event-stream",
            )
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(body);
        for (key, value) in &self.headers {
            req = req.header(key.as_str(), value.as_str());
        }
        req
    }

    pub(super) async fn notify(&self, method: &str, params: Value) -> Result<(), UpstreamFailure> {
        let body = json!({"jsonrpc":"2.0","method":method,"params":params});
        let mut req = self.build(&body);
        if let Some(session) = self.session.lock().await.clone() {
            req = req.header("mcp-session-id", session);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| http_failure(&self.upstream_id, &e))?;
        if !resp.status().is_success() {
            return Err(UpstreamFailure::error(
                &self.upstream_id,
                "MCP_UPSTREAM_ERROR",
                format!("{method} 返回 HTTP {}", resp.status()),
            ));
        }
        Ok(())
    }

    pub(super) async fn request(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Value, UpstreamFailure> {
        let id = next_request_id();
        let body = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        let mut req = self.build(&body);
        if let Some(session) = self.session.lock().await.clone() {
            req = req.header("mcp-session-id", session);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| http_failure(&self.upstream_id, &e))?;
        if let Some(session) = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
            .filter(|v| !v.is_empty())
        {
            *self.session.lock().await = Some(session.to_string());
        }
        let status = resp.status();
        if !status.is_success() {
            return Err(UpstreamFailure::error(
                &self.upstream_id,
                "MCP_UPSTREAM_UNREACHABLE",
                format!("{method} 返回 HTTP {status}"),
            ));
        }
        let sse = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("text/event-stream"));
        let envelope = if sse {
            read_sse_response(resp, id).await.map_err(|message| {
                UpstreamFailure::error(
                    &self.upstream_id,
                    "MCP_UPSTREAM_ERROR",
                    format!("{method}: {message}"),
                )
            })?
        } else {
            resp.json::<Value>()
                .await
                .map_err(|e| http_failure(&self.upstream_id, &e))?
        };
        rpc_result(&self.upstream_id, method, envelope)
    }
}

/// 从 SSE 流里取出与 `id` 匹配的 JSON-RPC 响应（忽略进度/日志等通知事件）。
async fn read_sse_response(mut resp: reqwest::Response, id: i64) -> Result<Value, String> {
    let mut buffer = String::new();
    loop {
        let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| format!("SSE 读取失败: {e}"))?
        else {
            break;
        };
        buffer.push_str(&String::from_utf8_lossy(&chunk).replace("\r\n", "\n"));
        while let Some(position) = buffer.find("\n\n") {
            let event: String = buffer.drain(..position + 2).collect();
            let data = event
                .lines()
                .filter_map(|line| line.strip_prefix("data:").map(str::trim))
                .collect::<Vec<_>>()
                .join("\n");
            if data.is_empty() {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(&data) else {
                continue;
            };
            let matches_id = value
                .get("id")
                .and_then(Value::as_i64)
                .is_some_and(|got| got == id);
            if matches_id && (value.get("result").is_some() || value.get("error").is_some()) {
                return Ok(value);
            }
        }
    }
    Err("SSE 流已结束但未收到匹配响应".to_string())
}
