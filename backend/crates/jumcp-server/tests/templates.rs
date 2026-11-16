//! 模板注册表契约测试（W3.2 / W3.3）：注册门禁 + 审批发布 + 渲染契约。不依赖真实 PG。
//!
//! 内置 basic 组的 `db_run_template` 已移除，模板执行面不再经过 MCP 工具；这里只锁模板层本身：
//! 注册/提审/发布/可见性生命周期、预检回执、命名→位置改写、变体路由、column_ref 引用与白名单、
//! 会话版本钉住与 deprecated 的 successor 语义。

use jumcp_core::registry::CapabilityStore;
use jumcp_core::template::{PreparedQuery, TemplateDef, TemplateRegistry, compile};
use serde_json::{Value, json};
use std::sync::Arc;

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

fn new_registry() -> Arc<TemplateRegistry> {
    let store = Arc::new(CapabilityStore::open(":memory:", None).expect("store"));
    Arc::new(TemplateRegistry::new(store))
}

/// 注册 → 提审 → 发布 → 开放（发布路径走完整审批）。
fn published_registry(value: &Value) -> Arc<TemplateRegistry> {
    let registry = new_registry();
    let id = value["template_id"].as_str().expect("template_id");
    registry.register(value).expect("register draft");
    registry.submit_for_review(id, "alice").expect("submit");
    registry.publish(id, "ap-pub", "bob").expect("publish");
    registry
        .set_visibility(id, Some(true), Some(true), "alice")
        .expect("visibility");
    registry
}

const SIMPLE_SQL: &str =
    "SELECT day FROM public.orders WHERE created_at >= :start_date AND created_at < :end_date";

/// 草案 → review → published 的阶段迁移与可见性写入必须同一份 store 可见。
#[tokio::test]
async fn publish_flow_moves_draft_through_review_to_published() {
    let value = template_value(SIMPLE_SQL, date_params(), json!({}));
    let registry = new_registry();
    registry.register(&value).expect("register draft");

    let draft = registry.current("order.daily_summary").expect("draft");
    assert_eq!(draft.status, "draft");
    assert_eq!(draft.version, 1);

    registry
        .submit_for_review("order.daily_summary", "alice")
        .expect("submit");
    assert_eq!(
        registry
            .current("order.daily_summary")
            .expect("review")
            .status,
        "review"
    );

    registry
        .publish("order.daily_summary", "ap-pub", "bob")
        .expect("publish");
    registry
        .set_visibility("order.daily_summary", Some(true), Some(true), "alice")
        .expect("visibility");
    let published = registry.current("order.daily_summary").expect("published");
    assert_eq!(published.status, "published");
    assert!(published.discoverable);
    assert!(published.callable);
    // 历史版本仍可按版本读回。
    assert_eq!(
        registry
            .at_version("order.daily_summary", 1)
            .expect("v1")
            .version,
        1
    );

    // 已发布状态不经过 review：二次发布必须报状态迁移错误。
    let again = registry
        .publish("order.daily_summary", "ap-2", "bob")
        .unwrap_err();
    assert_eq!(again.code, "MCP_STATE_TRANSITION_INVALID");
}

#[tokio::test]
async fn preflight_receipt_must_match_definition() {
    let value = template_value(SIMPLE_SQL, date_params(), json!({}));
    let registry = published_registry(&value);
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
async fn value_interpolation_marker_rejected_at_registration() {
    let value = template_value(
        "SELECT * FROM public.orders WHERE day = '{{ start_date }}'",
        date_params(),
        json!({}),
    );
    let registry = new_registry();
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
    let registry = new_registry();
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
    let registry = new_registry();
    let e = registry.register(&value).unwrap_err();
    assert_eq!(e.data["rule"], json!("template_marker_unclosed"));
}

/// 直接锁住渲染结果：命名→位置改写 + 参数序 + 参数哈希三者同时可见。
#[tokio::test]
async fn render_output_shape_is_stable() {
    let value = template_value(SIMPLE_SQL, date_params(), json!({}));
    let compiled = compile(TemplateDef::parse(&value).expect("parse")).expect("compile");
    // 请求里的键顺序与声明顺序相反：位置必须按声明序绑定，防止位置漂移。
    let prepared: PreparedQuery = compiled
        .render(&json!({"end_date":"2026-11-02", "start_date":"2026-11-01"}))
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
    assert_eq!(prepared.variant_id, None);
}

/// 未声明的参数一律拒绝：拼写错误若被忽略会静默变成 NULL。
#[tokio::test]
async fn undeclared_param_is_rejected() {
    let value = template_value(SIMPLE_SQL, date_params(), json!({}));
    let compiled = compile(TemplateDef::parse(&value).expect("parse")).expect("compile");
    let e = compiled
        .render(&json!({
            "start_date":"2026-11-01","end_date":"2026-11-02","strat_date":"2026-11-03"
        }))
        .unwrap_err();
    assert_eq!(e.code, "MCP_VALIDATION_ERROR");
    assert_eq!(e.data["fields"][0]["field"], json!("strat_date"));
    assert_eq!(e.data["fields"][0]["rule"], json!("unknown_param"));
}

#[tokio::test]
async fn optional_filter_variant_is_accepted() {
    let value = template_value(
        "SELECT day FROM public.orders WHERE (:start_date IS NULL OR created_at >= :start_date)",
        json!([{"name":"start_date","type":"timestamp","nullable":true}]),
        json!({}),
    );
    let compiled = compile(TemplateDef::parse(&value).expect("parse")).expect("compile");
    let prepared = compiled.render(&json!({})).expect("render");
    assert_eq!(prepared.values, vec![Value::Null]);
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
    let compiled = compile(TemplateDef::parse(&value).expect("parse")).expect("compile");
    let prepared = compiled.render(&json!({"col":"channel"})).expect("render");
    assert!(
        prepared.sql.contains("\"channel\""),
        "标识符必须被引用：{}",
        prepared.sql
    );
    assert!(prepared.sql.contains("GROUP BY \"channel\""));
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
    let compiled = compile(TemplateDef::parse(&value).expect("parse")).expect("compile");
    let prepared = compiled.render(&json!({"channel":"web"})).expect("render");
    assert_eq!(prepared.variant_id.as_deref(), Some("with_channel"));
    assert_eq!(prepared.values, vec![json!("web")]);
}

#[tokio::test]
async fn column_ref_outside_whitelist_rejected_at_render() {
    let params = json!([
        {"name":"col","type":"column_ref","required":true,"nullable":false,"enum":["channel"]}
    ]);
    let value = template_value("SELECT {{col:enum}} FROM public.orders", params, json!({}));
    let compiled = compile(TemplateDef::parse(&value).expect("parse")).expect("compile");
    let e = compiled
        .render(&json!({"col": "channel; DROP TABLE orders"}))
        .unwrap_err();
    assert_eq!(e.code, "MCP_VALIDATION_ERROR");
    assert_eq!(e.data["fields"][0]["rule"], json!("column_ref"));
}

/// 会话版本钉住：首调建立绑定，显式换版本即冲突。
#[tokio::test]
async fn session_pin_rejects_version_switch() {
    let v1 = template_value(SIMPLE_SQL, date_params(), json!({}));
    let registry = published_registry(&v1);
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

    assert_eq!(
        registry
            .pin("sess-1", "order.daily_summary", None, 2)
            .expect("pin"),
        2
    );
    let conflict = registry
        .pin("sess-1", "order.daily_summary", Some(1), 2)
        .unwrap_err();
    assert_eq!(conflict.code, "MCP_VERSION_CONFLICT");
    // 不传版本继续使用钉住的 v2；另一个会话互不影响。
    assert_eq!(
        registry
            .pin("sess-1", "order.daily_summary", None, 2)
            .expect("again"),
        2
    );
    assert_eq!(
        registry
            .pin("sess-2", "order.daily_summary", Some(1), 2)
            .expect("other session"),
        1
    );
}

#[tokio::test]
async fn deprecated_template_reports_successor() {
    let value = template_value(SIMPLE_SQL, date_params(), json!({}));
    let registry = published_registry(&value);
    registry
        .deprecate("order.daily_summary", "order.daily_summary.v2")
        .expect("deprecate");
    let e = registry.current("order.daily_summary").unwrap_err();
    assert_eq!(e.code, "MCP_CAPABILITY_NOT_FOUND");
    assert_eq!(e.data["successor_id"], json!("order.daily_summary.v2"));
}
