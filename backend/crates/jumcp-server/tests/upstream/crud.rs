use crate::support::fake_upstream::spawn_fake;
use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::fixtures::*;

#[tokio::test]
async fn upstream_crud_masks_secrets_and_preserves_them_on_put() {
    let h = harness();
    let (url, _fake) = spawn_fake(false).await;
    let body = json!({
        "id": "analytics",
        "name": "分析上游",
        "transport": "http",
        "url": url,
        "args": [],
        "env": {
            "API_TOKEN": "literal-secret",
            "PLAIN": "v1",
            "REF": "env:JUMCP_TEST_UPSTREAM_REF"
        },
        "headers": {
            "Authorization": "Bearer supersecsret",
            "X-Trace": "trace-1"
        },
        "category": "analysis",
        "enabled": true,
        "timeout_ms": 3000
    });
    let (status, created) = create_upstream(&h, body).await;
    assert_eq!(status, StatusCode::OK, "创建应成功: {created}");
    assert_eq!(created["id"], json!("analytics"));
    assert_eq!(created["status"], json!("unknown"), "尚未探测");
    assert_eq!(created["tool_count"], json!(0));
    assert_eq!(created["headers"]["Authorization"], json!("***"));
    assert_eq!(created["headers"]["X-Trace"], json!("trace-1"));
    assert_eq!(created["env"]["API_TOKEN"], json!("***"));
    assert_eq!(created["env"]["PLAIN"], json!("v1"));
    assert_eq!(created["env"]["REF"], json!("env:JUMCP_TEST_UPSTREAM_REF"));

    // 落库的是真实值，脱敏只发生在输出层。
    let stored = h.store.upstream("analytics").expect("store").expect("row");
    assert_eq!(stored.headers["Authorization"], "Bearer supersecsret");
    assert_eq!(stored.env["API_TOKEN"], "literal-secret");

    // GET 单项与列表都必须脱敏。
    let (status, item) = call(
        &h.app,
        "GET",
        "/admin/upstreams/analytics",
        Some(ALICE_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(item["headers"]["Authorization"], json!("***"));
    let (status, list) = call(&h.app, "GET", "/admin/upstreams", Some(ALICE_TOKEN), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["total"], json!(1));
    assert_eq!(list["items"][0]["env"]["API_TOKEN"], json!("***"));

    // PUT 里传 *** 必须保留旧值，不能把占位符写库。
    let put = json!({
        "name": "分析上游（改）",
        "transport": "http",
        "url": url,
        "args": [],
        "env": {"API_TOKEN": "***", "PLAIN": "v2", "REF": "***"},
        "headers": {"Authorization": "***", "X-Trace": "trace-2"},
        "category": "analysis",
        "enabled": true,
        "timeout_ms": 3000
    });
    let (status, updated) = call(
        &h.app,
        "PUT",
        "/admin/upstreams/analytics",
        Some(ALICE_TOKEN),
        Some(put),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "更新应成功: {updated}");
    assert_eq!(updated["name"], json!("分析上游（改）"));
    assert_eq!(updated["headers"]["Authorization"], json!("***"));
    assert_eq!(updated["headers"]["X-Trace"], json!("trace-2"));
    assert_eq!(updated["env"]["API_TOKEN"], json!("***"));
    assert_eq!(updated["env"]["PLAIN"], json!("v2"));
    assert_eq!(updated["env"]["REF"], json!("env:JUMCP_TEST_UPSTREAM_REF"));

    let stored = h.store.upstream("analytics").expect("store").expect("row");
    assert_eq!(
        stored.headers["Authorization"], "Bearer supersecsret",
        "*** 必须是保留语义，不能落库"
    );
    assert_eq!(stored.env["API_TOKEN"], "literal-secret");
    assert_eq!(stored.headers["X-Trace"], "trace-2");

    // 新建时用 *** 没有旧值可保留 → 400。
    let mut bad = upstream_body("second", "http://127.0.0.1:1/mcp");
    bad["headers"] = json!({"Authorization": "***"});
    let (status, error) = create_upstream(&h, bad).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"]["code"], json!("MCP_UPSTREAM_INVALID"));

    // 删除。
    let (status, deleted) = call(
        &h.app,
        "DELETE",
        "/admin/upstreams/analytics",
        Some(ALICE_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(deleted["deleted"], json!(true));
    let (status, _) = call(
        &h.app,
        "GET",
        "/admin/upstreams/analytics",
        Some(ALICE_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn upstream_writes_audit_admin_change_without_secret_values() {
    let h = harness();
    let (url, _fake) = spawn_fake(false).await;
    let mut body = upstream_body("audited", &url);
    body["headers"] = json!({"Authorization": "Bearer topsecret-token"});
    body["env"] = json!({"API_TOKEN": "another-secret"});
    let (status, _) = create_upstream(&h, body).await;
    assert_eq!(status, StatusCode::OK);

    let events = h
        .audit
        .query(Some(&json!({"event_type": "admin_change"})))
        .expect("audit query");
    let create = events
        .iter()
        .find(|e| e.payload["object_id"] == json!("audited"))
        .expect("create event");
    assert_eq!(create.payload["object_type"], json!("upstream"));
    assert_eq!(create.payload["action"], json!("create"));
    assert_eq!(create.payload["data"]["transport"], json!("http"));
    assert!(
        create.payload["data"].get("read_only").is_none(),
        "上游不再声明只读，审计 data 不得再出现 read_only"
    );
    let serialized = serde_json::to_string(&create.payload).expect("json");
    assert!(
        !serialized.contains("topsecret-token") && !serialized.contains("another-secret"),
        "审计不得包含上游密钥明文: {serialized}"
    );

    // 更新：只有真实变化的字段进 changes，密钥值同样不出现。
    let put = json!({
        "name": "改名",
        "transport": "http",
        "url": url,
        "category": "analysis",
        "headers": {"Authorization": "***"},
        "env": {"API_TOKEN": "***"}
    });
    let (status, _) = call(
        &h.app,
        "PUT",
        "/admin/upstreams/audited",
        Some(ALICE_TOKEN),
        Some(put),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let events = h
        .audit
        .query(Some(&json!({"event_type": "admin_change"})))
        .expect("audit query");
    let update = events
        .iter()
        .find(|e| {
            e.payload["object_id"] == json!("audited") && e.payload["action"] == json!("update")
        })
        .expect("update event");
    let changes = update.payload["changes"].as_array().expect("changes");
    let fields: Vec<&str> = changes.iter().filter_map(|c| c["field"].as_str()).collect();
    assert!(fields.contains(&"name"));
    assert!(
        fields.contains(&"category"),
        "category 改了就要出现在 changes"
    );
    assert!(
        !fields.contains(&"headers"),
        "headers 键值未变（*** 保留了旧值），不得写 changes"
    );
    let serialized = serde_json::to_string(&update.payload).expect("json");
    assert!(!serialized.contains("topsecret-token"));
}

#[tokio::test]
async fn switching_transport_clears_the_other_side_fields() {
    let h = harness_with(true);
    let (url, _fake) = spawn_fake(false).await;

    // stdio → http：PUT 仍带着旧 command（或 null），落库必须是空。
    let mut stdio = upstream_body("switch", "");
    stdio["transport"] = json!("stdio");
    stdio["url"] = Value::Null;
    stdio["command"] = json!("old-binary");
    stdio["args"] = json!(["--stdio"]);
    let (status, created) = create_upstream(&h, stdio).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["command"], json!("old-binary"));

    let (status, updated) = call(
        &h.app,
        "PUT",
        "/admin/upstreams/switch",
        Some(ALICE_TOKEN),
        Some(json!({
            "transport": "http",
            "url": url,
            "command": "old-binary",
            "category": "custom",
            "timeout_ms": 5000
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["command"], Value::Null, "http 不得保留 command");
    assert_eq!(updated["url"], json!(url));
    let stored = h.store.upstream("switch").expect("store").expect("row");
    assert_eq!(stored.command, "");

    // http → stdio：反过来同样清空 url。
    let (status, updated) = call(
        &h.app,
        "PUT",
        "/admin/upstreams/switch",
        Some(ALICE_TOKEN),
        Some(json!({
            "transport": "stdio",
            "command": "new-binary",
            "url": url,
            "category": "custom",
            "timeout_ms": 5000
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["url"], Value::Null, "stdio 不得保留 url");
    assert_eq!(updated["command"], json!("new-binary"));
    let stored = h.store.upstream("switch").expect("store").expect("row");
    assert_eq!(stored.url, "");
    assert_eq!(stored.command, "new-binary");
}

#[tokio::test]
async fn upstream_validation_rejects_bad_fields() {
    let h = harness();
    let (url, _fake) = spawn_fake(false).await;

    let mut bad = upstream_body("bad", &url);
    bad["timeout_ms"] = json!(0);
    let (status, error) = create_upstream(&h, bad).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"]["code"], json!("MCP_UPSTREAM_INVALID"));

    let mut bad = upstream_body("bad", &url);
    bad["category"] = json!("weird");
    let (status, error) = create_upstream(&h, bad).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"]["code"], json!("MCP_UPSTREAM_INVALID"));

    // http 缺 url / 带 userinfo → 400。
    let mut bad = upstream_body("bad", "");
    bad["url"] = Value::Null;
    assert_eq!(create_upstream(&h, bad).await.0, StatusCode::BAD_REQUEST);

    let mut userinfo = upstream_body("userinfo", "http://alice:secret@127.0.0.1:1/mcp");
    userinfo["name"] = json!("带凭据的 URL");
    let (status, error) = create_upstream(&h, userinfo).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"]["code"], json!("MCP_UPSTREAM_INVALID"));
    let serialized = serde_json::to_string(&error).expect("json");
    assert!(!serialized.contains("secret@"), "错误体不得回显 userinfo");

    // args 超过 64 项。
    let mut bad = upstream_body("bad", &url);
    bad["args"] = json!(vec!["x"; 65]);
    assert_eq!(create_upstream(&h, bad).await.0, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn env_refs_resolve_at_connect_and_missing_env_fails() {
    let h = harness();
    let (url, _fake) = spawn_fake(false).await;

    // PATH 一定存在：`env:PATH` 只用于证明引用会被解析（值本身不回显）。
    let mut ok_body = upstream_body("refs", &url);
    ok_body["headers"] = json!({"X-From-Env": "env:PATH"});
    assert_eq!(create_upstream(&h, ok_body).await.0, StatusCode::OK);
    let (status, refreshed) = call(
        &h.app,
        "POST",
        "/admin/upstreams/refs/refresh",
        Some(ALICE_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(refreshed["status"], json!("ok"), "{refreshed}");
    // GET 只回显引用本身，不回显解析后的值。
    assert_eq!(refreshed["headers"]["X-From-Env"], json!("env:PATH"));

    // 缺失的环境变量：建连显式失败，status_message 指出变量名。
    let mut missing = upstream_body("missing", &url);
    missing["env"] = json!({"API_TOKEN": "env:JUMCP_ROUTER_SURELY_MISSING_VAR"});
    assert_eq!(create_upstream(&h, missing).await.0, StatusCode::OK);
    let (status, refreshed) = call(
        &h.app,
        "POST",
        "/admin/upstreams/missing/refresh",
        Some(ALICE_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(refreshed["status"], json!("error"));
    assert!(
        refreshed["status_message"]
            .as_str()
            .is_some_and(|m| m.contains("JUMCP_ROUTER_SURELY_MISSING_VAR")),
        "{refreshed}"
    );
    assert_eq!(
        refreshed["env"]["API_TOKEN"],
        json!("env:JUMCP_ROUTER_SURELY_MISSING_VAR")
    );
}
