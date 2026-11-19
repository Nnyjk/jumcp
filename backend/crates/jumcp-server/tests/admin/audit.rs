use crate::support::*;
use axum::http::StatusCode;
use serde_json::json;

#[tokio::test]
async fn audit_events_filter_and_pagination() {
    let h = harness();
    for _ in 0..3 {
        h.audit
            .append(admin_audit_event("tool_call"))
            .expect("append");
    }
    h.audit
        .append(admin_audit_event("capability_search"))
        .expect("append");

    let (status, body) = call(
        &h.app,
        Req::get("/admin/audit-events?event_type=tool_call&limit=2&offset=1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(3));
    assert_eq!(body["limit"], json!(2));
    assert_eq!(body["offset"], json!(1));
    let events = body["events"].as_array().expect("events");
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|e| e["event_type"] == json!("tool_call")));
    assert!(events[0]["hash"].as_str().is_some());
    assert!(events[0]["prev_hash"].as_str().is_some());
    assert_eq!(body["order"], json!("desc"), "默认最新优先");

    // 时间范围过滤可组合。
    let (status, body) = call(
        &h.app,
        Req::get("/admin/audit-events?ts_after=1970-01-01T00:00:00Z&tenant=t1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(4));

    // object_id / actor_id / event_id 过滤走 payload 与 actor_json。
    let (status, body) = call(
        &h.app,
        Req::get("/admin/audit-events?object_id=db_query_readonly"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(4));
    let (status, body) = call(&h.app, Req::get("/admin/audit-events?actor_id=alice")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(4));
    let (status, body) = call(&h.app, Req::get("/admin/audit-events?order=asc")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["order"], json!("asc"));
    assert_eq!(body["events"][0]["event_type"], json!("tool_call"));
    let (status, body) = call(&h.app, Req::get("/admin/audit-events?order=bogus")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], json!("MCP_BAD_REQUEST"));
    // session_id 已从过滤契约删除：必须报不支持，而不是静默忽略。
    let (status, body) = call(&h.app, Req::get("/admin/audit-events?session_id=s-1")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], json!("MCP_FILTER_UNSUPPORTED"));
}

/// `surface` 过滤契约：缺省 `all`（平台治理写与 MCP 调用都要能被管理台看见），
/// `admin`/`mcp` 收窄到单面，`all` 等价缺省，其余取值 400。
#[tokio::test]
async fn audit_events_surface_filter_defaults_to_all() {
    let h = harness();
    h.audit
        .append(admin_audit_event("tool_call"))
        .expect("append");
    h.audit
        .append(audit_event("policy_decision"))
        .expect("append");

    // 缺省：两个面都在（`admin_change` 也走 mcp 面，隐藏它等于切断"从审计提取高频查询"）。
    let (status, body) = call(&h.app, Req::get("/admin/audit-events")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(2));
    let surfaces: Vec<&str> = body["events"]
        .as_array()
        .expect("events")
        .iter()
        .map(|e| e["surface"].as_str().unwrap_or_default())
        .collect();
    assert!(
        surfaces.contains(&"admin") && surfaces.contains(&"mcp"),
        "{surfaces:?}"
    );

    let (status, body) = call(&h.app, Req::get("/admin/audit-events?surface=admin")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(1));
    assert_eq!(body["events"][0]["surface"], json!("admin"));
    assert_eq!(body["events"][0]["event_type"], json!("tool_call"));

    let (status, body) = call(&h.app, Req::get("/admin/audit-events?surface=mcp")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(1));
    assert_eq!(body["events"][0]["surface"], json!("mcp"));
    assert_eq!(body["events"][0]["event_type"], json!("policy_decision"));

    let (status, body) = call(&h.app, Req::get("/admin/audit-events?surface=all")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(2));

    // 未知取值是请求错误（不是过滤键错误）：带上 field/value/supported 便于前端提示。
    let (status, body) = call(&h.app, Req::get("/admin/audit-events?surface=bogus")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], json!("MCP_BAD_REQUEST"));
    assert_eq!(body["error"]["data"]["field"], json!("surface"));
    assert_eq!(body["error"]["data"]["value"], json!("bogus"));
    assert_eq!(
        body["error"]["data"]["supported"],
        json!(["admin", "mcp", "all"])
    );
}

#[tokio::test]
async fn audit_events_limit_is_clamped() {
    let h = harness();
    h.audit
        .append(admin_audit_event("tool_call"))
        .expect("append");
    let (status, body) = call(&h.app, Req::get("/admin/audit-events?limit=9999")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["limit"], json!(200));
}

#[tokio::test]
async fn audit_events_unknown_filter_is_400() {
    let h = harness();
    let (status, body) = call(&h.app, Req::get("/admin/audit-events?bogus=1")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], json!("MCP_FILTER_UNSUPPORTED"));
    // 面筛选是过滤契约的一部分：前端据 `supported` 生成筛选器，漏掉它就是契约缺项。
    assert!(
        body["error"]["data"]["supported"]
            .as_array()
            .expect("supported")
            .contains(&json!("surface"))
    );
    let decisions = h
        .audit
        .query(Some(&json!({"event_type": "policy_decision"})))
        .expect("query");
    assert_eq!(
        decisions[0].payload["error_code"],
        json!("MCP_FILTER_UNSUPPORTED")
    );
}
