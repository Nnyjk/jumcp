use chrono::Duration;
use jumcp_core::audit::{ActorRef, AuditEvent, AuditLog};
use jumcp_core::registry::CapabilityStore;
use serde_json::{Value, json};

/// 相对当前时间的审计时间戳：固定日期会让"窗口内/窗口外"的断言随时间腐坏。
pub(crate) fn minutes_ago(minutes: i64) -> String {
    (chrono::Utc::now() - Duration::minutes(minutes)).to_rfc3339()
}

pub(crate) fn append_event(
    log: &AuditLog,
    event_type: &str,
    ts: String,
    session: &str,
    payload: Value,
) {
    log.append(AuditEvent {
        tenant: "t1".into(),
        actor: ActorRef {
            actor_type: "agent".into(),
            id: "a-1".into(),
            role: "analyst".into(),
            token_id: None,
            source: None,
        },
        event_type: event_type.into(),
        payload,
        session_id: session.into(),
        ts,
        event_id: String::new(),
    })
    .expect("append");
}

pub(crate) fn template_run(
    log: &AuditLog,
    id: &str,
    minutes: i64,
    session: &str,
    outcome: &str,
    cache: &str,
    tokens: Option<i64>,
) {
    let mut data = json!({
        "arguments": {"template_id": id, "params": {}},
        "executed_script": "SELECT 1",
        "bound_params": [],
        "cache": cache,
        "elapsed_ms": 5,
        "version": 1
    });
    if let Some(t) = tokens {
        data["tokens"] = json!(t);
    }
    append_event(
        log,
        "template_run",
        minutes_ago(minutes),
        session,
        json!({
            "object_type": "template",
            "object_id": id,
            "action": "invoke",
            "outcome": outcome,
            "data": data
        }),
    );
}

pub(crate) fn tool_call(
    log: &AuditLog,
    minutes: i64,
    session: &str,
    outcome: &str,
    tokens: Option<i64>,
) {
    let mut data = json!({
        "arguments": {"sql": "SELECT 1"},
        "executed_script": "SELECT 1",
        "bound_params": [],
        "elapsed_ms": 3
    });
    if let Some(t) = tokens {
        data["tokens"] = json!(t);
    }
    append_event(
        log,
        "tool_call",
        minutes_ago(minutes),
        session,
        json!({
            "object_type": "tool",
            "object_id": "search_capabilities",
            "action": "invoke",
            "outcome": outcome,
            "data": data
        }),
    );
}

/// 平台面事件（`actor_type == "admin"` → 审计 `surface == "admin"`）。
///
/// 存在的理由：统计口径必须能把"管理台自己产生的调用"排除掉，而这条路径在测试里
/// 只有直接写审计才能构造（MCP 面永远派生不出 admin 主体）。
pub(crate) fn append_admin_event(
    log: &AuditLog,
    event_type: &str,
    minutes: i64,
    session: &str,
    payload: Value,
) {
    log.append(AuditEvent {
        tenant: "default".into(),
        actor: ActorRef {
            actor_type: "admin".into(),
            id: "alice".into(),
            role: "admin".into(),
            token_id: None,
            source: Some("admin-api".into()),
        },
        event_type: event_type.into(),
        payload,
        session_id: session.into(),
        ts: minutes_ago(minutes),
        event_id: String::new(),
    })
    .expect("append");
}

pub(crate) fn seed_template_capability(store: &CapabilityStore, id: &str) {
    store
        .register(
            id,
            "template",
            "db.read.core",
            "read",
            json!({"description": id}),
        )
        .expect("register template");
}

pub(crate) fn find_tool<'a>(body: &'a Value, id: &str) -> &'a Value {
    body["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .find(|t| t["id"] == json!(id))
        .unwrap_or_else(|| panic!("统计里缺少 {id}"))
}

pub(crate) fn template_definition(id: &str) -> Value {
    json!({
        "template_id": id,
        "group": "db.read.core",
        "risk": "read",
        "description": "人工登记的模板",
        "sql": "SELECT day FROM public.orders WHERE created_at >= :start_date",
        "target": {"upstream": "ch", "tool": "query"},
        "params": [{"name":"start_date","type":"timestamp","required":true,"nullable":false}],
        "constraints": {
            "allowed_schemas": ["public"],
            "auto_limit": 100,
            "max_result_tokens": 1500,
            "statement_timeout_ms": 5000
        },
        "result_cache_ttl_s": 0
    })
}
