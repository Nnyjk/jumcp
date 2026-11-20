//! 进程内 fake MCP 上游：JSON 与 SSE 两种响应，记录收到的 tools/call 与 Authorization。
//!
//! 只服务集成测试；真实转发路径仍走 `UpstreamManager` 与真实 axum 链路。

#![allow(dead_code)]

use axum::Router;
use axum::body::Body;
use axum::extract::State as AxumState;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub struct FakeUpstream {
    pub sse: bool,
    /// 工具名可运行时替换，用于验证 refresh 真的重拉；`None` = 默认三件套。
    pub tool_names: Arc<Mutex<Option<Vec<String>>>>,
    pub requests: Arc<Mutex<Vec<Value>>>,
    pub auth: Arc<Mutex<Option<String>>>,
    /// `query` 工具的结构化结果；`None` = 与内置 clickhouse 包同形的默认结果。
    pub query_result: Arc<Mutex<Option<Value>>>,
}

impl FakeUpstream {
    /// 替换 `query` 工具的 structuredContent（用于构造超预算/异常形状的结果）。
    pub fn set_query_result(&self, payload: Value) {
        *self.query_result.lock().expect("lock") = Some(payload);
    }

    pub fn set_tools(&self, names: &[&str]) {
        *self.tool_names.lock().expect("lock") =
            Some(names.iter().map(|n| (*n).to_string()).collect());
    }

    fn tools(&self) -> Vec<Value> {
        let names = self
            .tool_names
            .lock()
            .expect("lock")
            .clone()
            .unwrap_or_else(|| vec!["echo".into(), "explode".into(), "weird name".into()]);
        names
            .into_iter()
            .map(|name| {
                let mut tool = json!({
                    "name": name,
                    "description": format!("{name} 工具"),
                    "inputSchema": {"type":"object","properties":{"msg":{"type":"string"}}}
                });
                if tool["name"] == json!("echo") {
                    tool["annotations"] = json!({"readOnlyHint": true, "idempotentHint": true});
                }
                tool
            })
            .collect()
    }
}

fn fake_envelope(fake: &FakeUpstream, request: &Value) -> Value {
    let method = request["method"].as_str().unwrap_or("");
    let id = request["id"].clone();
    match method {
        "initialize" => json!({
            "jsonrpc":"2.0","id":id,
            "result":{"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"fake-upstream","version":"0"}}
        }),
        "tools/list" => json!({
            "jsonrpc":"2.0","id":id,
            "result":{"tools": fake.tools()}
        }),
        "tools/call" => {
            let tool = request["params"]["name"].as_str().unwrap_or("");
            fake.requests.lock().expect("lock").push(request.clone());
            if tool == "query" {
                // 形状与内置 clickhouse 包一致；收到的 arguments 由 `requests` 全量记录供断言。
                let payload = fake
                    .query_result
                    .lock()
                    .expect("lock")
                    .clone()
                    .unwrap_or_else(|| {
                        json!({
                            "columns": [
                                {"name": "day", "type": "String"},
                                {"name": "orders", "type": "UInt64"}
                            ],
                            "rows": [["2026-11-18", 3]],
                            "row_count": 1,
                            "truncated": false,
                            "elapsed_ms": 7
                        })
                    });
                return json!({
                    "jsonrpc":"2.0","id":id,
                    "result":{
                        "content":[{"type":"text","text":payload.to_string()}],
                        "structuredContent": payload,
                        "isError": false
                    }
                });
            }
            if tool == "explode" {
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":"boom"}})
            } else {
                json!({
                    "jsonrpc":"2.0","id":id,
                    "result":{"content":[{"type":"text","text":format!("echo:{tool}")}],"isError":false}
                })
            }
        }
        other => {
            json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("Method not found: {other}")}})
        }
    }
}

async fn fake_handler(
    AxumState(fake): AxumState<FakeUpstream>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let request: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    if let Some(auth) = headers.get("authorization").and_then(|v| v.to_str().ok()) {
        *fake.auth.lock().expect("lock") = Some(auth.to_string());
    }
    if request["method"]
        .as_str()
        .is_some_and(|m| m.starts_with("notifications/"))
    {
        return (StatusCode::ACCEPTED, String::new()).into_response();
    }
    let envelope = fake_envelope(&fake, &request);
    if fake.sse {
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .header("mcp-session-id", "sse-session")
            .body(Body::from(format!("event: message\ndata: {envelope}\n\n")))
            .expect("sse response");
    }
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .header("mcp-session-id", "json-session")
        .body(Body::from(envelope.to_string()))
        .expect("json response")
}

pub async fn spawn_fake(sse: bool) -> (String, FakeUpstream) {
    spawn_fake_named_parts(sse, None).await
}

pub async fn spawn_fake_named(tools: &[&str]) -> (String, FakeUpstream) {
    spawn_fake_named_parts(
        false,
        Some(tools.iter().map(|t| (*t).to_string()).collect()),
    )
    .await
}

async fn spawn_fake_named_parts(
    sse: bool,
    tool_names: Option<Vec<String>>,
) -> (String, FakeUpstream) {
    let fake = FakeUpstream {
        sse,
        tool_names: Arc::new(Mutex::new(tool_names)),
        ..Default::default()
    };
    let app = Router::new()
        .route("/mcp", post(fake_handler))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake upstream");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/mcp"), fake)
}

/// 一个当前无人监听的地址（注册后必定不可达）。
pub async fn dead_url() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    drop(listener);
    format!("http://{addr}/mcp")
}
