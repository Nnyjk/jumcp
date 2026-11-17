use axum::http::StatusCode;
use jumcp_core::audit::Envelope;
use jumcp_core::registry::{ADMIN_CAPABILITY_IDS, BUILTIN_CAPABILITY_IDS};
use serde_json::{Value, json};

use crate::fixtures::*;

#[tokio::test]
async fn create_list_and_audit_token_without_secrets() {
    let h = harness();
    publish_capability(&h, "cap.core", "db.read.core");
    let (status, created) = create_token(&h.app, base_token_body()).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let plaintext = created["token"].as_str().expect("token 明文").to_string();
    assert!(plaintext.starts_with("jumcp_"), "{plaintext}");
    assert_eq!(plaintext.len(), 6 + 64);
    let item = &created["item"];
    assert!(item["id"].as_str().unwrap().starts_with("tok_"));
    assert_eq!(item["status"], json!("active"));
    assert_eq!(item["groups"], json!(["db.read.core"]));
    assert_eq!(item["builtin_tools"], base_token_body()["builtin_tools"]);
    assert_eq!(item["caller"], json!("agent-analyst"));
    assert_eq!(item["expires_at"], Value::Null);
    for forbidden in ["token_hash", "hash", "token"] {
        assert!(
            item.get(forbidden).is_none(),
            "item 不得返回 {forbidden}: {item}"
        );
    }
    assert!(!item.to_string().contains(&plaintext), "item 不得包含明文");

    let (status, listed) = call(&h.app, "GET", "/admin/tokens", Some(ALICE_TOKEN), &[], None).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert_eq!(listed["total"], json!(1));
    assert_eq!(listed["items"][0]["id"], item["id"]);
    assert!(!listed.to_string().contains(&plaintext), "列表不得包含明文");

    // admin_change：来源 admin-api、动作/token 前缀齐全，载荷无明文/hash。
    let events = h
        .audit
        .query(Some(&json!({"event_type": "admin_change"})))
        .expect("query");
    let event = events
        .iter()
        .find(|e| e.payload["action"] == json!("token_create"))
        .expect("token_create 审计");
    assert_eq!(event.actor["id"], json!("alice"));
    assert_eq!(event.actor["source"], json!("admin-api"));
    assert_eq!(event.payload["object_type"], json!("token"));
    assert_eq!(event.payload["data"]["role"], json!("analyst"));
    assert_eq!(
        event.payload["data"]["token_prefix"], item["token_prefix"],
        "审计只写前缀"
    );
    assert!(!event.payload.to_string().contains(&plaintext));

    // 明文 token 可用于 /mcp：tools/list 只输出 allowlist 内的内置工具 + core 组能力。
    let names = tool_names(&h.app, &plaintext).await;
    assert!(
        names.contains(&"search_capabilities".to_string()),
        "{names:?}"
    );
    assert!(names.contains(&"cap.core".to_string()), "{names:?}");
    assert!(
        !names.contains(&"list_groups".to_string()),
        "allowlist 未配置的 list_groups 不得出现: {names:?}"
    );
    for id in ADMIN_CAPABILITY_IDS {
        assert!(
            !names.contains(&id.to_string()),
            "admin 工具不得出现: {names:?}"
        );
    }
    // 调用未授权的内置工具：与不可见同构 + policy_decision。
    let denied = mcp(
        &h.app,
        Some(&plaintext),
        rpc(
            "tools/call",
            json!({"name": "list_groups", "arguments": {}}),
        ),
        &[],
    )
    .await;
    assert_eq!(err_code(&denied), "MCP_CAPABILITY_NOT_FOUND", "{denied}");
    let decisions = h
        .audit
        .query(Some(&json!({"event_type": "policy_decision"})))
        .expect("query");
    assert!(
        decisions.iter().any(|e| {
            e.actor["token_id"].as_str().is_some()
                && e.actor["source"] == json!("agent")
                && e.payload["error_code"] == json!("MCP_CAPABILITY_NOT_FOUND")
        }),
        "拒绝必须带 token_id/source: {:?}",
        decisions.iter().map(|e| &e.actor).collect::<Vec<_>>()
    );
}

/// 撤销：200 直接返回 item；撤销后 /mcp 拒绝；重复撤销 409 且不重复写审计。
#[tokio::test]
async fn revoke_blocks_mcp_and_is_idempotent_conflict() {
    let h = harness();
    let (_, created) = create_token(&h.app, base_token_body()).await;
    let plaintext = created["token"].as_str().unwrap().to_string();
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
    assert_eq!(revoked["id"], json!(id), "撤销响应直接是 AccessTokenItem");
    assert!(revoked.get("item").is_none(), "不得再包一层 item");
    assert_eq!(revoked["status"], json!("revoked"));
    assert!(revoked["revoked_at"].as_str().is_some());

    let denied = mcp(&h.app, Some(&plaintext), rpc("tools/list", json!({})), &[]).await;
    assert_eq!(
        denied["error"]["data"]["mcp_code"],
        json!("MCP_UNAUTHORIZED")
    );

    let (status, again) = call(
        &h.app,
        "POST",
        &format!("/admin/tokens/{id}/revoke"),
        Some(ALICE_TOKEN),
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{again}");
    assert_eq!(again["error"]["code"], json!("MCP_TOKEN_ALREADY_REVOKED"));

    let events = h
        .audit
        .query(Some(&json!({"event_type": "admin_change"})))
        .expect("query");
    assert_eq!(
        events
            .iter()
            .filter(|e| e.payload["action"] == json!("token_revoke"))
            .count(),
        1,
        "重复撤销不得重复写审计"
    );

    let (status, missing) = call(
        &h.app,
        "POST",
        "/admin/tokens/tok_missing/revoke",
        Some(ALICE_TOKEN),
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{missing}");
}

/// 校验：未知角色/组、非法 caller/source、非法/过去 expiry、admin 工具要求 admin 组。
#[tokio::test]
async fn create_rejects_invalid_configuration() {
    let h = harness();
    let cases: Vec<(&str, Value)> = vec![
        ("name", json!({"name": ""})),
        ("role", json!({"role": "ghost"})),
        ("groups", json!({"groups": ["db.read.unknown"]})),
        ("groups", json!({"groups": []})),
        ("caller", json!({"caller": "BAD CALLER"})),
        ("source", json!({"source": "Agent"})),
        ("expires_at", json!({"expires_at": "not-a-time"})),
        ("expires_at", json!({"expires_at": "2020-01-01T00:00:00Z"})),
        (
            "builtin_tools",
            json!({"builtin_tools": ["admin_review_queue"]}),
        ),
        ("builtin_tools", json!({"builtin_tools": ["nope"]})),
    ];
    for (field, patch) in cases {
        let mut body = base_token_body();
        for (key, value) in patch.as_object().unwrap() {
            body[key] = value.clone();
        }
        let (status, resp) = create_token(&h.app, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{field}: {resp}");
        assert_eq!(
            resp["error"]["code"],
            json!("MCP_VALIDATION_ERROR"),
            "{field}: {resp}"
        );
    }
    assert_eq!(h.store.access_token_total().unwrap(), 0, "非法请求不得落库");
}

/// options：角色/组/内置工具的稳定形状（中文 label、token_count、builtin/admin 分组）。
#[tokio::test]
async fn options_lists_roles_groups_and_builtin_labels() {
    let h = harness();
    let (status, body) = call(
        &h.app,
        "GET",
        "/admin/tokens/options",
        Some(ALICE_TOKEN),
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let roles = body["roles"].as_array().expect("roles");
    // 角色按 code 稳定排序；name 是中文展示名，不是 code 的重复。
    let codes: Vec<&str> = roles.iter().filter_map(|r| r["code"].as_str()).collect();
    assert_eq!(codes, vec!["admin", "analyst"], "角色按 code 稳定排序");
    assert_eq!(roles[0]["name"], json!("管理员"));
    assert_eq!(roles[1]["name"], json!("数据分析"));
    assert_eq!(
        roles[1]["default_groups"],
        json!(["db.read.core", "db.read.analytics"]),
        "默认组仍是 code"
    );
    assert_eq!(roles[1]["default_budget"], json!(2000));

    let groups = body["groups"].as_array().expect("groups");
    let core = groups
        .iter()
        .find(|g| g["code"] == json!("db.read.core"))
        .expect("core 组");
    assert_eq!(core["name"], json!("核心数据只读"));
    assert_eq!(core["source"], json!("policy"));
    assert_eq!(core["token_count"], json!(0));

    let builtin = body["builtin_tools"].as_array().expect("builtin_tools");
    assert_eq!(builtin.len(), BUILTIN_CAPABILITY_IDS.len());
    let by_name = |id: &str| {
        builtin
            .iter()
            .find(|t| t["name"] == json!(id))
            .unwrap_or_else(|| panic!("缺少 {id}"))
    };
    assert_eq!(by_name("search_capabilities")["label"], json!("检索能力"));
    assert_eq!(by_name("search_capabilities")["group"], json!("basic"));
    assert_eq!(by_name("admin_set_visibility")["label"], json!("改可见性"));
    assert_eq!(by_name("admin_set_visibility")["group"], json!("admin"));

    // 该 harness 没有上游管理器：授予面三个字段退化为空数组，而不是缺字段。
    let wildcards = body["tool_wildcards"].as_array().expect("tool_wildcards");
    assert_eq!(wildcards.len(), 0);
    assert_eq!(body["upstreams"], json!([]));
    assert_eq!(body["upstream_tools"], json!([]));
}

/// active 令牌拒绝删除：409 带状态与提示，记录仍在列表，且不写 token_delete 审计。
#[tokio::test]
async fn delete_rejects_active_token() {
    let h = harness();
    let (status, created) = create_token(&h.app, base_token_body()).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["item"]["id"].as_str().unwrap().to_string();

    let (status, rejected) = call(
        &h.app,
        "DELETE",
        &format!("/admin/tokens/{id}"),
        Some(ALICE_TOKEN),
        &[("x-admin-actor", "alice")],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
    assert_eq!(
        rejected["error"]["code"],
        json!("MCP_STATE_TRANSITION_INVALID"),
        "{rejected}"
    );
    assert_eq!(rejected["error"]["data"]["token_id"], json!(id));
    assert_eq!(rejected["error"]["data"]["status"], json!("active"));
    assert_eq!(rejected["error"]["data"]["hint"], json!("先撤销令牌再删除"));

    let (status, listed) = call(&h.app, "GET", "/admin/tokens", Some(ALICE_TOKEN), &[], None).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert_eq!(listed["total"], json!(1), "active 令牌不得被删除");
    assert_eq!(listed["items"][0]["id"], json!(id));

    let events = h
        .audit
        .query(Some(&json!({"event_type": "admin_change"})))
        .expect("query");
    assert_eq!(
        events
            .iter()
            .filter(|e| e.payload["action"] == json!("token_delete"))
            .count(),
        0,
        "拒绝路径不得写 token_delete 审计"
    );
}

/// 撤销后删除：200 硬删除、列表回到基线、重复删除 404；审计一条且不含明文/hash。
#[tokio::test]
async fn delete_after_revoke_removes_row_and_is_not_idempotent() {
    let h = harness();
    let (status, baseline) =
        call(&h.app, "GET", "/admin/tokens", Some(ALICE_TOKEN), &[], None).await;
    assert_eq!(status, StatusCode::OK, "{baseline}");
    assert_eq!(baseline["total"], json!(0));

    let (status, created) = create_token(&h.app, base_token_body()).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let plaintext = created["token"].as_str().unwrap().to_string();
    let id = created["item"]["id"].as_str().unwrap().to_string();
    let prefix = created["item"]["token_prefix"]
        .as_str()
        .unwrap()
        .to_string();
    let hash = h
        .store
        .get_access_token(&id)
        .expect("get")
        .expect("record")
        .token_hash;
    assert!(!hash.is_empty());

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

    let (status, deleted) = call(
        &h.app,
        "DELETE",
        &format!("/admin/tokens/{id}"),
        Some(ALICE_TOKEN),
        &[("x-admin-actor", "alice")],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{deleted}");
    assert_eq!(deleted, json!({"deleted": true, "id": id}), "{deleted}");
    assert!(h.store.get_access_token(&id).expect("get").is_none());

    let (status, listed) = call(&h.app, "GET", "/admin/tokens", Some(ALICE_TOKEN), &[], None).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert_eq!(listed["total"], baseline["total"], "删除后总数回到基线");
    assert!(!listed.to_string().contains(&id), "{listed}");

    let (status, again) = call(
        &h.app,
        "DELETE",
        &format!("/admin/tokens/{id}"),
        Some(ALICE_TOKEN),
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{again}");
    assert_eq!(again["error"]["code"], json!("MCP_TOKEN_NOT_FOUND"));

    let events = h
        .audit
        .query(Some(&json!({"event_type": "admin_change"})))
        .expect("query");
    let deletes: Vec<&Envelope> = events
        .iter()
        .filter(|e| e.payload["action"] == json!("token_delete"))
        .collect();
    assert_eq!(deletes.len(), 1, "删除审计恰好一条");
    let event = deletes[0];
    assert_eq!(event.actor["id"], json!("alice"));
    assert_eq!(event.actor["source"], json!("admin-api"));
    assert_eq!(event.payload["object_type"], json!("token"));
    assert_eq!(event.payload["object_id"], json!(id));
    assert_eq!(event.payload["action"], json!("token_delete"));
    assert_eq!(event.payload["outcome"], json!("ok"));
    let data = &event.payload["data"];
    assert_eq!(data["name"], created["item"]["name"]);
    assert_eq!(data["role"], json!("analyst"));
    assert_eq!(data["groups"], json!(["db.read.core"]));
    assert_eq!(data["builtin_tools"], base_token_body()["builtin_tools"]);
    assert_eq!(data["source"], json!("agent"));
    assert_eq!(data["caller"], json!("agent-analyst"));
    assert_eq!(data["token_prefix"], json!(prefix));
    assert_eq!(data["status"], json!("revoked"));
    assert!(data["created_at"].as_str().is_some());
    assert_eq!(data["expires_at"], Value::Null);
    assert!(data["revoked_at"].as_str().is_some());
    let serialized = event.payload.to_string();
    for secret in ["token_hash", "hash", plaintext.as_str(), hash.as_str()] {
        assert!(
            !serialized.contains(secret),
            "审计载荷不得包含 {secret}: {serialized}"
        );
    }
}

/// 过期令牌无需撤销即可删除；快照里 revoked_at 输出 null。
#[tokio::test]
async fn delete_removes_expired_token() {
    let h = harness();
    let mut body = base_token_body();
    body["expires_at"] =
        json!((chrono::Utc::now() + chrono::Duration::milliseconds(300)).to_rfc3339());
    let (status, created) = create_token(&h.app, body).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["item"]["id"].as_str().unwrap().to_string();

    tokio::time::sleep(std::time::Duration::from_millis(450)).await;

    let (status, deleted) = call(
        &h.app,
        "DELETE",
        &format!("/admin/tokens/{id}"),
        Some(ALICE_TOKEN),
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{deleted}");
    assert_eq!(deleted["deleted"], json!(true));

    let (status, listed) = call(&h.app, "GET", "/admin/tokens", Some(ALICE_TOKEN), &[], None).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert_eq!(listed["total"], json!(0));

    let events = h
        .audit
        .query(Some(&json!({"event_type": "admin_change"})))
        .expect("query");
    let event = events
        .iter()
        .find(|e| e.payload["action"] == json!("token_delete"))
        .expect("token_delete 审计");
    assert_eq!(event.payload["data"]["status"], json!("expired"));
    assert_eq!(event.payload["data"]["revoked_at"], Value::Null);
    assert!(event.payload["data"]["expires_at"].as_str().is_some());
}

/// 删除让组引用计数自然回落：只建一个 token，删除后该组 token_count 为 0。
#[tokio::test]
async fn delete_drops_group_token_count() {
    let h = harness();
    let (status, created) = create_token(&h.app, base_token_body()).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["item"]["id"].as_str().unwrap().to_string();

    let count = |body: &Value| {
        body["items"]
            .as_array()
            .expect("items")
            .iter()
            .find(|g| g["code"] == json!("db.read.core"))
            .expect("core 组")["token_count"]
            .clone()
    };
    let (status, groups) = call(&h.app, "GET", "/admin/groups", Some(ALICE_TOKEN), &[], None).await;
    assert_eq!(status, StatusCode::OK, "{groups}");
    assert_eq!(count(&groups), json!(1));

    let (status, revoked) = call(
        &h.app,
        "POST",
        &format!("/admin/tokens/{id}/revoke"),
        Some(ALICE_TOKEN),
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{revoked}");
    let (status, deleted) = call(
        &h.app,
        "DELETE",
        &format!("/admin/tokens/{id}"),
        Some(ALICE_TOKEN),
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{deleted}");

    let (status, groups) = call(&h.app, "GET", "/admin/groups", Some(ALICE_TOKEN), &[], None).await;
    assert_eq!(status, StatusCode::OK, "{groups}");
    assert_eq!(count(&groups), json!(0), "删除后组引用归零");
}
