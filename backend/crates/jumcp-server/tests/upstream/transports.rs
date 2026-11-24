use crate::support::fake_upstream::{dead_url, spawn_fake};
use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::fixtures::*;

#[tokio::test]
async fn sse_upstream_is_supported() {
    let h = harness();
    let (url, _fake) = spawn_fake(true).await;
    let (status, item) = create_upstream(&h, upstream_body("sse", &url)).await;
    assert_eq!(status, StatusCode::OK, "{item}");
    let token = issue_token(
        &h,
        "agent-sse",
        "analyst",
        &["db.read.analytics"],
        &["sse__echo"],
    )
    .await;
    let names = tool_names(&h.app, &token).await;
    assert!(names.contains(&"sse__echo".to_string()), "{names:?}");
    let resp = call_tool(&h.app, &token, "sse__echo", json!({})).await;
    assert_eq!(resp["result"]["content"][0]["text"], json!("echo:echo"));
}

#[tokio::test]
async fn unreachable_upstream_is_isolated_and_reported() {
    let h = harness();
    let (good_url, _fake) = spawn_fake(false).await;
    assert_eq!(
        create_upstream(&h, upstream_body("good", &good_url))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        create_upstream(&h, upstream_body("dead", &dead_url().await))
            .await
            .0,
        StatusCode::OK
    );
    let token = issue_token(
        &h,
        "agent-isolation",
        "analyst",
        &["db.read.analytics"],
        &["good__echo", "dead__echo"],
    )
    .await;

    // tools/list 仍返回内置 + 可达上游；不可达上游只是没有工具。
    let names = tool_names(&h.app, &token).await;
    assert!(names.contains(&"good__echo".to_string()), "{names:?}");
    assert!(!names.iter().any(|n| n.starts_with("dead__")));

    // 状态：不可达上游降为 error（无可用工具）。
    let (status, list) = call(&h.app, "GET", "/admin/upstreams", Some(ALICE_TOKEN), None).await;
    assert_eq!(status, StatusCode::OK);
    let items = list["items"].as_array().expect("items");
    let dead = items
        .iter()
        .find(|i| i["id"] == json!("dead"))
        .expect("dead item");
    assert_eq!(dead["status"], json!("error"));
    assert!(
        dead["status_message"]
            .as_str()
            .is_some_and(|m| !m.is_empty())
    );

    // 直接调用不可达上游 → isError 结果（不是协议错误）。
    let resp = mcp(
        &h.app,
        &token,
        "tools/call",
        json!({"name":"dead__echo","arguments":{}}),
    )
    .await;
    assert_eq!(
        resp["result"]["isError"],
        json!(true),
        "不可达上游必须回 isError 结果: {resp}"
    );
}

#[tokio::test]
async fn stdio_upstreams_are_gated_by_deployment_switch() {
    let h = harness();
    let mut stdio = upstream_body("local", "");
    stdio["transport"] = json!("stdio");
    stdio["url"] = Value::Null;
    stdio["command"] = json!("definitely-not-a-real-binary");
    let (status, error) = create_upstream(&h, stdio.clone()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"]["code"], json!("MCP_UPSTREAM_STDIO_DISABLED"));

    // 开关打开后同样的请求可通过（stdio 仍会因为进程不存在而 error，但不是配置拒绝）。
    let h = harness_with(true);
    let (status, created) = create_upstream(&h, stdio).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["transport"], json!("stdio"));
    assert_eq!(created["url"], Value::Null);
    let (status, refreshed) = call(
        &h.app,
        "POST",
        "/admin/upstreams/local/refresh",
        Some(ALICE_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(refreshed["status"], json!("error"));
}
