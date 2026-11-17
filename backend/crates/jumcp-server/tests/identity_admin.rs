//! S2/S3 契约：per-agent 身份（`JUMCP_AGENT_TOKENS`）+ 维护工具组 admin。
//!
//! 全部走真实链路（axum oneshot + 真实 `CapabilityStore` / `AuditLog`），只把执行器换成 fake：
//! 因此这里验证的是"Bearer token → Actor → scope → tools/list / tools/call → 审计"的完整路径。
//!
//! 关键不变量：
//! - HTTP `/mcp` 没有兜底身份：缺 token / token 未知 → `MCP_UNAUTHORIZED`；
//! - admin 工具对非 admin 身份"看不到也调不了"（发现面与调用面同一判定）；
//! - `review → published` 只能走 HTTP 管理面（人工审批），MCP 面不存在发布工具。

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use jumcp_core::audit::AuditLog;
use jumcp_core::budget::BudgetCaps;
use jumcp_core::config::{
    AdminTokens, AgentTokens, DatasourceRef, GroupDef, RoleDef, Settings, TenantPolicy,
};
use jumcp_core::entry::EffectiveScope;
use jumcp_core::registry::CapabilityStore;
use jumcp_core::template::TemplateRegistry;
use jumcp_server::admin::AdminState;
use jumcp_server::executor::{ExecFuture, QueryExecutor, QueryParams};
use jumcp_server::mcp::{McpServer, http_router};
use jumcp_server::service::{ADMIN_TOOL_NAMES, RESIDENT_TOOL_NAMES, Service};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

const ANALYST_TOKEN: &str = "tok_agent_analyst_0001";
const ANALYST_B_TOKEN: &str = "tok_agent_analyst_0002";
const ADMIN_TOKEN: &str = "tok_agent_admin_000001";
const ALICE_TOKEN: &str = "tok_alice_000000000";
const BOB_TOKEN: &str = "tok_bob_00000000000";

struct FakeExecutor;

impl QueryExecutor for FakeExecutor {
    fn execute<'a>(&'a self, _scope: &'a EffectiveScope, _q: &'a QueryParams) -> ExecFuture<'a> {
        Box::pin(async {
            Ok(json!({
                "rows": [["2026-11-05", 3]],
                "row_count": 1,
                "truncated": false,
                "effective_max_rows": 100,
                "elapsed_ms": 1,
                "tokens": 3
            }))
        })
    }
}

fn settings() -> Settings {
    let mut roles = std::collections::HashMap::new();
    roles.insert(
        "analyst".into(),
        RoleDef {
            name: "analyst".into(),
            default_groups: vec!["db.read.core".into(), "db.read.analytics".into()],
            default_budget: 2000,
        },
    );
    roles.insert(
        "admin".into(),
        RoleDef {
            name: "admin".into(),
            default_groups: vec![
                "db.read.core".into(),
                "db.read.analytics".into(),
                "admin".into(),
            ],
            default_budget: 2500,
        },
    );
    Settings {
        admin_tokens: AdminTokens::parse(&format!("alice:{ALICE_TOKEN},bob:{BOB_TOKEN}"))
            .expect("admin tokens"),
        agent_tokens: AgentTokens::parse(&format!(
            "agent-analyst:analyst:{ANALYST_TOKEN},agent-analyst-b:analyst:{ANALYST_B_TOKEN},agent-admin:admin:{ADMIN_TOKEN}"
        ))
        .expect("agent tokens"),
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
        datasources: vec![DatasourceRef {
            name: "main-pg".into(),
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
    harness_with_budget(None)
}

/// `caps` 用来把会话预算调小，验证"预算按身份隔离"；默认走生产上限。
fn harness_with_budget(caps: Option<BudgetCaps>) -> Harness {
    let audit = Arc::new(AuditLog::open(":memory:").expect("audit"));
    let store = Arc::new(CapabilityStore::open(":memory:", Some(audit.clone())).expect("store"));
    // 生产 /mcp 只认数据库 token：测试显式 seed（与启动引导同一条 `import_bootstrap_tokens`）。
    let boot = settings();
    store
        .import_bootstrap_tokens(boot.agent_tokens.principals(), &boot.policy)
        .expect("seed access tokens");
    let registry = Arc::new(TemplateRegistry::new(store.clone()));
    let admin_tokens = boot.admin_tokens.clone();
    let mut service = Service::new(boot)
        .with_executor(Arc::new(FakeExecutor))
        .with_audit(audit.clone())
        .with_templates(registry.clone())
        .with_scope(EffectiveScope {
            groups: vec!["db.read.core".into(), "db.read.analytics".into()],
            budget: 2000,
            degraded: false,
            source: "default".into(),
            mode: "strict".into(),
            allowed_builtin_tools: None,
        });
    if let Some(caps) = caps {
        service = service.with_budget_caps(caps);
    }
    let service = Arc::new(service);
    let admin = AdminState {
        service: service.clone(),
        store: store.clone(),
        tokens: admin_tokens,
    };
    let app = jumcp_server::admin::router(admin)
        .merge(http_router(Arc::new(McpServer::from_arc(service))));
    Harness { app, store, audit }
}

/// 走完整生命周期开放一条能力（与 admin.rs 调同一批 store 方法）。
fn publish_capability(h: &Harness, id: &str, group: &str) {
    h.store
        .register(
            id,
            "tool",
            group,
            "read",
            json!({
                "description": id,
                "params_schema": {"type":"object","properties":{
                    "day": {"type":"string"},
                    "channel": {"type":"string"},
                    "amount": {"type":"number"}
                }}
            }),
        )
        .expect("register");
    h.store.submit_for_review(id, "alice").expect("submit");
    h.store
        .promote_to_published(id, "ap-pub", "bob")
        .expect("publish");
    h.store
        .set_visibility(id, Some(true), Some(true), "alice")
        .expect("visibility");
}

/// 发一个请求；`token` 就是 `Authorization: Bearer <token>` 的值，`groups` 是收窄用请求头。
async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    groups: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    if let Some(groups) = groups {
        builder = builder.header("x-mcp-tool-groups", groups);
    }
    let body = match body {
        Some(v) => Body::from(v.to_string()),
        None => Body::empty(),
    };
    let request = builder
        .header(header::CONTENT_TYPE, "application/json")
        .body(body)
        .expect("request");
    let response = app.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    let value: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("json body")
    };
    (status, value)
}

fn rpc(method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","id":1,"method":method,"params":params})
}

async fn mcp(app: &Router, token: Option<&str>, body: Value) -> Value {
    let (status, value) = call(app, "POST", "/mcp", token, None, Some(body)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "MCP 入口应回 200 + JSON-RPC 错误: {value}"
    );
    value
}

async fn tools_list(app: &Router, token: &str) -> Vec<String> {
    let resp = mcp(app, Some(token), rpc("tools/list", json!({}))).await;
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
        Some(token),
        rpc("tools/call", json!({"name": name, "arguments": arguments})),
    )
    .await
}

async fn tools_list_with_groups(app: &Router, token: &str, groups: &str) -> Value {
    call(
        app,
        "POST",
        "/mcp",
        Some(token),
        Some(groups),
        Some(rpc("tools/list", json!({}))),
    )
    .await
    .1
}

fn names_of(resp: &Value) -> Vec<String> {
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

async fn search_ids(app: &Router, token: &str, query: &str) -> Vec<String> {
    let resp = call_tool(
        app,
        token,
        "search_capabilities",
        json!({"query": query, "limit": 50}),
    )
    .await;
    resp["result"]["content"]
        .as_array()
        .map(|hits| {
            hits.iter()
                .filter_map(|h| h["id"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// 工具错误的稳定码（`result.content.error.code`）。
fn err_code(resp: &Value) -> String {
    resp["result"]["content"]["error"]["code"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn template_value() -> Value {
    json!({
        "template_id": "order.daily_summary",
        "group": "db.read.analytics",
        "risk": "read",
        "description": "按日汇总订单",
        "tags": ["order", "daily"],
        "sql": "SELECT day FROM public.orders WHERE created_at >= :start_date AND created_at < :end_date",
        "params": [
            {"name":"start_date","type":"timestamp","required":true,"nullable":false},
            {"name":"end_date","type":"timestamp","required":true,"nullable":false}
        ],
        "constraints": {
            "allowed_schemas": ["public"],
            "auto_limit": 100,
            "max_result_tokens": 1500,
            "statement_timeout_ms": 5000
        },
        "owner": "data-platform"
    })
}

async fn audit_events(h: &Harness, event_type: &str) -> Vec<jumcp_core::audit::Envelope> {
    let (events, _) = h
        .audit
        .query_page(
            Some(&json!({"event_type": event_type})),
            200,
            0,
            jumcp_core::audit::Order::Asc,
        )
        .expect("query audit");
    events
}

/// 无 token / token 未知 → `MCP_UNAUTHORIZED`，且绝不允许回落到默认 analyst 身份。
#[tokio::test]
async fn mcp_without_valid_token_is_unauthorized() {
    let h = harness();
    for token in [None, Some("tok_not_configured_0001")] {
        let resp = mcp(&h.app, token, rpc("tools/list", json!({}))).await;
        assert_eq!(
            resp["error"]["data"]["mcp_code"],
            json!("MCP_UNAUTHORIZED"),
            "{resp}"
        );
        assert!(
            resp["result"].is_null(),
            "鉴权失败不得返回工具清单（不得回落默认身份）: {resp}"
        );
    }
    // 拒绝本身要进审计链，且不借用 analyst 人格。
    let events = audit_events(&h, "policy_decision").await;
    assert!(!events.is_empty(), "未授权访问必须写 policy_decision");
    assert!(
        events.iter().all(|e| e.actor["role"] != json!("analyst")),
        "匿名主体不得被记成 analyst: {:?}",
        events.iter().map(|e| &e.actor).collect::<Vec<_>>()
    );
}

/// 同一个进程、同一份清单源：admin 身份看到 4 个维护工具，analyst 身份一个都看不到。
#[tokio::test]
async fn tools_list_depends_on_derived_identity() {
    let h = harness();

    let analyst = tools_list(&h.app, ANALYST_TOKEN).await;
    assert_eq!(
        analyst.len(),
        RESIDENT_TOOL_NAMES.len(),
        "analyst 的 tools/list 只应有常驻工具: {analyst:?}"
    );
    for tool in ADMIN_TOOL_NAMES {
        assert!(
            !analyst.contains(&tool.to_string()),
            "analyst 不得看到维护工具 {tool}: {analyst:?}"
        );
    }

    let admin = tools_list(&h.app, ADMIN_TOKEN).await;
    for tool in ADMIN_TOOL_NAMES {
        assert!(
            admin.contains(&tool.to_string()),
            "admin 必须看到维护工具 {tool}: {admin:?}"
        );
    }
    assert_eq!(
        admin.len(),
        RESIDENT_TOOL_NAMES.len() + ADMIN_TOOL_NAMES.len(),
        "admin 的清单 = 常驻 + 维护: {admin:?}"
    );
    // 工具清单里不得出现任何发布/审批类工具（人工发布只能走 HTTP 管理面）。
    assert!(
        admin.iter().all(|n| !n.starts_with("admin_publish")),
        "MCP 面不得提供发布工具: {admin:?}"
    );

    // 发现面同口径：analyst 连 admin 工具的存在都搜不到。
    assert!(
        search_ids(&h.app, ANALYST_TOKEN, "admin_").await.is_empty(),
        "非 admin 身份不得从发现面探测到维护工具"
    );
    // capability_search 审计主体也必须是派生身份（不是硬编码 analyst 之外的空壳）。
    let searches = audit_events(&h, "capability_search").await;
    assert!(!searches.is_empty(), "检索必须写审计");
    assert!(
        searches.iter().all(|e| e.actor["id"] == json!("agent-analyst")
            && e.actor["role"] == json!("analyst")),
        "capability_search 的 actor 必须来自 token: {:?}",
        searches.iter().map(|e| &e.actor).collect::<Vec<_>>()
    );
    assert_eq!(
        search_ids(&h.app, ADMIN_TOKEN, "admin_register_template").await,
        vec!["admin_register_template".to_string()],
        "admin 身份应能在发现面检索到维护工具"
    );
}

/// 请求头只能**收窄**已派生身份：admin 可以把 admin 组裁掉，analyst 不能拿头把自升成 admin。
#[tokio::test]
async fn header_groups_can_only_narrow_the_derived_identity() {
    let h = harness();
    let (status, narrowed) = call(
        &h.app,
        "POST",
        "/mcp",
        Some(ADMIN_TOKEN),
        Some("db.read.core"),
        Some(rpc("tools/list", json!({}))),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{narrowed}");
    let names: Vec<String> = narrowed["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|t| t["name"].as_str().map(String::from))
        .collect();
    assert!(
        names
            .iter()
            .all(|n| !ADMIN_TOOL_NAMES.contains(&n.as_str())),
        "裁掉 admin 组后不得再看到维护工具: {names:?}"
    );
    assert_eq!(names.len(), RESIDENT_TOOL_NAMES.len());

    // analyst 用头声明 admin 组：越界即拒绝（strict），不得提权。
    let (_, escalated) = call(
        &h.app,
        "POST",
        "/mcp",
        Some(ANALYST_TOKEN),
        Some("admin"),
        Some(rpc("tools/list", json!({}))),
    )
    .await;
    assert_eq!(
        escalated["error"]["data"]["mcp_code"],
        json!("MCP_GROUP_FORBIDDEN"),
        "{escalated}"
    );
}

/// 非 admin 身份直接调用维护工具 → 与"不存在"同构的 `MCP_CAPABILITY_NOT_FOUND`（测试固定此选择），
/// 且被拒的写入不落库、拒绝写 policy_decision（actor 是派生身份）。
#[tokio::test]
async fn non_admin_cannot_call_admin_tools() {
    let h = harness();
    let resp = call_tool(
        &h.app,
        ANALYST_TOKEN,
        "admin_register_template",
        json!({"definition": template_value()}),
    )
    .await;
    assert_eq!(err_code(&resp), "MCP_CAPABILITY_NOT_FOUND", "{resp}");
    assert!(h.store.all().expect("all").is_empty(), "被拒的登记不得落库");

    let events = audit_events(&h, "policy_decision").await;
    assert!(
        events
            .iter()
            .any(|e| e.actor["id"] == json!("agent-analyst")
                && e.payload["error_code"] == json!("MCP_CAPABILITY_NOT_FOUND")),
        "拒绝必须记 policy_decision 且 actor 为派生身份: {:?}",
        events
            .iter()
            .map(|e| (&e.actor, &e.payload))
            .collect::<Vec<_>>()
    );
}

/// 维护通道只产草案：draft 对 analyst 不可见；经 HTTP 管理面（alice 提审、bob 批准 + 开可见性）后才可搜到。
#[tokio::test]
async fn admin_register_template_creates_draft_then_human_publish_exposes_it() {
    let h = harness();
    let resp = call_tool(
        &h.app,
        ADMIN_TOKEN,
        "admin_register_template",
        json!({"definition": template_value()}),
    )
    .await;
    assert_eq!(resp["result"]["isError"], json!(false), "{resp}");
    let content = &resp["result"]["content"];
    assert_eq!(content["id"], json!("order.daily_summary"));
    assert_eq!(content["status"], json!("draft"));
    assert_eq!(content["version"], json!(1));
    let definition_hash = content["definition_hash"]
        .as_str()
        .expect("definition_hash")
        .to_string();
    assert_ne!(definition_hash, "");

    // admin_change：actor.id = token 派生的 agent_id；统一主体/动作/对象/结果载荷。
    let events = audit_events(&h, "admin_change").await;
    let event = events
        .iter()
        .find(|e| e.payload["object_id"] == json!("order.daily_summary"))
        .expect("admin_change 必须记录草案登记");
    assert_eq!(
        event.actor["id"],
        json!("agent-admin"),
        "审计 actor.id 必须是 token 派生的 agent_id"
    );
    assert_eq!(event.actor["role"], json!("admin"));
    assert_eq!(event.payload["object_type"], json!("capability"));
    assert_eq!(event.payload["action"], json!("create"));
    assert_eq!(event.payload["outcome"], json!("ok"));
    assert_eq!(event.payload["to_status"], json!("draft"));
    assert_eq!(
        event.payload["data"]["definition_hash_after"],
        json!(definition_hash)
    );
    assert_eq!(event.payload["approval"], Value::Null);

    // draft 不在发现面（analyst 搜不到、describe 与不存在同构）。
    assert!(
        search_ids(&h.app, ANALYST_TOKEN, "order.daily_summary")
            .await
            .is_empty(),
        "draft 能力不得出现在发现面"
    );
    assert_eq!(
        err_code(
            &call_tool(
                &h.app,
                ANALYST_TOKEN,
                "describe_capability",
                json!({"capability_id": "order.daily_summary"})
            )
            .await
        ),
        "MCP_CAPABILITY_NOT_FOUND"
    );

    // 人工发布（HTTP 管理面）：alice 提审、bob 批准，再开 discoverable/callable。
    let (status, body) = call(
        &h.app,
        "POST",
        "/admin/capabilities/order.daily_summary/submit",
        Some(ALICE_TOKEN),
        None,
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = call(
        &h.app,
        "POST",
        "/admin/capabilities/order.daily_summary/publish",
        Some(BOB_TOKEN),
        None,
        Some(json!({"approval_id": "ap-pub-1"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        search_ids(&h.app, ANALYST_TOKEN, "order.daily_summary")
            .await
            .is_empty(),
        "published 但未开 discoverable 仍不可见"
    );
    let (status, body) = call(
        &h.app,
        "POST",
        "/admin/capabilities/order.daily_summary/visibility",
        Some(ALICE_TOKEN),
        None,
        Some(json!({"discoverable": true, "callable": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    assert_eq!(
        search_ids(&h.app, ANALYST_TOKEN, "order.daily_summary").await,
        vec!["order.daily_summary".to_string()],
        "人工发布并开可见性后，analyst 的**下一次**请求即可见"
    );
    // MCP 面始终不存在"自我批准"的入口。
    assert!(
        tools_list(&h.app, ADMIN_TOKEN)
            .await
            .iter()
            .all(|n| !n.starts_with("admin_publish") && !n.starts_with("admin_promote")),
        "维护通道不得提供发布/提审工具"
    );
}

/// `admin_set_visibility` 对内置 id 一律拒绝；对普通能力成功并写 `admin_change`（actor 为派生身份）。
#[tokio::test]
async fn admin_set_visibility_rejects_builtin_and_audits_success() {
    let h = harness();
    for id in ["search_capabilities", "admin_register_template"] {
        let resp = call_tool(
            &h.app,
            ADMIN_TOKEN,
            "admin_set_visibility",
            json!({"capability_id": id, "discoverable": false}),
        )
        .await;
        assert_eq!(err_code(&resp), "MCP_BUILTIN_IMMUTABLE", "{id}: {resp}");
    }

    h.store
        .register(
            "cap.managed",
            "tool",
            "db.read.core",
            "read",
            json!({"description": "受管能力"}),
        )
        .expect("register");
    let resp = call_tool(
        &h.app,
        ADMIN_TOKEN,
        "admin_set_visibility",
        json!({"capability_id": "cap.managed", "discoverable": true, "callable": true}),
    )
    .await;
    assert_eq!(resp["result"]["isError"], json!(false), "{resp}");
    assert_eq!(resp["result"]["content"]["discoverable"], json!(true));
    assert_eq!(resp["result"]["content"]["callable"], json!(true));

    let events = audit_events(&h, "admin_change").await;
    let event = events
        .iter()
        .find(|e| e.payload["object_id"] == json!("cap.managed"))
        .expect("可见性变更必须写 admin_change");
    assert_eq!(event.actor["id"], json!("agent-admin"));
    assert_eq!(event.payload["action"], json!("visibility"));
    assert_eq!(event.payload["approval"], Value::Null);
    let changes = event.payload["changes"].as_array().expect("changes 数组");
    assert!(
        changes
            .iter()
            .any(|c| { c["field"] == json!("discoverable") && c["after"] == json!(true) })
    );
    assert!(
        changes
            .iter()
            .any(|c| { c["field"] == json!("callable") && c["after"] == json!(true) })
    );
}

/// 只读维护工具：审核队列与审计查询（limit 夹紧 ≤200；非法过滤键拒绝并写 policy_decision）。
#[tokio::test]
async fn admin_read_tools_serve_queue_and_audit_pages() {
    let h = harness();
    h.store
        .register(
            "cap.draft",
            "tool",
            "db.read.core",
            "read",
            json!({"description": "草案"}),
        )
        .expect("register");

    let queue = call_tool(&h.app, ADMIN_TOKEN, "admin_review_queue", json!({})).await;
    assert_eq!(queue["result"]["isError"], json!(false), "{queue}");
    assert_eq!(queue["result"]["content"]["total"], json!(1));
    assert_eq!(
        queue["result"]["content"]["items"][0]["id"],
        json!("cap.draft")
    );
    assert_eq!(
        queue["result"]["content"]["items"][0]["status"],
        json!("draft")
    );

    let page = call_tool(
        &h.app,
        ADMIN_TOKEN,
        "admin_audit_queries",
        json!({"limit": 1000, "offset": 0}),
    )
    .await;
    assert_eq!(page["result"]["isError"], json!(false), "{page}");
    assert_eq!(
        page["result"]["content"]["limit"],
        json!(200),
        "limit 必须夹紧到 200"
    );

    let rejected = call_tool(
        &h.app,
        ADMIN_TOKEN,
        "admin_audit_queries",
        json!({"filter": {"unsupported": "x"}}),
    )
    .await;
    assert_eq!(err_code(&rejected), "MCP_FILTER_UNSUPPORTED", "{rejected}");
    let events = audit_events(&h, "policy_decision").await;
    assert!(
        events
            .iter()
            .any(|e| e.payload["error_code"] == json!("MCP_FILTER_UNSUPPORTED")),
        "被拒的审计查询也必须写 policy_decision"
    );
}

/// 通道优先级：Header > `_meta` 会话 scope > 角色默认。
///
/// 关键是 HTTP 入口不把 header 解析结果当成唯一来源：无 header 时必须用 `initialize`
/// 声明的 `_meta` 组，且两者同时存在时 header 赢（不求交集）。
#[tokio::test]
async fn meta_scope_applies_when_header_absent() {
    let h = harness();
    publish_capability(&h, "cap.core", "db.read.core");
    publish_capability(&h, "cap.analytics", "db.read.analytics");

    // admin 身份把可见组收窄到 db.read.core（admin 组随之消失）。
    let init = mcp(
        &h.app,
        Some(ADMIN_TOKEN),
        rpc(
            "initialize",
            json!({"_meta": {"mcp": {"groups": ["db.read.core"]}}}),
        ),
    )
    .await;
    assert_eq!(init["error"], Value::Null, "{init}");

    let narrowed = tools_list(&h.app, ADMIN_TOKEN).await;
    assert!(
        narrowed.contains(&"cap.core".to_string()),
        "无 header 时必须用 _meta 会话 scope: {narrowed:?}"
    );
    assert!(
        !narrowed.contains(&"cap.analytics".to_string()),
        "_meta 未声明的组不得出现: {narrowed:?}"
    );
    assert!(
        narrowed
            .iter()
            .all(|n| !ADMIN_TOOL_NAMES.contains(&n.as_str())),
        "收窄掉 admin 组后维护工具必须不可见: {narrowed:?}"
    );

    // header 与 _meta 同时存在：header 优先，不与 _meta 求交集。
    let by_header = names_of(&tools_list_with_groups(&h.app, ADMIN_TOKEN, "admin").await);
    for tool in ADMIN_TOOL_NAMES {
        assert!(
            by_header.contains(&tool.to_string()),
            "header 声明 admin 组后必须看到 {tool}: {by_header:?}"
        );
    }
    assert!(
        !by_header.contains(&"cap.core".to_string()),
        "header 赢：_meta 的 core 不得混入: {by_header:?}"
    );

    // header 消失后回到会话 scope（`_meta` 未被 header 请求覆盖）。
    let again = tools_list(&h.app, ADMIN_TOKEN).await;
    assert!(again.contains(&"cap.core".to_string()), "{again:?}");
    assert!(!again.contains(&"cap.analytics".to_string()), "{again:?}");

    // 越界 header 仍然拒绝（strict 不裁剪）。
    let denied = tools_list_with_groups(&h.app, ADMIN_TOKEN, "db.read.sensitive").await;
    assert_eq!(
        denied["error"]["data"]["mcp_code"],
        json!("MCP_GROUP_FORBIDDEN"),
        "{denied}"
    );
}

/// 预算按请求身份记账：A 打满 schema 预算不影响 B。
#[tokio::test]
async fn schema_budget_is_isolated_per_agent() {
    let h = harness_with_budget(Some(BudgetCaps {
        max_schema_count: 1,
        ..Default::default()
    }));
    publish_capability(&h, "cap.budget", "db.read.core");

    let first = call_tool(
        &h.app,
        ANALYST_TOKEN,
        "describe_capability",
        json!({"capability_id": "cap.budget"}),
    )
    .await;
    assert_eq!(first["result"]["isError"], json!(false), "{first}");
    let second = call_tool(
        &h.app,
        ANALYST_TOKEN,
        "describe_capability",
        json!({"capability_id": "cap.budget"}),
    )
    .await;
    assert_eq!(
        err_code(&second),
        "MCP_CONTEXT_BUDGET_EXHAUSTED",
        "同一 agent 的第二次 schema 必然超上限: {second}"
    );

    let other = call_tool(
        &h.app,
        ANALYST_B_TOKEN,
        "describe_capability",
        json!({"capability_id": "cap.budget"}),
    )
    .await;
    assert_eq!(
        other["result"]["isError"],
        json!(false),
        "另一个 agent 的 schema 账本必须独立: {other}"
    );
}

/// 审计未启用时的服务：`Service::audit = None`，store 也不挂审计。
///
/// 为什么必须单独一套 harness：`audit_event` 在无审计时是 **no-op**，
/// 若维护写工具不自己拒绝，就会"写成功但无痕迹"——治理面不可静默失效。
struct NoAuditHarness {
    app: Router,
    store: Arc<CapabilityStore>,
}

fn harness_without_audit() -> NoAuditHarness {
    let store = Arc::new(CapabilityStore::open(":memory:", None).expect("store"));
    let boot = settings();
    // 鉴权数据与审计无关：无审计 harness 也要 seed，才能走到"维护写工具拒绝"的分支。
    store
        .import_bootstrap_tokens(boot.agent_tokens.principals(), &boot.policy)
        .expect("seed access tokens");
    let registry = Arc::new(TemplateRegistry::new(store.clone()));
    let service = Service::new(boot.clone())
        .with_executor(Arc::new(FakeExecutor))
        // 关键：不调用 with_audit，store 也不接审计。
        .with_templates(registry)
        .with_scope(EffectiveScope {
            groups: vec!["db.read.core".into(), "db.read.analytics".into()],
            budget: 2000,
            degraded: false,
            source: "default".into(),
            mode: "strict".into(),
            allowed_builtin_tools: None,
        });
    let service = Arc::new(service);
    let admin = AdminState {
        service: service.clone(),
        store: store.clone(),
        tokens: boot.admin_tokens,
    };
    let app = jumcp_server::admin::router(admin)
        .merge(http_router(Arc::new(McpServer::from_arc(service))));
    NoAuditHarness { app, store }
}

/// 审计未启用 → `admin_register_template` 必须在任何编译/落库之前拒绝，模板记录不得存在。
#[tokio::test]
async fn admin_register_template_without_audit_rejects_and_persists_nothing() {
    let h = harness_without_audit();
    let resp = call_tool(
        &h.app,
        ADMIN_TOKEN,
        "admin_register_template",
        json!({"definition": template_value()}),
    )
    .await;
    assert_eq!(err_code(&resp), "MCP_AUDIT_ERROR", "{resp}");
    assert_eq!(
        resp["result"]["content"]["error"]["data"]["hint"],
        json!("审计未启用，维护通道拒绝写入"),
        "{resp}"
    );
    assert!(
        h.store.get("order.daily_summary").expect("get").is_none(),
        "审计缺失时登记必须被拒且未落库"
    );
    assert!(h.store.all().expect("all").is_empty(), "不得有任何记录");
}

/// 审计未启用 → `admin_set_visibility` 拒绝，已发布能力的可见性保持原样。
#[tokio::test]
async fn admin_set_visibility_without_audit_rejects_and_leaves_visibility_unchanged() {
    let h = harness_without_audit();
    h.store
        .register(
            "cap.managed",
            "tool",
            "db.read.core",
            "read",
            json!({"description": "受管能力"}),
        )
        .expect("register");
    h.store
        .submit_for_review("cap.managed", "alice")
        .expect("submit");
    h.store
        .promote_to_published("cap.managed", "ap-pub", "bob")
        .expect("publish");
    h.store
        .set_visibility("cap.managed", Some(true), Some(true), "alice")
        .expect("visibility");
    let before = h.store.get("cap.managed").expect("get").expect("record");
    assert!(before.discoverable && before.callable);

    let resp = call_tool(
        &h.app,
        ADMIN_TOKEN,
        "admin_set_visibility",
        json!({"capability_id": "cap.managed", "discoverable": false, "callable": false}),
    )
    .await;
    assert_eq!(err_code(&resp), "MCP_AUDIT_ERROR", "{resp}");

    let after = h.store.get("cap.managed").expect("get").expect("record");
    assert_eq!(
        after.discoverable, before.discoverable,
        "discoverable 不得变化"
    );
    assert_eq!(after.callable, before.callable, "callable 不得变化");
    assert_eq!(after.version, before.version, "被拒的写入不得产生新版本");
}

/// 维护工具的调用必须落 `tool_call` 审计：否则管理台台账里 `admin_*` 永远 calls=0/unknown，
/// 而台账的口径是"列出所有 MCP tool 的统计"。
#[tokio::test]
async fn admin_tool_calls_are_counted_in_stats() {
    let h = harness();
    h.store
        .register(
            "cap.draft",
            "tool",
            "db.read.core",
            "read",
            json!({"description": "草案"}),
        )
        .expect("register");

    // 1) 成功：admin 身份调 admin_review_queue。
    let ok = call_tool(&h.app, ADMIN_TOKEN, "admin_review_queue", json!({})).await;
    assert_eq!(ok["result"]["isError"], json!(false), "{ok}");
    // 2) 失败：admin_set_visibility 打 builtin（预期 MCP_BUILTIN_IMMUTABLE）。
    let builtin = call_tool(
        &h.app,
        ADMIN_TOKEN,
        "admin_set_visibility",
        json!({"capability_id": "search_capabilities", "discoverable": true}),
    )
    .await;
    assert_eq!(err_code(&builtin), "MCP_BUILTIN_IMMUTABLE", "{builtin}");
    // 3) 未授权探测：非 admin 身份调 admin 工具（与"不存在"同构）同样计入该工具失败。
    let denied = call_tool(&h.app, ANALYST_TOKEN, "admin_review_queue", json!({})).await;
    assert_eq!(err_code(&denied), "MCP_CAPABILITY_NOT_FOUND", "{denied}");

    let (status, body) = call(
        &h.app,
        "GET",
        "/admin/tools/stats",
        Some(ALICE_TOKEN),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let tools = body["tools"].as_array().expect("tools");
    let row = |id: &str| {
        tools
            .iter()
            .find(|t| t["id"] == json!(id))
            .unwrap_or_else(|| panic!("台账里缺少 {id}"))
            .clone()
    };

    let queue = row("admin_review_queue");
    assert_eq!(queue["source"], json!("builtin"));
    assert_eq!(queue["calls"], json!(2), "成功一次 + 未授权探测一次");
    assert_eq!(queue["success"], json!(1));
    assert_eq!(queue["failure"], json!(1));
    assert_eq!(
        queue["last_outcome"],
        json!("error"),
        "最后一次是未授权探测"
    );
    assert_eq!(queue["health"], json!("failing"), "1/2 = 0.5 < 0.80");
    assert!(
        queue["avg_response_tokens"]
            .as_f64()
            .is_some_and(|v| v > 0.0),
        "成功调用必须带 tokens 样本: {queue}"
    );

    let vis = row("admin_set_visibility");
    assert_eq!(vis["calls"], json!(1));
    assert_eq!(vis["failure"], json!(1));
    assert_eq!(vis["last_outcome"], json!("error"));

    // 明细页给出失败码，供管理台定位"为什么失败"。
    let (status, detail) = call(
        &h.app,
        "GET",
        "/admin/tools/admin_set_visibility/calls",
        Some(ALICE_TOKEN),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["total"], json!(1));
    assert_eq!(detail["calls"][0]["outcome"], json!("error"));
    assert_eq!(
        detail["calls"][0]["error_code"],
        json!("MCP_BUILTIN_IMMUTABLE")
    );
    assert_eq!(detail["calls"][0]["object_type"], json!("tool"));
    assert_eq!(
        detail["calls"][0]["object_id"],
        json!("admin_set_visibility")
    );
    // 明细必须回放调用方原始入参；维护工具不执行脚本 → executed_script 为 null。
    assert_eq!(
        detail["calls"][0]["arguments"],
        json!({"capability_id": "search_capabilities", "discoverable": true})
    );
    assert_eq!(detail["calls"][0]["executed_script"], Value::Null);

    // 无参维护工具：arguments 是空对象而不是 null。
    let (status, queue_detail) = call(
        &h.app,
        "GET",
        "/admin/tools/admin_review_queue/calls",
        Some(ALICE_TOKEN),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{queue_detail}");
    assert_eq!(queue_detail["total"], json!(2));
    for record in queue_detail["calls"].as_array().expect("calls") {
        assert_eq!(
            record["arguments"],
            json!({}),
            "无参工具记为 {{}}: {record}"
        );
        assert_eq!(record["executed_script"], Value::Null);
    }

    // 审计必须能复原"调用了什么参数"：管理面审计不额外打码，args 原样落库。
    let events = h
        .audit
        .query(Some(&json!({"event_type": "tool_call"})))
        .expect("query");
    let visibility = events
        .iter()
        .find(|e| e.payload["object_id"] == json!("admin_set_visibility"))
        .expect("admin_set_visibility 的 tool_call");
    assert_eq!(
        visibility.payload["data"]["arguments"],
        json!({"capability_id": "search_capabilities", "discoverable": true})
    );
    let queue_call = events
        .iter()
        .find(|e| {
            e.payload["object_id"] == json!("admin_review_queue")
                && e.payload["outcome"] == json!("ok")
        })
        .expect("admin_review_queue 的成功 tool_call");
    assert_eq!(queue_call.payload["data"]["arguments"], json!({}));
}

/// 通用分发入口（`tools/call` 按 name 路由）产生的 `tool_call` 必须能回放原始入参。
///
/// `invoke_capability` 未实现；内置 basic 组不直连数据源，
/// 当前仍有"任意 MCP 工具名分发"的入口是维护工具分支，这里端到端验证该路径的 calls 明细。
#[tokio::test]
async fn generic_dispatch_call_audits_arguments() {
    let h = harness();
    let args = json!({"filter": {"event_type": "tool_call"}, "limit": 5});
    let resp = call_tool(&h.app, ADMIN_TOKEN, "admin_audit_queries", args.clone()).await;
    assert_eq!(resp["result"]["isError"], json!(false), "{resp}");

    let (status, body) = call(
        &h.app,
        "GET",
        "/admin/tools/admin_audit_queries/calls",
        Some(ALICE_TOKEN),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total"], json!(1));
    let record = &body["calls"][0];
    assert_eq!(record["arguments"], args, "calls 必须回放原始入参");
    // 维护工具不执行脚本：executed_script 必须为 null，不得伪造 SQL。
    assert_eq!(record["executed_script"], Value::Null);
    assert_eq!(record["outcome"], json!("ok"));
}
