//! 组内两层暴露：第一层「组 → 上游」整台授权，第二层「组 → 单工具」include/exclude。
//!
//! 令牌只剩组，所以本文件的所有可见性断言都只经过组：同一台上游在两个组里可以有
//! 完全不同的工具集合，任意一个组放行即对 scope 可见。

use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::fixtures::*;
use crate::support::fake_upstream::spawn_fake_named;

async fn exposure(h: &Harness, group: &str) -> Value {
    let (status, body) = call(
        &h.app,
        "GET",
        &format!("/admin/groups/{group}/exposure"),
        Some(ALICE_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

fn tool_row<'a>(body: &'a Value, upstream: &str, tool: &str) -> &'a Value {
    body["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .find(|t| t["upstream_id"] == json!(upstream) && t["tool"] == json!(tool))
        .unwrap_or_else(|| panic!("{upstream}/{tool} 不在暴露视图里: {body}"))
}

fn upstream_row<'a>(body: &'a Value, id: &str) -> &'a Value {
    body["upstreams"]
        .as_array()
        .expect("upstreams")
        .iter()
        .find(|u| u["id"] == json!(id))
        .unwrap_or_else(|| panic!("{id} 不在暴露视图里: {body}"))
}

async fn refresh(h: &Harness, id: &str) {
    let (status, body) = call(
        &h.app,
        "POST",
        &format!("/admin/upstreams/{id}/refresh"),
        Some(ALICE_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn exposure_view_reports_both_levels() {
    let h = harness();
    let (url, _fake) = spawn_fake_named(&["echo", "explode"]).await;
    let (status, item) = create_upstream(&h, upstream_body("mixed", &url)).await;
    assert_eq!(status, StatusCode::OK, "{item}");

    // 未 refresh 时只有只读缓存：工具行数为 0，但第一层授予已经在。
    let before = exposure(&h, DEFAULT_GROUP).await;
    assert_eq!(upstream_row(&before, "mixed")["granted"], json!(true));
    assert_eq!(upstream_row(&before, "mixed")["tool_count"], json!(0));
    assert!(
        before["tools"].as_array().expect("tools").is_empty(),
        "未 refresh 不触网探测: {before}"
    );

    refresh(&h, "mixed").await;
    let after = exposure(&h, DEFAULT_GROUP).await;
    assert_eq!(upstream_row(&after, "mixed")["tool_count"], json!(2));
    let echo = tool_row(&after, "mixed", "echo");
    assert_eq!(echo["state"], json!("inherit"));
    assert_eq!(echo["visible"], json!(true));
    assert_eq!(echo["exposed"], json!("mixed__echo"));
    assert_eq!(echo["orphaned"], json!(false));

    // 另一个组看到同一台上游，但没授予 → 全部不可见。
    let other = exposure(&h, "db.read.core").await;
    assert_eq!(upstream_row(&other, "mixed")["granted"], json!(false));
    assert_eq!(tool_row(&other, "mixed", "echo")["visible"], json!(false));

    // 停用的上游仍要出现在视图里，否则它残留的规则没有入口清理。
    let mut disabled = upstream_body("paused", &url);
    disabled["enabled"] = json!(false);
    let (status, item) = create_upstream(&h, disabled).await;
    assert_eq!(status, StatusCode::OK, "{item}");
    let paused = exposure(&h, DEFAULT_GROUP).await;
    assert_eq!(upstream_row(&paused, "paused")["enabled"], json!(false));
}

#[tokio::test]
async fn tool_rules_override_inheritance_per_group() {
    let h = harness();
    let (url, _fake) = spawn_fake_named(&["echo", "explode"]).await;
    let (status, _) = create_upstream(&h, upstream_body("mixed", &url)).await;
    assert_eq!(status, StatusCode::OK);
    refresh(&h, "mixed").await;

    let analyst = issue_token(&h, "agent-analyst", "analyst", &["db.read.analytics"]).await;
    let core_only = issue_token(&h, "agent-core", "core-only", &["db.read.core"]).await;
    assert!(
        tool_names(&h.app, &core_only)
            .await
            .iter()
            .all(|n| !n.starts_with("mixed__")),
        "组内未授予的上游对另一个组完全不可见"
    );

    // 第二层 include：只把 echo 放进 db.read.core，explode 仍然不可见。
    let (status, body) = set_exposure(
        &h,
        "db.read.core",
        json!({}),
        json!({"mixed": {"echo": "include"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["changes"]["tools"], json!(1));
    let names = tool_names(&h.app, &core_only).await;
    assert!(names.contains(&"mixed__echo".to_string()), "{names:?}");
    assert!(!names.contains(&"mixed__explode".to_string()), "{names:?}");
    let listed = mcp(&h.app, &core_only, "tools/list", json!({})).await;
    let echo = listed["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .find(|t| t["name"] == json!("mixed__echo"))
        .expect("echo");
    assert_eq!(
        echo["group"],
        json!("db.read.core"),
        "生效组是放行它的那个组"
    );

    // 第二层 exclude：在已授予的组里把 explode 摘掉，echo 继续继承。
    let (status, body) = set_exposure(
        &h,
        "db.read.analytics",
        json!({}),
        json!({"mixed": {"explode": "exclude"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let names = tool_names(&h.app, &analyst).await;
    assert!(names.contains(&"mixed__echo".to_string()), "{names:?}");
    assert!(!names.contains(&"mixed__explode".to_string()), "{names:?}");
    let denied = call_tool(&h.app, &analyst, "mixed__explode", json!({})).await;
    assert_eq!(
        denied["error"]["code"],
        json!(-32602),
        "被规则排除的调用名与不存在同构: {denied}"
    );

    // null 删规则：回到继承，explode 重新可见。
    let (status, body) = set_exposure(
        &h,
        "db.read.analytics",
        json!({}),
        json!({"mixed": {"explode": null}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        tool_names(&h.app, &analyst)
            .await
            .contains(&"mixed__explode".to_string())
    );
    let view = exposure(&h, "db.read.analytics").await;
    assert_eq!(
        tool_row(&view, "mixed", "explode")["state"],
        json!("inherit")
    );
}

#[tokio::test]
async fn grant_covers_tools_added_later() {
    let h = harness();
    let (url, fake) = spawn_fake_named(&["echo"]).await;
    let (status, _) = create_upstream(&h, upstream_body("mixed", &url)).await;
    assert_eq!(status, StatusCode::OK);
    refresh(&h, "mixed").await;
    let token = issue_token(&h, "agent-later", "analyst", &["db.read.analytics"]).await;
    assert!(
        tool_names(&h.app, &token)
            .await
            .contains(&"mixed__echo".to_string())
    );

    // 上游新增工具：第一层授予沿用，第二层没有规则 → 立刻可见，不需要再授权。
    fake.set_tools(&["echo", "later"]);
    refresh(&h, "mixed").await;
    assert!(
        tool_names(&h.app, &token)
            .await
            .contains(&"mixed__later".to_string())
    );
}

#[tokio::test]
async fn orphan_rules_are_listed_and_clearable() {
    let h = harness();
    let (url, fake) = spawn_fake_named(&["echo", "keep"]).await;
    let (status, _) = create_upstream(&h, upstream_body("mixed", &url)).await;
    assert_eq!(status, StatusCode::OK);
    refresh(&h, "mixed").await;
    let (status, body) = set_exposure(
        &h,
        DEFAULT_GROUP,
        json!({}),
        json!({"mixed": {"echo": "include"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // 上游把 echo 改名：规则行保留，视图标 orphaned，等待管理员清理。
    fake.set_tools(&["keep", "other"]);
    refresh(&h, "mixed").await;
    let view = exposure(&h, DEFAULT_GROUP).await;
    let orphan = tool_row(&view, "mixed", "echo");
    assert_eq!(orphan["orphaned"], json!(true));
    assert_eq!(orphan["state"], json!("include"));
    assert_eq!(tool_row(&view, "mixed", "keep")["orphaned"], json!(false));

    // 已不存在的工具只允许清规则（null）；再写 include/exclude 是未知工具。
    let (status, body) = set_exposure(
        &h,
        DEFAULT_GROUP,
        json!({}),
        json!({"mixed": {"echo": "include"}}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], json!("MCP_UPSTREAM_TOOL_UNKNOWN"));
    let (status, body) = set_exposure(
        &h,
        DEFAULT_GROUP,
        json!({}),
        json!({"mixed": {"echo": null}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let view = exposure(&h, DEFAULT_GROUP).await;
    assert_eq!(
        view["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .filter(|t| t["orphaned"] == json!(true))
            .count(),
        0
    );
}

#[tokio::test]
async fn rules_are_rejected_whole_when_any_entry_is_invalid() {
    let h = harness();
    let (url, _fake) = spawn_fake_named(&["echo", "explode"]).await;
    let (status, _) = create_upstream(&h, upstream_body("mixed", &url)).await;
    assert_eq!(status, StatusCode::OK);
    refresh(&h, "mixed").await;

    // 未知工具与已知工具混在同一请求：整体拒绝，已知那条也不落库。
    let (status, body) = set_exposure(
        &h,
        "db.read.core",
        json!({}),
        json!({"mixed": {"echo": "include", "nope": "include"}}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], json!("MCP_UPSTREAM_TOOL_UNKNOWN"));
    assert_eq!(body["error"]["data"]["unknown"], json!(["nope"]));
    let view = exposure(&h, "db.read.core").await;
    assert_eq!(
        view["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .filter(|t| t["state"] != json!("inherit"))
            .count(),
        0,
        "被拒绝的请求不得留下部分写入: {view}"
    );

    // state 只接受 include|exclude|null。
    let (status, body) = set_exposure(
        &h,
        "db.read.core",
        json!({}),
        json!({"mixed": {"echo": "yes"}}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], json!("MCP_VALIDATION_ERROR"));

    // 不存在的上游 / 不存在的组。
    let (status, body) = set_exposure(&h, "db.read.core", json!({"ghost": true}), json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], json!("MCP_UPSTREAM_NOT_FOUND"));
    let (status, body) = set_exposure(&h, "no.such.group", json!({}), json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], json!("MCP_GROUP_NOT_FOUND"));
}

#[tokio::test]
async fn maintenance_tools_only_enter_the_admin_group() {
    let h = harness();
    let (url, _fake) = spawn_fake_named(&["echo"]).await;
    let mut body = upstream_body("ops", &url);
    body["category"] = json!("admin");
    let (status, item) = create_upstream(&h, body).await;
    assert_eq!(status, StatusCode::OK, "{item}");
    refresh(&h, "ops").await;

    // admin 类别上游不能整台授予普通组。
    let (status, body) = set_exposure(&h, "db.read.core", json!({"ops": true}), json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        body["error"]["code"],
        json!("MCP_UPSTREAM_TOOL_NOT_ELIGIBLE")
    );
    assert_eq!(
        body["error"]["data"]["reason"],
        json!("upstream_category_admin")
    );

    // `admin_` 前缀工具同理，即使上游类别是 custom。
    let (purl, _pfake) = spawn_fake_named(&["admin_purge"]).await;
    let (status, _) = create_upstream(&h, upstream_body("danger", &purl)).await;
    assert_eq!(status, StatusCode::OK);
    refresh(&h, "danger").await;
    let (status, body) = set_exposure(
        &h,
        "db.read.core",
        json!({}),
        json!({"danger": {"admin_purge": "include"}}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        body["error"]["data"]["reason"],
        json!("admin_prefix"),
        "拒绝原因要能让管理面直接说明: {body}"
    );

    // admin 组内合法：类别与前缀都不再是障碍。
    let (status, body) = set_exposure(
        &h,
        "admin",
        json!({"ops": true}),
        json!({"danger": {"admin_purge": "include"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let admin = issue_token(&h, "agent-ops", "admin", &["admin"]).await;
    let names = tool_names(&h.app, &admin).await;
    assert!(names.contains(&"ops__echo".to_string()), "{names:?}");
    assert!(
        names.contains(&"danger__admin_purge".to_string()),
        "{names:?}"
    );
}

/// 常驻元工具也归 `basic` 组：不勾 basic 的令牌看不到它们，勾了才看得见。
#[tokio::test]
async fn resident_tools_require_the_basic_group() {
    let h = harness();
    let (status, body) = call(
        &h.app,
        "POST",
        "/admin/tokens",
        Some(ALICE_TOKEN),
        Some(json!({
            "name": "core only",
            "role": "core-only",
            "groups": ["db.read.core"],
            "source": "agent",
            "caller": "agent-no-basic"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let token = body["token"].as_str().expect("token");
    let names = tool_names(&h.app, token).await;
    assert!(
        names.is_empty(),
        "没有 basic 组的令牌看不到常驻元工具: {names:?}"
    );
    let denied = call_tool(&h.app, token, "search_capabilities", json!({"query": ""})).await;
    assert_eq!(denied["result"]["isError"], json!(true), "{denied}");
    assert_eq!(
        denied["result"]["structuredContent"]["error"]["code"],
        json!("MCP_CAPABILITY_NOT_FOUND")
    );

    let with_basic = issue_token(&h, "agent-basic", "core-only", &["db.read.core"]).await;
    let names = tool_names(&h.app, &with_basic).await;
    for id in ["search_capabilities", "describe_capability", "list_groups"] {
        assert!(names.contains(&id.to_string()), "{id} 缺失: {names:?}");
    }
}

/// 组清单的计数要跟着暴露矩阵走：授予上游后按缓存工具数计入该组。
#[tokio::test]
async fn group_listing_counts_exposed_upstream_tools() {
    let h = harness();
    let before = call(&h.app, "GET", "/admin/groups", Some(ALICE_TOKEN), None).await;
    assert_eq!(before.0, StatusCode::OK, "{}", before.1);
    let count_of = |body: &Value, code: &str| -> Value {
        body["items"]
            .as_array()
            .expect("items")
            .iter()
            .find(|g| g["code"] == json!(code))
            .map(|g| g["capability_count"].clone())
            .unwrap_or(Value::Null)
    };
    assert_eq!(count_of(&before.1, "db.read.analytics"), json!(0));
    let basic = before
        .1
        .get("items")
        .and_then(|items| {
            items
                .as_array()
                .and_then(|rows| rows.iter().find(|g| g["code"] == json!("basic")))
        })
        .expect("basic 条目");
    assert_eq!(basic["grantable"], json!(true), "basic 已是普通可授权组");

    let (url, _fake) = spawn_fake_named(&["echo", "explode"]).await;
    let (status, _) = create_upstream(&h, upstream_body("mixed", &url)).await;
    assert_eq!(status, StatusCode::OK);
    refresh(&h, "mixed").await;
    let after = call(&h.app, "GET", "/admin/groups", Some(ALICE_TOKEN), None).await;
    assert_eq!(after.0, StatusCode::OK, "{}", after.1);
    assert_eq!(count_of(&after.1, "db.read.analytics"), json!(2));
}
