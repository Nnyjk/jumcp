//! 第三方 SQL 前置拦截：10 条规则各自的拒绝形状、放行路径、非 SQL 工具不受影响、库白名单。
//!
//! 每个拒绝用例断言三件事：上游**没有收到** `tools/call`（拦截在 `resolve` 之前）、
//! 响应是 MCP `isError` 而不是 JSON-RPC error、`policy_decision` 里能按 `rule` 聚合。

use crate::fixtures::*;
use crate::support::fake_upstream::{FakeUpstream, spawn_fake_named};
use axum::http::StatusCode;
use jumcp_server::sql_guard::GuardConfig;
use serde_json::{Value, json};

const TOOL: &str = "analytics__query";
/// 被拒/放行用例共用的库白名单：与部署里 `JUMCP_SQL_ALLOWED_SCHEMAS` 同形。
const SCHEMA: &str = "env_air";

fn guard(allowed_schemas: &[&str]) -> GuardConfig {
    GuardConfig {
        allowed_schemas: allowed_schemas.iter().map(|s| (*s).to_string()).collect(),
        ..GuardConfig::default()
    }
}

/// 起一个只有 `query` 工具的上游并签发只带它名字的令牌；`None` = 用默认配置（不限库）。
async fn setup(sql_guard: Option<GuardConfig>) -> (Harness, FakeUpstream, String) {
    let h = match sql_guard {
        Some(guard) => harness_with_sql_guard(guard),
        None => harness(),
    };
    let (url, fake) = spawn_fake_named(&["query"]).await;
    let (status, item) = create_upstream(&h, upstream_body("analytics", &url)).await;
    assert_eq!(status, StatusCode::OK, "注册上游失败: {item}");
    let token = issue_token(&h, "agent-sql", "analyst", &["db.read.analytics"], &[TOOL]).await;
    (h, fake, token)
}

fn forwarded_calls(fake: &FakeUpstream) -> usize {
    fake.requests.lock().expect("lock").len()
}

/// 拒绝用例的完整契约：响应形状 + 审计 + 上游零交互。
async fn assert_blocked(sql: &str, rule: &str) {
    assert_blocked_with(guard(&[]), json!({"sql": sql}), rule).await;
}

async fn assert_blocked_with(sql_guard: GuardConfig, arguments: Value, rule: &str) {
    let (h, fake, token) = setup(Some(sql_guard)).await;
    let resp = call_tool(&h.app, &token, TOOL, arguments.clone()).await;

    assert_eq!(
        forwarded_calls(&fake),
        0,
        "被拒的调用不得触达上游 tools/call: {arguments}"
    );
    assert_eq!(resp["result"]["isError"], json!(true), "{resp}");
    let structured = &resp["result"]["structuredContent"];
    assert_eq!(
        structured["error"]["code"],
        json!("MCP_SQL_BLOCKED"),
        "{resp}"
    );
    assert_eq!(structured["error"]["data"]["rule"], json!(rule), "{resp}");
    assert_eq!(
        structured["error"]["data"]["upstream_id"],
        json!("analytics")
    );
    assert_eq!(structured["error"]["data"]["tool"], json!(TOOL));
    assert!(
        structured["error"]["data"]["hint"]
            .as_str()
            .is_some_and(|hint| !hint.is_empty()),
        "拒绝必须给出可执行的 hint: {resp}"
    );
    // 文本块承载同一份结构：只认 content 的客户端也要能读到原因。
    assert_eq!(resp["result"]["content"][0]["text"], structured.to_string());

    let events = h
        .audit
        .query(Some(
            &json!({"event_type": "policy_decision", "object_id": TOOL}),
        ))
        .expect("audit query");
    let decision = events.last().expect("policy_decision 事件");
    assert_eq!(decision.payload["action"], json!("deny"));
    assert_eq!(decision.payload["error_code"], json!("MCP_SQL_BLOCKED"));
    let detail = &decision.payload["data"]["detail"];
    assert_eq!(detail["rule"], json!(rule));
    assert_eq!(detail["surface"], json!("mcp/sql-preflight"));
    assert_eq!(detail["tool"], json!(TOOL));
    assert_eq!(detail["upstream_id"], json!("analytics"));

    // 拦在调用前：这条路径不产生 tool_call 台账（否则统计会把被拒的调用算成一次调用）。
    let calls = h
        .audit
        .query(Some(&json!({"event_type": "tool_call", "object_id": TOOL})))
        .expect("audit query");
    assert!(calls.is_empty(), "被拒的调用不得写 tool_call: {calls:?}");
}

#[tokio::test]
async fn empty_sql_is_blocked() {
    assert_blocked("   ", "empty").await;
}

#[tokio::test]
async fn non_select_sql_is_blocked() {
    assert_blocked("EXPLAIN SELECT 1", "not_select").await;
}

#[tokio::test]
async fn multiple_statements_are_blocked() {
    assert_blocked("SELECT 1 LIMIT 1; SELECT 2 LIMIT 1", "multiple_statements").await;
}

#[tokio::test]
async fn comments_are_blocked() {
    assert_blocked("SELECT * FROM t LIMIT 10 -- 全表", "comment").await;
}

#[tokio::test]
async fn write_keywords_are_blocked() {
    assert_blocked("SELECT * FROM t LIMIT 1 FOR UPDATE", "write_statement").await;
}

#[tokio::test]
async fn system_tables_are_blocked() {
    assert_blocked("SELECT name FROM system.tables LIMIT 10", "system_table").await;
}

#[tokio::test]
async fn bypass_functions_are_blocked() {
    assert_blocked(
        "SELECT dictGet('d', 'v', id) FROM env_air.t LIMIT 10",
        "bypass_function",
    )
    .await;
}

#[tokio::test]
async fn unbounded_scan_is_blocked() {
    assert_blocked("SELECT * FROM env_air.air_point_info", "unbounded").await;
}

#[tokio::test]
async fn oversized_limit_is_blocked() {
    assert_blocked("SELECT id FROM env_air.t LIMIT 5000", "limit_too_large").await;
}

#[tokio::test]
async fn out_of_scope_schema_is_blocked() {
    assert_blocked_with(
        guard(&[SCHEMA]),
        json!({"sql": "SELECT id FROM other.t LIMIT 10"}),
        "schema_out_of_scope",
    )
    .await;
}

/// `query` / `statement` 也是 SQL 字段：第三方工具的入参命名不统一，拦截不能只认 `sql`。
#[tokio::test]
async fn sql_aliases_are_guarded_too() {
    assert_blocked_with(
        guard(&[]),
        json!({"query": "SELECT id FROM t LIMIT 5000"}),
        "limit_too_large",
    )
    .await;
}

/// 放行路径：护栏不能挡住正常的限行/聚合/点查——它们原样转发到上游。
#[tokio::test]
async fn bounded_queries_are_forwarded() {
    for sql in [
        "SELECT id FROM env_air.air_point_info LIMIT 10",
        "SELECT count() FROM env_air.air_point_info",
        "SELECT id FROM env_air.air_point_info WHERE id = 1",
    ] {
        let (h, fake, token) = setup(Some(guard(&[SCHEMA]))).await;
        let resp = call_tool(&h.app, &token, TOOL, json!({"sql": sql})).await;
        assert_eq!(resp["result"]["isError"], json!(false), "{sql}: {resp}");
        assert_eq!(forwarded_calls(&fake), 1, "放行的调用必须转发: {sql}");
        let forwarded = fake.requests.lock().expect("lock").clone();
        assert_eq!(
            forwarded.last().expect("forwarded")["params"]["arguments"]["sql"],
            json!(sql)
        );
    }
}

/// 非 SQL 工具（入参没有 sql/query/statement）完全不受拦截影响。
#[tokio::test]
async fn non_sql_tool_is_unaffected() {
    for arguments in [json!({"msg": "hi"}), json!({"sql": 5})] {
        let (h, fake, token) = setup(None).await;
        let resp = call_tool(&h.app, &token, TOOL, arguments.clone()).await;
        assert_eq!(
            resp["result"]["isError"],
            json!(false),
            "{arguments}: {resp}"
        );
        assert_eq!(forwarded_calls(&fake), 1, "{arguments}");
    }
}

/// 白名单为空 = 不做库范围限定：同样的跨库查询放行。
#[tokio::test]
async fn empty_allowlist_permits_any_schema() {
    let (h, fake, token) = setup(Some(guard(&[]))).await;
    let resp = call_tool(
        &h.app,
        &token,
        TOOL,
        json!({"sql": "SELECT id FROM other.t LIMIT 10"}),
    )
    .await;
    assert_eq!(resp["result"]["isError"], json!(false), "{resp}");
    assert_eq!(forwarded_calls(&fake), 1);
}
