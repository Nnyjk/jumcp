//! 契约测试：入口裁剪、sqlguard 各拒绝码、参数风格/个数、预算 fail|truncate、版本冲突、自检配置。
//!
//! 全部不依赖真实 PG：执行器用 FakeExecutor 注入；真实 PG 用例在 jumcp-server 单元里
//! `#[ignore]` + `JUMCP_TEST_PG_DSN` 门槛。

use jumcp_core::audit::AuditLog;
use jumcp_core::budget::BudgetCaps;
use jumcp_core::config::{DatasourceRef, GroupDef, RoleDef, Settings, TenantPolicy};
use jumcp_core::entry::{Actor, EffectiveScope, GroupRequest, resolve_scope};
use jumcp_core::errors::McpError;
use jumcp_server::catalog::Capability;
use jumcp_server::executor::{ExecFuture, QueryExecutor, QueryParams};
use jumcp_server::mcp::McpServer;
use jumcp_server::service::Service;
use serde_json::{Value, json};
use std::sync::Arc;

/// 注入型执行器：契约测试只需要可复现的结果或错误。
struct FakeExecutor {
    result: Result<Value, McpError>,
}

impl QueryExecutor for FakeExecutor {
    fn execute<'a>(&'a self, _scope: &'a EffectiveScope, _q: &'a QueryParams) -> ExecFuture<'a> {
        let result = self.result.clone();
        Box::pin(async move { result })
    }
}

fn policy() -> TenantPolicy {
    let mut roles = std::collections::HashMap::new();
    roles.insert(
        "analyst".into(),
        RoleDef {
            name: "analyst".into(),
            default_groups: vec!["db.read.core".into(), "db.read.analytics".into()],
            default_budget: 2000,
        },
    );
    TenantPolicy {
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
                code: "db.admin.schema".into(),
                name: "库表管理".into(),
                priority: 1,
                min_budget: 800,
            },
        ],
        roles,
        tenant_budget_cap: 2000,
        allow_intersect: false,
    }
}

fn settings(overflow: &str) -> Settings {
    Settings {
        policy: policy(),
        budget_overflow: overflow.to_string(),
        datasources: vec![DatasourceRef {
            name: "main-ch".into(),
            dsn_env: "JUMCP_TEST_MAIN_DSN".into(),
            engine: "clickhouse".into(),
            readonly: true,
            allowed_schemas: vec!["default".into()],
            statement_timeout_ms: 5000,
        }],
        ..Default::default()
    }
}

fn server(
    executor: Arc<dyn QueryExecutor>,
    overflow: &str,
    caps: Option<BudgetCaps>,
) -> (McpServer, Arc<AuditLog>) {
    let audit = Arc::new(AuditLog::open(":memory:").expect("in-memory audit"));
    let mut catalog = jumcp_server::catalog::Catalog::default();
    catalog.register(Capability {
        id: "db_query_readonly".into(),
        kind: "tool".into(),
        group: "db.read.core".into(),
        one_line: "通用只读 SQL 入口".into(),
        ..Capability::new("db_query_readonly", "tool", "db.read.core")
    });
    catalog.register(Capability {
        id: "order.daily_summary".into(),
        kind: "tool".into(),
        group: "db.read.analytics".into(),
        one_line: "订单日汇总".into(),
        params_schema: json!({"type":"object","properties":{"day":{"type":"string"}}}),
        ..Capability::new("order.daily_summary", "tool", "db.read.analytics")
    });
    let mut svc = Service::new(settings(overflow))
        .with_executor(executor)
        .with_catalog(catalog)
        .with_audit(audit.clone());
    if let Some(c) = caps {
        svc = svc.with_budget_caps(c);
    }
    (McpServer::new(svc), audit)
}

fn ok_executor() -> Arc<dyn QueryExecutor> {
    Arc::new(FakeExecutor {
        result: Ok(json!({
            "rows": [["订单"]],
            "row_count": 1,
            "truncated": false,
            "effective_max_rows": 100,
            "elapsed_ms": 3,
            "tokens": 12
        })),
    })
}

fn call(name: &str, args: Value) -> Value {
    json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":name,"arguments":args}})
}

fn err_code(resp: &Value) -> String {
    resp["result"]["content"]["error"]["code"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

async fn audit_events(audit: &AuditLog, event_type: &str) -> Vec<Value> {
    audit
        .query(Some(&json!({"event_type": event_type})))
        .expect("audit query")
        .into_iter()
        .map(|e| e.payload)
        .collect()
}

#[tokio::test]
async fn strict_rejects_group_outside_allowlist() {
    let actor = Actor::new("t1", "analyst");
    let req = GroupRequest {
        header_groups: Some("db.admin.schema".into()),
        ..Default::default()
    };
    let e = resolve_scope(&actor, &req, &policy(), "strict", None).unwrap_err();
    assert_eq!(e.code, "MCP_GROUP_FORBIDDEN");
    assert_eq!(e.data["offending_groups"], json!(["db.admin.schema"]));
}

#[tokio::test]
async fn sqlguard_rejects_writable_cte_and_audits_policy_decision() {
    let (server, audit) = server(ok_executor(), "fail", None);
    let resp = server
        .handle(&call(
            "db_query_readonly",
            json!({"sql": "WITH w AS (INSERT INTO t VALUES (1) RETURNING 1) SELECT * FROM w"}),
        ))
        .await;
    assert_eq!(err_code(&resp), "MCP_SQL_REJECTED");
    let decisions = audit_events(&audit, "policy_decision").await;
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0]["error_code"], json!("MCP_SQL_REJECTED"));
}

#[tokio::test]
async fn sqlguard_rejects_multi_statement() {
    let (server, _audit) = server(ok_executor(), "fail", None);
    let resp = server
        .handle(&call(
            "db_query_readonly",
            json!({"sql": "SELECT 1; SELECT 2"}),
        ))
        .await;
    assert_eq!(err_code(&resp), "MCP_MULTI_STATEMENT");
}

/// 参数个数不匹配在执行前就被拒：必须写 `tool_call`（台账/明细才看得到这次调用），
/// 同时保留 `policy_decision`；且只写一条，不重复计数。
#[tokio::test]
async fn param_count_mismatch_writes_tool_call_audit() {
    let (server, audit) = server(ok_executor(), "fail", None);
    let resp = server
        .handle(&call(
            "db_query_readonly",
            json!({"sql": "SELECT 1", "params": [42]}),
        ))
        .await;
    assert_eq!(err_code(&resp), "MCP_PARAM_COUNT_MISMATCH");

    let calls = audit_events(&audit, "tool_call").await;
    assert_eq!(calls.len(), 1, "拒绝也要计入 tool_call，且只记一条");
    assert_eq!(calls[0]["object_type"], json!("tool"));
    assert_eq!(calls[0]["object_id"], json!("db_query_readonly"));
    assert_eq!(calls[0]["action"], json!("invoke"));
    assert_eq!(calls[0]["outcome"], json!("error"));
    assert_eq!(calls[0]["error_code"], json!("MCP_PARAM_COUNT_MISMATCH"));
    assert!(calls[0]["error_message"].as_str().is_some());
    assert_eq!(calls[0]["data"]["executed_script"], json!("SELECT 1"));
    assert_eq!(calls[0]["data"]["bound_params"], json!([42]));
    // 早期拒绝没有实际执行脚本，但入参必须原样留存。
    assert_eq!(
        calls[0]["data"]["arguments"],
        json!({"sql": "SELECT 1", "params": [42]})
    );

    // 拒绝原因同时写 policy_decision，并关联到同一次 tool_call。
    let decisions = audit_events(&audit, "policy_decision").await;
    assert_eq!(decisions.len(), 1);
    assert_eq!(
        decisions[0]["error_code"],
        json!("MCP_PARAM_COUNT_MISMATCH")
    );
    assert_eq!(decisions[0]["action"], json!("deny"));
    assert_eq!(decisions[0]["outcome"], json!("denied"));
    assert_eq!(calls[0]["object_id"], decisions[0]["object_id"]);
    let call_event_id = audit
        .query(Some(&json!({"event_type": "tool_call"})))
        .expect("query")
        .first()
        .expect("tool_call 行")
        .event_id
        .clone();
    assert_eq!(decisions[0]["correlation_id"], json!(call_event_id));
}

#[tokio::test]
async fn sqlguard_rejects_schema_out_of_scope() {
    let (server, _audit) = server(ok_executor(), "fail", None);
    let resp = server
        .handle(&call(
            "db_query_readonly",
            json!({"sql": "SELECT * FROM secret.orders"}),
        ))
        .await;
    assert_eq!(err_code(&resp), "MCP_SCHEMA_OUT_OF_SCOPE");
}

#[tokio::test]
async fn named_placeholder_rejected_without_rewriting() {
    let (server, _audit) = server(ok_executor(), "fail", None);
    let resp = server
        .handle(&call(
            "db_query_readonly",
            json!({"sql": "SELECT * FROM orders WHERE id = :order_id"}),
        ))
        .await;
    assert_eq!(err_code(&resp), "MCP_PARAM_STYLE_UNSUPPORTED");
}

#[tokio::test]
async fn positional_param_count_must_match() {
    let (server, _audit) = server(ok_executor(), "fail", None);
    let missing = server
        .handle(&call(
            "db_query_readonly",
            json!({"sql": "SELECT * FROM orders WHERE id = ?"}),
        ))
        .await;
    assert_eq!(err_code(&missing), "MCP_PARAM_COUNT_MISMATCH");

    let extra = server
        .handle(&call(
            "db_query_readonly",
            json!({"sql": "SELECT 1", "params": [1]}),
        ))
        .await;
    assert_eq!(err_code(&extra), "MCP_PARAM_COUNT_MISMATCH");

    let ok_sql = "SELECT * FROM orders WHERE id = ?";
    let good = server
        .handle(&call(
            "db_query_readonly",
            json!({"sql": ok_sql, "params": [42]}),
        ))
        .await;
    assert_eq!(good["result"]["isError"], json!(false));
    assert_eq!(
        good["result"]["content"]["sql_hash"],
        json!(jumcp_core::canonical::sha256_hex(ok_sql)),
        "成功结果 content 必须回显 sql_hash"
    );
}

#[tokio::test]
async fn resident_budget_fail_blocks_tools_list_with_policy_decision() {
    let caps = BudgetCaps {
        resident_limit: 1,
        ..Default::default()
    };
    let (server, audit) = server(ok_executor(), "fail", Some(caps));
    let resp = server
        .handle(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
        .await;
    assert_eq!(err_code(&resp), "MCP_BUDGET_EXCEEDED");
    let decisions = audit_events(&audit, "policy_decision").await;
    assert_eq!(decisions[0]["error_code"], json!("MCP_BUDGET_EXCEEDED"));
}

#[tokio::test]
async fn resident_budget_truncate_drops_capability_tools() {
    let caps = BudgetCaps {
        resident_limit: 1,
        ..Default::default()
    };
    let (server, audit) = server(ok_executor(), "truncate", Some(caps));
    let resp = server
        .handle(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
        .await;
    assert_eq!(resp["result"]["truncated"], json!(true));
    let names: Vec<&str> = resp["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert_eq!(
        names,
        vec![
            "search_capabilities",
            "describe_capability",
            "list_groups",
            "db_run_template",
            "db_query_readonly"
        ]
    );
    assert_eq!(
        resp["result"]["dropped_groups"],
        json!(["db.read.analytics"])
    );
    let served = audit_events(&audit, "tools_list_served").await;
    assert_eq!(served[0]["object_type"], json!("catalog"));
    assert_eq!(served[0]["object_id"], json!("tools"));
    assert_eq!(served[0]["action"], json!("list_tools"));
    assert_eq!(served[0]["outcome"], json!("summary"));
    assert_eq!(served[0]["data"]["truncated"], json!(true));
}

#[tokio::test]
async fn result_budget_switches_to_summary_mode() {
    let caps = BudgetCaps {
        max_result_cumulative: 10,
        ..Default::default()
    };
    let (server, _audit) = server(ok_executor(), "fail", Some(caps));
    let resp = server
        .handle(&call("db_query_readonly", json!({"sql": "SELECT 1"})))
        .await;
    let content = &resp["result"]["content"];
    assert_eq!(content["summary"], json!(true));
    assert_eq!(content["row_count"], json!(1));
    assert_eq!(
        content["sql_hash"],
        json!(jumcp_core::canonical::sha256_hex("SELECT 1")),
        "摘要模式同样必须回显 sql_hash"
    );
    assert!(content.get("rows").is_none(), "摘要模式不得回明细");
}

#[tokio::test]
async fn describe_schema_budget_exhausted_rejects() {
    let caps = BudgetCaps {
        max_schema_count: 0,
        ..Default::default()
    };
    let (server, audit) = server(ok_executor(), "fail", Some(caps));
    let resp = server
        .handle(&call(
            "describe_capability",
            json!({"capability_id": "order.daily_summary"}),
        ))
        .await;
    assert_eq!(err_code(&resp), "MCP_CONTEXT_BUDGET_EXHAUSTED");
    let decisions = audit_events(&audit, "policy_decision").await;
    assert_eq!(
        decisions[0]["error_code"],
        json!("MCP_CONTEXT_BUDGET_EXHAUSTED")
    );
}

#[tokio::test]
async fn describe_schema_records_tokens() {
    let caps = BudgetCaps {
        max_schema_count: 12,
        ..Default::default()
    };
    let (server, _audit) = server(ok_executor(), "fail", Some(caps));
    let resp = server
        .handle(&call(
            "describe_capability",
            json!({"capability_id": "order.daily_summary"}),
        ))
        .await;
    assert_eq!(resp["result"]["isError"], json!(false));
    assert!(resp["result"]["schema_tokens"].as_i64().unwrap_or(0) >= 1);
}

#[tokio::test]
async fn happy_path_audits_tool_call_with_metrics() {
    let (server, audit) = server(ok_executor(), "fail", None);
    let resp = server
        .handle(&call("db_query_readonly", json!({"sql": "SELECT 1"})))
        .await;
    assert_eq!(resp["result"]["isError"], json!(false));
    assert_eq!(resp["result"]["content"]["row_count"], json!(1));
    assert_eq!(
        resp["result"]["content"]["sql_hash"],
        json!(jumcp_core::canonical::sha256_hex("SELECT 1"))
    );
    let calls = audit_events(&audit, "tool_call").await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["object_type"], json!("tool"));
    assert_eq!(calls[0]["object_id"], json!("db_query_readonly"));
    assert_eq!(calls[0]["outcome"], json!("ok"));
    assert_eq!(calls[0]["data"]["row_count"], json!(1));
    assert!(calls[0]["data"]["tokens"].as_i64().unwrap_or(0) > 0);
    // 审计必须能复现这次调用：执行脚本与绑定值原样落库。
    assert_eq!(calls[0]["data"]["executed_script"], json!("SELECT 1"));
    assert_eq!(calls[0]["data"]["bound_params"], json!([]));
    // 管理台明细读的是入参原样与执行脚本全文。
    assert_eq!(calls[0]["data"]["arguments"], json!({"sql": "SELECT 1"}));
    assert_eq!(calls[0]["data"]["executed_script"], json!("SELECT 1"));
    assert_eq!(calls[0]["data"]["cache"], Value::Null);
    assert_eq!(calls[0]["data"]["version"], Value::Null);
}

/// SQL 原文 + 绑定值 + 目标库/白名单：成功路径的审计上下文完整。
#[tokio::test]
async fn audit_records_full_sql_params_database_and_schemas() {
    let (server, audit) = server(ok_executor(), "fail", None);
    let sql = "SELECT * FROM orders WHERE id = ? AND day = ?";
    let resp = server
        .handle(&call(
            "db_query_readonly",
            json!({
                "sql": sql,
                "params": [42, "2026-11-01"],
                "allowed_schemas": ["default"],
                "database": "default"
            }),
        ))
        .await;
    assert_eq!(
        resp["result"]["content"]["error"],
        Value::Null,
        "调用应成功: {resp}"
    );
    let calls = audit_events(&audit, "tool_call").await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["data"]["executed_script"], json!(sql));
    assert_eq!(calls[0]["data"]["bound_params"], json!([42, "2026-11-01"]));
    assert_eq!(calls[0]["data"]["database"], json!("default"));
    assert_eq!(calls[0]["data"]["allowed_schemas"], json!(["default"]));
    assert_eq!(calls[0]["outcome"], json!("ok"));
    assert_eq!(
        calls[0]["data"]["arguments"],
        json!({
            "sql": sql,
            "params": [42, "2026-11-01"],
            "allowed_schemas": ["default"],
            "database": "default"
        })
    );
    assert_eq!(calls[0]["data"]["executed_script"], json!(sql));
}

#[tokio::test]
async fn timeout_marks_outcome_unknown_without_retry() {
    let executor = Arc::new(FakeExecutor {
        result: Err(McpError::timeout("statement timeout".into())),
    });
    let (server, audit) = server(executor, "fail", None);
    let resp = server
        .handle(&call("db_query_readonly", json!({"sql": "SELECT 1"})))
        .await;
    assert_eq!(err_code(&resp), "MCP_EXEC_TIMEOUT");
    let calls = audit_events(&audit, "tool_call").await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["outcome"], json!("unknown"));
    assert_eq!(calls[0]["error_code"], json!("MCP_EXEC_TIMEOUT"));
    assert!(
        calls[0]["error_message"]
            .as_str()
            .is_some_and(|m| m.contains("statement timeout"))
    );
    // 错误路径也要能复现：执行脚本 + 绑定值原样。
    assert_eq!(calls[0]["data"]["executed_script"], json!("SELECT 1"));
    assert_eq!(calls[0]["data"]["bound_params"], json!([]));
}

#[tokio::test]
async fn default_executor_reports_missing_datasource() {
    let (server, _audit) = server(
        Arc::new(jumcp_server::executor::DefaultQueryExecutor),
        "fail",
        None,
    );
    let resp = server
        .handle(&call("db_query_readonly", json!({"sql": "SELECT 1"})))
        .await;
    assert_eq!(err_code(&resp), "MCP_STORE_ERROR");
}

#[tokio::test]
async fn deprecated_capability_rejects_new_version() {
    use jumcp_core::registry::CapabilityStore;
    let store = CapabilityStore::open(":memory:", None).expect("store");
    store
        .register(
            "cap1",
            "tool",
            "db.read.core",
            "read",
            json!({"description":"v1"}),
        )
        .expect("register");
    store.deprecate("cap1", "cap2").expect("deprecate");
    // 下架后只能保留 successor_id，不允许再追加版本（版本钉住语义）。
    let e = store
        .publish_version("cap1", json!({"description":"v2"}), "ap-1", "alice", "bob")
        .unwrap_err();
    assert_eq!(e.code, "MCP_STATE_TRANSITION_INVALID");
}

#[tokio::test]
async fn max_tokens_exceeded_fails_by_default() {
    let (server, audit) = server(ok_executor(), "fail", None);
    // FakeExecutor 返回 tokens=12；调用方声明 5 即超限，默认 fail 且审计 outcome=budget_rejected。
    let resp = server
        .handle(&call(
            "db_query_readonly",
            json!({"sql": "SELECT 1", "max_tokens": 5}),
        ))
        .await;
    assert_eq!(err_code(&resp), "MCP_BUDGET_EXCEEDED");
    let calls = audit_events(&audit, "tool_call").await;
    assert_eq!(calls[0]["outcome"], json!("error"));
    assert_eq!(calls[0]["error_code"], json!("MCP_BUDGET_EXCEEDED"));
    assert_eq!(calls[0]["data"]["executed_script"], json!("SELECT 1"));
    assert_eq!(calls[0]["data"]["tokens"], json!(12));
}
