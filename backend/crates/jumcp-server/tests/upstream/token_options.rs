//! 令牌授予面：`/admin/tokens/options` 暴露上游与其工具名，allowlist 支持通配。
//!
//! 关键场景是"先建令牌、后接上游"：令牌创建后只能重签，因此 `<server_id>__*`
//! 必须能覆盖之后才出现的工具名，否则安装的上游对这个身份永远不可见。

use crate::fixtures::*;
use crate::support::fake_upstream::spawn_fake_named;
use axum::http::StatusCode;
use serde_json::{Value, json};

/// 目录与工具清单只读缓存：`refresh` 之后 options 才列出暴露名，未 refresh 时 tool_count=0。
#[tokio::test]
async fn options_lists_upstreams_wildcards_and_cached_tools() {
    let h = harness();
    let (url, _fake) = spawn_fake_named(&["echo", "weird-name"]).await;
    let (status, item) = create_upstream(&h, upstream_body("analytics", &url)).await;
    assert_eq!(status, StatusCode::OK, "{item}");

    let before = options(&h).await;
    let upstream = find_upstream(&before, "analytics");
    assert_eq!(upstream["tool_count"], json!(0), "未 refresh 时不触发探测");
    assert_eq!(upstream["wildcard"], json!("analytics__*"));
    assert!(
        before["upstream_tools"]
            .as_array()
            .expect("tools")
            .is_empty(),
        "未 refresh 时没有可授予的工具名: {before}"
    );

    let (status, body) = call(
        &h.app,
        "POST",
        "/admin/upstreams/analytics/refresh",
        Some(ALICE_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let after = options(&h).await;
    let upstream = find_upstream(&after, "analytics");
    assert_eq!(upstream["name"], json!("上游 analytics"));
    assert_eq!(upstream["group"], json!("db.read.analytics"));
    assert_eq!(upstream["enabled"], json!(true));
    assert_eq!(upstream["tool_count"], json!(2));
    assert_eq!(
        after["tool_wildcards"],
        json!([{"value": "*", "label": "全部工具（含后续安装的上游）"}])
    );

    let tools = after["upstream_tools"].as_array().expect("upstream_tools");
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert_eq!(names, vec!["analytics__echo", "analytics__weird-name"]);
    let echo = &tools[0];
    assert_eq!(echo["upstream_id"], json!("analytics"));
    assert_eq!(echo["tool"], json!("echo"));
    assert_eq!(echo["group"], json!("db.read.analytics"));
    assert_eq!(echo["label"], json!("上游 analytics / echo"));
}

/// 通配授权在工具出现之前就成立：令牌先签发，上游后接入，tools/list 仍能看到并调用。
#[tokio::test]
async fn wildcard_grant_covers_upstreams_installed_later() {
    let h = harness();
    let token = issue_token(
        &h,
        "agent-wildcard",
        "analyst",
        &["db.read.analytics"],
        &["search_capabilities", "analytics__*"],
    )
    .await;
    assert!(
        !tool_names(&h.app, &token)
            .await
            .iter()
            .any(|n| n.starts_with("analytics__")),
        "上游还没接入时不该凭空出现工具"
    );

    let (url, _fake) = spawn_fake_named(&["echo"]).await;
    let (status, item) = create_upstream(&h, upstream_body("analytics", &url)).await;
    assert_eq!(status, StatusCode::OK, "{item}");
    let (status, body) = call(
        &h.app,
        "POST",
        "/admin/upstreams/analytics/refresh",
        Some(ALICE_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let names = tool_names(&h.app, &token).await;
    assert!(
        names.iter().any(|n| n == "analytics__echo"),
        "通配授权应覆盖后接入的上游工具: {names:?}"
    );
    let resp = call_tool(&h.app, &token, "analytics__echo", json!({"text": "hi"})).await;
    assert!(
        resp.get("error").is_none(),
        "授权通过后应能直接调用: {resp}"
    );
}

/// 环境引导令牌（`JUMCP_AGENT_TOKENS` 导入）默认 `*`：上游后接入也无须重签即可见。
#[tokio::test]
async fn bootstrap_token_reaches_upstream_installed_later() {
    use jumcp_core::config::AgentTokens;

    let h = harness();
    let boot = "tok_boot_000000000000";
    let tokens = AgentTokens::parse(&format!("agent-boot:analyst:{boot}")).expect("agent tokens");
    h.store
        .import_bootstrap_tokens(tokens.principals(), &settings(false).policy)
        .expect("bootstrap import");
    assert!(
        !tool_names(&h.app, boot)
            .await
            .iter()
            .any(|n| n.starts_with("analytics__")),
        "接入前没有上游工具"
    );

    let (url, _fake) = spawn_fake_named(&["echo"]).await;
    let (status, item) = create_upstream(&h, upstream_body("analytics", &url)).await;
    assert_eq!(status, StatusCode::OK, "{item}");
    let (status, body) = call(
        &h.app,
        "POST",
        "/admin/upstreams/analytics/refresh",
        Some(ALICE_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let names = tool_names(&h.app, boot).await;
    assert!(
        names.iter().any(|n| n == "analytics__echo"),
        "引导令牌应看到安装的上游工具: {names:?}"
    );
}

/// 授予面与 `tools/list` 的可见性必须一致：覆盖到 basic 但已不合格的工具
/// （上游改成 admin 类别）在公共通道里看不到，`/admin/tokens/options` 也不能再列出它，
/// 否则管理员勾选后得到"授权了却调不到"。
#[tokio::test]
async fn options_hides_basic_overrides_that_tools_list_drops() {
    let h = harness_with(false);

    // 合法覆盖：非 admin 类别、名字合规 → 生效组就是 basic。
    let (url, _fake) = spawn_fake_named(&["echo", "plain"]).await;
    let (status, item) = create_upstream(&h, upstream_body("stable", &url)).await;
    assert_eq!(status, StatusCode::OK, "{item}");
    refresh_upstream(&h, "stable").await;
    let (status, body) = put_basic_tools(&h, "stable", json!({"echo": true})).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // 覆盖之后上游改成 admin 类别：覆盖行保留，但不再能作为公共通道成员。
    let (url, _fake) = spawn_fake_named(&["echo"]).await;
    let (status, item) = create_upstream(&h, upstream_body("turn", &url)).await;
    assert_eq!(status, StatusCode::OK, "{item}");
    refresh_upstream(&h, "turn").await;
    let (status, body) = put_basic_tools(&h, "turn", json!({"echo": true})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let mut body = upstream_body("turn", &url);
    body["category"] = json!("admin");
    let (status, resp) = call(
        &h.app,
        "PUT",
        "/admin/upstreams/turn",
        Some(ALICE_TOKEN),
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    refresh_upstream(&h, "turn").await;

    let body = options(&h).await;
    let tools = body["upstream_tools"].as_array().expect("upstream_tools");
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(
        !names.contains(&"turn__echo"),
        "失效的 basic 覆盖不得出现在授予面: {names:?}"
    );
    let stable_echo = tools
        .iter()
        .find(|t| t["name"] == json!("stable__echo"))
        .unwrap_or_else(|| panic!("合法覆盖必须仍可授予: {body}"));
    assert_eq!(stable_echo["group"], json!("basic"));
    let stable_plain = tools
        .iter()
        .find(|t| t["name"] == json!("stable__plain"))
        .unwrap_or_else(|| panic!("未覆盖工具必须保留: {body}"));
    assert_eq!(stable_plain["group"], json!("db.read.analytics"));

    // 只含 db.read.core 的令牌：组外工具全被门控，能看见的上游工具只剩 basic 成员。
    // 这正是 options 的"授予面"要表达的集合——两边必须一致（勾选即所得）。
    let token = issue_token(
        &h,
        "agent-options",
        "core-only",
        &["db.read.core"],
        &["stable__*", "turn__*"],
    )
    .await;
    let mut visible = tool_names(&h.app, &token).await;
    visible.sort();
    let mut grantable: Vec<String> = tools
        .iter()
        .filter(|t| t["group"] == json!("basic"))
        .filter_map(|t| t["name"].as_str().map(String::from))
        .collect();
    grantable.sort();
    assert_eq!(visible, vec!["stable__echo"], "只有合法 basic 成员可见");
    assert_eq!(
        visible, grantable,
        "options 的 basic 授予面与 tools/list 必须一致"
    );

    // 失效覆盖只影响公共通道：上游组在 scope 内时，该工具照旧按上游组可见；
    // 未覆盖的 stable__plain 也照旧随上游组出现。
    let analyst = issue_token(
        &h,
        "agent-scope",
        "analyst",
        &["db.read.analytics"],
        &["stable__plain", "turn__echo"],
    )
    .await;
    let scoped = tool_names(&h.app, &analyst).await;
    assert!(scoped.contains(&"stable__plain".to_string()), "{scoped:?}");
    assert!(scoped.contains(&"turn__echo".to_string()), "{scoped:?}");
}

async fn refresh_upstream(h: &Harness, id: &str) {
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

async fn options(h: &Harness) -> Value {
    let (status, body) = call(
        &h.app,
        "GET",
        "/admin/tokens/options",
        Some(ALICE_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

fn find_upstream(body: &Value, id: &str) -> Value {
    body["upstreams"]
        .as_array()
        .expect("upstreams")
        .iter()
        .find(|u| u["id"] == json!(id))
        .unwrap_or_else(|| panic!("缺少上游 {id}: {body}"))
        .clone()
}
