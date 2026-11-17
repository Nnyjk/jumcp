use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::fixtures::*;

/// 重新签发：旧明文 401 / 新明文可用、配置元数据保留、审计 token_reissue 且不含明文/hash。
#[tokio::test]
async fn reissue_rotates_plaintext_and_preserves_configuration() {
    let h = harness();
    publish_capability(&h, "cap.core", "db.read.core");
    let (status, created) = create_token(&h.app, base_token_body()).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let old = created["token"].as_str().unwrap().to_string();
    let id = created["item"]["id"].as_str().unwrap().to_string();
    let old_record = h.store.get_access_token(&id).unwrap().unwrap();
    assert!(old_record.id.starts_with("tok_"));
    // 先使用一次：重签发必须清空 last_used_at。
    assert!(h.store.authenticate_access_token(&old).unwrap().is_some());

    let (status, reissued) = call(
        &h.app,
        "POST",
        &format!("/admin/tokens/{id}/reissue"),
        Some(ALICE_TOKEN),
        &[("x-admin-actor", "alice")],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reissued}");
    let new = reissued["token"].as_str().unwrap().to_string();
    assert_ne!(new, old, "重签发必须换明文");
    assert!(new.starts_with("jumcp_"), "{new}");
    let item = &reissued["item"];
    assert_eq!(item["id"], json!(id), "同一记录原地换钥匙");
    assert_eq!(item["name"], created["item"]["name"]);
    assert_eq!(item["role"], json!("analyst"));
    assert_eq!(item["groups"], json!(["db.read.core"]));
    assert_eq!(item["builtin_tools"], base_token_body()["builtin_tools"]);
    assert_eq!(item["source"], json!("agent"));
    assert_eq!(item["caller"], json!("agent-analyst"));
    assert_eq!(item["expires_at"], Value::Null);
    assert_eq!(item["created_at"], created["item"]["created_at"]);
    assert_eq!(item["last_used_at"], Value::Null, "重签发清空最近使用");
    assert_eq!(item["token_prefix"], json!(new[..12]));
    assert_ne!(item["token_prefix"], created["item"]["token_prefix"]);
    for forbidden in ["token_hash", "hash", "token"] {
        assert!(
            item.get(forbidden).is_none(),
            "item 不得返回 {forbidden}: {item}"
        );
    }
    assert!(!item.to_string().contains(&new), "item 不得包含明文");

    // 旧明文立即失效，新明文立即可用。
    let denied = mcp(&h.app, Some(&old), rpc("tools/list", json!({})), &[]).await;
    assert_eq!(
        denied["error"]["data"]["mcp_code"],
        json!("MCP_UNAUTHORIZED"),
        "{denied}"
    );
    let names = tool_names(&h.app, &new).await;
    assert!(names.contains(&"cap.core".to_string()), "{names:?}");

    let new_record = h.store.get_access_token(&id).unwrap().unwrap();
    assert_ne!(new_record.token_hash, old_record.token_hash);
    assert_eq!(h.store.access_token_total().unwrap(), 1, "重签发不新增行");

    let events = h
        .audit
        .query(Some(&json!({"event_type": "admin_change"})))
        .expect("query");
    let reissue = events
        .iter()
        .find(|e| e.payload["action"] == json!("token_reissue"))
        .expect("token_reissue 审计");
    assert_eq!(reissue.actor["id"], json!("alice"));
    assert_eq!(reissue.actor["source"], json!("admin-api"));
    assert_eq!(reissue.payload["object_type"], json!("token"));
    assert_eq!(reissue.payload["object_id"], json!(id));
    assert_eq!(reissue.payload["outcome"], json!("ok"));
    assert_eq!(reissue.payload["data"]["role"], json!("analyst"));
    assert_eq!(
        reissue.payload["data"]["token_prefix"], item["token_prefix"],
        "审计只写新前缀"
    );
    let raw = reissue.payload.to_string();
    for secret in [old.as_str(), new.as_str(), new_record.token_hash.as_str()] {
        assert!(!raw.contains(secret), "审计载荷不得包含 {secret}: {raw}");
    }
}

/// 非 active：撤销/过期重签发 409 且哈希/前缀不变、不写 token_reissue；未知 id 404。
#[tokio::test]
async fn reissue_rejects_non_active_without_changes() {
    let h = harness();
    let (status, created) = create_token(&h.app, base_token_body()).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["item"]["id"].as_str().unwrap().to_string();

    let (status, revoked) = call(
        &h.app,
        "POST",
        &format!("/admin/tokens/{id}/revoke"),
        Some(ALICE_TOKEN),
        &[("x-admin-actor", "alice")],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{revoked}");
    let before = h.store.get_access_token(&id).unwrap().unwrap();

    let (status, rejected) = call(
        &h.app,
        "POST",
        &format!("/admin/tokens/{id}/reissue"),
        Some(ALICE_TOKEN),
        &[("x-admin-actor", "alice")],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
    assert_eq!(
        rejected["error"]["code"],
        json!("MCP_STATE_TRANSITION_INVALID")
    );
    assert_eq!(rejected["error"]["data"]["token_id"], json!(id));
    assert_eq!(rejected["error"]["data"]["status"], json!("revoked"));
    let after = h.store.get_access_token(&id).unwrap().unwrap();
    assert_eq!(after.token_hash, before.token_hash, "拒绝路径不得换哈希");
    assert_eq!(
        after.token_prefix, before.token_prefix,
        "拒绝路径不得换前缀"
    );

    // 过期令牌同样 409（过期时间由后端按读取时刻派生）。
    let mut expiring = base_token_body();
    expiring["name"] = json!("即将过期");
    expiring["expires_at"] =
        json!((chrono::Utc::now() + chrono::Duration::milliseconds(200)).to_rfc3339());
    let (status, created) = create_token(&h.app, expiring).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let expired_id = created["item"]["id"].as_str().unwrap().to_string();
    tokio::time::sleep(std::time::Duration::from_millis(350)).await;
    let expired_before = h.store.get_access_token(&expired_id).unwrap().unwrap();
    let (status, rejected) = call(
        &h.app,
        "POST",
        &format!("/admin/tokens/{expired_id}/reissue"),
        Some(ALICE_TOKEN),
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
    assert_eq!(rejected["error"]["data"]["status"], json!("expired"));
    let expired_after = h.store.get_access_token(&expired_id).unwrap().unwrap();
    assert_eq!(expired_after.token_hash, expired_before.token_hash);

    let (status, missing) = call(
        &h.app,
        "POST",
        "/admin/tokens/tok_missing/reissue",
        Some(ALICE_TOKEN),
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{missing}");
    assert_eq!(missing["error"]["code"], json!("MCP_TOKEN_NOT_FOUND"));

    let events = h
        .audit
        .query(Some(&json!({"event_type": "admin_change"})))
        .expect("query");
    assert_eq!(
        events
            .iter()
            .filter(|e| e.payload["action"] == json!("token_reissue"))
            .count(),
        0,
        "拒绝路径不得写 token_reissue 审计"
    );
}
