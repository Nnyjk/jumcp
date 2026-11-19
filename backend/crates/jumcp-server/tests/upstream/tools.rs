use crate::support::fake_upstream::{dead_url, spawn_fake, spawn_fake_named};
use axum::http::StatusCode;
use serde_json::json;

use crate::fixtures::*;

#[tokio::test]
async fn inserted_upstream_tools_are_listed_and_forwarded() {
    let h = harness();
    let (url, fake) = spawn_fake(false).await;
    let mut body = upstream_body("analytics", &url);
    body["headers"] = json!({"X-Trace": "router"});
    let (status, item) = create_upstream(&h, body).await;
    assert_eq!(status, StatusCode::OK, "{item}");
    let token = issue_token(
        &h,
        "agent-upstream",
        "analyst",
        &["db.read.analytics"],
        &[
            "search_capabilities",
            "describe_capability",
            "list_groups",
            "analytics__echo",
            "analytics__weird-name",
        ],
    )
    .await;

    // 下一次 tools/list 立即出现前缀工具（无需重启）。
    let names = tool_names(&h.app, &token).await;
    assert!(names.contains(&"analytics__echo".to_string()), "{names:?}");
    assert!(
        names.contains(&"analytics__weird-name".to_string()),
        "非法字符必须转 -：{names:?}"
    );
    assert!(!names.contains(&"echo".to_string()), "上游工具不得裸名暴露");

    // tools/list 条目带来源坐标、授权组、分类与上游注解。
    let resp = mcp(&h.app, &token, "tools/list", json!({})).await;
    let echo = resp["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .find(|t| t["name"] == json!("analytics__echo"))
        .expect("echo tool");
    assert_eq!(echo["source"], json!("upstream"));
    assert_eq!(echo["server_id"], json!("analytics"));
    assert_eq!(echo["upstream_tool"], json!("echo"));
    assert_eq!(echo["group"], json!("db.read.analytics"));
    assert_eq!(echo["category"], json!("custom"));
    assert_eq!(echo["annotations"]["readOnlyHint"], json!(true));
    assert_eq!(echo["annotations"]["idempotentHint"], json!(true));

    // tools/call 转发到上游。
    let resp = call_tool(&h.app, &token, "analytics__echo", json!({"msg":"hi"})).await;
    assert_eq!(resp["result"]["isError"], json!(false), "{resp}");
    assert_eq!(resp["result"]["content"][0]["text"], json!("echo:echo"));
    let forwarded = fake.requests.lock().expect("lock").clone();
    assert_eq!(
        forwarded.last().expect("forwarded")["params"]["arguments"]["msg"],
        json!("hi")
    );

    // 审计：tool_call + object_type=tool + 上游坐标。
    let calls = h
        .audit
        .query(Some(
            &json!({"event_type": "tool_call", "object_id": "analytics__echo"}),
        ))
        .expect("audit query");
    let call = calls.last().expect("tool_call event");
    assert_eq!(call.payload["action"], json!("call"));
    assert_eq!(call.payload["data"]["upstream_id"], json!("analytics"));
    assert_eq!(call.payload["data"]["upstream_tool"], json!("echo"));
    assert_eq!(call.payload["data"]["outcome"], json!("ok"));
    assert!(
        call.payload["data"]["definition_hash"]
            .as_str()
            .is_some_and(|h| !h.is_empty())
    );
    assert_eq!(call.payload["data"]["arguments"]["msg"], json!("hi"));

    // 上游工具进 search / describe / list_groups。
    let searched = call_tool(
        &h.app,
        &token,
        "search_capabilities",
        json!({"query":"analytics__echo"}),
    )
    .await;
    assert_eq!(
        searched["result"]["structuredContent"]["results"][0]["id"],
        json!("analytics__echo")
    );
    assert_eq!(
        searched["result"]["structuredContent"]["results"][0]["category"],
        json!("custom")
    );
    let described = call_tool(
        &h.app,
        &token,
        "describe_capability",
        json!({"capability_id":"analytics__echo"}),
    )
    .await;
    assert_eq!(
        described["result"]["structuredContent"]["source"],
        json!("upstream")
    );
    let groups = call_tool(&h.app, &token, "list_groups", json!({})).await;
    assert!(
        groups["result"]["structuredContent"]["groups"]
            .as_array()
            .expect("groups")
            .iter()
            .any(|g| g["group"] == json!("db.read.analytics")),
        "上游工具必须按注册组计数: {groups}"
    );
}

#[tokio::test]
async fn upstream_tools_respect_the_token_allowlist() {
    let h = harness();
    let (url, fake) = spawn_fake(false).await;
    let (status, _) = create_upstream(&h, upstream_body("analytics", &url)).await;
    assert_eq!(status, StatusCode::OK);

    // allowlist 不含上游暴露名：列表/检索不可见，调用回 MCP_CAPABILITY_NOT_FOUND 并写 policy_decision。
    let denied = issue_token(
        &h,
        "agent-no-upstream",
        "analyst",
        &["db.read.analytics"],
        &["search_capabilities"],
    )
    .await;
    let names = tool_names(&h.app, &denied).await;
    assert!(
        !names.iter().any(|n| n.contains("__")),
        "allowlist 之外的上游工具不得出现: {names:?}"
    );
    let searched = call_tool(
        &h.app,
        &denied,
        "search_capabilities",
        json!({"query":"analytics__", "limit":10}),
    )
    .await;
    assert_eq!(
        searched["result"]["structuredContent"]["results"],
        json!([])
    );

    let resp = call_tool(&h.app, &denied, "analytics__echo", json!({})).await;
    assert_eq!(resp["result"]["isError"], json!(true), "{resp}");
    assert_eq!(
        resp["result"]["structuredContent"]["error"]["code"],
        json!("MCP_CAPABILITY_NOT_FOUND")
    );
    assert!(
        fake.requests.lock().expect("lock").is_empty(),
        "allowlist 拒绝必须先于上游解析：不得产生任何上游调用"
    );
    let decisions = h
        .audit
        .query(Some(
            &json!({"event_type": "policy_decision", "object_id": "analytics__echo"}),
        ))
        .expect("audit query");
    assert!(
        !decisions.is_empty(),
        "allowlist 拒绝必须写 policy_decision"
    );
    assert_eq!(
        decisions.last().expect("decision").payload["error_code"],
        json!("MCP_CAPABILITY_NOT_FOUND")
    );

    // allowlist 含该名：正常可见/可调。
    let allowed = issue_token(
        &h,
        "agent-with-upstream",
        "analyst",
        &["db.read.analytics"],
        &["analytics__echo"],
    )
    .await;
    assert!(
        tool_names(&h.app, &allowed)
            .await
            .contains(&"analytics__echo".to_string())
    );
    let resp = call_tool(&h.app, &allowed, "analytics__echo", json!({})).await;
    assert_eq!(resp["result"]["isError"], json!(false), "{resp}");
    assert_eq!(
        fake.requests.lock().expect("lock").len(),
        1,
        "放行后只应产生一次上游调用"
    );
}

#[tokio::test]
async fn upstream_errors_become_is_error_and_prefix_fallback_works() {
    let h = harness();
    let (url, fake) = spawn_fake(false).await;
    assert_eq!(
        create_upstream(&h, upstream_body("analytics", &url))
            .await
            .0,
        StatusCode::OK
    );
    let token = issue_token(
        &h,
        "agent-errors",
        "analyst",
        &["db.read.analytics"],
        &["analytics__explode", "analytics__ghost"],
    )
    .await;
    let _ = tool_names(&h.app, &token).await;

    // 上游 JSON-RPC error → isError 结果 + MCP_UPSTREAM_ERROR（不冒泡成本服务协议错误）。
    let resp = call_tool(&h.app, &token, "analytics__explode", json!({})).await;
    assert_eq!(resp["result"]["isError"], json!(true), "{resp}");
    assert_eq!(
        resp["result"]["structuredContent"]["error"]["code"],
        json!("MCP_UPSTREAM_ERROR")
    );
    assert!(resp.get("error").is_none(), "不得变成 JSON-RPC 协议错误");

    // 旧客户端缓存了不在本次列表里的名字：按 `server_id__` 前缀回退，仍转发给上游。
    let resp = call_tool(&h.app, &token, "analytics__ghost", json!({})).await;
    assert_eq!(resp["result"]["isError"], json!(false), "{resp}");
    assert_eq!(resp["result"]["content"][0]["text"], json!("echo:ghost"));
    let forwarded: Vec<String> = fake
        .requests
        .lock()
        .expect("lock")
        .iter()
        .filter_map(|r| r["params"]["name"].as_str().map(String::from))
        .collect();
    assert!(forwarded.contains(&"ghost".to_string()), "{forwarded:?}");

    // 完全没有前缀的名字仍然 -32602。
    let resp = mcp(
        &h.app,
        &token,
        "tools/call",
        json!({"name":"nope__x","arguments":{}}),
    )
    .await;
    assert_eq!(resp["error"]["code"], json!(-32602));
}

#[tokio::test]
async fn refresh_reports_status_and_audits() {
    let h = harness();
    let (url, _fake) = spawn_fake(false).await;
    assert_eq!(
        create_upstream(&h, upstream_body("analytics", &url))
            .await
            .0,
        StatusCode::OK
    );
    let (status, refreshed) = call(
        &h.app,
        "POST",
        "/admin/upstreams/analytics/refresh",
        Some(ALICE_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{refreshed}");
    assert_eq!(refreshed["status"], json!("ok"));
    assert_eq!(refreshed["tool_count"], json!(3));
    assert!(refreshed["last_checked_at"].as_str().is_some());

    // 失败的 refresh 仍是 200 + status=error。
    assert_eq!(
        create_upstream(&h, upstream_body("dead", &dead_url().await))
            .await
            .0,
        StatusCode::OK
    );
    let (status, refreshed) = call(
        &h.app,
        "POST",
        "/admin/upstreams/dead/refresh",
        Some(ALICE_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(refreshed["status"], json!("error"));

    let events = h
        .audit
        .query(Some(&json!({"event_type": "admin_change"})))
        .expect("audit query");
    let refresh = events
        .iter()
        .filter(|e| e.payload["object_id"] == json!("analytics"))
        .find(|e| e.payload["action"] == json!("refresh"))
        .expect("refresh event");
    assert_eq!(refresh.payload["data"]["tool_count"], json!(3));
    assert_eq!(refresh.payload["data"]["status"], json!("ok"));
    assert_eq!(refresh.payload["data"]["transport"], json!("http"));
}

#[tokio::test]
async fn refresh_reconnects_and_refetches_tools() {
    let h = harness();
    let (url, fake) = spawn_fake_named(&["alpha_tool"]).await;
    assert_eq!(
        create_upstream(&h, upstream_body("refresh", &url)).await.0,
        StatusCode::OK
    );
    let token = issue_token(
        &h,
        "agent-refresh",
        "analyst",
        &["db.read.analytics"],
        &["refresh__alpha_tool", "refresh__beta_tool"],
    )
    .await;
    let names = tool_names(&h.app, &token).await;
    assert!(
        names.contains(&"refresh__alpha_tool".to_string()),
        "{names:?}"
    );

    // 上游换了工具集；refresh 必须清缓存并重新 initialize + tools/list。
    fake.set_tools(&["beta_tool"]);
    let (status, refreshed) = call(
        &h.app,
        "POST",
        "/admin/upstreams/refresh/refresh",
        Some(ALICE_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{refreshed}");
    assert_eq!(refreshed["status"], json!("ok"));
    assert_eq!(refreshed["tool_count"], json!(1));

    let names = tool_names(&h.app, &token).await;
    assert!(
        names.contains(&"refresh__beta_tool".to_string()),
        "refresh 后必须看到新工具列表: {names:?}"
    );
    assert!(
        !names.contains(&"refresh__alpha_tool".to_string()),
        "旧工具必须消失: {names:?}"
    );
}

#[tokio::test]
async fn naming_conflicts_are_rejected_instead_of_renamed() {
    let h = harness();

    // 跨上游撞名（id 禁止 `__`，但 `_` 边界仍可构造）：`a` + `_b` 与 `a_` + `b` 都暴露成 `a___b`。
    let (first_url, _first) = spawn_fake_named(&["_b"]).await;
    let (second_url, _second) = spawn_fake_named(&["b"]).await;
    assert_eq!(
        create_upstream(&h, upstream_body("ab", &first_url)).await.0,
        StatusCode::OK
    );
    assert_eq!(
        create_upstream(&h, upstream_body("ab_", &second_url))
            .await
            .0,
        StatusCode::OK
    );
    let token = issue_token(
        &h,
        "agent-conflict",
        "analyst",
        &["db.read.analytics"],
        &["ab___b", "dup__dup-name"],
    )
    .await;

    // 先触发一次投影（并行首拉两台上游），状态才有内容。
    let names = tool_names(&h.app, &token).await;
    let (status, list) = call(&h.app, "GET", "/admin/upstreams", Some(ALICE_TOKEN), None).await;
    assert_eq!(status, StatusCode::OK);
    let items = list["items"].as_array().expect("items");
    let statuses: Vec<(&str, &str)> = ["ab", "ab_"]
        .iter()
        .map(|id| {
            let item = items
                .iter()
                .find(|i| i["id"] == json!(id))
                .expect("upstream item");
            (
                item["status"].as_str().unwrap_or_default(),
                item["status_message"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    let ok_count = statuses.iter().filter(|(s, _)| *s == "ok").count();
    let error_count = statuses.iter().filter(|(s, _)| *s == "error").count();
    assert_eq!(
        (ok_count, error_count),
        (1, 1),
        "撞名的两台上游必须一台成功、一台 error: {statuses:?}"
    );
    assert!(
        statuses
            .iter()
            .filter(|(s, _)| *s == "error")
            .all(|(_, message)| message.contains("重名")),
        "冲突必须显式说明原因: {statuses:?}"
    );

    assert_eq!(
        names.iter().filter(|n| *n == "ab___b").count(),
        1,
        "同一暴露名只能出现一次: {names:?}"
    );

    // 同一上游内规范化后重名（空格与 ? 都变 -）→ 整台拒绝。
    let (dup_url, _dup) = spawn_fake_named(&["dup name", "dup?name"]).await;
    assert_eq!(
        create_upstream(&h, upstream_body("dup", &dup_url)).await.0,
        StatusCode::OK
    );
    let (status, refreshed) = call(
        &h.app,
        "POST",
        "/admin/upstreams/dup/refresh",
        Some(ALICE_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(refreshed["status"], json!("error"));
    assert!(
        refreshed["status_message"]
            .as_str()
            .is_some_and(|m| m.contains("重名"))
    );
    assert!(
        !tool_names(&h.app, &token)
            .await
            .iter()
            .any(|n| n.starts_with("dup__"))
    );

    // 超长暴露名：拒绝接入，不截断（截断会让旧名回退拿错名字调上游）。
    let long_tool = "t".repeat(60);
    let (long_url, _long) = spawn_fake_named(&[long_tool.as_str()]).await;
    assert_eq!(
        create_upstream(&h, upstream_body("long", &long_url))
            .await
            .0,
        StatusCode::OK
    );
    let (status, refreshed) = call(
        &h.app,
        "POST",
        "/admin/upstreams/long/refresh",
        Some(ALICE_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(refreshed["status"], json!("error"));
    assert!(
        refreshed["status_message"]
            .as_str()
            .is_some_and(|m| m.contains("64") && m.contains("不合规")),
        "{refreshed}"
    );
    assert_eq!(refreshed["tool_count"], json!(0));

    // 保留 id 与非法 id（含连续 `__`）：注册期就拒绝。
    let (url, _fake) = spawn_fake(false).await;
    for reserved in ["admin", "builtin", "basic", "analysis", "jumcp"] {
        let (status, error) = create_upstream(&h, upstream_body(reserved, &url)).await;
        assert_eq!(status, StatusCode::CONFLICT, "id={reserved}");
        assert_eq!(error["error"]["code"], json!("MCP_UPSTREAM_CONFLICT"));
    }
    for invalid in ["Bad_Id", "a__b", "x"] {
        let (status, error) = create_upstream(&h, upstream_body(invalid, &url)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "id={invalid}");
        assert_eq!(error["error"]["code"], json!("MCP_UPSTREAM_INVALID"));
        assert_eq!(error["error"]["data"]["field"], json!("id"));
    }
}

#[tokio::test]
async fn concurrent_first_fetch_never_exposes_duplicate_names() {
    let h = harness();
    // 两台上游在第一次并行拉取时会撞名（`a`+`_b` 与 `a_`+`b` → `a___b`）。
    let (first_url, _first) = spawn_fake_named(&["_b"]).await;
    let (second_url, _second) = spawn_fake_named(&["b"]).await;
    assert_eq!(
        create_upstream(&h, upstream_body("ab", &first_url)).await.0,
        StatusCode::OK
    );
    assert_eq!(
        create_upstream(&h, upstream_body("ab_", &second_url))
            .await
            .0,
        StatusCode::OK
    );
    let token = issue_token(
        &h,
        "agent-concurrent",
        "analyst",
        &["db.read.analytics"],
        &["ab___b"],
    )
    .await;

    // 一次 tools/list：projection 用 JoinSet 并行首拉两台上游（竞争同一暴露名）。
    let names = tool_names(&h.app, &token).await;
    assert_eq!(
        names.iter().filter(|n| *n == "ab___b").count(),
        1,
        "{names:?}"
    );

    let (_, list) = call(&h.app, "GET", "/admin/upstreams", Some(ALICE_TOKEN), None).await;
    let items = list["items"].as_array().expect("items");
    let status_of = |id: &str| {
        items.iter().find(|i| i["id"] == json!(id)).expect("item")["status"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    };
    let statuses = [status_of("ab"), status_of("ab_")];
    assert!(
        statuses.iter().any(|s| s == "error"),
        "并发撞名必须有一台显式失败: {statuses:?}"
    );
    assert!(
        statuses.iter().any(|s| s == "ok"),
        "另一台必须正常: {statuses:?}"
    );
}
