//! 契约测试：入口裁剪、令牌常驻预算 fail|truncate、schema 预算、版本冲突。
//!
//! 内置 basic 组不提供直连数据源的工具，这里只剩元工具三件套 + 目录/预算契约；
//! 真实 PG/ClickHouse 用例在 jumcp-server 单元里 `#[ignore]` + DSN 门槛。

use jumcp_core::audit::AuditLog;
use jumcp_core::budget::BudgetCaps;
use jumcp_core::config::{DatasourceRef, GroupDef, RoleDef, Settings, TenantPolicy};
use jumcp_core::entry::{Actor, GroupRequest, resolve_scope};
use jumcp_server::catalog::Capability;
use jumcp_server::mcp::McpServer;
use jumcp_server::service::Service;
use serde_json::{Value, json};
use std::sync::Arc;

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

fn server(overflow: &str, caps: Option<BudgetCaps>) -> (McpServer, Arc<AuditLog>) {
    let audit = Arc::new(AuditLog::open(":memory:").expect("in-memory audit"));
    let mut catalog = jumcp_server::catalog::Catalog::default();
    catalog.register(Capability {
        id: "order.daily_summary".into(),
        kind: "tool".into(),
        group: "db.read.analytics".into(),
        one_line: "订单日汇总".into(),
        params_schema: json!({"type":"object","properties":{"day":{"type":"string"}}}),
        ..Capability::new("order.daily_summary", "tool", "db.read.analytics")
    });
    let mut svc = Service::new(settings(overflow))
        .with_catalog(catalog)
        .with_audit(audit.clone());
    if let Some(c) = caps {
        svc = svc.with_budget_caps(c);
    }
    (McpServer::new(svc), audit)
}

fn call(name: &str, args: Value) -> Value {
    json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":name,"arguments":args}})
}

fn err_code(resp: &Value) -> String {
    resp["result"]["structuredContent"]["error"]["code"]
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
async fn resident_budget_fail_blocks_tools_list_with_policy_decision() {
    let caps = BudgetCaps {
        resident_limit: 1,
        ..Default::default()
    };
    let (server, audit) = server("fail", Some(caps));
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
    let (server, audit) = server("truncate", Some(caps));
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
    // 内置 basic 组 = 元工具三件套 + 模板执行臂：tools/list 精确等于常驻名单。
    assert_eq!(
        names,
        vec![
            "search_capabilities",
            "describe_capability",
            "list_groups",
            "db_run_template"
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
async fn describe_schema_budget_exhausted_rejects() {
    let caps = BudgetCaps {
        max_schema_count: 0,
        ..Default::default()
    };
    let (server, audit) = server("fail", Some(caps));
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
    let (server, _audit) = server("fail", Some(caps));
    let resp = server
        .handle(&call(
            "describe_capability",
            json!({"capability_id": "order.daily_summary"}),
        ))
        .await;
    assert_eq!(resp["result"]["isError"], json!(false));
    assert!(
        resp["result"]["structuredContent"]["schema_tokens"]
            .as_i64()
            .unwrap_or(0)
            >= 1
    );
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
    // successor 必须是真实存在的能力（deprecate 落库校验）。
    store
        .register(
            "cap2",
            "tool",
            "db.read.core",
            "read",
            json!({"description":"v2"}),
        )
        .expect("register successor");
    store.deprecate("cap1", "cap2").expect("deprecate");
    // 下架后只能保留 successor_id，不允许再追加版本（版本钉住语义）。
    let e = store
        .publish_version("cap1", json!({"description":"v2"}), "ap-1", "alice", "bob")
        .unwrap_err();
    assert_eq!(e.code, "MCP_STATE_TRANSITION_INVALID");
}
