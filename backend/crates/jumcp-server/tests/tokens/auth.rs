use axum::http::StatusCode;
use serde_json::json;

use crate::fixtures::*;

/// 过期 token 一律拒绝：创建 1 秒后过期的令牌，等待到期后 /mcp 401 且列表状态为 expired。
#[tokio::test]
async fn expired_token_is_rejected_by_mcp() {
    let h = harness();
    let mut body = base_token_body();
    body["expires_at"] =
        json!((chrono::Utc::now() + chrono::Duration::milliseconds(300)).to_rfc3339());
    let (status, created) = create_token(&h.app, body).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let plaintext = created["token"].as_str().unwrap().to_string();
    assert_eq!(created["item"]["status"], json!("active"));

    tokio::time::sleep(std::time::Duration::from_millis(450)).await;
    let denied = mcp(&h.app, Some(&plaintext), rpc("tools/list", json!({})), &[]).await;
    assert_eq!(
        denied["error"]["data"]["mcp_code"],
        json!("MCP_UNAUTHORIZED")
    );

    let (status, listed) = call(&h.app, "GET", "/admin/tokens", Some(ALICE_TOKEN), &[], None).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert_eq!(listed["items"][0]["status"], json!("expired"));
}

/// 过期时间在响应里是 RFC3339 字符串；状态过期后为 expired。
#[tokio::test]
async fn future_expiry_is_stored_and_listed() {
    let h = harness();
    let mut body = base_token_body();
    let at = (chrono::Utc::now() + chrono::Duration::days(1)).to_rfc3339();
    body["expires_at"] = json!(at);
    let (status, created) = create_token(&h.app, body).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["item"]["expires_at"], json!(at));
    assert_eq!(created["item"]["status"], json!("active"));
}
