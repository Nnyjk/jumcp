//! router 上游契约：管理面 CRUD/校验/脱敏/审计 + 动态插入后 tools/list 与 tools/call 的真实转发。
//!
//! 全程走真实 axum 链路（admin router + `/mcp`）与真实 SQLite 注册表，执行器换成 fake；
//! 上游侧用本进程内起的 axum fake MCP server（JSON 与 SSE 两种响应），不依赖外部网络。
//!
//! token 一律经 `POST /admin/tokens` 现场签发：内置与上游工具共用同一份 allowlist，
//! 测试必须显式列出它要看的名字（不做"引导令牌默认全量"的假设）。

use axum::Router;
use axum::body::Body;
use axum::extract::State as AxumState;
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use jumcp_core::audit::AuditLog;
use jumcp_core::config::{AdminTokens, DatasourceRef, GroupDef, RoleDef, Settings, TenantPolicy};
use jumcp_core::entry::EffectiveScope;
use jumcp_core::registry::{CapabilityStore, McpUpstream};
use jumcp_core::template::TemplateRegistry;
use jumcp_server::admin::AdminState;
use jumcp_server::executor::{ExecFuture, QueryExecutor, QueryParams};
use jumcp_server::mcp::{McpServer, http_router};
use jumcp_server::service::Service;
use jumcp_server::upstream::UpstreamManager;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

const ALICE_TOKEN: &str = "tok_alice_000000000";

struct FakeExecutor;

impl QueryExecutor for FakeExecutor {
    fn execute<'a>(&'a self, _scope: &'a EffectiveScope, _q: &'a QueryParams) -> ExecFuture<'a> {
        Box::pin(async { Ok(json!({"rows": [], "row_count": 0, "tokens": 1})) })
    }
}

fn settings(stdio: bool, write: bool) -> Settings {
    let mut roles = std::collections::HashMap::new();
    roles.insert(
        "analyst".into(),
        RoleDef {
            name: "数据分析".into(),
            default_groups: vec!["db.read.core".into(), "db.read.analytics".into()],
            default_budget: 2000,
        },
    );
    roles.insert(
        "core-only".into(),
        RoleDef {
            name: "只看核心".into(),
            default_groups: vec!["db.read.core".into()],
            default_budget: 2000,
        },
    );
    roles.insert(
        "admin".into(),
        RoleDef {
            name: "管理员".into(),
            default_groups: vec![
                "db.read.core".into(),
                "db.read.analytics".into(),
                "admin".into(),
            ],
            default_budget: 2500,
        },
    );
    Settings {
        admin_tokens: AdminTokens::parse(&format!("alice:{ALICE_TOKEN}")).expect("admin tokens"),
        policy: TenantPolicy {
            groups: vec![
                GroupDef {
                    code: "db.read.core".into(),
                    name: "核心数据只读".into(),
                    priority: 10,
                    min_budget: 800,
                },
                GroupDef {
                    code: "db.read.analytics".into(),
                    name: "分析数据只读".into(),
                    priority: 8,
                    min_budget: 800,
                },
                GroupDef {
                    code: "admin".into(),
                    name: "管理员通道".into(),
                    priority: 1,
                    min_budget: 800,
                },
            ],
            roles,
            tenant_budget_cap: 2000,
            allow_intersect: false,
        },
        budget_overflow: "fail".into(),
        upstream_stdio: stdio,
        upstream_write: write,
        datasources: vec![DatasourceRef {
            name: "main".into(),
            dsn_env: "JUMCP_TEST_MAIN_DSN".into(),
            engine: "clickhouse".into(),
            readonly: true,
            allowed_schemas: vec![],
            statement_timeout_ms: 5000,
        }],
        ..Default::default()
    }
}

struct Harness {
    app: Router,
    store: Arc<CapabilityStore>,
    audit: Arc<AuditLog>,
}

fn harness() -> Harness {
    harness_with(false, false)
}

fn harness_with(stdio: bool, write: bool) -> Harness {
    let audit = Arc::new(AuditLog::open(":memory:").expect("audit"));
    let store = Arc::new(CapabilityStore::open(":memory:", Some(audit.clone())).expect("store"));
    let boot = settings(stdio, write);
    let registry = Arc::new(TemplateRegistry::new(store.clone()));
    let manager = Arc::new(UpstreamManager::new(store.clone(), boot.upstream_write));
    let admin_tokens = boot.admin_tokens.clone();
    let service = Arc::new(
        Service::new(boot)
            .with_executor(Arc::new(FakeExecutor))
            .with_audit(audit.clone())
            .with_templates(registry)
            .with_upstreams(manager)
            .with_scope(scope(&["db.read.core", "db.read.analytics"])),
    );
    let admin = AdminState {
        service: service.clone(),
        store: store.clone(),
        tokens: admin_tokens,
    };
    let app = jumcp_server::admin::router(admin)
        .merge(http_router(Arc::new(McpServer::from_arc(service))));
    Harness { app, store, audit }
}

fn scope(groups: &[&str]) -> EffectiveScope {
    EffectiveScope {
        groups: groups.iter().map(|g| (*g).to_string()).collect(),
        budget: 2000,
        degraded: false,
        source: "default".into(),
        mode: "strict".into(),
        allowed_builtin_tools: None,
    }
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let request = builder
        .header(header::CONTENT_TYPE, "application/json")
        .body(body.map_or_else(Body::empty, |v| Body::from(v.to_string())))
        .expect("request");
    let response = app.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("json body")
    };
    (status, value)
}

/// 现场签发一个 agent token：allowlist 显式列出内置 id 与上游暴露名。
async fn issue_token(
    h: &Harness,
    caller: &str,
    role: &str,
    groups: &[&str],
    tools: &[&str],
) -> String {
    let (status, body) = call(
        &h.app,
        "POST",
        "/admin/tokens",
        Some(ALICE_TOKEN),
        Some(json!({
            "name": format!("token {caller}"),
            "role": role,
            "groups": groups,
            "builtin_tools": tools,
            "source": "agent",
            "caller": caller
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "签发令牌失败: {body}");
    body["token"].as_str().expect("token").to_string()
}

async fn mcp(app: &Router, token: &str, method: &str, params: Value) -> Value {
    let (status, body) = call(
        app,
        "POST",
        "/mcp",
        Some(token),
        Some(json!({"jsonrpc":"2.0","id":1,"method":method,"params":params})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "MCP 入口应回 200: {body}");
    body
}

async fn tool_names(app: &Router, token: &str) -> Vec<String> {
    let resp = mcp(app, token, "tools/list", json!({})).await;
    resp["result"]["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|t| t["name"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

async fn call_tool(app: &Router, token: &str, name: &str, arguments: Value) -> Value {
    mcp(
        app,
        token,
        "tools/call",
        json!({"name": name, "arguments": arguments}),
    )
    .await
}

fn upstream_body(id: &str, url: &str) -> Value {
    json!({
        "id": id,
        "name": format!("上游 {id}"),
        "transport": "http",
        "url": url,
        "args": [],
        "env": {},
        "headers": {},
        "group": "db.read.analytics",
        "category": "custom",
        "read_only": true,
        "enabled": true,
        "timeout_ms": 5000
    })
}

// ---------------------------- fake 上游 MCP server ----------------------------

#[derive(Clone, Default)]
struct FakeUpstream {
    sse: bool,
    /// 工具名可运行时替换，用于验证 refresh 真的重拉；`None` = 默认三件套。
    tool_names: Arc<Mutex<Option<Vec<String>>>>,
    requests: Arc<Mutex<Vec<Value>>>,
    auth: Arc<Mutex<Option<String>>>,
}

impl FakeUpstream {
    fn set_tools(&self, names: &[&str]) {
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

async fn spawn_fake(sse: bool) -> (String, FakeUpstream) {
    spawn_fake_named_parts(sse, None).await
}

async fn spawn_fake_named(tools: &[&str]) -> (String, FakeUpstream) {
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
async fn dead_url() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    drop(listener);
    format!("http://{addr}/mcp")
}

async fn create_upstream(h: &Harness, body: Value) -> (StatusCode, Value) {
    call(
        &h.app,
        "POST",
        "/admin/upstreams",
        Some(ALICE_TOKEN),
        Some(body),
    )
    .await
}

// ---------------------------- 管理面 CRUD / 脱敏 / 审计 ----------------------------

#[tokio::test]
async fn upstream_crud_masks_secrets_and_preserves_them_on_put() {
    let h = harness();
    let (url, _fake) = spawn_fake(false).await;
    let body = json!({
        "id": "analytics",
        "name": "分析上游",
        "transport": "http",
        "url": url,
        "args": [],
        "env": {
            "API_TOKEN": "literal-secret",
            "PLAIN": "v1",
            "REF": "env:JUMCP_TEST_UPSTREAM_REF"
        },
        "headers": {
            "Authorization": "Bearer supersecsret",
            "X-Trace": "trace-1"
        },
        "group": "db.read.analytics",
        "category": "analysis",
        "read_only": true,
        "enabled": true,
        "timeout_ms": 3000
    });
    let (status, created) = create_upstream(&h, body).await;
    assert_eq!(status, StatusCode::OK, "创建应成功: {created}");
    assert_eq!(created["id"], json!("analytics"));
    assert_eq!(created["status"], json!("unknown"), "尚未探测");
    assert_eq!(created["tool_count"], json!(0));
    assert_eq!(created["headers"]["Authorization"], json!("***"));
    assert_eq!(created["headers"]["X-Trace"], json!("trace-1"));
    assert_eq!(created["env"]["API_TOKEN"], json!("***"));
    assert_eq!(created["env"]["PLAIN"], json!("v1"));
    assert_eq!(created["env"]["REF"], json!("env:JUMCP_TEST_UPSTREAM_REF"));

    // 落库的是真实值，脱敏只发生在输出层。
    let stored = h.store.upstream("analytics").expect("store").expect("row");
    assert_eq!(stored.headers["Authorization"], "Bearer supersecsret");
    assert_eq!(stored.env["API_TOKEN"], "literal-secret");

    // GET 单项与列表都必须脱敏。
    let (status, item) = call(
        &h.app,
        "GET",
        "/admin/upstreams/analytics",
        Some(ALICE_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(item["headers"]["Authorization"], json!("***"));
    let (status, list) = call(&h.app, "GET", "/admin/upstreams", Some(ALICE_TOKEN), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["total"], json!(1));
    assert_eq!(list["items"][0]["env"]["API_TOKEN"], json!("***"));

    // PUT 里传 *** 必须保留旧值，不能把占位符写库。
    let put = json!({
        "name": "分析上游（改）",
        "transport": "http",
        "url": url,
        "args": [],
        "env": {"API_TOKEN": "***", "PLAIN": "v2", "REF": "***"},
        "headers": {"Authorization": "***", "X-Trace": "trace-2"},
        "group": "db.read.analytics",
        "category": "analysis",
        "read_only": true,
        "enabled": true,
        "timeout_ms": 3000
    });
    let (status, updated) = call(
        &h.app,
        "PUT",
        "/admin/upstreams/analytics",
        Some(ALICE_TOKEN),
        Some(put),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "更新应成功: {updated}");
    assert_eq!(updated["name"], json!("分析上游（改）"));
    assert_eq!(updated["headers"]["Authorization"], json!("***"));
    assert_eq!(updated["headers"]["X-Trace"], json!("trace-2"));
    assert_eq!(updated["env"]["API_TOKEN"], json!("***"));
    assert_eq!(updated["env"]["PLAIN"], json!("v2"));
    assert_eq!(updated["env"]["REF"], json!("env:JUMCP_TEST_UPSTREAM_REF"));

    let stored = h.store.upstream("analytics").expect("store").expect("row");
    assert_eq!(
        stored.headers["Authorization"], "Bearer supersecsret",
        "*** 必须是保留语义，不能落库"
    );
    assert_eq!(stored.env["API_TOKEN"], "literal-secret");
    assert_eq!(stored.headers["X-Trace"], "trace-2");

    // 新建时用 *** 没有旧值可保留 → 400。
    let mut bad = upstream_body("second", "http://127.0.0.1:1/mcp");
    bad["headers"] = json!({"Authorization": "***"});
    let (status, error) = create_upstream(&h, bad).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"]["code"], json!("MCP_UPSTREAM_INVALID"));

    // 删除。
    let (status, deleted) = call(
        &h.app,
        "DELETE",
        "/admin/upstreams/analytics",
        Some(ALICE_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(deleted["deleted"], json!(true));
    let (status, _) = call(
        &h.app,
        "GET",
        "/admin/upstreams/analytics",
        Some(ALICE_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn upstream_writes_audit_admin_change_without_secret_values() {
    let h = harness();
    let (url, _fake) = spawn_fake(false).await;
    let mut body = upstream_body("audited", &url);
    body["headers"] = json!({"Authorization": "Bearer topsecret-token"});
    body["env"] = json!({"API_TOKEN": "another-secret"});
    let (status, _) = create_upstream(&h, body).await;
    assert_eq!(status, StatusCode::OK);

    let events = h
        .audit
        .query(Some(&json!({"event_type": "admin_change"})))
        .expect("audit query");
    let create = events
        .iter()
        .find(|e| e.payload["object_id"] == json!("audited"))
        .expect("create event");
    assert_eq!(create.payload["object_type"], json!("upstream"));
    assert_eq!(create.payload["action"], json!("create"));
    assert_eq!(create.payload["data"]["transport"], json!("http"));
    assert_eq!(create.payload["data"]["group"], json!("db.read.analytics"));
    assert_eq!(create.payload["data"]["read_only"], json!(true));
    let serialized = serde_json::to_string(&create.payload).expect("json");
    assert!(
        !serialized.contains("topsecret-token") && !serialized.contains("another-secret"),
        "审计不得包含上游密钥明文: {serialized}"
    );

    // 更新：只有真实变化的字段进 changes，密钥值同样不出现。
    let put = json!({
        "name": "改名",
        "transport": "http",
        "url": url,
        "group": "db.read.analytics",
        "category": "analysis",
        "headers": {"Authorization": "***"},
        "env": {"API_TOKEN": "***"}
    });
    let (status, _) = call(
        &h.app,
        "PUT",
        "/admin/upstreams/audited",
        Some(ALICE_TOKEN),
        Some(put),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let events = h
        .audit
        .query(Some(&json!({"event_type": "admin_change"})))
        .expect("audit query");
    let update = events
        .iter()
        .find(|e| {
            e.payload["object_id"] == json!("audited") && e.payload["action"] == json!("update")
        })
        .expect("update event");
    let changes = update.payload["changes"].as_array().expect("changes");
    let fields: Vec<&str> = changes.iter().filter_map(|c| c["field"].as_str()).collect();
    assert!(fields.contains(&"name"));
    assert!(
        fields.contains(&"category"),
        "category 改了就要出现在 changes"
    );
    assert!(
        !fields.contains(&"headers"),
        "headers 键值未变（*** 保留了旧值），不得写 changes"
    );
    let serialized = serde_json::to_string(&update.payload).expect("json");
    assert!(!serialized.contains("topsecret-token"));
}

// ---------------------------- 动态插入 → tools/list / tools/call ----------------------------

#[tokio::test]
async fn inserted_upstream_tools_are_listed_and_forwarded() {
    let h = harness();
    let (url, fake) = spawn_fake(false).await;
    let mut body = upstream_body("analytics", &url);
    body["headers"] = json!({"X-Trace": "router"});
    let (status, item) = create_upstream(&h, body).await;
    assert_eq!(status, StatusCode::OK, "{item}");
    let token = issue_token(
        &h,
        "agent-upstream",
        "analyst",
        &["db.read.analytics"],
        &[
            "search_capabilities",
            "describe_capability",
            "list_groups",
            "analytics__echo",
            "analytics__weird-name",
        ],
    )
    .await;

    // 下一次 tools/list 立即出现前缀工具（无需重启）。
    let names = tool_names(&h.app, &token).await;
    assert!(names.contains(&"analytics__echo".to_string()), "{names:?}");
    assert!(
        names.contains(&"analytics__weird-name".to_string()),
        "非法字符必须转 -：{names:?}"
    );
    assert!(!names.contains(&"echo".to_string()), "上游工具不得裸名暴露");

    // tools/list 条目带来源坐标、授权组、分类与上游注解。
    let resp = mcp(&h.app, &token, "tools/list", json!({})).await;
    let echo = resp["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .find(|t| t["name"] == json!("analytics__echo"))
        .expect("echo tool");
    assert_eq!(echo["source"], json!("upstream"));
    assert_eq!(echo["server_id"], json!("analytics"));
    assert_eq!(echo["upstream_tool"], json!("echo"));
    assert_eq!(echo["group"], json!("db.read.analytics"));
    assert_eq!(echo["category"], json!("custom"));
    assert_eq!(echo["annotations"]["readOnlyHint"], json!(true));
    assert_eq!(echo["annotations"]["idempotentHint"], json!(true));

    // tools/call 转发到上游。
    let resp = call_tool(&h.app, &token, "analytics__echo", json!({"msg":"hi"})).await;
    assert_eq!(resp["result"]["isError"], json!(false), "{resp}");
    assert_eq!(resp["result"]["content"][0]["text"], json!("echo:echo"));
    let forwarded = fake.requests.lock().expect("lock").clone();
    assert_eq!(
        forwarded.last().expect("forwarded")["params"]["arguments"]["msg"],
        json!("hi")
    );

    // 审计：tool_call + object_type=tool + 上游坐标。
    let calls = h
        .audit
        .query(Some(
            &json!({"event_type": "tool_call", "object_id": "analytics__echo"}),
        ))
        .expect("audit query");
    let call = calls.last().expect("tool_call event");
    assert_eq!(call.payload["action"], json!("call"));
    assert_eq!(call.payload["data"]["upstream_id"], json!("analytics"));
    assert_eq!(call.payload["data"]["upstream_tool"], json!("echo"));
    assert_eq!(call.payload["data"]["outcome"], json!("ok"));
    assert!(
        call.payload["data"]["definition_hash"]
            .as_str()
            .is_some_and(|h| !h.is_empty())
    );
    assert_eq!(call.payload["data"]["arguments"]["msg"], json!("hi"));

    // 上游工具进 search / describe / list_groups。
    let searched = call_tool(
        &h.app,
        &token,
        "search_capabilities",
        json!({"query":"analytics__echo"}),
    )
    .await;
    assert_eq!(
        searched["result"]["content"][0]["id"],
        json!("analytics__echo")
    );
    assert_eq!(
        searched["result"]["content"][0]["category"],
        json!("custom")
    );
    let described = call_tool(
        &h.app,
        &token,
        "describe_capability",
        json!({"capability_id":"analytics__echo"}),
    )
    .await;
    assert_eq!(described["result"]["content"]["source"], json!("upstream"));
    let groups = call_tool(&h.app, &token, "list_groups", json!({})).await;
    assert!(
        groups["result"]["content"]
            .as_array()
            .expect("groups")
            .iter()
            .any(|g| g["group"] == json!("db.read.analytics")),
        "上游工具必须按注册组计数: {groups}"
    );
}

#[tokio::test]
async fn upstream_tools_respect_the_token_allowlist() {
    let h = harness();
    let (url, fake) = spawn_fake(false).await;
    let (status, _) = create_upstream(&h, upstream_body("analytics", &url)).await;
    assert_eq!(status, StatusCode::OK);

    // allowlist 不含上游暴露名：列表/检索不可见，调用回 MCP_CAPABILITY_NOT_FOUND 并写 policy_decision。
    let denied = issue_token(
        &h,
        "agent-no-upstream",
        "analyst",
        &["db.read.analytics"],
        &["search_capabilities"],
    )
    .await;
    let names = tool_names(&h.app, &denied).await;
    assert!(
        !names.iter().any(|n| n.contains("__")),
        "allowlist 之外的上游工具不得出现: {names:?}"
    );
    let searched = call_tool(
        &h.app,
        &denied,
        "search_capabilities",
        json!({"query":"analytics__", "limit":10}),
    )
    .await;
    assert_eq!(searched["result"]["content"], json!([]));

    let resp = call_tool(&h.app, &denied, "analytics__echo", json!({})).await;
    assert_eq!(resp["result"]["isError"], json!(true), "{resp}");
    assert_eq!(
        resp["result"]["content"]["error"]["code"],
        json!("MCP_CAPABILITY_NOT_FOUND")
    );
    assert!(
        fake.requests.lock().expect("lock").is_empty(),
        "allowlist 拒绝必须先于上游解析：不得产生任何上游调用"
    );
    let decisions = h
        .audit
        .query(Some(
            &json!({"event_type": "policy_decision", "object_id": "analytics__echo"}),
        ))
        .expect("audit query");
    assert!(
        !decisions.is_empty(),
        "allowlist 拒绝必须写 policy_decision"
    );
    assert_eq!(
        decisions.last().expect("decision").payload["error_code"],
        json!("MCP_CAPABILITY_NOT_FOUND")
    );

    // allowlist 含该名：正常可见/可调。
    let allowed = issue_token(
        &h,
        "agent-with-upstream",
        "analyst",
        &["db.read.analytics"],
        &["analytics__echo"],
    )
    .await;
    assert!(
        tool_names(&h.app, &allowed)
            .await
            .contains(&"analytics__echo".to_string())
    );
    let resp = call_tool(&h.app, &allowed, "analytics__echo", json!({})).await;
    assert_eq!(resp["result"]["isError"], json!(false), "{resp}");
    assert_eq!(
        fake.requests.lock().expect("lock").len(),
        1,
        "放行后只应产生一次上游调用"
    );
}

#[tokio::test]
async fn sse_upstream_is_supported() {
    let h = harness();
    let (url, _fake) = spawn_fake(true).await;
    let (status, item) = create_upstream(&h, upstream_body("sse", &url)).await;
    assert_eq!(status, StatusCode::OK, "{item}");
    let token = issue_token(
        &h,
        "agent-sse",
        "analyst",
        &["db.read.analytics"],
        &["sse__echo"],
    )
    .await;
    let names = tool_names(&h.app, &token).await;
    assert!(names.contains(&"sse__echo".to_string()), "{names:?}");
    let resp = call_tool(&h.app, &token, "sse__echo", json!({})).await;
    assert_eq!(resp["result"]["content"][0]["text"], json!("echo:echo"));
}

#[tokio::test]
async fn unreachable_upstream_is_isolated_and_reported() {
    let h = harness();
    let (good_url, _fake) = spawn_fake(false).await;
    assert_eq!(
        create_upstream(&h, upstream_body("good", &good_url))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        create_upstream(&h, upstream_body("dead", &dead_url().await))
            .await
            .0,
        StatusCode::OK
    );
    let token = issue_token(
        &h,
        "agent-isolation",
        "analyst",
        &["db.read.analytics"],
        &["good__echo", "dead__echo"],
    )
    .await;

    // tools/list 仍返回内置 + 可达上游；不可达上游只是没有工具。
    let names = tool_names(&h.app, &token).await;
    assert!(names.contains(&"good__echo".to_string()), "{names:?}");
    assert!(!names.iter().any(|n| n.starts_with("dead__")));

    // 状态：不可达上游降为 error（无可用工具）。
    let (status, list) = call(&h.app, "GET", "/admin/upstreams", Some(ALICE_TOKEN), None).await;
    assert_eq!(status, StatusCode::OK);
    let items = list["items"].as_array().expect("items");
    let dead = items
        .iter()
        .find(|i| i["id"] == json!("dead"))
        .expect("dead item");
    assert_eq!(dead["status"], json!("error"));
    assert!(
        dead["status_message"]
            .as_str()
            .is_some_and(|m| !m.is_empty())
    );

    // 直接调用不可达上游 → isError 结果（不是协议错误）。
    let resp = mcp(
        &h.app,
        &token,
        "tools/call",
        json!({"name":"dead__echo","arguments":{}}),
    )
    .await;
    assert_eq!(
        resp["result"]["isError"],
        json!(true),
        "不可达上游必须回 isError 结果: {resp}"
    );
}

#[tokio::test]
async fn upstream_errors_become_is_error_and_prefix_fallback_works() {
    let h = harness();
    let (url, fake) = spawn_fake(false).await;
    assert_eq!(
        create_upstream(&h, upstream_body("analytics", &url))
            .await
            .0,
        StatusCode::OK
    );
    let token = issue_token(
        &h,
        "agent-errors",
        "analyst",
        &["db.read.analytics"],
        &["analytics__explode", "analytics__ghost"],
    )
    .await;
    let _ = tool_names(&h.app, &token).await;

    // 上游 JSON-RPC error → isError 结果 + MCP_UPSTREAM_ERROR（不冒泡成本服务协议错误）。
    let resp = call_tool(&h.app, &token, "analytics__explode", json!({})).await;
    assert_eq!(resp["result"]["isError"], json!(true), "{resp}");
    assert_eq!(
        resp["result"]["content"]["error"]["code"],
        json!("MCP_UPSTREAM_ERROR")
    );
    assert!(resp.get("error").is_none(), "不得变成 JSON-RPC 协议错误");

    // 旧客户端缓存了不在本次列表里的名字：按 `server_id__` 前缀回退，仍转发给上游。
    let resp = call_tool(&h.app, &token, "analytics__ghost", json!({})).await;
    assert_eq!(resp["result"]["isError"], json!(false), "{resp}");
    assert_eq!(resp["result"]["content"][0]["text"], json!("echo:ghost"));
    let forwarded: Vec<String> = fake
        .requests
        .lock()
        .expect("lock")
        .iter()
        .filter_map(|r| r["params"]["name"].as_str().map(String::from))
        .collect();
    assert!(forwarded.contains(&"ghost".to_string()), "{forwarded:?}");

    // 完全没有前缀的名字仍然 -32602。
    let resp = mcp(
        &h.app,
        &token,
        "tools/call",
        json!({"name":"nope__x","arguments":{}}),
    )
    .await;
    assert_eq!(resp["error"]["code"], json!(-32602));
}

#[tokio::test]
async fn refresh_reports_status_and_audits() {
    let h = harness();
    let (url, _fake) = spawn_fake(false).await;
    assert_eq!(
        create_upstream(&h, upstream_body("analytics", &url))
            .await
            .0,
        StatusCode::OK
    );
    let (status, refreshed) = call(
        &h.app,
        "POST",
        "/admin/upstreams/analytics/refresh",
        Some(ALICE_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{refreshed}");
    assert_eq!(refreshed["status"], json!("ok"));
    assert_eq!(refreshed["tool_count"], json!(3));
    assert!(refreshed["last_checked_at"].as_str().is_some());

    // 失败的 refresh 仍是 200 + status=error。
    assert_eq!(
        create_upstream(&h, upstream_body("dead", &dead_url().await))
            .await
            .0,
        StatusCode::OK
    );
    let (status, refreshed) = call(
        &h.app,
        "POST",
        "/admin/upstreams/dead/refresh",
        Some(ALICE_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(refreshed["status"], json!("error"));

    let events = h
        .audit
        .query(Some(&json!({"event_type": "admin_change"})))
        .expect("audit query");
    let refresh = events
        .iter()
        .filter(|e| e.payload["object_id"] == json!("analytics"))
        .find(|e| e.payload["action"] == json!("refresh"))
        .expect("refresh event");
    assert_eq!(refresh.payload["data"]["tool_count"], json!(3));
    assert_eq!(refresh.payload["data"]["status"], json!("ok"));
    assert_eq!(refresh.payload["data"]["transport"], json!("http"));
}

#[tokio::test]
async fn refresh_reconnects_and_refetches_tools() {
    let h = harness();
    let (url, fake) = spawn_fake_named(&["alpha_tool"]).await;
    assert_eq!(
        create_upstream(&h, upstream_body("refresh", &url)).await.0,
        StatusCode::OK
    );
    let token = issue_token(
        &h,
        "agent-refresh",
        "analyst",
        &["db.read.analytics"],
        &["refresh__alpha_tool", "refresh__beta_tool"],
    )
    .await;
    let names = tool_names(&h.app, &token).await;
    assert!(
        names.contains(&"refresh__alpha_tool".to_string()),
        "{names:?}"
    );

    // 上游换了工具集；refresh 必须清缓存并重新 initialize + tools/list。
    fake.set_tools(&["beta_tool"]);
    let (status, refreshed) = call(
        &h.app,
        "POST",
        "/admin/upstreams/refresh/refresh",
        Some(ALICE_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{refreshed}");
    assert_eq!(refreshed["status"], json!("ok"));
    assert_eq!(refreshed["tool_count"], json!(1));

    let names = tool_names(&h.app, &token).await;
    assert!(
        names.contains(&"refresh__beta_tool".to_string()),
        "refresh 后必须看到新工具列表: {names:?}"
    );
    assert!(
        !names.contains(&"refresh__alpha_tool".to_string()),
        "旧工具必须消失: {names:?}"
    );
}

#[tokio::test]
async fn naming_conflicts_are_rejected_instead_of_renamed() {
    let h = harness();

    // 跨上游撞名（id 禁止 `__`，但 `_` 边界仍可构造）：`a` + `_b` 与 `a_` + `b` 都暴露成 `a___b`。
    let (first_url, _first) = spawn_fake_named(&["_b"]).await;
    let (second_url, _second) = spawn_fake_named(&["b"]).await;
    assert_eq!(
        create_upstream(&h, upstream_body("ab", &first_url)).await.0,
        StatusCode::OK
    );
    assert_eq!(
        create_upstream(&h, upstream_body("ab_", &second_url))
            .await
            .0,
        StatusCode::OK
    );
    let token = issue_token(
        &h,
        "agent-conflict",
        "analyst",
        &["db.read.analytics"],
        &["ab___b", "dup__dup-name"],
    )
    .await;

    // 先触发一次投影（并行首拉两台上游），状态才有内容。
    let names = tool_names(&h.app, &token).await;
    let (status, list) = call(&h.app, "GET", "/admin/upstreams", Some(ALICE_TOKEN), None).await;
    assert_eq!(status, StatusCode::OK);
    let items = list["items"].as_array().expect("items");
    let statuses: Vec<(&str, &str)> = ["ab", "ab_"]
        .iter()
        .map(|id| {
            let item = items
                .iter()
                .find(|i| i["id"] == json!(id))
                .expect("upstream item");
            (
                item["status"].as_str().unwrap_or_default(),
                item["status_message"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    let ok_count = statuses.iter().filter(|(s, _)| *s == "ok").count();
    let error_count = statuses.iter().filter(|(s, _)| *s == "error").count();
    assert_eq!(
        (ok_count, error_count),
        (1, 1),
        "撞名的两台上游必须一台成功、一台 error: {statuses:?}"
    );
    assert!(
        statuses
            .iter()
            .filter(|(s, _)| *s == "error")
            .all(|(_, message)| message.contains("重名")),
        "冲突必须显式说明原因: {statuses:?}"
    );

    assert_eq!(
        names.iter().filter(|n| *n == "ab___b").count(),
        1,
        "同一暴露名只能出现一次: {names:?}"
    );

    // 同一上游内规范化后重名（空格与 ? 都变 -）→ 整台拒绝。
    let (dup_url, _dup) = spawn_fake_named(&["dup name", "dup?name"]).await;
    assert_eq!(
        create_upstream(&h, upstream_body("dup", &dup_url)).await.0,
        StatusCode::OK
    );
    let (status, refreshed) = call(
        &h.app,
        "POST",
        "/admin/upstreams/dup/refresh",
        Some(ALICE_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(refreshed["status"], json!("error"));
    assert!(
        refreshed["status_message"]
            .as_str()
            .is_some_and(|m| m.contains("重名"))
    );
    assert!(
        !tool_names(&h.app, &token)
            .await
            .iter()
            .any(|n| n.starts_with("dup__"))
    );

    // 超长暴露名：拒绝接入，不截断（截断会让旧名回退拿错名字调上游）。
    let long_tool = "t".repeat(60);
    let (long_url, _long) = spawn_fake_named(&[long_tool.as_str()]).await;
    assert_eq!(
        create_upstream(&h, upstream_body("long", &long_url))
            .await
            .0,
        StatusCode::OK
    );
    let (status, refreshed) = call(
        &h.app,
        "POST",
        "/admin/upstreams/long/refresh",
        Some(ALICE_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(refreshed["status"], json!("error"));
    assert!(
        refreshed["status_message"]
            .as_str()
            .is_some_and(|m| m.contains("64") && m.contains("不合规")),
        "{refreshed}"
    );
    assert_eq!(refreshed["tool_count"], json!(0));

    // 保留 id 与非法 id（含连续 `__`）：注册期就拒绝。
    let (url, _fake) = spawn_fake(false).await;
    for reserved in ["admin", "builtin", "basic", "analysis", "jumcp"] {
        let (status, error) = create_upstream(&h, upstream_body(reserved, &url)).await;
        assert_eq!(status, StatusCode::CONFLICT, "id={reserved}");
        assert_eq!(error["error"]["code"], json!("MCP_UPSTREAM_CONFLICT"));
    }
    for invalid in ["Bad_Id", "a__b", "x"] {
        let (status, error) = create_upstream(&h, upstream_body(invalid, &url)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "id={invalid}");
        assert_eq!(error["error"]["code"], json!("MCP_UPSTREAM_INVALID"));
        assert_eq!(error["error"]["data"]["field"], json!("id"));
    }
}

#[tokio::test]
async fn concurrent_first_fetch_never_exposes_duplicate_names() {
    let h = harness();
    // 两台上游在第一次并行拉取时会撞名（`a`+`_b` 与 `a_`+`b` → `a___b`）。
    let (first_url, _first) = spawn_fake_named(&["_b"]).await;
    let (second_url, _second) = spawn_fake_named(&["b"]).await;
    assert_eq!(
        create_upstream(&h, upstream_body("ab", &first_url)).await.0,
        StatusCode::OK
    );
    assert_eq!(
        create_upstream(&h, upstream_body("ab_", &second_url))
            .await
            .0,
        StatusCode::OK
    );
    let token = issue_token(
        &h,
        "agent-concurrent",
        "analyst",
        &["db.read.analytics"],
        &["ab___b"],
    )
    .await;

    // 一次 tools/list：projection 用 JoinSet 并行首拉两台上游（竞争同一暴露名）。
    let names = tool_names(&h.app, &token).await;
    assert_eq!(
        names.iter().filter(|n| *n == "ab___b").count(),
        1,
        "{names:?}"
    );

    let (_, list) = call(&h.app, "GET", "/admin/upstreams", Some(ALICE_TOKEN), None).await;
    let items = list["items"].as_array().expect("items");
    let status_of = |id: &str| {
        items.iter().find(|i| i["id"] == json!(id)).expect("item")["status"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    };
    let statuses = [status_of("ab"), status_of("ab_")];
    assert!(
        statuses.iter().any(|s| s == "error"),
        "并发撞名必须有一台显式失败: {statuses:?}"
    );
    assert!(
        statuses.iter().any(|s| s == "ok"),
        "另一台必须正常: {statuses:?}"
    );
}

#[tokio::test]
async fn stdio_and_write_upstreams_are_gated_by_deployment_switches() {
    let h = harness();
    let mut stdio = upstream_body("local", "");
    stdio["transport"] = json!("stdio");
    stdio["url"] = Value::Null;
    stdio["command"] = json!("definitely-not-a-real-binary");
    let (status, error) = create_upstream(&h, stdio.clone()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"]["code"], json!("MCP_UPSTREAM_STDIO_DISABLED"));

    let (url, _fake) = spawn_fake(false).await;
    let mut write = upstream_body("writer", &url);
    write["read_only"] = json!(false);
    let (status, error) = create_upstream(&h, write.clone()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error["error"]["code"],
        json!("MCP_UPSTREAM_WRITE_FORBIDDEN")
    );

    // 开关打开后同样的请求可通过（stdio 仍会因为进程不存在而 error，但不是配置拒绝）。
    let h = harness_with(true, true);
    let (status, created) = create_upstream(&h, stdio).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["transport"], json!("stdio"));
    assert_eq!(created["url"], Value::Null);
    let (status, refreshed) = call(
        &h.app,
        "POST",
        "/admin/upstreams/local/refresh",
        Some(ALICE_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(refreshed["status"], json!("error"));
    let (status, created) = create_upstream(&h, write).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["read_only"], json!(false));
}

#[tokio::test]
async fn switching_transport_clears_the_other_side_fields() {
    let h = harness_with(true, true);
    let (url, _fake) = spawn_fake(false).await;

    // stdio → http：PUT 仍带着旧 command（或 null），落库必须是空。
    let mut stdio = upstream_body("switch", "");
    stdio["transport"] = json!("stdio");
    stdio["url"] = Value::Null;
    stdio["command"] = json!("old-binary");
    stdio["args"] = json!(["--stdio"]);
    let (status, created) = create_upstream(&h, stdio).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["command"], json!("old-binary"));

    let (status, updated) = call(
        &h.app,
        "PUT",
        "/admin/upstreams/switch",
        Some(ALICE_TOKEN),
        Some(json!({
            "transport": "http",
            "url": url,
            "command": "old-binary",
            "group": "db.read.analytics",
            "category": "custom",
            "timeout_ms": 5000
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["command"], Value::Null, "http 不得保留 command");
    assert_eq!(updated["url"], json!(url));
    let stored = h.store.upstream("switch").expect("store").expect("row");
    assert_eq!(stored.command, "");

    // http → stdio：反过来同样清空 url。
    let (status, updated) = call(
        &h.app,
        "PUT",
        "/admin/upstreams/switch",
        Some(ALICE_TOKEN),
        Some(json!({
            "transport": "stdio",
            "command": "new-binary",
            "url": url,
            "group": "db.read.analytics",
            "category": "custom",
            "timeout_ms": 5000
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["url"], Value::Null, "stdio 不得保留 url");
    assert_eq!(updated["command"], json!("new-binary"));
    let stored = h.store.upstream("switch").expect("store").expect("row");
    assert_eq!(stored.url, "");
    assert_eq!(stored.command, "new-binary");
}

#[tokio::test]
async fn upstream_validation_rejects_bad_fields() {
    let h = harness();
    let (url, _fake) = spawn_fake(false).await;

    let mut bad = upstream_body("bad", &url);
    bad["timeout_ms"] = json!(0);
    let (status, error) = create_upstream(&h, bad).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"]["code"], json!("MCP_UPSTREAM_INVALID"));

    let mut bad = upstream_body("bad", &url);
    bad["category"] = json!("weird");
    let (status, error) = create_upstream(&h, bad).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"]["code"], json!("MCP_UPSTREAM_INVALID"));

    let mut bad = upstream_body("bad", &url);
    bad["group"] = json!("db.read.missing");
    let (status, error) = create_upstream(&h, bad).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"]["code"], json!("MCP_UPSTREAM_INVALID"));
    assert!(error["error"]["data"]["known_groups"].is_array());

    // http 缺 url / 带 userinfo → 400。
    let mut bad = upstream_body("bad", "");
    bad["url"] = Value::Null;
    assert_eq!(create_upstream(&h, bad).await.0, StatusCode::BAD_REQUEST);

    let mut userinfo = upstream_body("userinfo", "http://alice:secret@127.0.0.1:1/mcp");
    userinfo["name"] = json!("带凭据的 URL");
    let (status, error) = create_upstream(&h, userinfo).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"]["code"], json!("MCP_UPSTREAM_INVALID"));
    let serialized = serde_json::to_string(&error).expect("json");
    assert!(!serialized.contains("secret@"), "错误体不得回显 userinfo");

    // args 超过 64 项。
    let mut bad = upstream_body("bad", &url);
    bad["args"] = json!(vec!["x"; 65]);
    assert_eq!(create_upstream(&h, bad).await.0, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn env_refs_resolve_at_connect_and_missing_env_fails() {
    let h = harness();
    let (url, _fake) = spawn_fake(false).await;

    // PATH 一定存在：`env:PATH` 只用于证明引用会被解析（值本身不回显）。
    let mut ok_body = upstream_body("refs", &url);
    ok_body["headers"] = json!({"X-From-Env": "env:PATH"});
    assert_eq!(create_upstream(&h, ok_body).await.0, StatusCode::OK);
    let (status, refreshed) = call(
        &h.app,
        "POST",
        "/admin/upstreams/refs/refresh",
        Some(ALICE_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(refreshed["status"], json!("ok"), "{refreshed}");
    // GET 只回显引用本身，不回显解析后的值。
    assert_eq!(refreshed["headers"]["X-From-Env"], json!("env:PATH"));

    // 缺失的环境变量：建连显式失败，status_message 指出变量名。
    let mut missing = upstream_body("missing", &url);
    missing["env"] = json!({"API_TOKEN": "env:JUMCP_ROUTER_SURELY_MISSING_VAR"});
    assert_eq!(create_upstream(&h, missing).await.0, StatusCode::OK);
    let (status, refreshed) = call(
        &h.app,
        "POST",
        "/admin/upstreams/missing/refresh",
        Some(ALICE_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(refreshed["status"], json!("error"));
    assert!(
        refreshed["status_message"]
            .as_str()
            .is_some_and(|m| m.contains("JUMCP_ROUTER_SURELY_MISSING_VAR")),
        "{refreshed}"
    );
    assert_eq!(
        refreshed["env"]["API_TOKEN"],
        json!("env:JUMCP_ROUTER_SURELY_MISSING_VAR")
    );
}

// ---------------------------- 分类、scope 与写开关 ----------------------------

#[tokio::test]
async fn builtin_categories_are_exposed_and_filterable() {
    let h = harness();
    let token = issue_token(
        &h,
        "agent-categories",
        "analyst",
        &["db.read.core"],
        &["search_capabilities", "describe_capability", "list_groups"],
    )
    .await;
    let resp = mcp(&h.app, &token, "tools/list", json!({})).await;
    let tools = resp["result"]["tools"].as_array().expect("tools");
    let category_of = |name: &str| -> Value {
        tools
            .iter()
            .find(|t| t["name"] == json!(name))
            .map(|t| t["category"].clone())
            .unwrap_or(Value::Null)
    };
    assert_eq!(category_of("search_capabilities"), json!("basic"));
    assert_eq!(category_of("describe_capability"), json!("basic"));
    assert_eq!(category_of("list_groups"), json!("basic"));

    // 数据入口不在 allowlist 里 → 列表不出现，但 search 的分类过滤仍然可用。
    assert_eq!(category_of("db_query_readonly"), Value::Null);

    let basic = call_tool(
        &h.app,
        &token,
        "search_capabilities",
        json!({"query":"", "category":"basic", "limit":50}),
    )
    .await;
    let ids: Vec<&str> = basic["result"]["content"]
        .as_array()
        .expect("hits")
        .iter()
        .filter_map(|h| h["id"].as_str())
        .collect();
    assert!(ids.contains(&"search_capabilities"));
    assert!(!ids.contains(&"db_query_readonly"));

    // admin 组的工具对 analyst 不可见（分类不改变授权）。
    let admin = call_tool(
        &h.app,
        &token,
        "search_capabilities",
        json!({"query":"admin_", "category":"admin", "limit":50}),
    )
    .await;
    assert_eq!(admin["result"]["content"], json!([]));

    // 数据入口（db_query_readonly / db_run_template）归 basic：allowlist 命中后
    // 既出现在 tools/list，也能被 basic 分类过滤命中。
    let data_token = issue_token(
        &h,
        "agent-data",
        "analyst",
        &["db.read.core"],
        &[
            "search_capabilities",
            "db_query_readonly",
            "db_run_template",
        ],
    )
    .await;
    let data_basic = call_tool(
        &h.app,
        &data_token,
        "search_capabilities",
        json!({"query":"", "category":"basic", "limit":50}),
    )
    .await;
    let data_hits = data_basic["result"]["content"].as_array().expect("hits");
    for id in ["db_query_readonly", "db_run_template"] {
        let hit = data_hits
            .iter()
            .find(|h| h["id"] == json!(id))
            .unwrap_or_else(|| panic!("{id} 应被 basic 过滤命中: {data_basic}"));
        assert_eq!(hit["category"], json!("basic"), "{id}");
    }
    let names = tool_names(&h.app, &data_token).await;
    assert!(names.contains(&"db_query_readonly".to_string()));
    assert!(names.contains(&"db_run_template".to_string()));

    // analysis 归工具列表分析与变更提交审计，admin 只剩审计查询：都要 admin 组 +
    // allowlist 双重命中才可见，分类本身不放宽权限。
    let admin_token = issue_token(
        &h,
        "agent-category-admin",
        "admin",
        &["admin"],
        &[
            "search_capabilities",
            "admin_review_queue",
            "admin_register_template",
            "admin_set_visibility",
            "admin_audit_queries",
        ],
    )
    .await;
    let admin_resp = mcp(&h.app, &admin_token, "tools/list", json!({})).await;
    let admin_tools = admin_resp["result"]["tools"].as_array().expect("tools");
    let admin_category_of = |name: &str| -> Value {
        admin_tools
            .iter()
            .find(|t| t["name"] == json!(name))
            .map(|t| t["category"].clone())
            .unwrap_or(Value::Null)
    };
    for id in [
        "admin_review_queue",
        "admin_register_template",
        "admin_set_visibility",
    ] {
        assert_eq!(admin_category_of(id), json!("analysis"), "{id}");
    }
    assert_eq!(admin_category_of("admin_audit_queries"), json!("admin"));

    let analysis = call_tool(
        &h.app,
        &admin_token,
        "search_capabilities",
        json!({"query":"", "category":"analysis", "limit":50}),
    )
    .await;
    let analysis_ids: Vec<&str> = analysis["result"]["content"]
        .as_array()
        .expect("hits")
        .iter()
        .filter_map(|h| h["id"].as_str())
        .collect();
    for id in [
        "admin_review_queue",
        "admin_register_template",
        "admin_set_visibility",
    ] {
        assert!(analysis_ids.contains(&id), "{id} 应被 analysis 过滤命中");
    }
    assert!(!analysis_ids.contains(&"admin_audit_queries"));

    let admin_only = call_tool(
        &h.app,
        &admin_token,
        "search_capabilities",
        json!({"query":"", "category":"admin", "limit":50}),
    )
    .await;
    let admin_only_ids: Vec<&str> = admin_only["result"]["content"]
        .as_array()
        .expect("hits")
        .iter()
        .filter_map(|h| h["id"].as_str())
        .collect();
    assert_eq!(admin_only_ids, vec!["admin_audit_queries"]);
}

#[tokio::test]
async fn upstream_tools_follow_the_token_group_scope() {
    let h = harness();
    let (url, _fake) = spawn_fake(false).await;
    assert_eq!(
        create_upstream(&h, upstream_body("analytics", &url))
            .await
            .0,
        StatusCode::OK
    );
    let allowed = issue_token(
        &h,
        "agent-scope-full",
        "analyst",
        &["db.read.analytics"],
        &["analytics__echo"],
    )
    .await;
    let core_only = issue_token(
        &h,
        "agent-scope-core",
        "core-only",
        &["db.read.core"],
        &["analytics__echo"],
    )
    .await;

    assert!(
        tool_names(&h.app, &allowed)
            .await
            .contains(&"analytics__echo".to_string())
    );
    let names = tool_names(&h.app, &core_only).await;
    assert!(!names.iter().any(|n| n.starts_with("analytics__")));
    let resp = mcp(
        &h.app,
        &core_only,
        "tools/call",
        json!({"name":"analytics__echo","arguments":{}}),
    )
    .await;
    assert_eq!(
        resp["error"]["code"],
        json!(-32602),
        "越界组的调用名必须与不存在同构: {resp}"
    );
}

/// 注册表里的 managed capability 与上游暴露名撞名：整台上游拒绝，不静默去重。
fn publish_managed_capability(store: &CapabilityStore, id: &str) {
    store
        .register(
            id,
            "tool",
            "db.read.analytics",
            "read",
            json!({"description": id}),
        )
        .expect("register");
    store.submit_for_review(id, "alice").expect("submit");
    store
        .promote_to_published(id, "ap-pub", "bob")
        .expect("publish");
    store
        .set_visibility(id, Some(true), Some(true), "alice")
        .expect("visibility");
}

#[tokio::test]
async fn upstream_conflicts_with_managed_capability_are_rejected() {
    let h = harness();
    publish_managed_capability(&h.store, "xy__y");
    let (url, _fake) = spawn_fake_named(&["y"]).await;
    assert_eq!(
        create_upstream(&h, upstream_body("xy", &url)).await.0,
        StatusCode::OK
    );

    let (status, refreshed) = call(
        &h.app,
        "POST",
        "/admin/upstreams/xy/refresh",
        Some(ALICE_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(refreshed["status"], json!("error"), "{refreshed}");
    assert!(
        refreshed["status_message"]
            .as_str()
            .is_some_and(|m| m.contains("注册表能力重名")),
        "{refreshed}"
    );
    assert_eq!(refreshed["tool_count"], json!(0));

    // tools/list 只保留 managed 的那一份，不出现重复 id。
    let token = issue_token(
        &h,
        "agent-managed-conflict",
        "analyst",
        &["db.read.analytics"],
        &["xy__y"],
    )
    .await;
    let names = tool_names(&h.app, &token).await;
    assert_eq!(
        names.iter().filter(|n| *n == "xy__y").count(),
        1,
        "{names:?}"
    );
}

#[tokio::test]
async fn admin_write_tools_declare_non_readonly_annotations() {
    let h = harness();
    let token = issue_token(
        &h,
        "agent-annotations",
        "admin",
        &["admin"],
        &[
            "admin_register_template",
            "admin_review_queue",
            "admin_audit_queries",
            "admin_set_visibility",
        ],
    )
    .await;
    let resp = mcp(&h.app, &token, "tools/list", json!({})).await;
    let tools = resp["result"]["tools"].as_array().expect("tools");
    let annotations_of = |name: &str| -> Value {
        tools
            .iter()
            .find(|t| t["name"] == json!(name))
            .map(|t| t["annotations"].clone())
            .unwrap_or(Value::Null)
    };
    for write_tool in ["admin_register_template", "admin_set_visibility"] {
        let annotations = annotations_of(write_tool);
        assert_eq!(annotations["readOnlyHint"], json!(false), "{write_tool}");
        assert_eq!(annotations["destructiveHint"], json!(false), "{write_tool}");
        assert_eq!(annotations["idempotentHint"], json!(false), "{write_tool}");
        assert_eq!(annotations["openWorldHint"], json!(false), "{write_tool}");
    }
    for read_tool in ["admin_review_queue", "admin_audit_queries"] {
        let annotations = annotations_of(read_tool);
        assert_eq!(annotations["readOnlyHint"], json!(true), "{read_tool}");
        assert_eq!(annotations["destructiveHint"], json!(false), "{read_tool}");
    }
}

#[tokio::test]
async fn upstream_annotations_default_by_risk() {
    let h = harness_with(false, true);
    let (read_url, _read) = spawn_fake_named(&["plain"]).await;
    let (write_url, _write) = spawn_fake_named(&["plain"]).await;
    assert_eq!(
        create_upstream(&h, upstream_body("ro", &read_url)).await.0,
        StatusCode::OK
    );
    let mut write_body = upstream_body("rw", &write_url);
    write_body["read_only"] = json!(false);
    assert_eq!(create_upstream(&h, write_body).await.0, StatusCode::OK);

    let token = issue_token(
        &h,
        "agent-risk-annotations",
        "analyst",
        &["db.read.analytics"],
        &["ro__plain", "rw__plain"],
    )
    .await;
    let resp = mcp(&h.app, &token, "tools/list", json!({})).await;
    let tools = resp["result"]["tools"].as_array().expect("tools");
    let annotations_of = |name: &str| -> Value {
        tools
            .iter()
            .find(|t| t["name"] == json!(name))
            .map(|t| t["annotations"].clone())
            .unwrap_or(Value::Null)
    };

    // 只读上游没声明注解 → 只读注解。
    let read_only = annotations_of("ro__plain");
    assert_eq!(read_only["readOnlyHint"], json!(true));
    assert_eq!(read_only["idempotentHint"], json!(true));

    // 可写上游没声明注解 → 协议最保守缺省值，绝不出现 readOnlyHint=true。
    let writable = annotations_of("rw__plain");
    assert_eq!(writable["readOnlyHint"], json!(false));
    assert_eq!(writable["destructiveHint"], json!(true));
    assert_eq!(writable["idempotentHint"], json!(false));
    assert_eq!(writable["openWorldHint"], json!(true));
}

#[tokio::test]
async fn write_upstream_rows_are_hidden_when_deployment_forbids_writes() {
    let h = harness_with(false, false);
    let (url, _fake) = spawn_fake(false).await;
    // 绕过管理面直接落库：模拟"配置文件/旧部署留下的 read_only=false"。
    let def = McpUpstream {
        id: "writer".into(),
        name: "写上游".into(),
        transport: "http".into(),
        url: url.clone(),
        command: String::new(),
        args: Vec::new(),
        env: Default::default(),
        headers: Default::default(),
        group: "db.read.analytics".into(),
        category: "custom".into(),
        read_only: false,
        enabled: true,
        timeout_ms: 5000,
        created_at: String::new(),
        updated_at: String::new(),
    };
    h.store.insert_upstream(&def, "alice").expect("insert");
    let token = issue_token(
        &h,
        "agent-write-gate",
        "analyst",
        &["db.read.analytics"],
        &["writer__echo"],
    )
    .await;

    let names = tool_names(&h.app, &token).await;
    assert!(
        !names.iter().any(|n| n.starts_with("writer__")),
        "写上游在开关关闭时不得投影: {names:?}"
    );
    let resp = mcp(
        &h.app,
        &token,
        "tools/call",
        json!({"name":"writer__echo","arguments":{}}),
    )
    .await;
    assert_eq!(resp["error"]["code"], json!(-32602));
}

// ---------------------------- basic 逐工具通道 ----------------------------

async fn get_upstream_tools(h: &Harness, id: &str) -> (StatusCode, Value) {
    call(
        &h.app,
        "GET",
        &format!("/admin/upstreams/{id}/tools"),
        Some(ALICE_TOKEN),
        None,
    )
    .await
}

async fn put_basic_tools(h: &Harness, id: &str, tools: Value) -> (StatusCode, Value) {
    call(
        &h.app,
        "PUT",
        &format!("/admin/upstreams/{id}/tools/basic"),
        Some(ALICE_TOKEN),
        Some(json!({"tools": tools})),
    )
    .await
}

async fn refresh(h: &Harness, id: &str) -> (StatusCode, Value) {
    call(
        &h.app,
        "POST",
        &format!("/admin/upstreams/{id}/refresh"),
        Some(ALICE_TOKEN),
        None,
    )
    .await
}

fn tool_row<'a>(body: &'a Value, name: &str) -> &'a Value {
    body["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .find(|t| t["name"] == json!(name))
        .unwrap_or_else(|| panic!("工具 {name} 不在视图中: {body}"))
}

#[tokio::test]
async fn upstream_group_basic_is_rejected_on_create_and_update() {
    let h = harness();
    let (url, _fake) = spawn_fake(false).await;

    // 整台上游注册到 basic 会让全部工具（含写工具）对所有人可见：只能逐工具加入。
    let mut body = upstream_body("mixed", &url);
    body["group"] = json!("basic");
    let (status, resp) = create_upstream(&h, body.clone()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{resp}");
    assert_eq!(resp["error"]["code"], json!("MCP_UPSTREAM_INVALID"));
    assert_eq!(resp["error"]["data"]["field"], json!("group"));

    // 已存在的上游改成 basic 同样拒绝。
    let (status, _) = create_upstream(&h, upstream_body("mixed", &url)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, resp) = call(
        &h.app,
        "PUT",
        "/admin/upstreams/mixed",
        Some(ALICE_TOKEN),
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{resp}");
    assert_eq!(resp["error"]["code"], json!("MCP_UPSTREAM_INVALID"));
    assert_eq!(resp["error"]["data"]["field"], json!("group"));
}

#[tokio::test]
async fn basic_tool_override_is_merged_and_idempotent() {
    let h = harness();
    let (url, _fake) = spawn_fake(false).await;
    let (status, _) = create_upstream(&h, upstream_body("mixed", &url)).await;
    assert_eq!(status, StatusCode::OK);

    // 未 refresh 时只读缓存：状态 unknown、没有工具行，GET 不触网。
    let (status, body) = get_upstream_tools(&h, "mixed").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], json!("unknown"));
    assert_eq!(body["total"], json!(0));
    assert_eq!(body["basic_total"], json!(0));
    assert_eq!(body["last_checked_at"], Value::Null);

    let (status, _) = refresh(&h, "mixed").await;
    assert_eq!(status, StatusCode::OK);
    let (_, body) = get_upstream_tools(&h, "mixed").await;
    assert_eq!(body["total"], json!(3), "{body}");
    for tool in body["tools"].as_array().expect("tools") {
        assert_eq!(tool["group"], json!("db.read.analytics"), "{tool}");
        assert_eq!(tool["group_source"], json!("upstream"));
        assert_eq!(tool["basic"], json!(false));
        assert_eq!(tool["overridden"], json!(false));
        assert_eq!(tool["eligible"], json!(true));
        assert_eq!(tool["orphaned"], json!(false));
    }
    assert_eq!(
        tool_row(&body, "echo")["exposed_name"],
        json!("mixed__echo")
    );

    // 加入 basic：响应带 changed 与新的生效组。
    let (status, body) = put_basic_tools(&h, "mixed", json!({"echo": true})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["changed"],
        json!([{"field": "tool_group", "tool": "echo", "before": null, "after": "basic"}])
    );
    assert_eq!(body["basic_total"], json!(1));
    let echo = tool_row(&body, "echo");
    assert_eq!(echo["group"], json!("basic"));
    assert_eq!(echo["group_source"], json!("basic"));
    assert_eq!(echo["basic"], json!(true));
    assert_eq!(echo["overridden"], json!(true));
    // 未覆盖的工具继续继承上游组。
    assert_eq!(
        tool_row(&body, "explode")["group"],
        json!("db.read.analytics")
    );

    let audit_rows = || {
        h.audit
            .query(Some(
                &json!({"event_type": "admin_change", "object_id": "mixed"}),
            ))
            .expect("audit query")
            .into_iter()
            .filter(|e| e.payload["action"] == json!("update_tool_groups"))
            .count()
    };
    assert_eq!(audit_rows(), 1, "首次加入必须写一条审计");
    let events = h
        .audit
        .query(Some(
            &json!({"event_type": "admin_change", "object_id": "mixed"}),
        ))
        .expect("audit query");
    let event = events
        .iter()
        .find(|e| e.payload["action"] == json!("update_tool_groups"))
        .expect("update_tool_groups 审计");
    assert_eq!(event.payload["object_type"], json!("upstream"));
    assert_eq!(
        event.payload["changes"],
        json!([{"field": "tool_group", "tool": "echo", "before": null, "after": "basic"}])
    );
    assert!(
        event.payload["data"].get("env").is_none()
            && event.payload["data"].get("headers").is_none(),
        "审计不得泄露 env/headers: {event:?}"
    );

    // 幂等：重复同值不写库、不写审计、changed 为空。
    let (status, body) = put_basic_tools(&h, "mixed", json!({"echo": true})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["changed"], json!([]));
    assert_eq!(body["basic_total"], json!(1));
    assert_eq!(audit_rows(), 1, "无变化不得新增审计");

    // 清除：false 删除覆盖，回到继承上游组。
    let (status, body) = put_basic_tools(&h, "mixed", json!({"echo": false})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["changed"],
        json!([{"field": "tool_group", "tool": "echo", "before": "basic", "after": null}])
    );
    assert_eq!(body["basic_total"], json!(0));
    assert_eq!(tool_row(&body, "echo")["group"], json!("db.read.analytics"));
    assert_eq!(tool_row(&body, "echo")["overridden"], json!(false));
    assert_eq!(audit_rows(), 2);
}

#[tokio::test]
async fn basic_tool_override_rejects_unknown_write_and_admin_tools() {
    let h = harness();
    let (url, _fake) = spawn_fake(false).await;
    let (status, _) = create_upstream(&h, upstream_body("mixed", &url)).await;
    assert_eq!(status, StatusCode::OK);
    refresh(&h, "mixed").await;

    // 未知工具：整请求拒绝，data.unknown 指出是哪一个。
    let (status, body) = put_basic_tools(&h, "mixed", json!({"nope": true})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], json!("MCP_UPSTREAM_TOOL_UNKNOWN"));
    assert_eq!(body["error"]["data"]["unknown"], json!(["nope"]));
    // 未知与已知混在同一请求时也不做部分写入。
    let (status, body) = put_basic_tools(&h, "mixed", json!({"echo": true, "nope": true})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let (_, view) = get_upstream_tools(&h, "mixed").await;
    assert_eq!(view["basic_total"], json!(0), "拒绝的请求不得落库");

    // 不存在的上游 → 404。
    let (status, body) = put_basic_tools(&h, "ghost", json!({"echo": true})).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], json!("MCP_UPSTREAM_NOT_FOUND"));

    // 写上游：basic 只允许只读工具。
    let hw = harness_with(false, true);
    let (wurl, _wfake) = spawn_fake(false).await;
    let mut wbody = upstream_body("writer", &wurl);
    wbody["read_only"] = json!(false);
    let (status, body) = create_upstream(&hw, wbody).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    refresh(&hw, "writer").await;
    let (status, body) = put_basic_tools(&hw, "writer", json!({"echo": true})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        body["error"]["code"],
        json!("MCP_UPSTREAM_TOOL_NOT_READ_ONLY")
    );

    // 上游类别是 admin：维护通道不能借 basic 变成人人可见。
    let (aurl, _afake) = spawn_fake(false).await;
    let mut abody = upstream_body("ops", &aurl);
    abody["category"] = json!("admin");
    let (status, body) = create_upstream(&h, abody).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    refresh(&h, "ops").await;
    let (status, body) = put_basic_tools(&h, "ops", json!({"echo": true})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        body["error"]["code"],
        json!("MCP_UPSTREAM_TOOL_NOT_ELIGIBLE")
    );
    assert_eq!(
        body["error"]["data"]["reason"],
        json!("upstream_category_admin")
    );

    // 工具名以 admin_ 开头：同上。
    let (purl, _pfake) = spawn_fake_named(&["admin_purge"]).await;
    let (status, body) = create_upstream(&h, upstream_body("danger", &purl)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    refresh(&h, "danger").await;
    let (status, body) = put_basic_tools(&h, "danger", json!({"admin_purge": true})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        body["error"]["code"],
        json!("MCP_UPSTREAM_TOOL_NOT_ELIGIBLE")
    );
    assert_eq!(body["error"]["data"]["reason"], json!("admin_prefix"));
    let (_, view) = get_upstream_tools(&h, "danger").await;
    assert_eq!(tool_row(&view, "admin_purge")["eligible"], json!(false));
}

#[tokio::test]
async fn basic_tool_override_survives_refresh_and_is_orphaned_not_dropped() {
    let h = harness_with(false, false);
    let (url, fake) = spawn_fake_named(&["echo", "keep"]).await;
    let (status, _) = create_upstream(&h, upstream_body("mixed", &url)).await;
    assert_eq!(status, StatusCode::OK);
    refresh(&h, "mixed").await;
    let (status, body) = put_basic_tools(&h, "mixed", json!({"echo": true})).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // 上游把 echo 改名成 other：覆盖行保留，标成 orphaned 由管理面清理。
    fake.set_tools(&["keep", "other"]);
    refresh(&h, "mixed").await;
    let (_, body) = get_upstream_tools(&h, "mixed").await;
    assert_eq!(body["total"], json!(3), "{body}");
    assert_eq!(body["basic_total"], json!(1));
    assert_eq!(body["orphaned_total"], json!(1));
    let orphan = tool_row(&body, "echo");
    assert_eq!(orphan["orphaned"], json!(true));
    assert_eq!(orphan["basic"], json!(true));
    assert_eq!(orphan["overridden"], json!(true));
    assert_eq!(orphan["eligible"], json!(false));
    assert_eq!(tool_row(&body, "keep")["orphaned"], json!(false));
    assert_eq!(tool_row(&body, "keep")["overridden"], json!(false));

    // 漂移后的清理：false 删除孤行（此时 echo 已不在缓存里）。
    let (status, body) = put_basic_tools(&h, "mixed", json!({"echo": false})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["changed"],
        json!([{"field": "tool_group", "tool": "echo", "before": "basic", "after": null}])
    );
    assert_eq!(body["orphaned_total"], json!(0));

    // 上游删除时同一事务级联清掉覆盖行（不能留下悬挂配置）。
    let (status, body) = put_basic_tools(&h, "mixed", json!({"keep": true})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(h.store.upstream_tool_groups().expect("rows").len(), 1);
    let (status, _) = call(
        &h.app,
        "DELETE",
        "/admin/upstreams/mixed",
        Some(ALICE_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(h.store.upstream_tool_groups().expect("rows").is_empty());
    let (status, _) = get_upstream_tools(&h, "mixed").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn basic_precheck_matches_sanitized_exposed_name() {
    let h = harness();
    let (url, _fake) = spawn_fake_named(&["orders.lookup"]).await;
    let (status, _) = create_upstream(&h, upstream_body("dots", &url)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = refresh(&h, "dots").await;
    assert_eq!(status, StatusCode::OK);

    // 真实清洗规则：`.` 不是合法字符，暴露名把它换成 `-`，原始名留在 `name` 里。
    let (_, view) = get_upstream_tools(&h, "dots").await;
    assert_eq!(
        tool_row(&view, "orders.lookup")["exposed_name"],
        json!("dots__orders-lookup")
    );

    let manager = UpstreamManager::new(h.store.clone(), false);
    let out_of_scope = scope(&["db.read.core"]); // 不含上游组 db.read.analytics
    assert!(
        !manager.matches_upstream(&out_of_scope, "dots__orders-lookup"),
        "没有 basic 覆盖时不得命中"
    );

    let (status, body) = put_basic_tools(&h, "dots", json!({"orders.lookup": true})).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // 覆盖行按原始名存储、预检拿到的却是清洗后的暴露名：必须按 exposed_tool_name 比较。
    assert!(
        manager.matches_upstream(&out_of_scope, "dots__orders-lookup"),
        "清洗过名字的 basic 工具必须被预检命中"
    );
    assert!(!manager.matches_upstream(&out_of_scope, "dots__orders.lookup"));
    assert!(manager.matches_upstream(&scope(&["db.read.analytics"]), "dots__orders-lookup"));

    // 端到端：预检命中后仍由 allowlist 拒绝，绝不因为名字清洗而漏过检查去执行。
    let token = issue_token(
        &h,
        "agent-no-basic",
        "core-only",
        &["db.read.core"],
        &["search_capabilities"],
    )
    .await;
    let denied = call_tool(&h.app, &token, "dots__orders-lookup", json!({})).await;
    assert_eq!(denied["result"]["isError"], json!(true), "{denied}");
    assert_eq!(
        denied["result"]["content"]["error"]["code"],
        json!("MCP_CAPABILITY_NOT_FOUND"),
        "{denied}"
    );
}

#[tokio::test]
async fn basic_override_stays_visible_when_upstream_turns_write() {
    let h = harness_with(false, true);
    let (url, _fake) = spawn_fake_named(&["echo", "keep"]).await;
    let (status, _) = create_upstream(&h, upstream_body("mixed", &url)).await;
    assert_eq!(status, StatusCode::OK);
    refresh(&h, "mixed").await;
    let (status, body) = put_basic_tools(&h, "mixed", json!({"echo": true})).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // 上游改成写通道：旧覆盖仍可见（overridden=true），但不再生效（basic=false）。
    let mut body = upstream_body("mixed", &url);
    body["read_only"] = json!(false);
    let (status, resp) = call(
        &h.app,
        "PUT",
        "/admin/upstreams/mixed",
        Some(ALICE_TOKEN),
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    refresh(&h, "mixed").await;

    let (_, view) = get_upstream_tools(&h, "mixed").await;
    let echo = tool_row(&view, "echo");
    assert_eq!(echo["overridden"], json!(true));
    assert_eq!(echo["basic"], json!(false));
    assert_eq!(echo["group"], json!("db.read.analytics"));
    assert_eq!(echo["group_source"], json!("upstream"));
    assert_eq!(echo["eligible"], json!(false));
    assert_eq!(view["basic_total"], json!(0));

    // 管理面据此提交 null 清理失效覆盖。
    let (status, body) = put_basic_tools(&h, "mixed", json!({"echo": null})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["changed"],
        json!([{"field": "tool_group", "tool": "echo", "before": "basic", "after": null}])
    );
    let (_, view) = get_upstream_tools(&h, "mixed").await;
    assert_eq!(tool_row(&view, "echo")["overridden"], json!(false));
}

async fn tool_names_with_groups(app: &Router, token: &str, groups: &str) -> Vec<String> {
    let request = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-mcp-tool-groups", groups)
        .body(Body::from(
            json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}).to_string(),
        ))
        .expect("request");
    let response = app.clone().oneshot(request).await.expect("response");
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    let value: Value = serde_json::from_slice(&bytes).expect("json body");
    value["result"]["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|t| t["name"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// basic 成员对所有 scope 可见/可调（不要求 scope.groups 含 basic 或上游组），
/// 但仍受 token allowlist、上游 enabled/只读约束；非 basic 工具照旧按上游组门控。
#[tokio::test]
async fn basic_members_bypass_group_scope_but_not_the_allowlist() {
    let h = harness();
    let (url, _fake) = spawn_fake(false).await;
    let (status, _) = create_upstream(&h, upstream_body("mixed", &url)).await;
    assert_eq!(status, StatusCode::OK);
    refresh(&h, "mixed").await;
    let (status, body) = put_basic_tools(&h, "mixed", json!({"echo": true})).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // 令牌组只有 db.read.core：上游组（db.read.analytics）不在 scope 内。
    let token = issue_token(
        &h,
        "agent-basic",
        "core-only",
        &["db.read.core"],
        &["search_capabilities", "mixed__echo"],
    )
    .await;
    let names = tool_names(&h.app, &token).await;
    assert!(names.contains(&"mixed__echo".to_string()), "{names:?}");
    assert!(
        !names.contains(&"mixed__explode".to_string()),
        "非 basic 工具必须仍受上游组门控: {names:?}"
    );
    let resp = mcp(&h.app, &token, "tools/list", json!({})).await;
    let echo = resp["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .find(|t| t["name"] == json!("mixed__echo"))
        .expect("basic 成员");
    assert_eq!(echo["group"], json!("basic"), "{echo}");
    assert_eq!(echo["source"], json!("upstream"));

    let ok = call_tool(&h.app, &token, "mixed__echo", json!({"msg":"hi"})).await;
    assert_eq!(ok["result"]["isError"], json!(false), "{ok}");
    let denied = call_tool(&h.app, &token, "mixed__explode", json!({})).await;
    assert!(
        denied["result"].is_null(),
        "非 basic 且组不在 scope 内必须拒绝: {denied}"
    );

    // allowlist 仍然优先：不含 mixed__echo 的令牌既看不到也调不到。
    let without = issue_token(
        &h,
        "agent-no-basic",
        "core-only",
        &["db.read.core"],
        &["search_capabilities"],
    )
    .await;
    let names = tool_names(&h.app, &without).await;
    assert!(!names.iter().any(|n| n.contains("__")), "{names:?}");
    let denied = call_tool(&h.app, &without, "mixed__echo", json!({})).await;
    assert_eq!(denied["result"]["isError"], json!(true), "{denied}");
    assert_eq!(
        denied["result"]["content"]["error"]["code"],
        json!("MCP_CAPABILITY_NOT_FOUND"),
        "{denied}"
    );

    // 上游组在 scope 内时，非 basic 工具照旧可见（basic 不是唯一通道）。
    let analyst = issue_token(
        &h,
        "agent-scope",
        "analyst",
        &["db.read.analytics"],
        &["mixed__explode"],
    )
    .await;
    let names = tool_names(&h.app, &analyst).await;
    assert!(names.contains(&"mixed__explode".to_string()), "{names:?}");

    // X-MCP-Tool-Groups 收窄到不含上游组：basic 成员不得消失。
    let narrowed = tool_names_with_groups(&h.app, &token, "db.read.core").await;
    assert!(
        narrowed.contains(&"mixed__echo".to_string()),
        "basic 成员不受组收窄影响: {narrowed:?}"
    );
}

/// 组索引只在存在 basic 覆盖时出现 `basic`，并按生效组计数（不把常驻内置工具算进去）。
#[tokio::test]
async fn basic_members_appear_in_group_index_only_when_overridden() {
    let h = harness();
    let (url, _fake) = spawn_fake(false).await;
    let (status, _) = create_upstream(&h, upstream_body("mixed", &url)).await;
    assert_eq!(status, StatusCode::OK);
    refresh(&h, "mixed").await;
    let token = issue_token(
        &h,
        "agent-index",
        "core-only",
        &["db.read.core"],
        &["list_groups", "mixed__echo"],
    )
    .await;

    let groups = call_tool(&h.app, &token, "list_groups", json!({})).await;
    let rows = groups["result"]["content"]
        .as_array()
        .expect("groups")
        .clone();
    assert!(
        !rows.iter().any(|g| g["group"] == json!("basic")),
        "没有 basic 覆盖时不得出现 basic: {groups}"
    );

    let (status, body) = put_basic_tools(&h, "mixed", json!({"echo": true})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let groups = call_tool(&h.app, &token, "list_groups", json!({})).await;
    let rows = groups["result"]["content"].as_array().expect("groups");
    let basic = rows
        .iter()
        .find(|g| g["group"] == json!("basic"))
        .unwrap_or_else(|| panic!("basic 必须出现在组索引: {groups}"));
    assert_eq!(basic["capability_count"], json!(1));
    assert!(
        !rows
            .iter()
            .any(|g| g["group"] == json!("db.read.analytics")),
        "scope 外的上游组不得出现: {groups}"
    );
}
