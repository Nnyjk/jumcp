//! 工具语义的单测：fake 执行器替代 ClickHouse，`jumcp-sqlguard` 是纯函数。
//!
//! 全程不连真库；协议/信封（initialize、tools/list、JSON-RPC 错误码）由 server 的
//! 内置 HTTP 端点负责，这里只测插件自己的工具行为。

use std::sync::{Arc, Mutex};

use jumcp_connector_clickhouse::AgentError;
use jumcp_connector_protocol::{ColumnDef, QueryRequest, QueryResult};
use jumcp_mcp_clickhouse::{ExecFuture, Executor, Server, plugin};
use serde_json::{Value, json};

/// 可观测的 fake 执行器：记录收到的请求，回放预置结果。
struct FakeExecutor {
    seen: Mutex<Vec<QueryRequest>>,
    outcome: Result<QueryResult, AgentError>,
}

impl FakeExecutor {
    fn new(outcome: Result<QueryResult, AgentError>) -> Arc<Self> {
        Arc::new(Self {
            seen: Mutex::new(Vec::new()),
            outcome,
        })
    }

    fn seen(&self) -> Vec<QueryRequest> {
        self.seen.lock().expect("seen 锁").clone()
    }
}

impl Executor for FakeExecutor {
    fn query<'a>(&'a self, request: &'a QueryRequest) -> ExecFuture<'a> {
        self.seen.lock().expect("seen 锁").push(request.clone());
        let outcome = self.outcome.clone();
        Box::pin(async move { outcome })
    }
}

/// 三行结果，其中第三行是 5000 字符的长值（用来验证单格夹紧与续读偏移）。
fn sample_result() -> QueryResult {
    QueryResult {
        columns: vec![ColumnDef {
            name: "s".to_string(),
            data_type: "String".to_string(),
        }],
        rows: vec![
            vec![json!("alpha")],
            vec![json!("beta")],
            vec![json!("x".repeat(5000))],
        ],
        row_count: 3,
        truncated: false,
        elapsed_ms: 7,
    }
}

fn tables_result() -> QueryResult {
    QueryResult {
        columns: vec![
            ColumnDef {
                name: "database".to_string(),
                data_type: "String".to_string(),
            },
            ColumnDef {
                name: "name".to_string(),
                data_type: "String".to_string(),
            },
            ColumnDef {
                name: "engine".to_string(),
                data_type: "String".to_string(),
            },
        ],
        rows: vec![
            vec![json!("analytics"), json!("events"), json!("MergeTree")],
            vec![json!("analytics"), json!("orders"), json!("Log")],
        ],
        row_count: 2,
        truncated: false,
        elapsed_ms: 3,
    }
}

fn server(executor: Arc<FakeExecutor>, allowed: &[&str]) -> Server {
    Server::new(
        executor,
        allowed.iter().map(|item| item.to_string()).collect(),
        15_000,
    )
}

/// 工具失败结果里的稳定 code。
fn failure_code(result: &Value) -> String {
    assert_eq!(result["isError"], json!(true), "{result}");
    let text = result["content"][0]["text"]
        .as_str()
        .expect("失败结果必须有 text");
    serde_json::from_str::<Value>(text)
        .expect("text 是 JSON")
        .get("code")
        .and_then(Value::as_str)
        .expect("text 里必须有 code")
        .to_string()
}

#[tokio::test]
async fn tools_expose_three_definitions_with_full_schema() {
    let tools = plugin().tools();
    assert_eq!(tools.len(), 3, "工具数量固定为 3");
    let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
    assert_eq!(names, ["query", "list_databases", "list_tables"]);

    let query = tools
        .iter()
        .find(|tool| tool.name == "query")
        .expect("query 工具");
    let schema = &query.input_schema;
    assert_eq!(schema["type"], json!("object"));
    assert_eq!(schema["required"], json!(["sql"]));
    for property in [
        "sql",
        "params",
        "max_rows",
        "database",
        "cell_char_limit",
        "cell_char_offset",
        "timeout_ms",
    ] {
        assert!(
            schema["properties"].get(property).is_some(),
            "缺少属性 {property}"
        );
    }
    assert_eq!(schema["properties"]["max_rows"]["default"], json!(200));
    assert_eq!(
        schema["properties"]["cell_char_limit"]["default"],
        json!(2000)
    );
    assert_eq!(schema["properties"]["max_rows"]["maximum"], json!(1000));

    let list_tables = tools
        .iter()
        .find(|tool| tool.name == "list_tables")
        .expect("list_tables 工具");
    assert!(
        list_tables.input_schema["properties"]
            .get("database")
            .is_some()
    );
    assert!(list_tables.input_schema["properties"].get("like").is_some());

    let list_databases = tools
        .iter()
        .find(|tool| tool.name == "list_databases")
        .expect("list_databases 工具");
    assert_eq!(
        list_databases.input_schema["properties"],
        json!({}),
        "list_databases 无参"
    );
}

#[tokio::test]
async fn unknown_tool_and_bad_arguments_are_tool_errors() {
    let result = server(FakeExecutor::new(Ok(sample_result())), &[])
        .call_tool("no_such_tool", &json!({}))
        .await;
    assert_eq!(failure_code(&result), "MCP_VALIDATION_ERROR");
    let text = result["content"][0]["text"].as_str().expect("text");
    assert!(text.contains("known_tools"), "{text}");

    let result = server(FakeExecutor::new(Ok(sample_result())), &[])
        .call_tool("query", &json!([1, 2]))
        .await;
    assert_eq!(failure_code(&result), "MCP_VALIDATION_ERROR");

    let result = server(FakeExecutor::new(Ok(sample_result())), &[])
        .call_tool("query", &json!({"sql": 42}))
        .await;
    assert_eq!(failure_code(&result), "MCP_VALIDATION_ERROR");
}

#[tokio::test]
async fn query_clamps_zero_and_negative_max_rows_to_one() {
    for max_rows in [0, -5] {
        let executor = FakeExecutor::new(Ok(sample_result()));
        let result = server(executor.clone(), &[])
            .call_tool(
                "query",
                &json!({"sql": "SELECT s FROM t", "max_rows": max_rows}),
            )
            .await;
        let structured = &result["structuredContent"];
        assert_eq!(structured["effective_max_rows"], json!(1));
        assert_eq!(structured["row_count"], json!(1));
        assert_eq!(structured["truncated"], json!(true));
        assert_eq!(structured["rows"].as_array().expect("rows").len(), 1);
        assert_eq!(executor.seen()[0].max_rows, 1);
    }
}

#[tokio::test]
async fn query_returns_text_and_structured_content_with_clamped_limits() {
    let executor = FakeExecutor::new(Ok(sample_result()));
    let result = server(executor.clone(), &[])
        .call_tool(
            "query",
            &json!({"sql": "SELECT s FROM t", "max_rows": 5000, "cell_char_limit": 999999}),
        )
        .await;
    assert_eq!(result["isError"], json!(false));
    let structured = &result["structuredContent"];
    assert_eq!(
        structured["columns"],
        json!([{"name": "s", "type": "String"}])
    );
    assert_eq!(structured["row_count"], json!(3));
    assert_eq!(structured["effective_max_rows"], json!(1000));
    assert_eq!(structured["truncated"], json!(false));
    // content 文本与 structuredContent 必须是同一份 JSON（紧凑格式）。
    let text = result["content"][0]["text"].as_str().expect("text");
    assert!(!text.contains('\n'), "文本必须是紧凑 JSON");
    assert_eq!(serde_json::from_str::<Value>(text).unwrap(), *structured);

    // 单格夹到 4000 字符，并给出续读偏移。
    let long = structured["rows"][2][0].as_str().expect("长值");
    assert_eq!(long.chars().count(), 4000);
    assert_eq!(
        structured["next_offsets"],
        json!([{"row": 2, "column": 0, "next_offset": 4000}])
    );

    // 执行器收到的入参已夹紧。
    let seen = executor.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].max_rows, 1000);
    assert_eq!(seen[0].timeout_ms, 15_000);
    assert_eq!(seen[0].database, None);
}

#[tokio::test]
async fn query_defaults_to_200_rows_and_2000_chars() {
    let executor = FakeExecutor::new(Ok(sample_result()));
    let result = server(executor.clone(), &[])
        .call_tool("query", &json!({"sql": "SELECT s FROM t"}))
        .await;
    let structured = &result["structuredContent"];
    assert_eq!(structured["effective_max_rows"], json!(200));
    let long = structured["rows"][2][0].as_str().expect("长值");
    assert_eq!(long.chars().count(), 2000);
    assert_eq!(
        structured["next_offsets"],
        json!([{"row": 2, "column": 0, "next_offset": 2000}])
    );
    assert_eq!(executor.seen()[0].max_rows, 200);
}

#[tokio::test]
async fn query_guard_failures_are_tool_errors_with_stable_codes() {
    let executor = FakeExecutor::new(Ok(sample_result()));
    let server = server(executor.clone(), &["analytics"]);
    let cases = [
        (
            json!({"sql": "DELETE FROM analytics.events"}),
            "MCP_SQL_REJECTED",
        ),
        (json!({"sql": "SELECT 1; SELECT 2"}), "MCP_MULTI_STATEMENT"),
        (
            json!({"sql": "SELECT * FROM other.events"}),
            "MCP_SCHEMA_OUT_OF_SCOPE",
        ),
        (
            json!({"sql": "SELECT * FROM events", "database": "other"}),
            "MCP_SCHEMA_OUT_OF_SCOPE",
        ),
        (json!({"sql": "   "}), "MCP_SQL_REJECTED"),
    ];
    for (arguments, expected) in cases {
        let result = server.call_tool("query", &arguments).await;
        assert_eq!(failure_code(&result), expected, "{arguments}");
        assert!(
            result.get("structuredContent").is_none(),
            "失败结果只回文本"
        );
        assert_eq!(result["content"][0]["type"], json!("text"));
    }
    assert!(executor.seen().is_empty(), "校验失败不得触达执行器");
}

#[tokio::test]
async fn executor_failure_maps_to_timeout_code() {
    let executor = FakeExecutor::new(Err(AgentError::timeout("ClickHouse 查询超时".to_string())));
    let result = server(executor, &[])
        .call_tool("query", &json!({"sql": "SELECT 1"}))
        .await;
    assert_eq!(failure_code(&result), "MCP_EXEC_TIMEOUT");
    let text = result["content"][0]["text"].as_str().expect("text");
    assert!(text.contains("\"outcome\":\"unknown\""), "{text}");
}

#[tokio::test]
async fn executor_failure_codes_are_stable_across_error_kinds() {
    let cases = [
        (
            AgentError::db("ClickHouse 返回错误: Unknown table".to_string()),
            "MCP_STORE_ERROR",
        ),
        (
            AgentError::config("DSN 非法".to_string()),
            "MCP_CONFIG_ERROR",
        ),
        (
            AgentError::invalid_params("位置参数只支持标量".to_string()),
            "MCP_SQL_REJECTED",
        ),
    ];
    for (error, expected) in cases {
        let expected_outcome = if error.outcome_unknown {
            json!("unknown")
        } else {
            json!("failed")
        };
        let agent_code = error.mcp_code.to_string();
        let executor = FakeExecutor::new(Err(error));
        let result = server(executor, &[])
            .call_tool("query", &json!({"sql": "SELECT 1"}))
            .await;
        assert_eq!(failure_code(&result), expected);
        let text = result["content"][0]["text"].as_str().expect("text");
        let payload: Value = serde_json::from_str(text).expect("text 是 JSON");
        // 原始 agent 码要保留在 data 里，便于排障。
        assert_eq!(payload["data"]["agent_code"], json!(agent_code), "{text}");
        assert_eq!(payload["data"]["outcome"], expected_outcome, "{text}");
    }
}

#[tokio::test]
async fn list_databases_returns_sorted_names() {
    let result = QueryResult {
        columns: vec![ColumnDef {
            name: "name".to_string(),
            data_type: "String".to_string(),
        }],
        rows: vec![vec![json!("analytics")], vec![json!("default")]],
        row_count: 2,
        truncated: false,
        elapsed_ms: 1,
    };
    let executor = FakeExecutor::new(Ok(result));
    let result = server(executor.clone(), &[])
        .call_tool("list_databases", &json!({}))
        .await;
    assert_eq!(result["isError"], json!(false));
    assert_eq!(
        result["structuredContent"],
        json!({"databases": ["analytics", "default"], "truncated": false})
    );
    let seen = executor.seen();
    assert_eq!(
        seen[0].sql,
        "SELECT name FROM system.databases ORDER BY name"
    );
    assert!(seen[0].params.is_empty());
}

#[tokio::test]
async fn list_tables_binds_filters_instead_of_concatenating() {
    let executor = FakeExecutor::new(Ok(tables_result()));
    let result = server(executor.clone(), &["analytics"])
        .call_tool(
            "list_tables",
            &json!({"database": "analytics", "like": "%ord%"}),
        )
        .await;
    assert_eq!(result["isError"], json!(false));
    let structured = &result["structuredContent"];
    assert_eq!(structured["database"], json!("analytics"));
    assert_eq!(structured["truncated"], json!(false));
    assert_eq!(
        structured["tables"],
        json!([
            {"database": "analytics", "name": "events", "engine": "MergeTree"},
            {"database": "analytics", "name": "orders", "engine": "Log"}
        ])
    );

    let seen = executor.seen();
    assert_eq!(seen.len(), 1);
    // 用户输入只出现在参数里，SQL 文本里没有。
    assert!(!seen[0].sql.contains("analytics"), "{}", seen[0].sql);
    assert!(!seen[0].sql.contains("%ord%"), "{}", seen[0].sql);
    assert_eq!(
        seen[0].params,
        vec![
            json!("analytics"),
            json!("analytics"),
            json!("%ord%"),
            json!("%ord%")
        ]
    );
}

#[tokio::test]
async fn catalogs_never_reveal_schemas_outside_the_allowlist() {
    let databases = QueryResult {
        columns: vec![ColumnDef {
            name: "name".to_string(),
            data_type: "String".to_string(),
        }],
        rows: vec![
            vec![json!("analytics")],
            vec![json!("default")],
            vec![json!("sales")],
        ],
        row_count: 3,
        truncated: false,
        elapsed_ms: 1,
    };
    let result = server(FakeExecutor::new(Ok(databases)), &["sales"])
        .call_tool("list_databases", &json!({}))
        .await;
    assert_eq!(
        result["structuredContent"],
        json!({"databases": ["sales"], "truncated": false})
    );

    // tables_result() 只有 analytics 的表，白名单是 sales → 输出必须为空。
    let result = server(FakeExecutor::new(Ok(tables_result())), &["sales"])
        .call_tool("list_tables", &json!({}))
        .await;
    assert_eq!(
        result["structuredContent"],
        json!({"database": null, "tables": [], "truncated": false})
    );
}

#[tokio::test]
async fn query_pins_session_database_to_the_allowlist_head() {
    let executor = FakeExecutor::new(Ok(sample_result()));
    let result = server(executor.clone(), &["analytics", "ods"])
        .call_tool("query", &json!({"sql": "SELECT * FROM events"}))
        .await;
    assert_eq!(result["isError"], json!(false), "{result:?}");
    assert_eq!(executor.seen()[0].database.as_deref(), Some("analytics"));

    // 显式指定白名单内的库：按调用方给的库执行。
    let executor = FakeExecutor::new(Ok(sample_result()));
    let result = server(executor.clone(), &["analytics"])
        .call_tool(
            "query",
            &json!({"sql": "SELECT * FROM events", "database": "analytics"}),
        )
        .await;
    assert_eq!(result["isError"], json!(false));
    assert_eq!(executor.seen()[0].database.as_deref(), Some("analytics"));

    // 显式指定白名单外的库仍然被拦，且不触达执行器。
    let executor = FakeExecutor::new(Ok(sample_result()));
    let result = server(executor.clone(), &["analytics"])
        .call_tool(
            "query",
            &json!({"sql": "SELECT * FROM events", "database": "other"}),
        )
        .await;
    assert_eq!(failure_code(&result), "MCP_SCHEMA_OUT_OF_SCOPE");
    assert!(executor.seen().is_empty());

    // 未配置白名单时不钉库：仍用配置的默认库。
    let executor = FakeExecutor::new(Ok(sample_result()));
    let result = server(executor.clone(), &[])
        .call_tool("query", &json!({"sql": "SELECT * FROM events"}))
        .await;
    assert_eq!(result["isError"], json!(false));
    assert_eq!(executor.seen()[0].database, None);
}

#[tokio::test]
async fn catalogs_report_truncation() {
    let databases = QueryResult {
        columns: vec![ColumnDef {
            name: "name".to_string(),
            data_type: "String".to_string(),
        }],
        rows: vec![vec![json!("analytics")]],
        row_count: 1,
        truncated: true,
        elapsed_ms: 1,
    };
    let result = server(FakeExecutor::new(Ok(databases)), &[])
        .call_tool("list_databases", &json!({}))
        .await;
    assert_eq!(result["structuredContent"]["truncated"], json!(true));

    let mut tables = tables_result();
    tables.truncated = true;
    let result = server(FakeExecutor::new(Ok(tables)), &["analytics"])
        .call_tool("list_tables", &json!({}))
        .await;
    let structured = &result["structuredContent"];
    assert_eq!(structured["truncated"], json!(true));
    assert_eq!(structured["tables"].as_array().map(Vec::len), Some(2));
}

#[tokio::test]
async fn whitelist_compares_database_names_case_insensitively() {
    // 运营者填了 `Analytics`：显式给 `analytics` 应放行，目录结果按原串返回，钉库用原串。
    let executor = FakeExecutor::new(Ok(sample_result()));
    let result = server(executor.clone(), &["Analytics"])
        .call_tool(
            "query",
            &json!({"sql": "SELECT * FROM events", "database": "analytics"}),
        )
        .await;
    assert_eq!(result["isError"], json!(false), "{result:?}");
    assert_eq!(executor.seen()[0].database.as_deref(), Some("analytics"));

    let executor = FakeExecutor::new(Ok(sample_result()));
    let result = server(executor.clone(), &["Analytics"])
        .call_tool("query", &json!({"sql": "SELECT * FROM events"}))
        .await;
    assert_eq!(result["isError"], json!(false));
    assert_eq!(executor.seen()[0].database.as_deref(), Some("Analytics"));

    let databases = QueryResult {
        columns: vec![ColumnDef {
            name: "name".to_string(),
            data_type: "String".to_string(),
        }],
        rows: vec![vec![json!("Analytics")], vec![json!("default")]],
        row_count: 2,
        truncated: false,
        elapsed_ms: 1,
    };
    let result = server(FakeExecutor::new(Ok(databases)), &["Analytics"])
        .call_tool("list_databases", &json!({}))
        .await;
    assert_eq!(
        result["structuredContent"],
        json!({"databases": ["Analytics"], "truncated": false})
    );
}

#[tokio::test]
async fn list_tables_rejects_database_outside_allowlist() {
    let executor = FakeExecutor::new(Ok(tables_result()));
    let result = server(executor.clone(), &["analytics"])
        .call_tool("list_tables", &json!({"database": "other"}))
        .await;
    assert_eq!(failure_code(&result), "MCP_SCHEMA_OUT_OF_SCOPE");
    assert!(executor.seen().is_empty());
}
