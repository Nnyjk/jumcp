//! 静态客户端路径：平铺阈值、收缩形态、常驻预算与 `_meta` 通道。

use jumcp_core::budget::BudgetCaps;
use jumcp_core::config::{GroupDef, RoleDef, Settings, TenantPolicy};
use jumcp_core::entry::EffectiveScope;
use jumcp_server::catalog::{Capability, Catalog};
use jumcp_server::executor::{ExecFuture, QueryExecutor, QueryParams};
use jumcp_server::mcp::{McpServer, READONLY_ANNOTATIONS};
use jumcp_server::service::{RESIDENT_TOOL_NAMES, Service};
use serde_json::{Value, json};
use std::sync::Arc;

struct NoopExecutor;

impl QueryExecutor for NoopExecutor {
    fn execute<'a>(&'a self, _scope: &'a EffectiveScope, _q: &'a QueryParams) -> ExecFuture<'a> {
        Box::pin(async { Ok(json!({"rows": [], "row_count": 0, "tokens": 1})) })
    }
}

fn settings(threshold: i64, overflow: &str) -> Settings {
    let mut roles = std::collections::HashMap::new();
    roles.insert(
        "analyst".into(),
        RoleDef {
            name: "analyst".into(),
            default_groups: vec!["db.read.core".into(), "db.read.analytics".into()],
            default_budget: 2000,
        },
    );
    Settings {
        budget_overflow: overflow.to_string(),
        flatten_threshold: threshold,
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
        ..Default::default()
    }
}

fn capability(id: &str, group: &str) -> Capability {
    Capability {
        id: id.into(),
        one_line: format!("{id} 一行说明"),
        description: format!("{id} 详细说明"),
        params_schema: json!({
            "type": "object",
            "properties": {"day": {"type": "string"}, "channel": {"type": "string"}}
        }),
        ..Capability::new(id, "tool", group)
    }
}

fn catalog_with(count: usize, group: &str) -> Catalog {
    let mut catalog = Catalog::default();
    for i in 0..count {
        catalog.register(capability(&format!("cap.tool.{i:02}"), group));
    }
    catalog
}

fn server_with(
    catalog: Catalog,
    threshold: i64,
    overflow: &str,
    caps: Option<BudgetCaps>,
) -> McpServer {
    let mut service = Service::new(settings(threshold, overflow))
        .with_executor(Arc::new(NoopExecutor))
        .with_catalog(catalog)
        .with_scope(EffectiveScope {
            groups: vec!["db.read.core".into(), "db.read.analytics".into()],
            budget: 2000,
            degraded: false,
            source: "default".into(),
            mode: "strict".into(),
            allowed_builtin_tools: None,
        });
    if let Some(c) = caps {
        service = service.with_budget_caps(c);
    }
    McpServer::new(service)
}

fn tool_names(resp: &Value) -> Vec<String> {
    resp["result"]["tools"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|t| t["name"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

async fn tools_list(server: &McpServer) -> Value {
    server
        .handle(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
        .await
}

#[tokio::test]
async fn flattens_when_visible_tools_within_threshold() {
    let server = server_with(catalog_with(5, "db.read.core"), 40, "fail", None);
    let resp = tools_list(&server).await;
    assert_eq!(resp["result"]["flattened"], json!(true));
    assert_eq!(resp["result"]["flattened_count"], json!(5));
    let names = tool_names(&resp);
    assert_eq!(names.len(), RESIDENT_TOOL_NAMES.len() + 5);
    for i in 0..5 {
        assert!(names.contains(&format!("cap.tool.{i:02}")));
    }
    // 平铺定义必须显式带 annotations（不依赖协议默认值）。
    let expected: Value = serde_json::from_str(READONLY_ANNOTATIONS).expect("annotations");
    for tool in resp["result"]["tools"].as_array().expect("tools") {
        assert_eq!(
            tool["annotations"], expected,
            "工具 {} 缺 annotations",
            tool["name"]
        );
    }
    // 组索引随目录一起返回（收缩形态的主要常驻内容）。
    assert!(
        resp["result"]["groups"]
            .as_array()
            .is_some_and(|g| !g.is_empty())
    );
}

#[tokio::test]
async fn collapses_when_over_threshold() {
    let server = server_with(catalog_with(45, "db.read.core"), 40, "fail", None);
    let resp = tools_list(&server).await;
    assert_eq!(resp["result"]["flattened"], json!(false));
    assert_eq!(resp["result"]["flattened_count"], json!(0));
    let names = tool_names(&resp);
    assert_eq!(names.len(), RESIDENT_TOOL_NAMES.len());
    assert!(
        names
            .iter()
            .all(|n| RESIDENT_TOOL_NAMES.contains(&n.as_str()))
    );
    let groups = resp["result"]["groups"].as_array().expect("groups");
    assert!(
        groups.iter().any(|g| g["capability_count"] == json!(45)),
        "收缩形态仍需组索引：{groups:?}"
    );
}

#[tokio::test]
async fn threshold_zero_disables_flattening() {
    let server = server_with(catalog_with(3, "db.read.core"), 0, "fail", None);
    let resp = tools_list(&server).await;
    assert_eq!(resp["result"]["flattened"], json!(false));
    assert_eq!(tool_names(&resp).len(), RESIDENT_TOOL_NAMES.len());
}

#[tokio::test]
async fn flatten_over_budget_fails_with_hint() {
    let caps = BudgetCaps {
        resident_limit: 50,
        ..Default::default()
    };
    let server = server_with(catalog_with(5, "db.read.core"), 40, "fail", Some(caps));
    let resp = tools_list(&server).await;
    let error = &resp["result"]["structuredContent"]["error"];
    assert_eq!(error["code"], json!("MCP_BUDGET_EXCEEDED"));
    assert_eq!(error["data"]["flattened_count"], json!(5));
    assert!(error["data"]["suggestions"].as_array().is_some_and(|s| {
        s.iter()
            .any(|v| v.as_str().unwrap_or_default().contains("FLATTEN_THRESHOLD"))
    }));
}

#[tokio::test]
async fn flatten_over_budget_truncates_when_enabled() {
    let caps = BudgetCaps {
        resident_limit: 50,
        ..Default::default()
    };
    let server = server_with(catalog_with(5, "db.read.core"), 40, "truncate", Some(caps));
    let resp = tools_list(&server).await;
    assert_eq!(resp["result"]["truncated"], json!(true));
    let names = tool_names(&resp);
    assert_eq!(names.len(), RESIDENT_TOOL_NAMES.len());
    let dropped = resp["result"]["dropped_groups"]
        .as_array()
        .expect("dropped_groups");
    assert!(dropped.contains(&json!("db.read.core")));
}

#[tokio::test]
async fn flattened_definition_matches_describe() {
    let server = server_with(catalog_with(1, "db.read.core"), 40, "fail", None);
    let listed = tools_list(&server).await;
    let flattened = listed["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .find(|t| t["name"] == json!("cap.tool.00"))
        .cloned()
        .expect("flattened tool");
    let described = server
        .handle(&json!({
            "jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"describe_capability","arguments":{"capability_id":"cap.tool.00"}}
        }))
        .await;
    let content = &described["result"]["structuredContent"];
    assert_eq!(
        flattened["inputSchema"], content["params_schema"],
        "平铺 schema 与 describe 必须同源"
    );
    assert_eq!(flattened["description"], json!("cap.tool.00 一行说明"));
    assert_eq!(content["id"], json!("cap.tool.00"));
}

#[tokio::test]
async fn meta_groups_channel_applies_when_header_absent() {
    let mut catalog = Catalog::default();
    catalog.register(capability("cap.core", "db.read.core"));
    catalog.register(capability("cap.analytics", "db.read.analytics"));
    let server = server_with(catalog, 40, "fail", None);

    let init = server
        .handle(&json!({
            "jsonrpc":"2.0","id":1,"method":"initialize",
            "params":{"_meta":{"mcp":{"groups":["db.read.core"]}}}
        }))
        .await;
    assert_eq!(init["result"]["serverInfo"]["name"], json!("jumcp"));

    let names = tool_names(&tools_list(&server).await);
    assert!(names.contains(&"cap.core".to_string()));
    assert!(
        !names.contains(&"cap.analytics".to_string()),
        "_meta 声明的组必须收窄可见集：{names:?}"
    );
}

#[tokio::test]
async fn meta_groups_out_of_scope_rejected_in_strict_mode() {
    let server = server_with(catalog_with(1, "db.read.core"), 40, "fail", None);
    let resp = server
        .handle(&json!({
            "jsonrpc":"2.0","id":1,"method":"initialize",
            "params":{"_meta":{"mcp":{"groups":["db.admin.schema"]}}}
        }))
        .await;
    assert_eq!(
        resp["error"]["data"]["mcp_code"],
        json!("MCP_GROUP_FORBIDDEN")
    );
    assert_eq!(
        resp["error"]["data"]["offending_groups"],
        json!(["db.admin.schema"])
    );
}

/// 回归：自由的只读 SQL 工具（`db_query_readonly`）仍移除，`tools/list` 不含它，
/// 调用落到 unknown-tool 路径；模板执行臂 `db_run_template` 是唯一取数入口。
#[tokio::test]
async fn removed_free_form_query_tool_is_not_listed_nor_callable() {
    let server = server_with(catalog_with(1, "db.read.core"), 40, "fail", None);
    let names = tool_names(&tools_list(&server).await);
    assert!(
        names.contains(&"db_run_template".to_string()),
        "db_run_template 必须出现在 tools/list：{names:?}"
    );
    let id = "db_query_readonly";
    assert!(
        !names.contains(&id.to_string()),
        "{id} 不得出现在 tools/list：{names:?}"
    );
    let resp = server
        .handle(&json!({
            "jsonrpc":"2.0","id":7,"method":"tools/call",
            "params":{"name": id, "arguments": {}}
        }))
        .await;
    assert_eq!(resp["error"]["code"], json!(-32602), "{id}: {resp}");
    assert_eq!(resp["error"]["message"], json!("Unknown tool"), "{id}");
    assert_eq!(resp["error"]["data"]["tool"], json!(id));
}
