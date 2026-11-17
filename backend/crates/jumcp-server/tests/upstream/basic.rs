use crate::support::fake_upstream::{spawn_fake, spawn_fake_named};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use jumcp_server::upstream::UpstreamManager;
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::fixtures::*;

async fn get_upstream_tools(h: &Harness, id: &str) -> (StatusCode, Value) {
    call(
        &h.app,
        "GET",
        &format!("/admin/upstreams/{id}/tools"),
        Some(ALICE_TOKEN),
        None,
    )
    .await
}

async fn put_basic_tools(h: &Harness, id: &str, tools: Value) -> (StatusCode, Value) {
    call(
        &h.app,
        "PUT",
        &format!("/admin/upstreams/{id}/tools/basic"),
        Some(ALICE_TOKEN),
        Some(json!({"tools": tools})),
    )
    .await
}

async fn refresh(h: &Harness, id: &str) -> (StatusCode, Value) {
    call(
        &h.app,
        "POST",
        &format!("/admin/upstreams/{id}/refresh"),
        Some(ALICE_TOKEN),
        None,
    )
    .await
}

fn tool_row<'a>(body: &'a Value, name: &str) -> &'a Value {
    body["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .find(|t| t["name"] == json!(name))
        .unwrap_or_else(|| panic!("工具 {name} 不在视图中: {body}"))
}

#[tokio::test]
async fn upstream_group_basic_is_rejected_on_create_and_update() {
    let h = harness();
    let (url, _fake) = spawn_fake(false).await;

    // 整台上游注册到 basic 会让全部工具（含写工具）对所有人可见：只能逐工具加入。
    let mut body = upstream_body("mixed", &url);
    body["group"] = json!("basic");
    let (status, resp) = create_upstream(&h, body.clone()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{resp}");
    assert_eq!(resp["error"]["code"], json!("MCP_UPSTREAM_INVALID"));
    assert_eq!(resp["error"]["data"]["field"], json!("group"));

    // 已存在的上游改成 basic 同样拒绝。
    let (status, _) = create_upstream(&h, upstream_body("mixed", &url)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, resp) = call(
        &h.app,
        "PUT",
        "/admin/upstreams/mixed",
        Some(ALICE_TOKEN),
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{resp}");
    assert_eq!(resp["error"]["code"], json!("MCP_UPSTREAM_INVALID"));
    assert_eq!(resp["error"]["data"]["field"], json!("group"));
}

#[tokio::test]
async fn basic_tool_override_is_merged_and_idempotent() {
    let h = harness();
    let (url, _fake) = spawn_fake(false).await;
    let (status, _) = create_upstream(&h, upstream_body("mixed", &url)).await;
    assert_eq!(status, StatusCode::OK);

    // 未 refresh 时只读缓存：状态 unknown、没有工具行，GET 不触网。
    let (status, body) = get_upstream_tools(&h, "mixed").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], json!("unknown"));
    assert_eq!(body["total"], json!(0));
    assert_eq!(body["basic_total"], json!(0));
    assert_eq!(body["last_checked_at"], Value::Null);

    let (status, _) = refresh(&h, "mixed").await;
    assert_eq!(status, StatusCode::OK);
    let (_, body) = get_upstream_tools(&h, "mixed").await;
    assert_eq!(body["total"], json!(3), "{body}");
    for tool in body["tools"].as_array().expect("tools") {
        assert_eq!(tool["group"], json!("db.read.analytics"), "{tool}");
        assert_eq!(tool["group_source"], json!("upstream"));
        assert_eq!(tool["basic"], json!(false));
        assert_eq!(tool["overridden"], json!(false));
        assert_eq!(tool["eligible"], json!(true));
        assert_eq!(tool["orphaned"], json!(false));
    }
    assert_eq!(
        tool_row(&body, "echo")["exposed_name"],
        json!("mixed__echo")
    );

    // 加入 basic：响应带 changed 与新的生效组。
    let (status, body) = put_basic_tools(&h, "mixed", json!({"echo": true})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["changed"],
        json!([{"field": "tool_group", "tool": "echo", "before": null, "after": "basic"}])
    );
    assert_eq!(body["basic_total"], json!(1));
    let echo = tool_row(&body, "echo");
    assert_eq!(echo["group"], json!("basic"));
    assert_eq!(echo["group_source"], json!("basic"));
    assert_eq!(echo["basic"], json!(true));
    assert_eq!(echo["overridden"], json!(true));
    // 未覆盖的工具继续继承上游组。
    assert_eq!(
        tool_row(&body, "explode")["group"],
        json!("db.read.analytics")
    );

    let audit_rows = || {
        h.audit
            .query(Some(
                &json!({"event_type": "admin_change", "object_id": "mixed"}),
            ))
            .expect("audit query")
            .into_iter()
            .filter(|e| e.payload["action"] == json!("update_tool_groups"))
            .count()
    };
    assert_eq!(audit_rows(), 1, "首次加入必须写一条审计");
    let events = h
        .audit
        .query(Some(
            &json!({"event_type": "admin_change", "object_id": "mixed"}),
        ))
        .expect("audit query");
    let event = events
        .iter()
        .find(|e| e.payload["action"] == json!("update_tool_groups"))
        .expect("update_tool_groups 审计");
    assert_eq!(event.payload["object_type"], json!("upstream"));
    assert_eq!(
        event.payload["changes"],
        json!([{"field": "tool_group", "tool": "echo", "before": null, "after": "basic"}])
    );
    assert!(
        event.payload["data"].get("env").is_none()
            && event.payload["data"].get("headers").is_none(),
        "审计不得泄露 env/headers: {event:?}"
    );

    // 幂等：重复同值不写库、不写审计、changed 为空。
    let (status, body) = put_basic_tools(&h, "mixed", json!({"echo": true})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["changed"], json!([]));
    assert_eq!(body["basic_total"], json!(1));
    assert_eq!(audit_rows(), 1, "无变化不得新增审计");

    // 清除：false 删除覆盖，回到继承上游组。
    let (status, body) = put_basic_tools(&h, "mixed", json!({"echo": false})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["changed"],
        json!([{"field": "tool_group", "tool": "echo", "before": "basic", "after": null}])
    );
    assert_eq!(body["basic_total"], json!(0));
    assert_eq!(tool_row(&body, "echo")["group"], json!("db.read.analytics"));
    assert_eq!(tool_row(&body, "echo")["overridden"], json!(false));
    assert_eq!(audit_rows(), 2);
}

#[tokio::test]
async fn basic_tool_override_rejects_unknown_write_and_admin_tools() {
    let h = harness();
    let (url, _fake) = spawn_fake(false).await;
    let (status, _) = create_upstream(&h, upstream_body("mixed", &url)).await;
    assert_eq!(status, StatusCode::OK);
    refresh(&h, "mixed").await;

    // 未知工具：整请求拒绝，data.unknown 指出是哪一个。
    let (status, body) = put_basic_tools(&h, "mixed", json!({"nope": true})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], json!("MCP_UPSTREAM_TOOL_UNKNOWN"));
    assert_eq!(body["error"]["data"]["unknown"], json!(["nope"]));
    // 未知与已知混在同一请求时也不做部分写入。
    let (status, body) = put_basic_tools(&h, "mixed", json!({"echo": true, "nope": true})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let (_, view) = get_upstream_tools(&h, "mixed").await;
    assert_eq!(view["basic_total"], json!(0), "拒绝的请求不得落库");

    // 不存在的上游 → 404。
    let (status, body) = put_basic_tools(&h, "ghost", json!({"echo": true})).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], json!("MCP_UPSTREAM_NOT_FOUND"));

    // 写上游：basic 只允许只读工具。
    let hw = harness_with(false, true);
    let (wurl, _wfake) = spawn_fake(false).await;
    let mut wbody = upstream_body("writer", &wurl);
    wbody["read_only"] = json!(false);
    let (status, body) = create_upstream(&hw, wbody).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    refresh(&hw, "writer").await;
    let (status, body) = put_basic_tools(&hw, "writer", json!({"echo": true})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        body["error"]["code"],
        json!("MCP_UPSTREAM_TOOL_NOT_READ_ONLY")
    );

    // 上游类别是 admin：维护通道不能借 basic 变成人人可见。
    let (aurl, _afake) = spawn_fake(false).await;
    let mut abody = upstream_body("ops", &aurl);
    abody["category"] = json!("admin");
    let (status, body) = create_upstream(&h, abody).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    refresh(&h, "ops").await;
    let (status, body) = put_basic_tools(&h, "ops", json!({"echo": true})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        body["error"]["code"],
        json!("MCP_UPSTREAM_TOOL_NOT_ELIGIBLE")
    );
    assert_eq!(
        body["error"]["data"]["reason"],
        json!("upstream_category_admin")
    );

    // 工具名以 admin_ 开头：同上。
    let (purl, _pfake) = spawn_fake_named(&["admin_purge"]).await;
    let (status, body) = create_upstream(&h, upstream_body("danger", &purl)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    refresh(&h, "danger").await;
    let (status, body) = put_basic_tools(&h, "danger", json!({"admin_purge": true})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        body["error"]["code"],
        json!("MCP_UPSTREAM_TOOL_NOT_ELIGIBLE")
    );
    assert_eq!(body["error"]["data"]["reason"], json!("admin_prefix"));
    let (_, view) = get_upstream_tools(&h, "danger").await;
    assert_eq!(tool_row(&view, "admin_purge")["eligible"], json!(false));
}

#[tokio::test]
async fn basic_tool_override_survives_refresh_and_is_orphaned_not_dropped() {
    let h = harness_with(false, false);
    let (url, fake) = spawn_fake_named(&["echo", "keep"]).await;
    let (status, _) = create_upstream(&h, upstream_body("mixed", &url)).await;
    assert_eq!(status, StatusCode::OK);
    refresh(&h, "mixed").await;
    let (status, body) = put_basic_tools(&h, "mixed", json!({"echo": true})).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // 上游把 echo 改名成 other：覆盖行保留，标成 orphaned 由管理面清理。
    fake.set_tools(&["keep", "other"]);
    refresh(&h, "mixed").await;
    let (_, body) = get_upstream_tools(&h, "mixed").await;
    assert_eq!(body["total"], json!(3), "{body}");
    assert_eq!(body["basic_total"], json!(1));
    assert_eq!(body["orphaned_total"], json!(1));
    let orphan = tool_row(&body, "echo");
    assert_eq!(orphan["orphaned"], json!(true));
    assert_eq!(orphan["basic"], json!(true));
    assert_eq!(orphan["overridden"], json!(true));
    assert_eq!(orphan["eligible"], json!(false));
    assert_eq!(tool_row(&body, "keep")["orphaned"], json!(false));
    assert_eq!(tool_row(&body, "keep")["overridden"], json!(false));

    // 漂移后的清理：false 删除孤行（此时 echo 已不在缓存里）。
    let (status, body) = put_basic_tools(&h, "mixed", json!({"echo": false})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["changed"],
        json!([{"field": "tool_group", "tool": "echo", "before": "basic", "after": null}])
    );
    assert_eq!(body["orphaned_total"], json!(0));

    // 上游删除时同一事务级联清掉覆盖行（不能留下悬挂配置）。
    let (status, body) = put_basic_tools(&h, "mixed", json!({"keep": true})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(h.store.upstream_tool_groups().expect("rows").len(), 1);
    let (status, _) = call(
        &h.app,
        "DELETE",
        "/admin/upstreams/mixed",
        Some(ALICE_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(h.store.upstream_tool_groups().expect("rows").is_empty());
    let (status, _) = get_upstream_tools(&h, "mixed").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn basic_precheck_matches_sanitized_exposed_name() {
    let h = harness();
    let (url, _fake) = spawn_fake_named(&["orders.lookup"]).await;
    let (status, _) = create_upstream(&h, upstream_body("dots", &url)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = refresh(&h, "dots").await;
    assert_eq!(status, StatusCode::OK);

    // 真实清洗规则：`.` 不是合法字符，暴露名把它换成 `-`，原始名留在 `name` 里。
    let (_, view) = get_upstream_tools(&h, "dots").await;
    assert_eq!(
        tool_row(&view, "orders.lookup")["exposed_name"],
        json!("dots__orders-lookup")
    );

    let manager = UpstreamManager::new(h.store.clone(), false);
    let out_of_scope = scope(&["db.read.core"]); // 不含上游组 db.read.analytics
    assert!(
        !manager.matches_upstream(&out_of_scope, "dots__orders-lookup"),
        "没有 basic 覆盖时不得命中"
    );

    let (status, body) = put_basic_tools(&h, "dots", json!({"orders.lookup": true})).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // 覆盖行按原始名存储、预检拿到的却是清洗后的暴露名：必须按 exposed_tool_name 比较。
    assert!(
        manager.matches_upstream(&out_of_scope, "dots__orders-lookup"),
        "清洗过名字的 basic 工具必须被预检命中"
    );
    assert!(!manager.matches_upstream(&out_of_scope, "dots__orders.lookup"));
    assert!(manager.matches_upstream(&scope(&["db.read.analytics"]), "dots__orders-lookup"));

    // 端到端：预检命中后仍由 allowlist 拒绝，绝不因为名字清洗而漏过检查去执行。
    let token = issue_token(
        &h,
        "agent-no-basic",
        "core-only",
        &["db.read.core"],
        &["search_capabilities"],
    )
    .await;
    let denied = call_tool(&h.app, &token, "dots__orders-lookup", json!({})).await;
    assert_eq!(denied["result"]["isError"], json!(true), "{denied}");
    assert_eq!(
        denied["result"]["content"]["error"]["code"],
        json!("MCP_CAPABILITY_NOT_FOUND"),
        "{denied}"
    );
}

#[tokio::test]
async fn basic_override_stays_visible_when_upstream_turns_write() {
    let h = harness_with(false, true);
    let (url, _fake) = spawn_fake_named(&["echo", "keep"]).await;
    let (status, _) = create_upstream(&h, upstream_body("mixed", &url)).await;
    assert_eq!(status, StatusCode::OK);
    refresh(&h, "mixed").await;
    let (status, body) = put_basic_tools(&h, "mixed", json!({"echo": true})).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // 上游改成写通道：覆盖行仍可见（overridden=true），但不生效（basic=false）。
    let mut body = upstream_body("mixed", &url);
    body["read_only"] = json!(false);
    let (status, resp) = call(
        &h.app,
        "PUT",
        "/admin/upstreams/mixed",
        Some(ALICE_TOKEN),
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    refresh(&h, "mixed").await;

    let (_, view) = get_upstream_tools(&h, "mixed").await;
    let echo = tool_row(&view, "echo");
    assert_eq!(echo["overridden"], json!(true));
    assert_eq!(echo["basic"], json!(false));
    assert_eq!(echo["group"], json!("db.read.analytics"));
    assert_eq!(echo["group_source"], json!("upstream"));
    assert_eq!(echo["eligible"], json!(false));
    assert_eq!(view["basic_total"], json!(0));

    // 管理面据此提交 null 清理失效覆盖。
    let (status, body) = put_basic_tools(&h, "mixed", json!({"echo": null})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["changed"],
        json!([{"field": "tool_group", "tool": "echo", "before": "basic", "after": null}])
    );
    let (_, view) = get_upstream_tools(&h, "mixed").await;
    assert_eq!(tool_row(&view, "echo")["overridden"], json!(false));
}

async fn tool_names_with_groups(app: &Router, token: &str, groups: &str) -> Vec<String> {
    let request = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-mcp-tool-groups", groups)
        .body(Body::from(
            json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}).to_string(),
        ))
        .expect("request");
    let response = app.clone().oneshot(request).await.expect("response");
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    let value: Value = serde_json::from_slice(&bytes).expect("json body");
    value["result"]["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|t| t["name"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// basic 成员对所有 scope 可见/可调（不要求 scope.groups 含 basic 或上游组），
/// 但仍受 token allowlist、上游 enabled/只读约束；非 basic 工具照旧按上游组门控。
#[tokio::test]
async fn basic_members_bypass_group_scope_but_not_the_allowlist() {
    let h = harness();
    let (url, _fake) = spawn_fake(false).await;
    let (status, _) = create_upstream(&h, upstream_body("mixed", &url)).await;
    assert_eq!(status, StatusCode::OK);
    refresh(&h, "mixed").await;
    let (status, body) = put_basic_tools(&h, "mixed", json!({"echo": true})).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // 令牌组只有 db.read.core：上游组（db.read.analytics）不在 scope 内。
    let token = issue_token(
        &h,
        "agent-basic",
        "core-only",
        &["db.read.core"],
        &["search_capabilities", "mixed__echo"],
    )
    .await;
    let names = tool_names(&h.app, &token).await;
    assert!(names.contains(&"mixed__echo".to_string()), "{names:?}");
    assert!(
        !names.contains(&"mixed__explode".to_string()),
        "非 basic 工具必须仍受上游组门控: {names:?}"
    );
    let resp = mcp(&h.app, &token, "tools/list", json!({})).await;
    let echo = resp["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .find(|t| t["name"] == json!("mixed__echo"))
        .expect("basic 成员");
    assert_eq!(echo["group"], json!("basic"), "{echo}");
    assert_eq!(echo["source"], json!("upstream"));

    let ok = call_tool(&h.app, &token, "mixed__echo", json!({"msg":"hi"})).await;
    assert_eq!(ok["result"]["isError"], json!(false), "{ok}");
    let denied = call_tool(&h.app, &token, "mixed__explode", json!({})).await;
    assert!(
        denied["result"].is_null(),
        "非 basic 且组不在 scope 内必须拒绝: {denied}"
    );

    // allowlist 仍然优先：不含 mixed__echo 的令牌既看不到也调不到。
    let without = issue_token(
        &h,
        "agent-no-basic",
        "core-only",
        &["db.read.core"],
        &["search_capabilities"],
    )
    .await;
    let names = tool_names(&h.app, &without).await;
    assert!(!names.iter().any(|n| n.contains("__")), "{names:?}");
    let denied = call_tool(&h.app, &without, "mixed__echo", json!({})).await;
    assert_eq!(denied["result"]["isError"], json!(true), "{denied}");
    assert_eq!(
        denied["result"]["content"]["error"]["code"],
        json!("MCP_CAPABILITY_NOT_FOUND"),
        "{denied}"
    );

    // 上游组在 scope 内时，非 basic 工具照旧可见（basic 不是唯一通道）。
    let analyst = issue_token(
        &h,
        "agent-scope",
        "analyst",
        &["db.read.analytics"],
        &["mixed__explode"],
    )
    .await;
    let names = tool_names(&h.app, &analyst).await;
    assert!(names.contains(&"mixed__explode".to_string()), "{names:?}");

    // X-MCP-Tool-Groups 收窄到不含上游组：basic 成员不得消失。
    let narrowed = tool_names_with_groups(&h.app, &token, "db.read.core").await;
    assert!(
        narrowed.contains(&"mixed__echo".to_string()),
        "basic 成员不受组收窄影响: {narrowed:?}"
    );
}

/// 组索引只在存在 basic 覆盖时出现 `basic`，并按生效组计数（不把常驻内置工具算进去）。
#[tokio::test]
async fn basic_members_appear_in_group_index_only_when_overridden() {
    let h = harness();
    let (url, _fake) = spawn_fake(false).await;
    let (status, _) = create_upstream(&h, upstream_body("mixed", &url)).await;
    assert_eq!(status, StatusCode::OK);
    refresh(&h, "mixed").await;
    let token = issue_token(
        &h,
        "agent-index",
        "core-only",
        &["db.read.core"],
        &["list_groups", "mixed__echo"],
    )
    .await;

    let groups = call_tool(&h.app, &token, "list_groups", json!({})).await;
    let rows = groups["result"]["content"]
        .as_array()
        .expect("groups")
        .clone();
    assert!(
        !rows.iter().any(|g| g["group"] == json!("basic")),
        "没有 basic 覆盖时不得出现 basic: {groups}"
    );

    let (status, body) = put_basic_tools(&h, "mixed", json!({"echo": true})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let groups = call_tool(&h.app, &token, "list_groups", json!({})).await;
    let rows = groups["result"]["content"].as_array().expect("groups");
    let basic = rows
        .iter()
        .find(|g| g["group"] == json!("basic"))
        .unwrap_or_else(|| panic!("basic 必须出现在组索引: {groups}"));
    assert_eq!(basic["capability_count"], json!(1));
    assert!(
        !rows
            .iter()
            .any(|g| g["group"] == json!("db.read.analytics")),
        "scope 外的上游组不得出现: {groups}"
    );
}
