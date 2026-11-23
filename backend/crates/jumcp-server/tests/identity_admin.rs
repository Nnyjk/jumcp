//! S2/S3 契约：per-agent 身份（`JUMCP_AGENT_TOKENS`）+ 维护工具组 admin。
//!
//! 全部走真实链路（axum oneshot + 真实 `CapabilityStore` / `AuditLog`），只把执行器换成 fake：
//! 因此这里验证的是"Bearer token → Actor → scope → tools/list / tools/call → 审计"的完整路径。
//!
//! 关键不变量：
//! - HTTP `/mcp` 没有兜底身份：缺 token / token 未知 → `MCP_UNAUTHORIZED`；
//! - admin 工具对非 admin 身份"看不到也调不了"（发现面与调用面同一判定）；
//! - 发布有两条路：人走 HTTP 管理面（必带外部单号，`approval.source="external"`），
//!   admin agent 走 `admin_publish_capability`（agent 自审，`approval.source="agent"`）；
//!   两条路都只推进状态，MCP 面不存在改定义后即发布的通道。

mod support;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use jumcp_core::audit::AuditLog;
use jumcp_core::budget::BudgetCaps;
use jumcp_core::config::{AgentTokens, GroupDef, RoleDef, Settings, TenantPolicy};
use jumcp_core::entry::EffectiveScope;
use jumcp_core::registry::CapabilityStore;
use jumcp_core::template::TemplateRegistry;
use jumcp_server::admin::AdminState;
use jumcp_server::mcp::{McpServer, http_router};
use jumcp_server::service::{ADMIN_TOOL_NAMES, RESIDENT_TOOL_NAMES, Service};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

const ANALYST_TOKEN: &str = "tok_agent_analyst_0001";
const ANALYST_B_TOKEN: &str = "tok_agent_analyst_0002";
const ADMIN_TOKEN: &str = "tok_agent_admin_000001";
// 管理面只认会话 cookie：这两个常量就是 `support::seed_admin_accounts` 建好的会话 id。
const ALICE_TOKEN: &str = support::ALICE_TOKEN;
const BOB_TOKEN: &str = support::BOB_TOKEN;

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
        ..Default::default()
    }
}

struct Harness {
    app: Router,
    store: Arc<CapabilityStore>,
    audit: Arc<AuditLog>,
    service: Arc<Service>,
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
    let ttl = boot.admin_session_ttl_secs;
    let mut service = Service::new(boot)
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
    support::seed_admin_accounts(&store, ttl);
    let admin = AdminState {
        service: service.clone(),
        store: store.clone(),
        transport_key: None,
    };
    let app = jumcp_server::admin::router(admin)
        .merge(http_router(Arc::new(McpServer::from_arc(service.clone()))));
    Harness {
        app,
        store,
        audit,
        service,
    }
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
    let mut builder = support::attach_identity(Request::builder().method(method).uri(uri), token);
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
    resp["result"]["structuredContent"]["results"]
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
    resp["result"]["structuredContent"]["error"]["code"]
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
        "target": {"upstream": "ch", "tool": "query"},
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
    // 发布工具必须存在（agent 自审），但只能是 admin 组成员；不存在"改定义后直接发布"的入口。
    assert!(
        admin.iter().any(|n| n == "admin_publish_capability"),
        "admin 清单必须含发布工具: {admin:?}"
    );
    assert!(
        admin
            .iter()
            .all(|n| !n.starts_with("admin_promote") && !n.contains("approve")),
        "MCP 面不应另有提审/审批工具: {admin:?}"
    );
    assert!(
        !tools_list(&h.app, ANALYST_TOKEN)
            .await
            .iter()
            .any(|n| n == "admin_publish_capability"),
        "非 admin 身份不得看到发布工具"
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
    let content = &resp["result"]["structuredContent"];
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
    // 人发布的留痕仍是 external（agent 主动发布走 admin_publish_capability，不在这里）。
    let approvals = audit_events(&h, "admin_change").await;
    let publish = approvals
        .iter()
        .find(|e| {
            e.payload["object_id"] == json!("order.daily_summary")
                && e.payload["action"] == json!("publish")
        })
        .expect("人工发布必须写 admin_change");
    assert_eq!(publish.payload["approval"]["source"], json!("external"));
    assert_eq!(publish.payload["approval"]["verified"], json!(false));
    assert_eq!(publish.payload["approval"]["id"], json!("ap-pub-1"));
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
    assert_eq!(
        resp["result"]["structuredContent"]["discoverable"],
        json!(true)
    );
    assert_eq!(resp["result"]["structuredContent"]["callable"], json!(true));

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
    assert_eq!(queue["result"]["structuredContent"]["total"], json!(1));
    assert_eq!(
        queue["result"]["structuredContent"]["items"][0]["id"],
        json!("cap.draft")
    );
    assert_eq!(
        queue["result"]["structuredContent"]["items"][0]["status"],
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
        page["result"]["structuredContent"]["limit"],
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
    support::seed_admin_accounts(&store, boot.admin_session_ttl_secs);
    let admin = AdminState {
        service: service.clone(),
        store: store.clone(),
        transport_key: None,
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
        resp["result"]["structuredContent"]["error"]["data"]["hint"],
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

/// 候选评分在 MCP 面与会话面同源：审计 agent 拿到的排名必须与看板一致，
/// 否则"为什么推荐这张表"没法对照，评审只能重来一遍。
#[tokio::test]
async fn admin_tool_candidates_matches_the_admin_endpoint() {
    let h = harness();
    // 三次连续调用同一张表：后半段 1 次加权 → 1.5 分。
    for minutes in [30, 29, 28] {
        h.audit
            .append(jumcp_core::audit::AuditEvent {
                tenant: "default".into(),
                actor: jumcp_core::audit::ActorRef {
                    actor_type: "agent".into(),
                    id: "agent-analyst".into(),
                    role: "analyst".into(),
                    token_id: Some(ANALYST_TOKEN.into()),
                    source: None,
                },
                event_type: "tool_call".into(),
                payload: json!({
                    "object_type": "tool",
                    "object_id": "ch__query",
                    "action": "call",
                    "outcome": "ok",
                    "data": {
                        "arguments": {"sql": "SELECT id FROM env_air.orders LIMIT 10"},
                        "upstream_id": "ch",
                        "upstream_tool": "query",
                        "elapsed_ms": 4
                    }
                }),
                session_id: "s-1".into(),
                ts: (chrono::Utc::now() - chrono::Duration::minutes(minutes)).to_rfc3339(),
                event_id: String::new(),
            })
            .expect("append");
    }

    let resp = call_tool(&h.app, ADMIN_TOKEN, "admin_tool_candidates", json!({})).await;
    assert_eq!(resp["result"]["isError"], json!(false), "{resp}");
    let via_mcp = resp["result"]["structuredContent"].clone();
    let (status, via_http) = call(
        &h.app,
        "GET",
        "/admin/tools/candidates",
        Some(ALICE_TOKEN),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{via_http}");
    // `window.from/to` 是各自取的时刻，只比对可复现的部分：窗口天数、权重与候选排名。
    assert_eq!(via_mcp["window"]["days"], via_http["window"]["days"]);
    assert_eq!(via_mcp["weights"], via_http["weights"]);
    assert_eq!(via_mcp["candidates"], via_http["candidates"]);
    assert_eq!(via_mcp["candidates"][0]["subject"], json!("env_air.orders"));
    assert_eq!(via_mcp["candidates"][0]["score"], json!(1.5));

    // 管理台的这次查询本身不得变成一个候选（平台面事件不进评分）。
    let after = call_tool(&h.app, ADMIN_TOKEN, "admin_tool_candidates", json!({})).await;
    assert_eq!(
        after["result"]["structuredContent"]["candidates"], via_mcp["candidates"],
        "读候选的动作不能改变候选"
    );

    // 非 admin 身份：与"不存在"同构，不泄露候选内容。
    let denied = call_tool(&h.app, ANALYST_TOKEN, "admin_tool_candidates", json!({})).await;
    assert_eq!(err_code(&denied), "MCP_CAPABILITY_NOT_FOUND", "{denied}");
}

/// 后台静态审计快照：调度刷新后，MCP 默认调用直接读快照（不再扫审计库）；
/// 新事件只进显式实时重算。这是"缩减审计 agent 工作链路"的核心行为。
#[tokio::test]
async fn admin_tool_candidates_serves_the_static_audit_snapshot() {
    let h = harness();
    let append_call = |minutes: i64, token: &str| {
        h.audit
            .append(jumcp_core::audit::AuditEvent {
                tenant: "default".into(),
                actor: jumcp_core::audit::ActorRef {
                    actor_type: "agent".into(),
                    id: format!("agent-{token}"),
                    role: "analyst".into(),
                    token_id: Some(token.into()),
                    source: None,
                },
                event_type: "tool_call".into(),
                payload: json!({
                    "object_type": "tool",
                    "object_id": "ch__query",
                    "action": "call",
                    "outcome": "ok",
                    "data": {
                        "arguments": {"sql": "SELECT id FROM env_air.orders LIMIT 10"},
                        "upstream_id": "ch",
                        "upstream_tool": "query",
                        "elapsed_ms": 4
                    }
                }),
                session_id: format!("s-{token}"),
                ts: (chrono::Utc::now() - chrono::Duration::minutes(minutes)).to_rfc3339(),
                event_id: String::new(),
            })
            .expect("append");
    };
    for minutes in [30, 29, 28] {
        append_call(minutes, "tok-a");
    }
    jumcp_server::static_audit::refresh(&h.service)
        .await
        .expect("static audit refresh");

    let snapshot = call_tool(&h.app, ADMIN_TOKEN, "admin_tool_candidates", json!({})).await;
    assert_eq!(snapshot["result"]["isError"], json!(false), "{snapshot}");
    let snapshot = snapshot["result"]["structuredContent"].clone();
    assert_eq!(snapshot["source"], json!("scheduled"), "{snapshot}");
    assert_eq!(snapshot["window"]["days"], json!(7));
    assert_eq!(
        snapshot["candidates"][0]["subject"],
        json!("env_air.orders")
    );
    assert_eq!(snapshot["candidates"][0]["calls"], json!(3));
    assert_eq!(
        snapshot["candidates"][0]["last_arguments"],
        json!({"sql": "SELECT id FROM env_air.orders LIMIT 10"}),
        "快照自带最近一次调用证据，agent 不必回查审计流水"
    );

    // 新事件不改变已生成的快照：默认调用命中的是后台算好的结果。
    append_call(1, "tok-b");
    let cached = call_tool(&h.app, ADMIN_TOKEN, "admin_tool_candidates", json!({})).await;
    assert_eq!(
        cached["result"]["structuredContent"], snapshot,
        "默认调用必须读快照，而不是每次现扫审计"
    );

    // 显式窗口/条数 = 主动要实时口径：新事件立即反映。
    let live = call_tool(
        &h.app,
        ADMIN_TOKEN,
        "admin_tool_candidates",
        json!({"window_days": 7}),
    )
    .await;
    let live = live["result"]["structuredContent"].clone();
    assert_eq!(live["source"], json!("live"), "{live}");
    assert_eq!(live["candidates"][0]["calls"], json!(4));
    assert_eq!(live["candidates"][0]["distinct_tokens"], json!(2));
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

/// 内置工具结果必须是 content block 数组，且文本块与 `structuredContent` 同源。
///
/// 有的 MCP 客户端只认数组（mcp-go 对对象 `content` 直接报 `content is not an array`），
/// 所以这里钉住形状：单块 text + 同一份 JSON 放 `structuredContent`。
fn assert_text_blocks(resp: &Value) -> Value {
    let result = &resp["result"];
    let blocks = result["content"]
        .as_array()
        .unwrap_or_else(|| panic!("result.content 必须是数组: {resp}"));
    assert_eq!(blocks.len(), 1, "单块结果: {resp}");
    assert_eq!(blocks[0]["type"], json!("text"), "{resp}");
    let text = blocks[0]["text"].as_str().expect("text 块必须带 text");
    let parsed: Value = serde_json::from_str(text).expect("text 块必须是 JSON");
    assert_eq!(
        parsed, result["structuredContent"],
        "文本块与 structuredContent 必须等价: {resp}"
    );
    parsed
}

/// 不支持自定义头的 MCP 客户端（现网 mcp-go 只在 `tools/call` 上带 Authorization）：
/// `initialize` / `tools/list` 可用 `?access_token=` 完成鉴权。
#[tokio::test]
async fn query_access_token_authenticates_without_headers() {
    let h = harness();

    let (status, init) = call(
        &h.app,
        "POST",
        &format!("/mcp?access_token={ANALYST_TOKEN}"),
        None,
        None,
        Some(rpc("initialize", json!({}))),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{init}");
    assert!(
        init["error"].is_null(),
        "query token 必须能完成 initialize: {init}"
    );
    assert_eq!(
        init["result"]["serverInfo"]["name"],
        json!("jumcp"),
        "{init}"
    );
    assert!(init["result"]["protocolVersion"].is_string(), "{init}");

    let (status, listed) = call(
        &h.app,
        "POST",
        &format!("/mcp?access_token={ANALYST_TOKEN}"),
        None,
        None,
        Some(rpc("tools/list", json!({}))),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert!(
        !names_of(&listed).is_empty(),
        "query token 必须能列工具: {listed}"
    );

    // 头与 query 同时给：以头为准。query 里就算是垃圾 token，也不该把带合法头的请求拖成拒绝。
    let (status, both) = call(
        &h.app,
        "POST",
        "/mcp?access_token=tok_bogus_000000",
        Some(ADMIN_TOKEN),
        None,
        Some(rpc("tools/list", json!({}))),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{both}");
    assert!(
        names_of(&both).contains(&"admin_audit_queries".to_string()),
        "头优先：身份必须来自 Authorization: {both}"
    );
}

/// 未知 query token 与未知头 token 走同一条拒绝路径：状态码与错误体都不因来源分叉。
#[tokio::test]
async fn query_access_token_rejects_unknown_token() {
    let h = harness();
    // HTTP 状态沿用现状（200 + JSON-RPC 错误），鉴权结果只在 `data.mcp_code`。
    let (status, body) = call(
        &h.app,
        "POST",
        "/mcp?access_token=tok_not_configured_0001",
        None,
        None,
        Some(rpc("initialize", json!({}))),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["error"]["data"]["mcp_code"],
        json!("MCP_UNAUTHORIZED"),
        "{body}"
    );
    assert!(
        body["result"].is_null(),
        "非法 token 不得初始化成功: {body}"
    );

    // 空值、以及别的参数名（token=）都不算凭据：未授权不因"URL 上带了个东西"而放行。
    for uri in ["/mcp?access_token=", "/mcp?token=tok_bogus"] {
        let (_, body) = call(
            &h.app,
            "POST",
            uri,
            None,
            None,
            Some(rpc("initialize", json!({}))),
        )
        .await;
        assert_eq!(
            body["error"]["data"]["mcp_code"],
            json!("MCP_UNAUTHORIZED"),
            "{uri}: {body}"
        );
    }
}

/// query token 派生的身份与头 token 完全一致：`tool_call` 审计里必须有 token_id。
#[tokio::test]
async fn query_access_token_records_token_identity_in_audit() {
    let h = harness();
    let (_, resp) = call(
        &h.app,
        "POST",
        &format!("/mcp?access_token={ADMIN_TOKEN}"),
        None,
        None,
        Some(rpc(
            "tools/call",
            json!({"name": "admin_audit_queries", "arguments": {}}),
        )),
    )
    .await;
    assert_eq!(resp["result"]["isError"], json!(false), "{resp}");

    let events = audit_events(&h, "tool_call").await;
    let called = events
        .iter()
        .find(|e| e.payload["object_id"] == json!("admin_audit_queries"))
        .expect("必须写 tool_call");
    assert_eq!(
        called.actor["id"],
        json!("agent-admin"),
        "{:?}",
        called.actor
    );
    let token_id = called.actor["token_id"]
        .as_str()
        .expect("query token 的审计必须带 token_id");
    assert!(!token_id.is_empty(), "{:?}", called.actor);
}

/// `/admin/*` 只认头：URL 上的 token 不被接受（回落仅限 MCP 面）。
#[tokio::test]
async fn admin_surface_rejects_query_token() {
    let h = harness();
    let (status, body) = call(
        &h.app,
        "GET",
        &format!("/admin/audit-events?access_token={ALICE_TOKEN}"),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["error"]["code"], json!("MCP_UNAUTHORIZED"), "{body}");
}

/// 内置工具（搜索/描述/组索引/维护通道/错误路径）结果统一成 content block 数组。
#[tokio::test]
async fn builtin_tool_results_are_content_blocks() {
    let h = harness();
    for (name, arguments) in [
        ("search_capabilities", json!({"query": ""})),
        (
            "describe_capability",
            json!({"capability_id": "search_capabilities"}),
        ),
        ("list_groups", json!({})),
        ("admin_review_queue", json!({})),
        ("admin_audit_queries", json!({})),
    ] {
        let resp = call_tool(&h.app, ADMIN_TOKEN, name, arguments).await;
        assert_eq!(resp["result"]["isError"], json!(false), "{name}: {resp}");
        let structured = assert_text_blocks(&resp);
        assert!(
            structured.is_object(),
            "{name} 的 structuredContent 必须是对象: {resp}"
        );
    }
    // 列表类结果落在结构化对象的具名字段里（对象不能直接是数组）。
    let searched = call_tool(
        &h.app,
        ADMIN_TOKEN,
        "search_capabilities",
        json!({"query": ""}),
    )
    .await;
    assert!(
        assert_text_blocks(&searched)["results"].is_array(),
        "{searched}"
    );
    let groups = call_tool(&h.app, ADMIN_TOKEN, "list_groups", json!({})).await;
    assert!(assert_text_blocks(&groups)["groups"].is_array(), "{groups}");

    // 错误路径同样是数组 + `isError:true`，错误对象原样放 structuredContent。
    let missing = call_tool(
        &h.app,
        ADMIN_TOKEN,
        "describe_capability",
        json!({"capability_id": "nope"}),
    )
    .await;
    assert_eq!(missing["result"]["isError"], json!(true), "{missing}");
    let structured = assert_text_blocks(&missing);
    assert_eq!(
        structured["error"]["code"],
        json!("MCP_CAPABILITY_NOT_FOUND"),
        "{missing}"
    );
}

// ---------------------------------------------------------------- agent 自审发布

/// 某条能力已写入的 `publish` 审计条数（幂等断言用）。
async fn publish_events(h: &Harness, id: &str) -> Vec<jumcp_core::audit::Envelope> {
    audit_events(h, "admin_change")
        .await
        .into_iter()
        .filter(|e| e.payload["object_id"] == json!(id) && e.payload["action"] == json!("publish"))
        .collect()
}

/// agent 自审发布：draft → review → published，审批留痕 `source="agent"`/`verified=true`，
/// 版本行的 `approval_id` 由服务端生成（`agent:<actor_id>:<rfc3339>`）。
#[tokio::test]
async fn admin_publish_capability_publishes_draft_as_agent() {
    let h = harness();
    let registered = call_tool(
        &h.app,
        ADMIN_TOKEN,
        "admin_register_template",
        json!({"definition": template_value()}),
    )
    .await;
    assert_eq!(
        registered["result"]["isError"],
        json!(false),
        "{registered}"
    );

    let resp = call_tool(
        &h.app,
        ADMIN_TOKEN,
        "admin_publish_capability",
        json!({"capability_id": "order.daily_summary"}),
    )
    .await;
    assert_eq!(resp["result"]["isError"], json!(false), "{resp}");
    let content = &resp["result"]["structuredContent"];
    assert_eq!(content["id"], json!("order.daily_summary"));
    assert_eq!(content["status"], json!("published"));
    assert_eq!(content["approval"]["source"], json!("agent"));
    assert_eq!(content["approval"]["verified"], json!(true));
    let approval_id = content["approval"]["id"].as_str().expect("approval id");
    assert!(
        approval_id.starts_with("agent:agent-admin:"),
        "缺省单号必须由服务端按 agent:<actor_id>:<rfc3339> 生成: {approval_id}"
    );
    assert!(
        !content["definition_hash"]
            .as_str()
            .unwrap_or_default()
            .is_empty()
    );

    // 版本行同样留痕（不是只写在审计里）。
    let versions = h.store.versions("order.daily_summary").expect("versions");
    assert_eq!(versions.last().expect("version").approval_id, approval_id);
    assert_eq!(versions.last().expect("version").approved_by, "agent-admin");

    // draft→review 与 review→published 各有一条 admin_change，发布那条带 agent 审批。
    let changes = audit_events(&h, "admin_change").await;
    assert!(
        changes
            .iter()
            .any(|e| e.payload["action"] == json!("submit")),
        "draft 起步必须先提审并留痕: {:?}",
        changes.iter().map(|e| &e.payload).collect::<Vec<_>>()
    );
    let publishes = publish_events(&h, "order.daily_summary").await;
    assert_eq!(publishes.len(), 1, "发布只写一条 publish 审计");
    assert_eq!(publishes[0].payload["approval"]["source"], json!("agent"));
    assert_eq!(publishes[0].payload["approval"]["verified"], json!(true));
    assert_eq!(
        publishes[0].payload["approval"]["approved_by"],
        json!("agent-admin")
    );
    assert_eq!(
        publishes[0].payload["approval"]["submitted_by"],
        json!("agent-admin")
    );
    assert_eq!(publishes[0].actor["id"], json!("agent-admin"));
}

/// 同一次调用带 `discoverable`/`callable`：发布即开放，管理面详情立刻反映。
#[tokio::test]
async fn admin_publish_capability_applies_visibility_in_one_call() {
    let h = harness();
    let registered = call_tool(
        &h.app,
        ADMIN_TOKEN,
        "admin_register_template",
        json!({"definition": template_value()}),
    )
    .await;
    assert_eq!(
        registered["result"]["isError"],
        json!(false),
        "{registered}"
    );

    let resp = call_tool(
        &h.app,
        ADMIN_TOKEN,
        "admin_publish_capability",
        json!({"capability_id": "order.daily_summary", "discoverable": true, "callable": true}),
    )
    .await;
    assert_eq!(resp["result"]["isError"], json!(false), "{resp}");
    let content = &resp["result"]["structuredContent"];
    assert_eq!(content["status"], json!("published"));
    assert_eq!(content["discoverable"], json!(true));
    assert_eq!(content["callable"], json!(true));

    let (status, body) = call(
        &h.app,
        "GET",
        "/admin/capabilities/order.daily_summary",
        Some(ALICE_TOKEN),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], json!("published"));
    assert_eq!(body["discoverable"], json!(true));
    assert_eq!(body["callable"], json!(true));
    assert_eq!(
        search_ids(&h.app, ANALYST_TOKEN, "order.daily_summary").await,
        vec!["order.daily_summary".to_string()],
        "agent 发布 + 开可见性后 analyst 立即可见"
    );
    // 可见性仍走既有写路径：一条 visibility 审计。
    let changes = audit_events(&h, "admin_change").await;
    assert!(
        changes
            .iter()
            .any(|e| e.payload["action"] == json!("visibility")),
        "可见性变更必须写 admin_change: {:?}",
        changes.iter().map(|e| &e.payload).collect::<Vec<_>>()
    );
}

/// 重复调用幂等：不重复盖章、不新增 publish 审计；已发布时仍允许补开可见性。
#[tokio::test]
async fn admin_publish_capability_is_idempotent() {
    let h = harness();
    call_tool(
        &h.app,
        ADMIN_TOKEN,
        "admin_register_template",
        json!({"definition": template_value()}),
    )
    .await;
    let first = call_tool(
        &h.app,
        ADMIN_TOKEN,
        "admin_publish_capability",
        json!({"capability_id": "order.daily_summary"}),
    )
    .await;
    assert_eq!(first["result"]["isError"], json!(false), "{first}");
    let first_approval = first["result"]["structuredContent"]["approval"].clone();

    let second = call_tool(
        &h.app,
        ADMIN_TOKEN,
        "admin_publish_capability",
        json!({"capability_id": "order.daily_summary", "discoverable": true}),
    )
    .await;
    assert_eq!(second["result"]["isError"], json!(false), "{second}");
    let content = &second["result"]["structuredContent"];
    assert_eq!(content["status"], json!("published"));
    assert_eq!(
        content["approval"], first_approval,
        "幂等返回必须回放首次发布的审批载荷，而不是伪造一条新的 agent 自审"
    );
    assert_eq!(content["discoverable"], json!(true));
    assert_eq!(
        publish_events(&h, "order.daily_summary").await.len(),
        1,
        "重复发布不得新增 publish 审计"
    );
}

/// 无 admin 组：与"能力不存在"同构，并写 policy_decision。
#[tokio::test]
async fn admin_publish_capability_is_invisible_to_non_admin() {
    let h = harness();
    call_tool(
        &h.app,
        ADMIN_TOKEN,
        "admin_register_template",
        json!({"definition": template_value()}),
    )
    .await;

    let resp = call_tool(
        &h.app,
        ANALYST_TOKEN,
        "admin_publish_capability",
        json!({"capability_id": "order.daily_summary"}),
    )
    .await;
    assert_eq!(err_code(&resp), "MCP_CAPABILITY_NOT_FOUND", "{resp}");
    let record = h
        .store
        .get("order.daily_summary")
        .expect("get")
        .expect("record");
    assert_eq!(record.status, "draft", "被拒的发布不得改动状态");

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

/// `deprecated` 能力不可再发布。
#[tokio::test]
async fn admin_publish_capability_rejects_deprecated() {
    let h = harness();
    publish_capability(&h, "cap.keep", "db.read.core");
    publish_capability(&h, "cap.old", "db.read.core");
    h.store.deprecate("cap.old", "cap.keep").expect("deprecate");

    let resp = call_tool(
        &h.app,
        ADMIN_TOKEN,
        "admin_publish_capability",
        json!({"capability_id": "cap.old"}),
    )
    .await;
    assert_eq!(err_code(&resp), "MCP_STATE_TRANSITION_INVALID", "{resp}");
}
