use super::*;
use jumcp_connector_protocol::rpc_code;

fn config(dsn: &str, allowed: &[&str]) -> ConnectorConfig {
    ConnectorConfig {
        datasource_name: "test-ch".into(),
        dsn: dsn.into(),
        statement_timeout_ms: 5_000,
        allowed_schemas: allowed.iter().map(|s| s.to_string()).collect(),
    }
}

#[test]
fn config_rejects_empty_dsn_and_bad_timeout() {
    let e = config("", &[]).validate().unwrap_err();
    assert_eq!(e.code, "MCP_CONFIG_ERROR");
    assert!(e.message.contains("test-ch"));
    assert!(e.message.contains("DSN"));

    let mut bad_timeout = config("http://u:p@h:18123", &[]);
    bad_timeout.statement_timeout_ms = 0;
    let e = bad_timeout.validate().unwrap_err();
    assert_eq!(e.code, "MCP_CONFIG_ERROR");
    assert!(e.message.contains("statement_timeout_ms"));

    let e = config("http://u:p@h:18123", &[""]).validate().unwrap_err();
    assert_eq!(e.code, "MCP_CONFIG_ERROR");
    assert!(e.message.contains("allowed_schemas"));
}

#[test]
fn default_database_must_be_in_whitelist() {
    let env_air = vec!["env_air".to_string()];
    assert!(ensure_database_allowed(&[], "default", "test-ch").is_ok());
    assert!(ensure_database_allowed(&env_air, "env_air", "test-ch").is_ok());
    let e = ensure_database_allowed(&env_air, "default", "test-ch").unwrap_err();
    assert_eq!(e.code, "MCP_CONFIG_ERROR");
    assert!(e.message.contains("test-ch"));
    assert!(e.message.contains("default"));
    assert!(e.message.contains("env_air"));
}

#[test]
fn redact_removes_url_credentials() {
    assert_eq!(
        redact("connect http://default:secret@192.0.2.1:18123 failed"),
        "connect http://***@192.0.2.1:18123 failed"
    );
    assert_eq!(redact("plain line"), "plain line");
}

#[test]
fn map_rpc_error_preserves_unknown_outcome() {
    let error = map_rpc_error(&json!({
        "code": rpc_code::EXEC_TIMEOUT,
        "message": "timeout",
        "data": {"code": "MCP_EXEC_TIMEOUT", "outcome": "unknown"}
    }));
    assert_eq!(error.code, "MCP_EXEC_TIMEOUT");
    assert_eq!(error.data["outcome"], json!("unknown"));
    assert_eq!(error.data["connector"], json!("clickhouse"));
}

#[tokio::test]
#[ignore = "需要真实 ClickHouse：JUMCP_TEST_CLICKHOUSE_DSN"]
async fn connector_real_version_query() {
    let Ok(dsn) = std::env::var("JUMCP_TEST_CLICKHOUSE_DSN") else {
        return;
    };
    let executor = ConnectorExecutor::connect(config(&dsn, &[]))
        .await
        .expect("connect");
    let q = QueryParams::new("SELECT version()".into(), &[], 5_000, &json!({})).unwrap();
    executor.validate(&q).expect("validate");
    let result = executor
        .execute(&scope(), &q)
        .await
        .expect("execute version");
    assert_eq!(result["row_count"], json!(1));
    assert!(result["rows"][0][0].as_str().unwrap().contains("25."));
    assert_eq!(result["columns"][0]["name"], json!("version()"));
}

#[tokio::test]
#[ignore = "需要真实 ClickHouse：JUMCP_TEST_CLICKHOUSE_DSN"]
async fn connector_real_max_rows_truncates() {
    let Ok(dsn) = std::env::var("JUMCP_TEST_CLICKHOUSE_DSN") else {
        return;
    };
    let executor = ConnectorExecutor::connect(config(&dsn, &[]))
        .await
        .expect("connect");
    let q = QueryParams::new(
        "SELECT number FROM system.numbers LIMIT 10".into(),
        &[],
        5_000,
        &json!({"max_rows": 3}),
    )
    .unwrap();
    let result = executor.execute(&scope(), &q).await.expect("execute");
    assert_eq!(result["row_count"], json!(3));
    assert_eq!(result["truncated"], json!(true));
    assert_eq!(result["rows"].as_array().unwrap().len(), 3);
}

#[tokio::test]
#[ignore = "需要真实 ClickHouse：JUMCP_TEST_CLICKHOUSE_DSN"]
async fn connector_real_positional_binding() {
    let Ok(dsn) = std::env::var("JUMCP_TEST_CLICKHOUSE_DSN") else {
        return;
    };
    let executor = ConnectorExecutor::connect(config(&dsn, &[]))
        .await
        .expect("connect");
    let q = QueryParams::new("SELECT ? + 1".into(), &[], 5_000, &json!({"params": [41]})).unwrap();
    let result = executor.execute(&scope(), &q).await.expect("execute");
    assert_eq!(result["rows"][0][0], json!(42));
}

#[tokio::test]
#[ignore = "需要真实 ClickHouse：JUMCP_TEST_CLICKHOUSE_DSN"]
async fn connector_real_ddl_rejected_at_l4() {
    let Ok(dsn) = std::env::var("JUMCP_TEST_CLICKHOUSE_DSN") else {
        return;
    };
    let executor = ConnectorExecutor::connect(config(&dsn, &[]))
        .await
        .expect("connect");
    let q = QueryParams::new(
        "DROP TABLE IF EXISTS default.__jumcp_connector_probe".into(),
        &[],
        5_000,
        &json!({}),
    )
    .unwrap();
    let e = executor.validate(&q).unwrap_err();
    assert!(e.code == "MCP_SQL_REJECTED" || e.code == "MCP_MULTI_STATEMENT");
}

#[tokio::test]
#[ignore = "需要真实 ClickHouse：JUMCP_TEST_CLICKHOUSE_DSN"]
async fn connector_real_agent_readonly_rejects_ddl() {
    let Ok(dsn) = std::env::var("JUMCP_TEST_CLICKHOUSE_DSN") else {
        return;
    };
    let executor = ConnectorExecutor::connect(config(&dsn, &[]))
        .await
        .expect("connect");
    // 绕过 host L4 直发 agent：证明 agent 请求侧的 readonly=1 生效。
    // `SET` 是安全探针：即使只读未生效，也只影响本次请求的设置，不产生数据/结构副作用。
    let q = QueryParams::new("SET max_threads=2".into(), &[], 5_000, &json!({})).unwrap();
    let e = executor.execute(&scope(), &q).await.unwrap_err();
    assert_eq!(e.code, "MCP_STORE_ERROR");
    assert!(
        e.message.contains("READONLY") || e.message.contains("readonly"),
        "期望 ClickHouse 只读拒绝，实际: {}",
        e.message
    );
}

#[tokio::test]
#[ignore = "需要真实 ClickHouse：JUMCP_TEST_CLICKHOUSE_DSN"]
async fn connector_real_template_draft_review_publish_and_execute() {
    use jumcp_core::registry::CapabilityStore;
    use jumcp_core::template::TemplateRegistry;

    let Ok(dsn) = std::env::var("JUMCP_TEST_CLICKHOUSE_DSN") else {
        return;
    };
    let executor = ConnectorExecutor::connect(config(&dsn, &[]))
        .await
        .expect("connect");

    let store = Arc::new(CapabilityStore::open(":memory:", None).unwrap());
    let registry = TemplateRegistry::with_dialect(store, jumcp_sqlguard::Dialect::ClickHouse);
    let value = json!({
        "template_id": "ch.params_probe",
        "group": "db.read.core",
        "risk": "read",
        "description": "参数绑定探针",
        "sql": "SELECT :x + 1",
        "params": [{"name":"x","type":"int","required":true,"nullable":false}],
        "constraints": {"allowed_schemas": []},
        "discoverable": true,
        "callable": true
    });
    registry.register(&value).expect("register");
    registry
        .submit_for_review("ch.params_probe", "alice")
        .expect("review");
    registry
        .publish("ch.params_probe", "ap-1", "bob")
        .expect("publish");
    registry
        .set_visibility("ch.params_probe", Some(true), Some(true), "alice")
        .expect("visibility");

    let record = registry.current("ch.params_probe").expect("current");
    assert_eq!(record.compiled.dialect, jumcp_sqlguard::Dialect::ClickHouse);
    assert_eq!(record.compiled.base_sql, "SELECT ? + 1");
    let prepared = record.compiled.render(&json!({"x": 41})).expect("render");
    let q = QueryParams::from_parts(prepared.sql, prepared.values, 100, 200, 0, vec![], 5_000);
    let result = executor.execute(&scope(), &q).await.expect("execute");
    assert_eq!(result["rows"][0][0], json!(42));

    // `database=env_air`：用 DSN 默认库覆盖，确认模板在真实库上可执行。
    let dsn_env_air = format!("{dsn}/env_air");
    let cfg = ConnectorConfig {
        datasource_name: "test-ch-env-air".into(),
        dsn: dsn_env_air,
        statement_timeout_ms: 5_000,
        allowed_schemas: vec!["env_air".into()],
    };
    let executor = ConnectorExecutor::connect(cfg)
        .await
        .expect("connect env_air");
    let exists = executor
            .execute(
                &scope(),
                &QueryParams::new(
                    "SELECT count() FROM system.tables WHERE database = 'env_air' AND name = 'air_orig_h'"
                        .into(),
                    &[],
                    5_000,
                    &json!({}),
                )
                .unwrap(),
            )
            .await
            .expect("probe env_air");
    let present = exists["rows"][0][0]
        .as_str()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    if present == 0 {
        return;
    }

    let store = Arc::new(CapabilityStore::open(":memory:", None).unwrap());
    let registry = TemplateRegistry::with_dialect(store, jumcp_sqlguard::Dialect::ClickHouse);
    let value = json!({
        "template_id": "ch.env_air_count",
        "group": "db.read.core",
        "risk": "read",
        "description": "env_air 行数探针",
        "sql": "SELECT count() FROM air_orig_h",
        "params": [],
        "constraints": {"allowed_schemas": ["env_air"]},
        "discoverable": true,
        "callable": true
    });
    registry.register(&value).expect("register env_air");
    registry
        .submit_for_review("ch.env_air_count", "alice")
        .expect("review env_air");
    registry
        .publish("ch.env_air_count", "ap-2", "bob")
        .expect("publish env_air");
    let record = registry
        .current("ch.env_air_count")
        .expect("current env_air");
    let prepared = record.compiled.render(&json!({})).expect("render env_air");
    let q = QueryParams::from_parts(
        prepared.sql,
        prepared.values,
        10,
        200,
        0,
        vec!["env_air".into()],
        5_000,
    );
    let result = executor
        .execute(&scope(), &q)
        .await
        .expect("execute env_air");
    let count = result["rows"][0][0]
        .as_str()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    assert!(count > 0, "env_air.air_orig_h 应有数据，实际: {count}");
}

fn scope() -> EffectiveScope {
    EffectiveScope {
        groups: vec![],
        budget: 2_000,
        degraded: false,
        source: "test".into(),
        mode: "strict".into(),
        allowed_builtin_tools: None,
    }
}
