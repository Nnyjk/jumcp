//! 配置读取的单测：环境变量缺失/非法/超限，以及错误文案不泄露 DSN。

use jumcp_mcp_clickhouse::{Config, DEFAULT_TIMEOUT_MS, TIMEOUT_CEILING_MS};

fn lookup_with(
    dsn: Option<&str>,
    schemas: Option<&str>,
    timeout: Option<&str>,
) -> impl Fn(&str) -> Option<String> {
    let dsn = dsn.map(str::to_string);
    let schemas = schemas.map(str::to_string);
    let timeout = timeout.map(str::to_string);
    move |key: &str| match key {
        "JUMCP_CLICKHOUSE_DSN" => dsn.clone(),
        "JUMCP_CLICKHOUSE_ALLOWED_SCHEMAS" => schemas.clone(),
        "JUMCP_CLICKHOUSE_TIMEOUT_MS" => timeout.clone(),
        _ => None,
    }
}

#[test]
fn missing_dsn_refuses_to_start_without_echoing_anything() {
    let error = Config::from_lookup(lookup_with(None, None, None)).expect_err("缺 DSN 必须报错");
    assert!(error.contains("JUMCP_CLICKHOUSE_DSN"), "{error}");
    // 空白 DSN 等同于缺失。
    let error =
        Config::from_lookup(lookup_with(Some("   "), None, None)).expect_err("空 DSN 必须报错");
    assert!(error.contains("JUMCP_CLICKHOUSE_DSN"), "{error}");
}

#[test]
fn schemas_are_trimmed_and_empty_items_dropped() {
    let config = Config::from_lookup(lookup_with(
        Some("http://reader:secret@127.0.0.1:8123"),
        Some(" analytics , ,sales,"),
        None,
    ))
    .expect("合法配置");
    assert_eq!(config.allowed_schemas, ["analytics", "sales"]);
    assert_eq!(config.timeout_ms, DEFAULT_TIMEOUT_MS);
    assert_eq!(config.dsn, "http://reader:secret@127.0.0.1:8123");
}

#[test]
fn mixed_case_duplicates_keep_the_first_spelling() {
    let config = Config::from_lookup(lookup_with(
        Some("http://h:8123"),
        Some("ANALYTICS,analytics,ods"),
        None,
    ))
    .expect("合法配置");
    // 比较不区分大小写（与 sqlguard 一致），但 with_database 要真实拼写，所以保留首现原串。
    assert_eq!(config.allowed_schemas, ["ANALYTICS", "ods"]);
}

#[test]
fn empty_schema_variable_means_no_restriction() {
    let config = Config::from_lookup(lookup_with(
        Some("http://reader@127.0.0.1:8123"),
        Some(""),
        None,
    ))
    .expect("合法配置");
    assert!(config.allowed_schemas.is_empty());
}

#[test]
fn timeout_defaults_caps_and_rejects_garbage() {
    let config = Config::from_lookup(lookup_with(Some("http://h:8123"), None, Some("2500")))
        .expect("合法配置");
    assert_eq!(config.timeout_ms, 2_500);

    let config = Config::from_lookup(lookup_with(Some("http://h:8123"), None, Some("6172839")))
        .expect("合法配置");
    assert_eq!(config.timeout_ms, TIMEOUT_CEILING_MS);

    for bad in ["abc", "0", "-3", "1.5"] {
        let error = Config::from_lookup(lookup_with(Some("http://h:8123"), None, Some(bad)))
            .expect_err("非法超时必须报错");
        assert!(error.contains("JUMCP_CLICKHOUSE_TIMEOUT_MS"), "{error}");
        assert!(!error.contains(bad), "错误文案不应回显取值：{error}");
    }
}
