//! `db_run_template` 端到端契约：MCP `tools/call` → 注册表版本钉住 → 渲染 →
//! 执行器 → 结果预算 → `template_run` 审计 → 管理面台账读回。
//!
//! 这些用例锁的不是模板层本身（见 `tests/templates.rs`），而是"链路真的通了"：
//! 只要执行臂被删或审计键名漂移，台账就会空掉，用例立刻失败。
//!
//! 装配与 `tests/tokens/fixtures.rs` 同形：自带 fake executor（不 spawn ClickHouse）+
//! `admin::router` ∪ `mcp::http_router`，令牌走 `POST /admin/tokens` 的 `builtin_tools`。

use axum::Router;
use axum::http::StatusCode;
use jumcp_core::audit::AuditLog;
use jumcp_core::config::{DatasourceRef, GroupDef, RoleDef, Settings, TenantPolicy};
use jumcp_core::entry::EffectiveScope;
use jumcp_core::registry::CapabilityStore;
use jumcp_core::template::TemplateRegistry;
use jumcp_server::admin::AdminState;
use jumcp_server::executor::{ExecFuture, QueryExecutor, QueryParams};
use jumcp_server::mcp::{McpServer, http_router};
use jumcp_server::service::Service;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

use crate::support::{ALICE_TOKEN, seed_admin_accounts, send, transport_key};

const TEMPLATE_ID: &str = "order.by_day";

/// 记录收到的 `QueryParams` 并返回固定结果：模板执行臂必须把渲染结果原样交给执行器。
#[derive(Default)]
struct RecordingExecutor {
    seen: Mutex<Vec<QueryParams>>,
}

impl QueryExecutor for RecordingExecutor {
    fn execute<'a>(&'a self, _scope: &'a EffectiveScope, q: &'a QueryParams) -> ExecFuture<'a> {
        if let Ok(mut seen) = self.seen.lock() {
            seen.push(q.clone());
        }
        Box::pin(async {
            Ok(json!({
                "rows": [{"day": "2026-11-18", "orders": 3}],
                "row_count": 1,
                "truncated": false,
                "tokens": 7
            }))
        })
    }
}

struct Fixture {
    app: Router,
    audit: Arc<AuditLog>,
    executor: Arc<RecordingExecutor>,
}

fn settings() -> Settings {
    let mut roles = std::collections::HashMap::new();
    roles.insert(
        "analyst".into(),
        RoleDef {
            name: "数据分析".into(),
            default_groups: vec!["db.read.core".into(), "db.read.analytics".into()],
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

fn template_value(ttl_s: i64) -> Value {
    json!({
        "template_id": TEMPLATE_ID,
        "group": "db.read.core",
        "risk": "read",
        "description": "按日下单量",
        "tags": ["order"],
        "sql": "SELECT day, count(*) AS orders FROM public.orders WHERE day = :day",
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

async fn fixture(ttl_s: i64) -> Fixture {
    let audit = Arc::new(AuditLog::open(":memory:").expect("audit"));
    let store = Arc::new(CapabilityStore::open(":memory:", Some(audit.clone())).expect("store"));
    // 与生产入口同方言：ClickHouse 模板编译成 `?` 位置占位符，Postgres 方言会渲染成 `$1`，
    // 那样 `check_placeholders` 立刻数不上（这正是执行臂必须挂在真实方言链路上的原因）。
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
    let executor = Arc::new(RecordingExecutor::default());
    let boot = settings();
    let service = Arc::new(
        Service::new(boot.clone())
            .with_executor(executor.clone())
            .with_audit(audit.clone())
            .with_templates(registry)
            .with_scope(EffectiveScope {
                groups: vec!["db.read.core".into(), "db.read.analytics".into()],
                budget: 2000,
                degraded: false,
                source: "default".into(),
                mode: "strict".into(),
                allowed_builtin_tools: None,
            }),
    );
    seed_admin_accounts(&store, boot.admin_session_ttl_secs);
    let app = jumcp_server::admin::router(AdminState {
        service: service.clone(),
        store: store.clone(),
        transport_key: Some(transport_key()),
    })
    .merge(http_router(Arc::new(McpServer::from_arc(service))));
    Fixture {
        app,
        audit,
        executor,
    }
}

async fn issue_token(app: &Router, groups: Vec<&str>, caller: &str) -> String {
    let (status, body) = send(
        app,
        "POST",
        "/admin/tokens",
        Some(ALICE_TOKEN),
        &[("x-admin-actor", "alice")],
        Some(json!({
            "name": format!("token {caller}"),
            "role": "analyst",
            "groups": groups,
            "builtin_tools": ["db_run_template"],
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

#[tokio::test]
async fn published_template_runs_through_mcp_and_lands_in_audit() {
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
        structured["content"]["rows"][0]["orders"],
        json!(3),
        "{structured}"
    );
    assert_eq!(structured["cache"], json!("miss"), "{structured}");

    let seen = h.executor.seen.lock().expect("lock").clone();
    assert_eq!(seen.len(), 1, "执行器应被调用一次");
    assert_eq!(seen[0].sql.matches('?').count(), 1, "{:?}", seen[0].sql);
    assert_eq!(seen[0].params, vec![json!("2026-11-18")]);
    assert_eq!(seen[0].max_rows, 100, "auto_limit 即执行上限");
    assert_eq!(seen[0].cell_char_limit, 200);
    assert_eq!(seen[0].statement_timeout_ms, 5000);

    let runs = template_runs(&h.audit);
    assert_eq!(runs.len(), 1, "{runs:?}");
    let payload = &runs[0];
    assert_eq!(payload["object_type"], json!("template"));
    assert_eq!(payload["object_id"], json!(TEMPLATE_ID));
    assert_eq!(payload["action"], json!("invoke"));
    assert_eq!(payload["outcome"], json!("ok"));
    assert!(
        payload["data"]["executed_script"]
            .as_str()
            .is_some_and(|s| s.contains("FROM public.orders")),
        "{payload}"
    );
    assert_eq!(payload["data"]["bound_params"], json!(["2026-11-18"]));
    assert_eq!(
        payload["data"]["arguments"]["params"]["day"],
        json!("2026-11-18")
    );
    assert_eq!(payload["data"]["tokens"], json!(7));
    assert_eq!(payload["data"]["cache"], json!("miss"));
    assert_eq!(payload["data"]["version"], json!(1));
    assert!(payload["data"]["elapsed_ms"].is_i64(), "{payload}");

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
    assert_eq!(row["tokens"], json!(7));
    assert_eq!(row["version"], json!(1));
    assert_eq!(row["cache"], json!("miss"));
    assert!(row["executed_script"].as_str().is_some(), "{row}");
    assert_eq!(row["bound_params"], json!(["2026-11-18"]));

    // `/admin/tools/stats` 里模板行的 kind/calls/hit_rate 来自同一批事件。
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
    assert_eq!(h.executor.seen.lock().expect("lock").len(), 0);
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
    assert_eq!(
        h.executor.seen.lock().expect("lock").len(),
        1,
        "命中的第二次调用不得再进执行器"
    );

    let runs = template_runs(&h.audit);
    assert_eq!(runs.len(), 2, "{runs:?}");
    let hit = runs
        .iter()
        .find(|p| p["data"]["cache"] == json!("hit"))
        .expect("缓存命中必须留审计");
    assert_eq!(hit["data"]["tokens"], json!(7), "命中分支沿用缓存的 tokens");
    assert_eq!(hit["data"]["version"], json!(1));
    assert_eq!(hit["outcome"], json!("ok"));
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
    assert_eq!(h.executor.seen.lock().expect("lock").len(), 0);
    let runs = template_runs(&h.audit);
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(runs[0]["error_code"], json!("MCP_CAPABILITY_NOT_FOUND"));
}
