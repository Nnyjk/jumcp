//! `db_run_template` 端到端契约：MCP `tools/call` → 注册表版本钉住 → 渲染成可直接发送的 SQL →
//! 已接入的上游 MCP 工具 → 结果预算 → `template_run` 审计 → 管理面台账读回。
//!
//! 这些用例锁的不是模板层本身（见 `tests/templates.rs`），而是"链路真的通了"：
//! 只要执行臂被删、送上游的不是渲染后文本、或审计键名漂移，用例立刻失败。
//!
//! 上游用进程内 fake MCP（`tests/support/fake_upstream.rs` 的 `query` 工具）而不是真实
//! ClickHouse：这里要锁的是"谁去执行"，不是 SQL 引擎行为。

use axum::Router;
use axum::http::StatusCode;
use jumcp_core::audit::AuditLog;
use jumcp_core::config::{GroupDef, RoleDef, Settings, TenantPolicy};
use jumcp_core::entry::EffectiveScope;
use jumcp_core::registry::CapabilityStore;
use jumcp_core::template::TemplateRegistry;
use jumcp_server::admin::AdminState;
use jumcp_server::mcp::{McpServer, http_router};
use jumcp_server::service::Service;
use jumcp_server::upstream::UpstreamManager;
use serde_json::{Value, json};
use std::sync::Arc;

use crate::support::fake_upstream::{FakeUpstream, dead_url, spawn_fake_named};
use crate::support::{ALICE_TOKEN, seed_admin_accounts, send, transport_key};

const TEMPLATE_ID: &str = "order.by_day";
const UPSTREAM_ID: &str = "ch";
const RENDERED_SQL: &str =
    "SELECT day, count(*) AS orders FROM public.orders WHERE day = '2026-11-18'";

struct Fixture {
    app: Router,
    audit: Arc<AuditLog>,
    upstream: FakeUpstream,
}

fn settings() -> Settings {
    let mut roles = std::collections::HashMap::new();
    roles.insert(
        "analyst".into(),
        RoleDef {
            name: "数据分析".into(),
            default_groups: vec![
                "basic".into(),
                "db.read.core".into(),
                "db.read.analytics".into(),
            ],
            default_budget: 2000,
        },
    );
    Settings {
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
            ],
            roles,
            tenant_budget_cap: 2000,
            allow_intersect: false,
        },
        budget_overflow: "fail".into(),
        ..Default::default()
    }
}

fn template_value(ttl_s: i64) -> Value {
    json!({
        "template_id": TEMPLATE_ID,
        "group": "db.read.core",
        "risk": "read",
        "description": "按日下单量",
        "tags": ["order"],
        "sql": "SELECT day, count(*) AS orders FROM public.orders WHERE day = :day",
        // 执行不再直连数据库：模板自己声明由哪个上游工具的哪个工具代劳。
        "target": {"upstream": UPSTREAM_ID, "tool": "query", "database": "analytics"},
        "params": [{"name":"day","type":"string","required":true,"nullable":false}],
        "constraints": {
            "allowed_schemas": [],
            "auto_limit": 100,
            "max_result_tokens": 1500,
            "statement_timeout_ms": 5000
        },
        "owner": "data-platform",
        "result_cache_ttl_s": ttl_s
    })
}

fn upstream_body(url: &str) -> Value {
    json!({
        "id": UPSTREAM_ID,
        "name": "ClickHouse 查询",
        "transport": "http",
        "url": url,
        "args": [],
        "env": {},
        "headers": {},
        "category": "custom",
        "enabled": true,
        "timeout_ms": 5000
    })
}

/// 装配：fake 上游（暴露 `query`）→ 注册上游 → 注册/发布模板 → admin ∪ mcp 路由。
async fn fixture_at(url: &str, group: &str, ttl_s: i64) -> Fixture {
    let audit = Arc::new(AuditLog::open(":memory:").expect("audit"));
    let store = Arc::new(CapabilityStore::open(":memory:", Some(audit.clone())).expect("store"));
    // 与生产入口同方言：ClickHouse 模板编译成 `?` 位置占位符，渲染期按空位序替换成字面量。
    let registry = Arc::new(TemplateRegistry::with_dialect(
        store.clone(),
        jumcp_sqlguard::Dialect::ClickHouse,
    ));
    registry.register(&template_value(ttl_s)).expect("register");
    registry
        .submit_for_review(TEMPLATE_ID, "alice")
        .expect("submit");
    registry
        .publish(TEMPLATE_ID, "ap-pub", "bob")
        .expect("publish");
    registry
        .set_visibility(TEMPLATE_ID, Some(true), Some(true), "alice")
        .expect("visibility");
    let boot = settings();
    let manager = Arc::new(UpstreamManager::new(store.clone()));
    let service = Arc::new(
        Service::new(boot.clone())
            .with_audit(audit.clone())
            .with_templates(registry)
            .with_upstreams(manager)
            .with_scope(EffectiveScope {
                groups: vec![
                    "basic".into(),
                    "db.read.core".into(),
                    "db.read.analytics".into(),
                ],
                budget: 2000,
                degraded: false,
                source: "default".into(),
                mode: "strict".into(),
            }),
    );
    seed_admin_accounts(&store, boot.admin_session_ttl_secs);
    let app = jumcp_server::admin::router(AdminState {
        service: service.clone(),
        store: store.clone(),
        transport_key: Some(transport_key()),
    })
    .merge(http_router(Arc::new(McpServer::from_arc(service))));

    let (status, created) = send(
        &app,
        "POST",
        "/admin/upstreams",
        Some(ALICE_TOKEN),
        &[("x-admin-actor", "alice")],
        Some(upstream_body(url)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "接入上游失败: {created}");
    // 接入不再绑定组：模板 target 所在的上游由组的暴露矩阵授权。
    let (status, error) = send(
        &app,
        "PUT",
        &format!("/admin/groups/{group}/exposure"),
        Some(ALICE_TOKEN),
        &[("x-admin-actor", "alice")],
        Some(json!({"upstreams": {UPSTREAM_ID: true}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "组授权失败: {error}");

    Fixture {
        app,
        audit,
        upstream: FakeUpstream::default(),
    }
}

async fn fixture(ttl_s: i64) -> Fixture {
    let (url, upstream) = spawn_fake_named(&["query"]).await;
    let mut h = fixture_at(&url, "db.read.core", ttl_s).await;
    h.upstream = upstream;
    h
}

async fn issue_token(app: &Router, groups: Vec<&str>, caller: &str) -> String {
    // basic 放行 db_run_template 这个常驻工具本身；模板可见性仍由各自的组决定。
    let mut all = vec!["basic"];
    all.extend(groups);
    let (status, body) = send(
        app,
        "POST",
        "/admin/tokens",
        Some(ALICE_TOKEN),
        &[("x-admin-actor", "alice")],
        Some(json!({
            "name": format!("token {caller}"),
            "role": "analyst",
            "groups": all,
            "source": "agent",
            "caller": caller,
            "expires_at": null
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "签发令牌失败: {body}");
    body["token"].as_str().expect("token").to_string()
}

async fn call_tool(app: &Router, token: &str, arguments: Value) -> Value {
    let (status, body) = send(
        app,
        "POST",
        "/mcp",
        Some(token),
        &[],
        Some(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": "db_run_template", "arguments": arguments}
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "MCP 入口应回 200: {body}");
    body
}

fn template_runs(audit: &AuditLog) -> Vec<Value> {
    audit
        .query(Some(&json!({"event_type": "template_run"})))
        .expect("audit query")
        .into_iter()
        .map(|e| e.payload)
        .collect()
}

/// 送给上游的 `tools/call` 参数列表（每次调用一条）。
fn upstream_args(h: &Fixture) -> Vec<Value> {
    h.upstream
        .requests
        .lock()
        .expect("lock")
        .iter()
        .map(|req| req["params"]["arguments"].clone())
        .collect()
}

#[tokio::test]
async fn published_template_runs_through_upstream_and_lands_in_audit() {
    let h = fixture(60).await;
    let token = issue_token(&h.app, vec!["db.read.core"], "agent-template").await;
    let resp = call_tool(
        &h.app,
        &token,
        json!({"template_id": TEMPLATE_ID, "params": {"day": "2026-11-18"}}),
    )
    .await;
    assert_ne!(resp["result"]["isError"], json!(true), "{resp}");
    let structured = &resp["result"]["structuredContent"];
    assert_eq!(
        structured["content"]["rows"][0][1],
        json!(3),
        "{structured}"
    );
    assert_eq!(structured["content"]["row_count"], json!(1), "{structured}");
    assert_eq!(structured["cache"], json!("miss"), "{structured}");

    // 上游收到的必须是"渲染后的 SQL"：占位符已换成转义字面量，且带上 target.database。
    let sent = upstream_args(&h);
    assert_eq!(sent.len(), 1, "上游应被调用一次");
    assert_eq!(sent[0]["sql"], json!(RENDERED_SQL), "{sent:?}");
    assert_eq!(sent[0]["database"], json!("analytics"), "{sent:?}");

    let runs = template_runs(&h.audit);
    assert_eq!(runs.len(), 1, "{runs:?}");
    let payload = &runs[0];
    assert_eq!(payload["object_type"], json!("template"));
    assert_eq!(payload["object_id"], json!(TEMPLATE_ID));
    assert_eq!(payload["action"], json!("invoke"));
    assert_eq!(payload["outcome"], json!("ok"));
    assert_eq!(
        payload["data"]["executed_script"],
        json!(RENDERED_SQL),
        "审计里必须是被真正发出去的那条 SQL"
    );
    assert_eq!(
        payload["data"]["bound_params"],
        json!({"day": "2026-11-18"}),
        "审计保留调用方给的原始参数，不做渲染后覆盖"
    );
    assert_eq!(payload["data"]["upstream_id"], json!(UPSTREAM_ID));
    assert_eq!(payload["data"]["upstream_tool"], json!("query"));
    assert_eq!(payload["data"]["cache"], json!("miss"));
    assert_eq!(payload["data"]["version"], json!(1));
    assert_eq!(payload["data"]["row_count"], json!(1));
    assert_eq!(
        payload["data"]["elapsed_ms"],
        json!(7),
        "elapsed_ms 用上游自报值，缺失才回落到本地计时"
    );
    let tokens = payload["data"]["tokens"]
        .as_i64()
        .expect("tokens 必须是整数");
    assert!(tokens > 0, "{payload}");

    // 台账联通：`admin/stats.rs` 读的就是这些键，键名一漂移台账立刻空。
    let (status, calls) = send(
        &h.app,
        "GET",
        &format!("/admin/tools/{TEMPLATE_ID}/calls"),
        Some(ALICE_TOKEN),
        &[("x-admin-actor", "alice")],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{calls}");
    assert_eq!(calls["total"], json!(1), "{calls}");
    let row = &calls["calls"][0];
    assert_eq!(row["event_type"], json!("template_run"));
    assert_eq!(row["object_type"], json!("template"));
    assert_eq!(row["object_id"], json!(TEMPLATE_ID));
    assert_eq!(row["tokens"], json!(tokens));
    assert_eq!(row["version"], json!(1));
    assert_eq!(row["cache"], json!("miss"));
    assert_eq!(row["executed_script"], json!(RENDERED_SQL));
    assert_eq!(row["bound_params"], json!({"day": "2026-11-18"}));

    let (status, stats) = send(
        &h.app,
        "GET",
        "/admin/tools/stats",
        Some(ALICE_TOKEN),
        &[("x-admin-actor", "alice")],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{stats}");
    let item = stats["tools"]
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["id"] == json!(TEMPLATE_ID)))
        .expect("模板应出现在统计里");
    assert_eq!(item["kind"], json!("template"));
    assert_eq!(item["calls"], json!(1));
    assert_eq!(item["cache_hits"], json!(0));
}

#[tokio::test]
async fn unknown_template_is_an_error_and_audited() {
    let h = fixture(60).await;
    let token = issue_token(&h.app, vec!["db.read.core"], "agent-unknown").await;
    let resp = call_tool(&h.app, &token, json!({"template_id": "nope.missing"})).await;
    assert_eq!(resp["result"]["isError"], json!(true), "{resp}");
    assert_eq!(
        resp["result"]["structuredContent"]["error"]["code"],
        json!("MCP_CAPABILITY_NOT_FOUND"),
        "{resp}"
    );

    let runs = template_runs(&h.audit);
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(runs[0]["outcome"], json!("error"));
    assert_eq!(
        runs[0]["error_code"],
        json!("MCP_CAPABILITY_NOT_FOUND"),
        "{runs:?}"
    );
    assert_eq!(upstream_args(&h).len(), 0, "模板不存在不得触达上游");
}

#[tokio::test]
async fn pinned_session_rejects_version_switch() {
    let h = fixture(60).await;
    let token = issue_token(&h.app, vec!["db.read.core"], "agent-pin").await;
    let first = call_tool(
        &h.app,
        &token,
        json!({"template_id": TEMPLATE_ID, "params": {"day": "2026-11-18"}, "version": 1}),
    )
    .await;
    assert_ne!(first["result"]["isError"], json!(true), "{first}");

    let second = call_tool(
        &h.app,
        &token,
        json!({"template_id": TEMPLATE_ID, "params": {"day": "2026-11-18"}, "version": 2}),
    )
    .await;
    assert_eq!(second["result"]["isError"], json!(true), "{second}");
    let error = &second["result"]["structuredContent"]["error"];
    assert_eq!(error["code"], json!("MCP_VERSION_CONFLICT"), "{second}");
    assert_eq!(error["data"]["pinned"], json!(1));
    assert_eq!(error["data"]["requested"], json!(2));
}

#[tokio::test]
async fn second_identical_call_hits_result_cache() {
    let h = fixture(60).await;
    let token = issue_token(&h.app, vec!["db.read.core"], "agent-cache").await;
    let args = json!({"template_id": TEMPLATE_ID, "params": {"day": "2026-11-18"}});
    let first = call_tool(&h.app, &token, args.clone()).await;
    let second = call_tool(&h.app, &token, args).await;
    assert_eq!(first["result"]["structuredContent"]["cache"], json!("miss"));
    assert_eq!(
        second["result"]["structuredContent"]["cache"],
        json!("hit"),
        "{second}"
    );
    assert_eq!(
        first["result"]["structuredContent"]["content"],
        second["result"]["structuredContent"]["content"],
        "命中的内容必须与首次一致"
    );
    assert_eq!(upstream_args(&h).len(), 1, "命中的第二次调用不得再触达上游");

    let runs = template_runs(&h.audit);
    assert_eq!(runs.len(), 2, "{runs:?}");
    let miss = runs
        .iter()
        .find(|p| p["data"]["cache"] == json!("miss"))
        .expect("首次调用必须留审计");
    let hit = runs
        .iter()
        .find(|p| p["data"]["cache"] == json!("hit"))
        .expect("缓存命中必须留审计");
    assert_eq!(hit["data"]["tokens"], miss["data"]["tokens"]);
    assert_eq!(hit["data"]["version"], json!(1));
    assert_eq!(hit["outcome"], json!("ok"));
    assert_eq!(hit["data"]["upstream_id"], json!(UPSTREAM_ID));
}

#[tokio::test]
async fn oversized_result_is_rejected_by_budget() {
    let h = fixture(60).await;
    // 单行就是一个大文本：随便的正经结果都不会到这个量级，但足以越过 max_result_tokens。
    let huge = "x".repeat(20_000);
    h.upstream.set_query_result(json!({
        "columns": [{"name": "day", "type": "String"}],
        "rows": [[huge]],
        "row_count": 1,
        "truncated": false,
        "elapsed_ms": 3
    }));
    let token = issue_token(&h.app, vec!["db.read.core"], "agent-budget").await;
    let resp = call_tool(
        &h.app,
        &token,
        json!({"template_id": TEMPLATE_ID, "params": {"day": "2026-11-18"}}),
    )
    .await;
    assert_eq!(resp["result"]["isError"], json!(true), "{resp}");
    let error = &resp["result"]["structuredContent"]["error"];
    assert_eq!(error["code"], json!("MCP_BUDGET_EXCEEDED"), "{resp}");
    assert_eq!(error["data"]["scope"], json!("max_result_tokens"));

    let runs = template_runs(&h.audit);
    assert_eq!(runs[0]["outcome"], json!("error"), "{runs:?}");
    assert_eq!(runs[0]["error_code"], json!("MCP_BUDGET_EXCEEDED"));
    assert_eq!(runs[0]["data"]["upstream_id"], json!(UPSTREAM_ID));
}

#[tokio::test]
async fn unreachable_upstream_maps_to_upstream_unreachable() {
    let url = dead_url().await;
    let h = fixture_at(&url, "db.read.core", 60).await;
    let token = issue_token(&h.app, vec!["db.read.core"], "agent-dead").await;
    let resp = call_tool(
        &h.app,
        &token,
        json!({"template_id": TEMPLATE_ID, "params": {"day": "2026-11-18"}}),
    )
    .await;
    assert_eq!(resp["result"]["isError"], json!(true), "{resp}");
    let error = &resp["result"]["structuredContent"]["error"];
    assert_eq!(error["code"], json!("MCP_UPSTREAM_UNREACHABLE"), "{resp}");
    assert_eq!(error["data"]["upstream_id"], json!(UPSTREAM_ID));

    let runs = template_runs(&h.audit);
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(runs[0]["outcome"], json!("error"), "{runs:?}");
    assert_eq!(runs[0]["error_code"], json!("MCP_UPSTREAM_UNREACHABLE"));
}

#[tokio::test]
async fn upstream_outside_scope_is_not_found() {
    // 上游挂在分析组：令牌只有核心组，投影里就没有它——模板自己不能扩大调用方的作用域。
    // 上游地址故意不可达：解析阶段就该失败，任何"先连一下再判断"的写法会在这里暴露。
    let url = dead_url().await;
    let h = fixture_at(&url, "db.read.analytics", 60).await;
    let token = issue_token(&h.app, vec!["db.read.core"], "agent-scope").await;
    let resp = call_tool(
        &h.app,
        &token,
        json!({"template_id": TEMPLATE_ID, "params": {"day": "2026-11-18"}}),
    )
    .await;
    // 模板在核心组可见可调，但 target 指向上游时作用域不匹配。
    assert_eq!(resp["result"]["isError"], json!(true), "{resp}");
    let error = &resp["result"]["structuredContent"]["error"];
    assert_eq!(error["code"], json!("MCP_UPSTREAM_NOT_FOUND"), "{resp}");
    assert_eq!(error["data"]["upstream"], json!(UPSTREAM_ID));
    assert_eq!(error["data"]["tool"], json!("query"));
    assert_eq!(upstream_args(&h).len(), 0, "解析不到上游不得发起调用");
}

#[tokio::test]
async fn template_group_outside_scope_is_not_callable() {
    let h = fixture(60).await;
    // 令牌只授 `db.read.analytics`：模板在 `db.read.core`，可见性不放宽调用权。
    let token = issue_token(&h.app, vec!["db.read.analytics"], "agent-outside").await;
    let resp = call_tool(
        &h.app,
        &token,
        json!({"template_id": TEMPLATE_ID, "params": {"day": "2026-11-18"}}),
    )
    .await;
    assert_eq!(resp["result"]["isError"], json!(true), "{resp}");
    assert_eq!(
        resp["result"]["structuredContent"]["error"]["code"],
        json!("MCP_CAPABILITY_NOT_FOUND"),
        "{resp}"
    );
    assert_eq!(upstream_args(&h).len(), 0);
    let runs = template_runs(&h.audit);
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(runs[0]["error_code"], json!("MCP_CAPABILITY_NOT_FOUND"));
}

#[tokio::test]
async fn template_without_target_is_rejected_at_registration() {
    let store = Arc::new(CapabilityStore::open(":memory:", None).expect("store"));
    let registry = TemplateRegistry::with_dialect(store, jumcp_sqlguard::Dialect::ClickHouse);
    let mut def = template_value(60);
    def.as_object_mut().expect("object").remove("target");
    let err = registry.register(&def).expect_err("缺 target 必须拒绝");
    assert_eq!(err.code, "MCP_VALIDATION_ERROR", "{err:?}");
    assert_eq!(err.data["field"], json!("target"), "{err:?}");
    assert!(
        err.message.contains("target.upstream"),
        "提示必须指明该声明什么: {}",
        err.message
    );
}

#[tokio::test]
async fn template_target_tool_must_not_be_an_admin_tool() {
    let store = Arc::new(CapabilityStore::open(":memory:", None).expect("store"));
    let registry = TemplateRegistry::with_dialect(store, jumcp_sqlguard::Dialect::ClickHouse);
    let mut def = template_value(60);
    def["target"]["tool"] = json!("admin_x");
    let err = registry.register(&def).expect_err("admin_ 前缀必须拒绝");
    assert_eq!(err.code, "MCP_VALIDATION_ERROR", "{err:?}");
    assert_eq!(err.data["rule"], json!("tool_reserved"), "{err:?}");
}

#[tokio::test]
async fn template_target_upstream_must_not_be_reserved() {
    let store = Arc::new(CapabilityStore::open(":memory:", None).expect("store"));
    let registry = TemplateRegistry::with_dialect(store, jumcp_sqlguard::Dialect::ClickHouse);
    for reserved in ["basic", "jumcp", "search_capabilities"] {
        let mut def = template_value(60);
        def["target"]["upstream"] = json!(reserved);
        let err = registry.register(&def).expect_err("保留 id 必须拒绝");
        assert_eq!(err.code, "MCP_VALIDATION_ERROR", "{err:?}");
    }
}
