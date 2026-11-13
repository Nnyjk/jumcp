//! 模板注册表与 db_run_template 契约测试（W3.2 / W3.3）：不依赖真实 PG。
//!
//! 覆盖：命名→位置改写、参数错位/未声明参数、三类合法结构变体、Jinja 与值插值拒绝、
//! column_ref 白名单、会话版本钉住、deprecated 的 successor、缓存命中与 TTL、
//! 结果 token 超 max_result_tokens 的 fail / summary 两条路径。

use jumcp_core::audit::AuditLog;
use jumcp_core::config::{DatasourceRef, GroupDef, RoleDef, Settings, TenantPolicy};
use jumcp_core::errors::McpError;
use jumcp_core::registry::CapabilityStore;
use jumcp_core::template::{PreparedQuery, TemplateRegistry};
use jumcp_server::catalog::Catalog;
use jumcp_server::executor::{ExecFuture, QueryExecutor, QueryParams};
use jumcp_server::mcp::McpServer;
use jumcp_server::service::Service;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

struct FakeExecutor {
    result: Result<Value, McpError>,
    seen: Mutex<Vec<QueryParams>>,
}

impl FakeExecutor {
    fn ok(tokens: i64) -> Arc<Self> {
        Arc::new(Self {
            result: Ok(json!({
                "rows": [["2026-11-05", 3]],
                "row_count": 1,
                "truncated": false,
                "effective_max_rows": 100,
                "elapsed_ms": 1,
                "tokens": tokens
            })),
            seen: Mutex::new(Vec::new()),
        })
    }
    fn captured(&self) -> Vec<QueryParams> {
        self.seen.lock().map(|m| m.clone()).unwrap_or_default()
    }
}

impl QueryExecutor for FakeExecutor {
    fn execute<'a>(
        &'a self,
        _scope: &jumcp_core::entry::EffectiveScope,
        q: &'a QueryParams,
    ) -> ExecFuture<'a> {
        if let Ok(mut seen) = self.seen.lock() {
            seen.push(q.clone());
        }
        let result = self.result.clone();
        Box::pin(async move { result })
    }
}

fn settings(overflow: &str) -> Settings {
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
        budget_overflow: overflow.to_string(),
        datasources: vec![DatasourceRef {
            name: "main-pg".into(),
            dsn_env: "JUMCP_TEST_MAIN_DSN".into(),
            engine: "clickhouse".into(),
            readonly: true,
            allowed_schemas: vec!["public".into()],
            statement_timeout_ms: 5000,
        }],
        ..Default::default()
    }
}

fn template_value(sql: &str, params: Value, extra: Value) -> Value {
    let mut v = json!({
        "template_id": "order.daily_summary",
        "group": "db.read.analytics",
        "risk": "read",
        "description": "按日汇总订单",
        "tags": ["order", "daily"],
        "sql": sql,
        "params": params,
        "constraints": {
            "allowed_schemas": ["public"],
            "auto_limit": 100,
            "max_result_tokens": 1500,
            "statement_timeout_ms": 5000
        },
        "owner": "data-platform",
        "result_cache_ttl_s": 60
    });
    if let Some(extra_obj) = extra.as_object() {
        for (k, val) in extra_obj {
            v[k] = val.clone();
        }
    }
    v
}

fn date_params() -> Value {
    json!([
        {"name":"start_date","type":"timestamp","required":true,"nullable":false},
        {"name":"end_date","type":"timestamp","required":true,"nullable":false}
    ])
}

/// 注册 → 提审 → 发布 → 开放，返回注册表与审计（发布路径走完整审批）。
fn published_registry(
    value: &Value,
) -> (Arc<TemplateRegistry>, Arc<CapabilityStore>, Arc<AuditLog>) {
    let audit = Arc::new(AuditLog::open(":memory:").expect("audit"));
    let store = Arc::new(CapabilityStore::open(":memory:", None).expect("store"));
    let registry = Arc::new(TemplateRegistry::new(store.clone()));
    let id = value["template_id"].as_str().expect("template_id");
    registry.register(value).expect("register draft");
    registry.submit_for_review(id, "alice").expect("submit");
    registry.publish(id, "ap-pub", "bob").expect("publish");
    registry
        .set_visibility(id, Some(true), Some(true), "alice")
        .expect("visibility");
    (registry, store, audit)
}

fn server_with(
    registry: Arc<TemplateRegistry>,
    audit: Arc<AuditLog>,
    executor: Arc<dyn QueryExecutor>,
    overflow: &str,
) -> McpServer {
    let service = Service::new(settings(overflow))
        .with_executor(executor)
        .with_catalog(Catalog::default())
        .with_audit(audit)
        .with_templates(registry);
    McpServer::new(service)
}

fn run_template(template_id: &str, params: Value, version: Option<i64>) -> Value {
    let mut args = json!({"template_id": template_id, "params": params});
    if let Some(v) = version {
        args["version"] = json!(v);
    }
    json!({"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"db_run_template","arguments":args}})
}

fn err_code(resp: &Value) -> String {
    resp["result"]["content"]["error"]["code"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

const SIMPLE_SQL: &str =
    "SELECT day FROM public.orders WHERE created_at >= :start_date AND created_at < :end_date";

#[tokio::test]
async fn named_placeholders_become_positionals_in_declaration_order() {
    let value = template_value(SIMPLE_SQL, date_params(), json!({}));
    let (registry, _store, audit) = published_registry(&value);
    let executor = FakeExecutor::ok(10);
    let server = server_with(registry, audit, executor.clone(), "fail");

    let resp = server
        .handle(&run_template(
            "order.daily_summary",
            json!({"end_date": "2026-11-02", "start_date": "2026-11-01"}),
            None,
        ))
        .await;
    assert_eq!(resp["result"]["isError"], json!(false));
    let captured = executor.captured();
    assert_eq!(captured.len(), 1);
    assert_eq!(
        captured[0].sql,
        "SELECT day FROM public.orders WHERE created_at >= $1 AND created_at < $2"
    );
    // 值按声明顺序绑定，与请求里键的插入顺序无关（防止位置漂移）。
    assert_eq!(
        captured[0].params,
        vec![json!("2026-11-01"), json!("2026-11-02")]
    );
}

#[tokio::test]
async fn undeclared_param_is_rejected() {
    let value = template_value(SIMPLE_SQL, date_params(), json!({}));
    let (registry, _store, audit) = published_registry(&value);
    let server = server_with(registry, audit.clone(), FakeExecutor::ok(10), "fail");

    let resp = server
        .handle(&run_template(
            "order.daily_summary",
            json!({"start_date":"2026-11-01","end_date":"2026-11-02","strat_date":"2026-11-03"}),
            None,
        ))
        .await;
    assert_eq!(err_code(&resp), "MCP_VALIDATION_ERROR");
    assert_eq!(
        resp["result"]["content"]["error"]["data"]["fields"][0]["rule"],
        json!("unknown_param")
    );
    // render/参数校验属于早期拒绝：同样要落 tool_call（模板还没 render，不带 sql）。
    let calls = audit
        .query(Some(&json!({"event_type": "tool_call"})))
        .expect("query");
    assert_eq!(calls.len(), 1);
    let payload = &calls[0].payload;
    assert_eq!(payload["object_type"], json!("template"));
    assert_eq!(payload["object_id"], json!("order.daily_summary"));
    assert_eq!(payload["action"], json!("invoke"));
    assert_eq!(payload["outcome"], json!("error"));
    assert_eq!(payload["error_code"], json!("MCP_VALIDATION_ERROR"));
    assert_eq!(payload["data"]["template_id"], json!("order.daily_summary"));
    assert_eq!(
        payload["data"]["bound_params"],
        json!({"start_date":"2026-11-01","end_date":"2026-11-02","strat_date":"2026-11-03"}),
        "早期拒绝必须带原始 args.params"
    );
    assert!(
        payload["data"].get("executed_script").is_none(),
        "未 render 不应伪造 sql"
    );
}

/// 缺 template_id 与模板不存在都属于早期拒绝：都要计入工具台账。
#[tokio::test]
async fn early_template_errors_write_tool_call_audit() {
    let value = template_value(SIMPLE_SQL, date_params(), json!({}));
    let (registry, _store, audit) = published_registry(&value);
    let server = server_with(registry, audit.clone(), FakeExecutor::ok(10), "fail");

    let missing_id = server
        .handle(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"db_run_template","arguments":{"params":{"a":1}}}}))
        .await;
    assert_eq!(err_code(&missing_id), "MCP_VALIDATION_ERROR");

    let missing_template = server
        .handle(&run_template("order.missing", json!({"x": 1}), Some(3)))
        .await;
    assert_eq!(err_code(&missing_template), "MCP_CAPABILITY_NOT_FOUND");

    let calls = audit
        .query(Some(&json!({"event_type": "tool_call"})))
        .expect("query");
    assert_eq!(calls.len(), 2, "两次早期拒绝各记一条 tool_call");

    let first = &calls[0].payload;
    assert_eq!(first["object_type"], json!("template"));
    assert_eq!(first["object_id"], json!("db_run_template"));
    assert_eq!(first["outcome"], json!("error"));
    assert_eq!(first["error_code"], json!("MCP_VALIDATION_ERROR"));
    assert_eq!(first["data"]["bound_params"], json!({"a": 1}));
    assert!(
        first["data"].get("template_id").is_none(),
        "缺省时不得伪造 template_id"
    );

    let second = &calls[1].payload;
    assert_eq!(second["object_id"], json!("order.missing"));
    assert_eq!(second["error_code"], json!("MCP_CAPABILITY_NOT_FOUND"));
    assert_eq!(second["data"]["bound_params"], json!({"x": 1}));
    assert_eq!(second["data"]["version"], json!(3));
}

#[tokio::test]
async fn optional_filter_variant_is_accepted() {
    let value = template_value(
        "SELECT day FROM public.orders WHERE (:start_date IS NULL OR created_at >= :start_date)",
        json!([{"name":"start_date","type":"timestamp","nullable":true}]),
        json!({}),
    );
    let (registry, _store, audit) = published_registry(&value);
    let executor = FakeExecutor::ok(10);
    let server = server_with(registry, audit, executor.clone(), "fail");
    let resp = server
        .handle(&run_template("order.daily_summary", json!({}), None))
        .await;
    assert_eq!(resp["result"]["isError"], json!(false));
    assert_eq!(executor.captured()[0].params, vec![Value::Null]);
}

#[tokio::test]
async fn column_ref_variant_quotes_identifier() {
    let params = json!([
        {"name":"col","type":"column_ref","required":true,"nullable":false,"enum":["channel","region"]}
    ]);
    let value = template_value(
        "SELECT {{col:enum}}, count(*) FROM public.orders GROUP BY {{col:enum}}",
        params,
        json!({}),
    );
    let (registry, _store, audit) = published_registry(&value);
    let executor = FakeExecutor::ok(10);
    let server = server_with(registry, audit, executor.clone(), "fail");
    let resp = server
        .handle(&run_template(
            "order.daily_summary",
            json!({"col":"channel"}),
            None,
        ))
        .await;
    assert_eq!(resp["result"]["isError"], json!(false));
    let sql = &executor.captured()[0].sql;
    assert!(sql.contains("\"channel\""), "标识符必须被引用：{sql}");
    assert!(sql.contains("GROUP BY \"channel\""));
}

#[tokio::test]
async fn explicit_variant_matrix_routes_by_condition() {
    let value = template_value(
        "SELECT day FROM public.orders",
        json!([{"name":"channel","type":"enum","nullable":true,"enum":["web","app"]}]),
        json!({"variants": [{
            "sql_id": "with_channel",
            "when": {"channel": "not_null"},
            "sql": "SELECT day FROM public.orders WHERE channel = :channel"
        }]}),
    );
    let (registry, _store, audit) = published_registry(&value);
    let executor = FakeExecutor::ok(10);
    let server = server_with(registry, audit, executor.clone(), "fail");
    let resp = server
        .handle(&run_template(
            "order.daily_summary",
            json!({"channel":"web"}),
            None,
        ))
        .await;
    assert_eq!(resp["result"]["isError"], json!(false));
    assert_eq!(resp["result"]["content"]["variant"], json!("with_channel"));
    assert_eq!(executor.captured()[0].params, vec![json!("web")]);
}

#[tokio::test]
async fn value_interpolation_marker_rejected_at_registration() {
    let value = template_value(
        "SELECT * FROM public.orders WHERE day = '{{ start_date }}'",
        date_params(),
        json!({}),
    );
    let store = Arc::new(CapabilityStore::open(":memory:", None).expect("store"));
    let registry = TemplateRegistry::new(store);
    let e = registry.register(&value).unwrap_err();
    assert_eq!(e.code, "MCP_TEMPLATE_INVALID");
    assert_eq!(e.data["rule"], json!("template_marker_invalid"));
}

#[tokio::test]
async fn jinja_expression_rejected_at_registration() {
    let value = template_value(
        "SELECT * FROM public.orders {% if true %} WHERE 1=1 {% endif %}",
        date_params(),
        json!({}),
    );
    let store = Arc::new(CapabilityStore::open(":memory:", None).expect("store"));
    let registry = TemplateRegistry::new(store);
    let e = registry.register(&value).unwrap_err();
    assert_eq!(e.data["rule"], json!("jinja_forbidden"));
}

#[tokio::test]
async fn unclosed_marker_rejected_at_registration() {
    let value = template_value(
        "SELECT * FROM public.orders WHERE day = '{{ start_date'",
        date_params(),
        json!({}),
    );
    let store = Arc::new(CapabilityStore::open(":memory:", None).expect("store"));
    let registry = TemplateRegistry::new(store);
    let e = registry.register(&value).unwrap_err();
    assert_eq!(e.data["rule"], json!("template_marker_unclosed"));
}

#[tokio::test]
async fn column_ref_outside_whitelist_rejected_at_call() {
    let params = json!([
        {"name":"col","type":"column_ref","required":true,"nullable":false,"enum":["channel"]}
    ]);
    let value = template_value("SELECT {{col:enum}} FROM public.orders", params, json!({}));
    let (registry, _store, audit) = published_registry(&value);
    let server = server_with(registry, audit, FakeExecutor::ok(10), "fail");
    let resp = server
        .handle(&run_template(
            "order.daily_summary",
            json!({"col": "channel; DROP TABLE orders"}),
            None,
        ))
        .await;
    assert_eq!(err_code(&resp), "MCP_VALIDATION_ERROR");
    assert_eq!(
        resp["result"]["content"]["error"]["data"]["fields"][0]["rule"],
        json!("column_ref")
    );
}

#[tokio::test]
async fn session_version_is_pinned_and_switch_conflicts() {
    let v1 = template_value(SIMPLE_SQL, date_params(), json!({}));
    let (registry, _store, audit) = published_registry(&v1);
    // 发布 v2：必须出示预检回执（审批人可与提审人相同）。
    let receipt = registry.preflight(&v1).expect("preflight");
    let v2 = registry
        .publish_version(
            "order.daily_summary",
            &v1,
            "ap-v2",
            "alice",
            "bob",
            &receipt,
        )
        .expect("publish v2");
    assert_eq!(v2.def.template_id, "order.daily_summary");

    let executor = FakeExecutor::ok(10);
    let server = server_with(registry, audit, executor.clone(), "fail");
    let first = server
        .handle(&run_template(
            "order.daily_summary",
            json!({"start_date":"2026-11-01","end_date":"2026-11-02"}),
            None,
        ))
        .await;
    assert_eq!(first["result"]["version"], json!(2));
    // 会话已钉住 v2：显式要 v1 即冲突。
    let conflict = server
        .handle(&run_template(
            "order.daily_summary",
            json!({"start_date":"2026-11-01","end_date":"2026-11-02"}),
            Some(1),
        ))
        .await;
    assert_eq!(err_code(&conflict), "MCP_VERSION_CONFLICT");
    // 不传版本继续使用钉住的 v2。
    let again = server
        .handle(&run_template(
            "order.daily_summary",
            json!({"start_date":"2026-11-01","end_date":"2026-11-02"}),
            None,
        ))
        .await;
    assert_eq!(again["result"]["version"], json!(2));
}

#[tokio::test]
async fn deprecated_template_reports_successor() {
    let value = template_value(SIMPLE_SQL, date_params(), json!({}));
    let (registry, _store, audit) = published_registry(&value);
    registry
        .deprecate("order.daily_summary", "order.daily_summary.v2")
        .expect("deprecate");
    let server = server_with(registry, audit, FakeExecutor::ok(10), "fail");
    let resp = server
        .handle(&run_template(
            "order.daily_summary",
            json!({"start_date":"2026-11-01","end_date":"2026-11-02"}),
            None,
        ))
        .await;
    assert_eq!(err_code(&resp), "MCP_CAPABILITY_NOT_FOUND");
    assert_eq!(
        resp["result"]["content"]["error"]["data"]["successor_id"],
        json!("order.daily_summary.v2")
    );
}

#[tokio::test]
async fn cache_hit_reports_hit_and_ttl_zero_disables() {
    let value = template_value(SIMPLE_SQL, date_params(), json!({}));
    let (registry, _store, audit) = published_registry(&value);
    let executor = FakeExecutor::ok(10);
    let server = server_with(registry, audit.clone(), executor.clone(), "fail");
    let params = json!({"start_date":"2026-11-01","end_date":"2026-11-02"});

    let first = server
        .handle(&run_template("order.daily_summary", params.clone(), None))
        .await;
    assert_eq!(first["result"]["cache"], json!("miss"));
    let second = server
        .handle(&run_template("order.daily_summary", params.clone(), None))
        .await;
    assert_eq!(second["result"]["cache"], json!("hit"));
    // 命中不重复执行：执行器只被调用一次。
    assert_eq!(executor.captured().len(), 1);
    // 命中分支也要落 tokens（从缓存内容估算），否则统计面的 avg/p95 会漏掉全部命中调用。
    let events = audit
        .query(Some(&json!({"event_type": "template_run"})))
        .expect("query");
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].payload["data"]["cache"], json!("miss"));
    assert_eq!(events[1].payload["data"]["cache"], json!("hit"));
    assert_eq!(events[1].payload["outcome"], json!("ok"));
    assert!(
        events[1].payload["data"]["tokens"]
            .as_i64()
            .is_some_and(|t| t > 0),
        "命中分支必须带估算 tokens: {:?}",
        events[1].payload
    );
    // 命中分支同样要带渲染 SQL 与绑定值（缓存不改变审计口径）。
    assert_eq!(
        events[1].payload["data"]["executed_script"],
        json!("SELECT day FROM public.orders WHERE created_at >= $1 AND created_at < $2")
    );
    assert_eq!(
        events[1].payload["data"]["arguments"],
        json!({"template_id": "order.daily_summary", "params": {"start_date":"2026-11-01","end_date":"2026-11-02"}})
    );
    assert_eq!(
        events[1].payload["data"]["bound_params"],
        json!(["2026-11-01", "2026-11-02"])
    );

    // ttl=0 的模板永不缓存。
    let mut no_cache = value.clone();
    no_cache["template_id"] = json!("order.no_cache");
    no_cache["result_cache_ttl_s"] = json!(0);
    let (registry2, _s2, audit2) = published_registry(&no_cache);
    let executor2 = FakeExecutor::ok(10);
    let server2 = server_with(registry2, audit2, executor2.clone(), "fail");
    server2
        .handle(&run_template("order.no_cache", params.clone(), None))
        .await;
    let again = server2
        .handle(&run_template("order.no_cache", params.clone(), None))
        .await;
    assert_eq!(again["result"]["cache"], json!("miss"));
    assert_eq!(executor2.captured().len(), 2);
}

#[tokio::test]
async fn result_tokens_over_limit_fail_then_summary() {
    let mut value = template_value(SIMPLE_SQL, date_params(), json!({}));
    value["constraints"]["max_result_tokens"] = json!(50);
    let params = json!({"start_date":"2026-11-01","end_date":"2026-11-02"});

    // fail（默认）：显式报预算错误，不返回半套结果。
    let (registry, _store, audit) = published_registry(&value);
    let server = server_with(registry, audit, FakeExecutor::ok(200), "fail");
    let resp = server
        .handle(&run_template("order.daily_summary", params.clone(), None))
        .await;
    assert_eq!(err_code(&resp), "MCP_BUDGET_EXCEEDED");
    assert_eq!(
        resp["result"]["content"]["error"]["data"]["scope"],
        json!("max_result_tokens")
    );

    // truncate（显式开启）：只回聚合值 + summary 标记。
    let (registry2, _s2, audit2) = published_registry(&value);
    let server2 = server_with(registry2, audit2, FakeExecutor::ok(200), "truncate");
    let resp2 = server2
        .handle(&run_template("order.daily_summary", params.clone(), None))
        .await;
    assert_eq!(resp2["result"]["content"]["summary"], json!(true));
    assert_eq!(resp2["result"]["content"]["row_count"], json!(1));
    assert!(resp2["result"]["content"].get("rows").is_none());
}

#[tokio::test]
async fn preflight_receipt_must_match_definition() {
    let value = template_value(SIMPLE_SQL, date_params(), json!({}));
    let (registry, _store, _audit) = published_registry(&value);
    let receipt = registry.preflight(&value).expect("preflight");
    // 定义了 SQL 但复用旧回执：必须拒绝（防止预检 A 发布 B）。
    let mut changed = value.clone();
    changed["sql"] = json!("SELECT day FROM public.orders");
    let e = registry
        .publish_version(
            "order.daily_summary",
            &changed,
            "ap-x",
            "alice",
            "bob",
            &receipt,
        )
        .unwrap_err();
    assert_eq!(e.code, "MCP_PREFLIGHT_REQUIRED");
}

#[tokio::test]
async fn template_audit_records_required_fields() {
    let value = template_value(SIMPLE_SQL, date_params(), json!({}));
    let (registry, _store, audit) = published_registry(&value);
    let server = server_with(registry, audit.clone(), FakeExecutor::ok(10), "fail");
    server
        .handle(&run_template(
            "order.daily_summary",
            json!({"start_date":"2026-11-01","end_date":"2026-11-02"}),
            None,
        ))
        .await;
    let events = audit
        .query(Some(&json!({"event_type": "template_run"})))
        .expect("query");
    assert_eq!(events.len(), 1);
    let payload = &events[0].payload;
    assert_eq!(payload["object_type"], json!("template"));
    assert_eq!(payload["object_id"], json!("order.daily_summary"));
    assert_eq!(payload["action"], json!("invoke"));
    assert_eq!(payload["outcome"], json!("ok"));
    assert_eq!(payload["data"]["version"], json!(1));
    assert_eq!(payload["data"]["cache"], json!("miss"));
    // 渲染后的 SQL 与实际绑定值：审计必须能复现这次模板执行。
    assert_eq!(
        payload["data"]["executed_script"],
        json!("SELECT day FROM public.orders WHERE created_at >= $1 AND created_at < $2")
    );
    assert_eq!(
        payload["data"]["arguments"],
        json!({"template_id": "order.daily_summary", "params": {"start_date":"2026-11-01","end_date":"2026-11-02"}})
    );
    assert_eq!(
        payload["data"]["bound_params"],
        json!(["2026-11-01", "2026-11-02"])
    );
    assert!(payload["data"]["elapsed_ms"].as_i64().is_some());
    // 统计面依赖这三个字段：miss 分支必须带 token 估算、行数与截断标记。
    assert_eq!(payload["data"]["tokens"], json!(10));
    assert_eq!(payload["data"]["row_count"], json!(1));
    assert_eq!(payload["data"]["truncated"], json!(false));
}

#[tokio::test]
async fn render_output_shape_is_stable() {
    // 直接锁住渲染结果：命名→位置改写 + 参数序 + 参数哈希三者同时可见。
    let value = template_value(SIMPLE_SQL, date_params(), json!({}));
    let compiled = jumcp_core::template::compile(
        jumcp_core::template::TemplateDef::parse(&value).expect("parse"),
    )
    .expect("compile");
    let prepared: PreparedQuery = compiled
        .render(&json!({"start_date":"2026-11-01","end_date":"2026-11-02"}))
        .expect("render");
    assert_eq!(
        prepared.sql,
        "SELECT day FROM public.orders WHERE created_at >= $1 AND created_at < $2"
    );
    assert_eq!(
        prepared.values,
        vec![json!("2026-11-01"), json!("2026-11-02")]
    );
    assert_eq!(prepared.params_hash.len(), 64);
}
