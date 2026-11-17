//! 模板注册期的字段、结构与渲染门禁（sqlguard 复核）与运行期绑定。

use super::*;
use crate::registry::CapabilityStore;
use jumcp_sqlguard::Dialect;
use serde_json::{Value, json};
use std::sync::Arc;

fn def_value(sql: &str, params: Value, variants: Value) -> Value {
    json!({
        "template_id": "order.daily_summary",
        "group": "db.read.analytics",
        "risk": "read",
        "description": "按 day 汇总",
        "sql": sql,
        "params": params,
        "variants": variants,
        "constraints": {"allowed_schemas": ["public"]},
        "result_cache_ttl_s": 60
    })
}

fn simple_params() -> Value {
    json!([
        {"name":"start_date","type":"timestamp","required":true,"nullable":false},
        {"name":"end_date","type":"timestamp","required":true,"nullable":false}
    ])
}

#[test]
fn non_read_risk_rejected_at_registration() {
    let mut value = def_value("SELECT 1", json!([]), json!([]));
    value["risk"] = json!("write");
    let e = TemplateDef::parse(&value).unwrap_err();
    assert_eq!(e.code, "MCP_TEMPLATE_INVALID");
    assert_eq!(e.data["rule"], json!("risk_not_readonly"));
}

#[test]
fn named_placeholders_rewritten_in_declaration_order() {
    let v = def_value(
        "SELECT day FROM public.orders WHERE created_at >= :start_date AND created_at < :end_date",
        simple_params(),
        json!([]),
    );
    let compiled = compile(TemplateDef::parse(&v).unwrap()).unwrap();
    assert_eq!(
        compiled.base_sql,
        "SELECT day FROM public.orders WHERE created_at >= $1 AND created_at < $2"
    );
    assert_eq!(compiled.bind_order, vec!["start_date", "end_date"]);
}

#[test]
fn unknown_placeholder_rejected() {
    let v = def_value(
        "SELECT 1 FROM public.orders WHERE a = :nope",
        simple_params(),
        json!([]),
    );
    let e = TemplateDef::parse(&v).and_then(compile).unwrap_err();
    assert_eq!(e.code, "MCP_TEMPLATE_INVALID");
    assert_eq!(e.data["rule"], json!("unknown_placeholder"));
}

#[test]
fn jinja_rejected() {
    let v = def_value(
        "SELECT 1 FROM public.orders {% if x %} WHERE 1=1 {% endif %}",
        simple_params(),
        json!([]),
    );
    let compiled = TemplateDef::parse(&v).and_then(compile).unwrap_err();
    assert_eq!(compiled.data["rule"], json!("jinja_forbidden"));
}

#[test]
fn value_interpolation_marker_rejected() {
    let v = def_value(
        "SELECT 1 FROM public.orders WHERE a = '{{ start_date }}'",
        simple_params(),
        json!([]),
    );
    let e = TemplateDef::parse(&v).and_then(compile).unwrap_err();
    assert_eq!(e.data["rule"], json!("template_marker_invalid"));
}

#[test]
fn unclosed_marker_rejected() {
    let v = def_value(
        "SELECT 1 FROM public.orders WHERE a = '{{ start_date'",
        simple_params(),
        json!([]),
    );
    let e = TemplateDef::parse(&v).and_then(compile).unwrap_err();
    assert_eq!(e.data["rule"], json!("template_marker_unclosed"));
}

#[test]
fn array_params_rejected_at_registration() {
    for ty in ["array<int>", "array<string>", "array<timestamp>"] {
        let params = json!([{"name":"ids","type":ty}]);
        let v = def_value(
            "SELECT 1 FROM public.orders WHERE id = :ids",
            params,
            json!([]),
        );
        let e = TemplateDef::parse(&v).unwrap_err();
        assert_eq!(e.code, "MCP_TEMPLATE_INVALID");
        assert_eq!(e.data["rule"], json!("array_param_unsupported"));
        assert!(
            e.message.contains("未启用数组参数"),
            "message 需说明未启用：{}",
            e.message
        );
    }
    // 标量模板不受影响。
    let ok = def_value(
        "SELECT 1 FROM public.orders WHERE id = :start_date",
        json!([{"name":"start_date","type":"timestamp","required":true}]),
        json!([]),
    );
    assert!(TemplateDef::parse(&ok).is_ok());
}

#[test]
fn column_ref_requires_enum_and_quotes_identifier() {
    let params = json!([
        {"name":"col","type":"column_ref","required":true,"nullable":false,"enum":["channel","region"]}
    ]);
    let v = def_value(
        "SELECT {{col:enum}} FROM public.orders GROUP BY {{col:enum}}",
        params,
        json!([]),
    );
    let compiled = compile(TemplateDef::parse(&v).unwrap()).unwrap();
    let prepared = compiled.render(&json!({"col": "channel"})).unwrap();
    assert!(prepared.sql.contains("\"channel\""));
    assert!(!prepared.sql.contains("{{"));
    assert!(prepared.values.is_empty());
}

#[test]
fn column_ref_outside_whitelist_rejected() {
    let params = json!([
        {"name":"col","type":"column_ref","required":true,"nullable":false,"enum":["channel"]}
    ]);
    let v = def_value("SELECT {{col:enum}} FROM public.orders", params, json!([]));
    let compiled = compile(TemplateDef::parse(&v).unwrap()).unwrap();
    let e = compiled
        .render(&json!({"col": "amount; DROP TABLE x"}))
        .unwrap_err();
    assert_eq!(e.code, "MCP_VALIDATION_ERROR");
    assert_eq!(e.data["fields"][0]["rule"], json!("column_ref"));
}

#[test]
fn marker_on_non_column_ref_rejected() {
    let v = def_value(
        "SELECT {{start_date:enum}} FROM public.orders",
        simple_params(),
        json!([]),
    );
    let e = TemplateDef::parse(&v).and_then(compile).unwrap_err();
    assert_eq!(e.data["rule"], json!("template_marker_not_column_ref"));
}

#[test]
fn write_sql_rejected_by_gate() {
    let v = def_value("DELETE FROM public.orders", simple_params(), json!([]));
    let e = TemplateDef::parse(&v).and_then(compile).unwrap_err();
    assert_eq!(e.code, "MCP_TEMPLATE_INVALID");
    assert_eq!(e.data["rule"], json!("guard_rejected"));
}

#[test]
fn forbidden_column_rejected() {
    let mut v = def_value(
        "SELECT user_phone FROM public.orders",
        simple_params(),
        json!([]),
    );
    v["constraints"]["forbidden_columns"] = json!(["user_phone"]);
    let e = TemplateDef::parse(&v).and_then(compile).unwrap_err();
    assert_eq!(e.data["rule"], json!("forbidden_column"));
}

#[test]
fn optional_filter_variant_passes() {
    let v = def_value(
        "SELECT day FROM public.orders WHERE (:start_date IS NULL OR created_at >= :start_date)",
        json!([{"name":"start_date","type":"timestamp"}]),
        json!([]),
    );
    let compiled = compile(TemplateDef::parse(&v).unwrap()).unwrap();
    assert_eq!(compiled.bind_order, vec!["start_date"]);
}

#[test]
fn explicit_variant_matrix_selects_by_condition() {
    let v = def_value(
        "SELECT day FROM public.orders",
        json!([{"name":"channel","type":"enum","enum":["web","app"]}]),
        json!([{
            "sql_id": "with_channel",
            "when": {"channel": "not_null"},
            "sql": "SELECT day FROM public.orders WHERE channel = :channel"
        }]),
    );
    let compiled = compile(TemplateDef::parse(&v).unwrap()).unwrap();
    let with = compiled.render(&json!({"channel":"web"})).unwrap();
    assert_eq!(with.variant_id.as_deref(), Some("with_channel"));
    assert_eq!(with.values, vec![json!("web")]);
    let without = compiled.render(&json!({})).unwrap();
    assert!(without.variant_id.is_none());
}

#[test]
fn param_validation_field_errors() {
    let v = def_value(
        "SELECT 1 FROM public.orders WHERE created_at >= :start_date AND created_at < :end_date",
        simple_params(),
        json!([]),
    );
    let compiled = compile(TemplateDef::parse(&v).unwrap()).unwrap();
    let e = compiled
        .render(&json!({"start_date": "not-a-date"}))
        .unwrap_err();
    assert_eq!(e.code, "MCP_VALIDATION_ERROR");
    let fields = e.data["fields"].as_array().unwrap();
    assert!(fields.iter().any(|f| f["field"] == json!("start_date")));
    assert!(
        fields
            .iter()
            .any(|f| f["field"] == json!("end_date") && f["rule"] == json!("required"))
    );
}

#[test]
fn empty_string_becomes_null_by_default() {
    let v = def_value(
        "SELECT 1 FROM public.orders WHERE (:a IS NULL OR channel = :a)",
        json!([{"name":"a","type":"string"}]),
        json!([]),
    );
    let compiled = compile(TemplateDef::parse(&v).unwrap()).unwrap();
    let out = compiled.render(&json!({"a": ""})).unwrap();
    assert_eq!(out.values, vec![Value::Null]);
}

#[test]
fn cache_ttl_zero_disables_cache() {
    let store = Arc::new(CapabilityStore::open(":memory:", None).unwrap());
    let reg = TemplateRegistry::new(store);
    reg.cache_store("t", 1, "h", json!({"a":1}), 0);
    assert!(reg.cache_lookup("t", 1, "h").is_none());
    reg.cache_store("t", 1, "h", json!({"a":1}), 60);
    assert_eq!(reg.cache_lookup("t", 1, "h"), Some(json!({"a":1})));
}

#[test]
fn clickhouse_rewrite_uses_question_marks_and_duplicates_bindings() {
    let mut v = def_value(
        "SELECT number FROM numbers(10) WHERE number >= :n AND number < :n + 1",
        json!([{"name":"n","type":"int","required":true,"nullable":false}]),
        json!([]),
    );
    v["constraints"] = json!({"allowed_schemas": []});
    let compiled = compile_with_dialect(
        TemplateDef::parse(&v).unwrap(),
        jumcp_sqlguard::Dialect::ClickHouse,
    )
    .unwrap();
    assert_eq!(
        compiled.base_sql,
        "SELECT number FROM numbers(10) WHERE number >= ? AND number < ? + 1"
    );
    assert_eq!(compiled.bind_order, vec!["n", "n"]);
    let prepared = compiled.render(&json!({"n": 3})).unwrap();
    assert_eq!(prepared.values, vec![json!(3), json!(3)]);
}

#[test]
fn clickhouse_rejects_native_question_mark_in_template() {
    let mut v = def_value("SELECT ? AS n", json!([]), json!([]));
    v["constraints"] = json!({"allowed_schemas": []});
    let e = compile_with_dialect(
        TemplateDef::parse(&v).unwrap(),
        jumcp_sqlguard::Dialect::ClickHouse,
    )
    .unwrap_err();
    assert_eq!(e.data["rule"], json!("template_placeholder_style"));
}

#[test]
fn compiled_dialect_survives_roundtrip_and_registry_read() {
    let mut v = def_value(
        "SELECT number FROM numbers(10) WHERE number > :n",
        json!([{"name":"n","type":"int","required":true,"nullable":false}]),
        json!([]),
    );
    v["constraints"] = json!({"allowed_schemas": []});
    let compiled =
        compile_with_dialect(TemplateDef::parse(&v).unwrap(), Dialect::ClickHouse).unwrap();
    let stored = compiled.to_definition();
    assert_eq!(stored["dialect"], json!("clickhouse"));

    // 旧调用方即使传 PG 作为 fallback，也必须以入库的方言为准。
    let restored = CompiledTemplate::from_definition(&stored, Dialect::Postgres).unwrap();
    assert_eq!(restored.dialect, Dialect::ClickHouse);

    let store = Arc::new(CapabilityStore::open(":memory:", None).unwrap());
    let registry = TemplateRegistry::with_dialect(store, Dialect::ClickHouse);
    registry.register(&v).unwrap();
    let current = registry.current("order.daily_summary").unwrap();
    assert_eq!(current.compiled.dialect, Dialect::ClickHouse);
    assert_eq!(current.compiled.bind_order, vec!["n"]);
}

#[test]
fn corrupt_variants_fail_loudly_instead_of_silently_dropping() {
    let v = def_value(
        "SELECT id FROM public.orders WHERE id > :n",
        json!([{"name":"n","type":"int","required":true,"nullable":false}]),
        json!([]),
    );
    let compiled = compile(TemplateDef::parse(&v).unwrap()).unwrap();
    let stored = compiled.to_definition();
    // 入库形态里的 variants 是编译产物；结构被破坏时必须报错，而不是静默当"没有变体"
    // 回落到 base_sql（调用方会拿到与所选变体不同的结果集）。
    let mut broken = stored.clone();
    broken["variants"] =
        json!([{"sql_id": "with_x", "when": {"x": "not_null"}, "sql": "SELECT 1"}]);
    let e = CompiledTemplate::from_definition(&broken, Dialect::Postgres).unwrap_err();
    assert_eq!(e.code, "MCP_TEMPLATE_INVALID");
    assert_eq!(e.data["rule"], json!("variants_invalid"));

    // 缺省字段仍是合法的"没有变体"。
    let mut without = stored.clone();
    without.as_object_mut().unwrap().remove("variants");
    let restored = CompiledTemplate::from_definition(&without, Dialect::Postgres).unwrap();
    assert!(restored.variants.is_empty());
}

#[test]
fn pin_version_conflict() {
    let store = Arc::new(CapabilityStore::open(":memory:", None).unwrap());
    let reg = TemplateRegistry::new(store);
    assert_eq!(reg.pin("s1", "t", None, 3).unwrap(), 3);
    assert_eq!(reg.pin("s1", "t", Some(3), 4).unwrap(), 3);
    let e = reg.pin("s1", "t", Some(2), 4).unwrap_err();
    assert_eq!(e.code, "MCP_VERSION_CONFLICT");
}

/// 草案定义修订必须与注册走同一道 SQL 门禁：不能借管理面绕过模板校验。
#[test]
fn revise_draft_compiles_checks_id_and_keeps_draft() {
    let store = Arc::new(CapabilityStore::open(":memory:", None).unwrap());
    let registry = TemplateRegistry::new(store.clone());
    let value = def_value(
        "SELECT day FROM public.orders WHERE created_at >= :start_date",
        simple_params(),
        json!([]),
    );
    registry.register(&value).unwrap();

    let mut revised = value.clone();
    revised["description"] = json!("改过的说明");
    registry
        .revise_draft("order.daily_summary", &revised, "draft-edit:alice", "alice")
        .unwrap();
    let rec = store.get("order.daily_summary").unwrap().unwrap();
    assert_eq!(rec.version, 2);
    assert_eq!(rec.status, "draft");
    assert_eq!(
        rec.definition["template"]["description"],
        json!("改过的说明")
    );
    assert_eq!(
        store
            .get_version("order.daily_summary", 1)
            .unwrap()
            .unwrap()
            .definition["template"]["description"],
        json!("按 day 汇总"),
        "旧版本定义必须原样保留"
    );

    // template_id 必须与路径 id 一致。
    let e = registry
        .revise_draft("order.other", &revised, "ap", "alice")
        .unwrap_err();
    assert_eq!(e.code, "MCP_TEMPLATE_INVALID");
    assert_eq!(e.data["rule"], json!("template_id_mismatch"));

    // 写语句在编译/门禁阶段被拒，不落库。
    let mut bad = value.clone();
    bad["sql"] = json!("DELETE FROM public.orders WHERE 1=1");
    assert!(
        registry
            .revise_draft("order.daily_summary", &bad, "ap", "alice")
            .is_err()
    );
    assert_eq!(
        store.get("order.daily_summary").unwrap().unwrap().version,
        2,
        "门禁拒绝不得推进版本"
    );
}
