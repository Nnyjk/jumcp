//! 内置 MCP 包：目录（Discover）→ 安装成 HTTP 上游 → 改配置 → 卸载。
//!
//! 这一组用例走真实 listener：安装产物指向本进程的 `/internal/mcp/...`，刷新时上游客户端
//! 必须能通过 TCP 打回来（`oneshot` 打不到 reqwest），因此 `BuiltinHarness` 先把 app 起在
//! `127.0.0.1:0` 上。admin 请求仍用 `oneshot`，两条路径共用同一个 Router。
//!
//! 断言重点：目录契约（HTTP、无二进制字段）、落库 env 的加密、响应脱敏、
//! 配置编辑的部分更新/密钥轮换、secret 必须传输密文（明文/换钥密文一律拒绝）、
//! 包管理字段不可手改、旧 stdio 安装的迁移。

use crate::support::{
    BuiltinHarness, Req, TEST_SECRET_KEY, TEST_TRANSPORT_KEY, call, call_with_headers,
    transport_cipher,
};
use axum::http::StatusCode;
use jumcp_core::registry::McpUpstream;
use jumcp_core::secret::{SecretKey, is_encrypted};
use jumcp_server::builtin::migrate_legacy_packages;
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn install_body() -> Value {
    json!({
        "group": "db.read.core",
        "config": {
            "url": "http://127.0.0.1:8123",
            "username": "default",
            // 浏览器行为：secret 字段先用传输密钥加密再提交。
            "password": transport_cipher("s3cret-pass"),
        }
    })
}

async fn install(harness: &BuiltinHarness, body: Value) -> (StatusCode, Value) {
    call(
        &harness.admin,
        Req::post("/admin/mcp/catalog/clickhouse-basic/install").body(body),
    )
    .await
}

async fn catalog(harness: &BuiltinHarness) -> Value {
    let (status, body) = call(&harness.admin, Req::get("/admin/mcp/catalog")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

fn installation<'a>(catalog: &'a Value, id: &str) -> &'a Value {
    catalog["items"][0]["installations"]
        .as_array()
        .expect("installations")
        .iter()
        .find(|item| item["upstream_id"] == id)
        .expect("installation")
}

#[tokio::test]
async fn catalog_describes_http_package_without_binary_state() {
    let harness = BuiltinHarness::start().await;
    let body = catalog(&harness).await;
    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 1, "只注册了 clickhouse-basic：{body}");
    let item = &items[0];
    assert_eq!(item["id"], "clickhouse-basic");
    assert_eq!(item["transport"], "http");
    assert_eq!(item["default_upstream_id"], "clickhouse");
    assert!(
        item["installations"]
            .as_array()
            .expect("installations")
            .is_empty()
    );

    // 只暴露前端能渲染的字段：二进制/stdio 开关属于已删除的进程模型。
    for gone in [
        "binary_present",
        "stdio_enabled",
        "requires_stdio",
        "command",
        "args",
        "status",
        "tool_count",
    ] {
        assert!(item.get(gone).is_none(), "目录不应再有 {gone}：{item}");
    }

    let fields = item["fields"].as_array().expect("fields");
    let keys: Vec<&str> = fields.iter().filter_map(|f| f["key"].as_str()).collect();
    assert_eq!(
        keys,
        vec!["url", "username", "password", "database", "allowed_schemas"]
    );
    let password = fields
        .iter()
        .find(|f| f["key"] == "password")
        .expect("password");
    assert_eq!(password["kind"], "password");
    assert_eq!(password["secret"], true);
    assert_eq!(password["required"], false);
    assert!(password.get("env").is_none(), "env 是服务端实现细节");
    let username = fields
        .iter()
        .find(|f| f["key"] == "username")
        .expect("username");
    assert_eq!(username["default"], "default");

    let tools = item["tools"].as_array().expect("tools");
    assert_eq!(tools.len(), 3);
    assert!(tools.iter().any(|t| t["name"] == "query"));
}

#[tokio::test]
async fn install_writes_http_upstream_with_encrypted_secret() {
    let harness = BuiltinHarness::start().await;
    let (status, body) = install(&harness, install_body()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let upstream = &body["upstream"];
    assert_eq!(upstream["transport"], "http");
    assert_eq!(upstream["catalog_id"], "clickhouse-basic");
    assert_eq!(upstream["read_only"], true);
    assert_eq!(
        upstream["url"],
        harness
            .endpoint
            .url_for("clickhouse-basic", "clickhouse")
            .as_str()
    );
    assert!(upstream.get("command").is_none() || upstream["command"].is_null());
    // 响应必须脱敏：env 里的密码是 ***，明密文都不得出现。
    let text = body.to_string();
    assert!(!text.contains("s3cret-pass"), "响应泄漏明文密码：{text}");
    assert!(!text.contains("enc:v1:"), "响应泄漏密文：{text}");

    let stored = harness
        .store
        .upstream("clickhouse")
        .expect("query")
        .expect("row");
    let sealed = stored
        .env
        .get("JUMCP_CLICKHOUSE_PASSWORD")
        .expect("password");
    assert!(is_encrypted(sealed), "{sealed}");
    let key = SecretKey::parse(TEST_SECRET_KEY).expect("key");
    assert_eq!(key.decrypt(sealed).expect("decrypt"), "s3cret-pass");
    assert_eq!(
        stored.env.get("JUMCP_CLICKHOUSE_URL").map(String::as_str),
        Some("http://127.0.0.1:8123")
    );
    assert_eq!(
        stored.env.get("JUMCP_CLICKHOUSE_USER").map(String::as_str),
        Some("default")
    );

    // 目录只回显非密字段 + secrets_set。
    let body = catalog(&harness).await;
    let row = installation(&body, "clickhouse");
    assert_eq!(row["transport"], "http");
    assert_eq!(row["config"]["url"], "http://127.0.0.1:8123");
    assert_eq!(row["config"]["username"], "default");
    assert!(row["config"].get("password").is_none());
    assert_eq!(row["secrets_set"]["password"], true);
    assert!(!row.to_string().contains("s3cret-pass"));
}

#[tokio::test]
async fn transport_key_endpoint_returns_key_and_no_store() {
    let harness = BuiltinHarness::start().await;
    let (status, body, headers) =
        call_with_headers(&harness.admin, Req::get("/admin/transport-key")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["algorithm"], "AES-256-GCM");
    assert_eq!(body["key"], TEST_TRANSPORT_KEY);
    assert_eq!(
        headers
            .get("cache-control")
            .and_then(|value| value.to_str().ok()),
        Some("no-store")
    );
}

#[tokio::test]
async fn install_rejects_plaintext_secret() {
    let harness = BuiltinHarness::start().await;
    let mut body = install_body();
    body["config"]["password"] = json!("plain-text-pass");
    let (status, body) = install(&harness, body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "MCP_TRANSPORT_CIPHERTEXT_REQUIRED");
    assert_eq!(body["error"]["data"]["field"], "password");
    assert!(!body.to_string().contains("plain-text-pass"));
    assert!(
        harness
            .store
            .upstream("clickhouse")
            .expect("query")
            .is_none(),
        "被拒的安装不得落库"
    );
}

#[tokio::test]
async fn install_rejects_unreadable_secret() {
    let harness = BuiltinHarness::start().await;
    let other = SecretKey::parse(&"7".repeat(64)).expect("other key");
    let mut body = install_body();
    body["config"]["password"] = json!(other.encrypt("s3cret-pass").expect("encrypt"));
    let (status, body) = install(&harness, body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "MCP_TRANSPORT_DECRYPT_FAILED");
    assert_eq!(body["error"]["data"]["field"], "password");
    assert!(!body.to_string().contains("s3cret-pass"));
    assert!(
        harness
            .store
            .upstream("clickhouse")
            .expect("query")
            .is_none()
    );
}

#[tokio::test]
async fn install_rejects_empty_decrypted_secret() {
    let harness = BuiltinHarness::start().await;
    let mut body = install_body();
    body["config"]["password"] = json!(transport_cipher(""));
    let (status, body) = install(&harness, body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "MCP_VALIDATION_ERROR");
    assert_eq!(body["error"]["data"]["field"], "password");
}

#[tokio::test]
async fn patch_requires_ciphertext_secret() {
    let harness = BuiltinHarness::start().await;
    let (status, body) = install(&harness, install_body()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let before = harness
        .store
        .upstream("clickhouse")
        .expect("query")
        .expect("row")
        .env
        .get("JUMCP_CLICKHOUSE_PASSWORD")
        .expect("password")
        .clone();

    let (status, body) = call(
        &harness.admin,
        Req::patch("/admin/mcp/catalog/clickhouse-basic/install/clickhouse/config")
            .body(json!({"config": {"password": "plain-text-pass"}})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "MCP_TRANSPORT_CIPHERTEXT_REQUIRED");
    assert_eq!(body["error"]["data"]["field"], "password");
    assert!(!body.to_string().contains("plain-text-pass"));

    let after = harness
        .store
        .upstream("clickhouse")
        .expect("query")
        .expect("row");
    assert_eq!(
        after.env.get("JUMCP_CLICKHOUSE_PASSWORD"),
        Some(&before),
        "被拒的 PATCH 不得改动密文"
    );
}

#[tokio::test]
async fn installed_upstream_refreshes_over_loopback_http() {
    let harness = BuiltinHarness::start().await;
    let (status, body) = install(&harness, install_body()).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = call(
        &harness.admin,
        Req::post("/admin/upstreams/clickhouse/refresh"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "ok", "{body}");
    assert_eq!(body["tool_count"], 3);
}

#[tokio::test]
async fn install_rejects_missing_required_and_unknown_keys() {
    let harness = BuiltinHarness::start().await;
    let (status, body) = install(
        &harness,
        json!({"group": "db.read.core", "config": {"username": "default"}}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "MCP_VALIDATION_ERROR");
    assert_eq!(body["error"]["data"]["field"], "url");

    let (status, body) = install(
        &harness,
        json!({"group": "db.read.core", "config": {
            "url": "http://127.0.0.1:8123",
            "Allowed_Schemas": "analytics",
        }}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["data"]["field"], "Allowed_Schemas");

    let (status, body) = install(&harness, json!({"group": "", "config": {}})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["data"]["field"], "group");

    assert!(
        harness
            .store
            .upstream("clickhouse")
            .expect("query")
            .is_none()
    );
}

#[tokio::test]
async fn install_conflicts_on_existing_id() {
    let harness = BuiltinHarness::start().await;
    let (status, body) = install(&harness, install_body()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = install(&harness, install_body()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
}

#[tokio::test]
async fn uninstall_only_removes_own_installation() {
    let harness = BuiltinHarness::start().await;
    harness
        .store
        .insert_upstream(&manual_upstream("manual"), &[], "alice")
        .expect("seed manual");
    let (status, body) = install(&harness, install_body()).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = call(
        &harness.admin,
        Req::delete("/admin/mcp/catalog/clickhouse-basic/install/manual"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], "MCP_UPSTREAM_NOT_FOUND");
    assert!(harness.store.upstream("manual").expect("query").is_some());

    let (status, body) = call(
        &harness.admin,
        Req::delete("/admin/mcp/catalog/clickhouse-basic/install/clickhouse"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["deleted"], true);
    assert!(
        harness
            .store
            .upstream("clickhouse")
            .expect("query")
            .is_none()
    );
    assert!(
        catalog(&harness).await["items"][0]["installations"]
            .as_array()
            .expect("installations")
            .is_empty()
    );
}

#[tokio::test]
async fn patch_empty_secret_keeps_ciphertext() {
    let harness = BuiltinHarness::start().await;
    install(&harness, install_body()).await;
    let before = harness
        .store
        .upstream("clickhouse")
        .expect("query")
        .expect("row");
    let sealed_before = before
        .env
        .get("JUMCP_CLICKHOUSE_PASSWORD")
        .expect("password")
        .clone();

    let (status, body) = call(
        &harness.admin,
        Req::patch("/admin/mcp/catalog/clickhouse-basic/install/clickhouse/config")
            .body(json!({"config": {"url": "https://ch.internal", "password": ""}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let row = &body["installation"];
    assert_eq!(row["config"]["url"], "https://ch.internal");
    assert_eq!(row["config"]["username"], "default");
    assert_eq!(row["secrets_set"]["password"], true);
    let text = body.to_string();
    assert!(!text.contains("s3cret-pass"));
    assert!(!text.contains("enc:v1:"));

    let after = harness
        .store
        .upstream("clickhouse")
        .expect("query")
        .expect("row");
    assert_eq!(
        after.env.get("JUMCP_CLICKHOUSE_URL").map(String::as_str),
        Some("https://ch.internal")
    );
    assert_eq!(
        after.env.get("JUMCP_CLICKHOUSE_PASSWORD"),
        Some(&sealed_before),
        "空串的 secret 必须保持原密文"
    );
    assert_eq!(
        after.env.get("JUMCP_CLICKHOUSE_USER").map(String::as_str),
        Some("default")
    );

    // 轮换：旧密文必须消失，新值可解出。
    let (status, body) = call(
        &harness.admin,
        Req::patch("/admin/mcp/catalog/clickhouse-basic/install/clickhouse/config")
            .body(json!({"config": {"password": transport_cipher("rotated-pass")}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.to_string().contains("rotated-pass"));
    let rotated = harness
        .store
        .upstream("clickhouse")
        .expect("query")
        .expect("row");
    let sealed = rotated
        .env
        .get("JUMCP_CLICKHOUSE_PASSWORD")
        .expect("password")
        .clone();
    assert_ne!(sealed, sealed_before);
    let key = SecretKey::parse(TEST_SECRET_KEY).expect("key");
    assert_eq!(key.decrypt(&sealed).expect("decrypt"), "rotated-pass");
}

#[tokio::test]
async fn config_patch_rejects_clearing_required_field() {
    let harness = BuiltinHarness::start().await;
    install(&harness, install_body()).await;
    let (status, body) = call(
        &harness.admin,
        Req::patch("/admin/mcp/catalog/clickhouse-basic/install/clickhouse/config")
            .body(json!({"config": {"url": ""}})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "MCP_VALIDATION_ERROR");
    assert_eq!(body["error"]["data"]["field"], "url");
    let row = harness
        .store
        .upstream("clickhouse")
        .expect("query")
        .expect("row");
    assert_eq!(
        row.env.get("JUMCP_CLICKHOUSE_URL").map(String::as_str),
        Some("http://127.0.0.1:8123"),
        "被拒的 PATCH 不得写库"
    );

    let (status, body) = call(
        &harness.admin,
        Req::patch("/admin/mcp/catalog/clickhouse-basic/install/clickhouse/config")
            .body(json!({"config": {"unknown": "x"}})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["data"]["field"], "unknown");
}

#[tokio::test]
async fn put_cannot_change_package_managed_fields_but_can_toggle() {
    let harness = BuiltinHarness::start().await;
    install(&harness, install_body()).await;
    let (status, body) = call(&harness.admin, Req::get("/admin/upstreams/clickhouse")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let item = body;

    let mut tampered = item.clone();
    tampered["url"] = json!("http://evil.example/mcp");
    let (status, body) = call(
        &harness.admin,
        Req::put("/admin/upstreams/clickhouse").body(tampered.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "MCP_UPSTREAM_PACKAGE_MANAGED");
    assert_eq!(body["error"]["data"]["fields"], json!(["url"]));

    let mut toggled = item.clone();
    toggled["enabled"] = json!(false);
    toggled["name"] = json!("改名后的安装");
    let (status, body) = call(
        &harness.admin,
        Req::put("/admin/upstreams/clickhouse").body(toggled),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["enabled"], false);
    assert_eq!(body["name"], "改名后的安装");
}

#[tokio::test]
async fn legacy_stdio_installation_is_migrated() {
    let harness = BuiltinHarness::start().await;
    harness
        .store
        .insert_upstream(
            &legacy_upstream(
                "legacy-ch",
                "http://default:s3cret@127.0.0.1:8123/analytics",
            ),
            &[],
            "alice",
        )
        .expect("seed legacy");
    harness
        .store
        .insert_upstream(
            &legacy_upstream("broken-ch", "definitely-not-a-url"),
            &[],
            "alice",
        )
        .expect("seed broken");

    let key = SecretKey::parse(TEST_SECRET_KEY).expect("key");
    let migrated =
        migrate_legacy_packages(&harness.store, &key, &harness.endpoint).expect("migrate");
    assert_eq!(migrated, 1);

    let row = harness
        .store
        .upstream("legacy-ch")
        .expect("query")
        .expect("row");
    assert_eq!(row.transport, "http");
    assert_eq!(
        row.url,
        harness.endpoint.url_for("clickhouse-basic", "legacy-ch")
    );
    assert!(row.command.is_empty());
    assert!(row.args.is_empty());
    assert!(!row.env.contains_key("JUMCP_CLICKHOUSE_DSN"));
    assert_eq!(
        row.env.get("JUMCP_CLICKHOUSE_URL").map(String::as_str),
        Some("http://127.0.0.1:8123")
    );
    assert_eq!(
        row.env.get("JUMCP_CLICKHOUSE_USER").map(String::as_str),
        Some("default")
    );
    assert_eq!(
        row.env.get("JUMCP_CLICKHOUSE_DATABASE").map(String::as_str),
        Some("analytics")
    );
    assert_eq!(
        row.env
            .get("JUMCP_CLICKHOUSE_ALLOWED_SCHEMAS")
            .map(String::as_str),
        Some("analytics")
    );
    let sealed = row.env.get("JUMCP_CLICKHOUSE_PASSWORD").expect("password");
    assert!(is_encrypted(sealed));
    assert_eq!(key.decrypt(sealed).expect("decrypt"), "s3cret");

    // 解析失败的旧行保留原样：迁移不静默丢配置。
    let broken = harness
        .store
        .upstream("broken-ch")
        .expect("query")
        .expect("row");
    assert_eq!(broken.transport, "stdio");
    assert_eq!(
        broken.env.get("JUMCP_CLICKHOUSE_DSN").map(String::as_str),
        Some("definitely-not-a-url")
    );
}

#[tokio::test]
async fn patch_after_secret_key_rotation_keeps_ciphertext() {
    let harness = BuiltinHarness::start().await;
    let (status, body) = install(&harness, install_body()).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // 模拟 `JUMCP_SECRET_KEY` 轮换：库里的密文由另一把密钥加密，当前进程已解不开。
    let rotated = SecretKey::parse(&"7".repeat(64)).expect("rotated key");
    let sealed = rotated.encrypt("s3cret-pass").expect("encrypt");
    let mut row = harness
        .store
        .upstream("clickhouse")
        .expect("query")
        .expect("row");
    row.env
        .insert("JUMCP_CLICKHOUSE_PASSWORD".into(), sealed.clone());
    harness
        .store
        .update_upstream(&row, &["JUMCP_CLICKHOUSE_PASSWORD"], "test")
        .expect("rewrite");

    // 只改 url：未提交的 secret 不解密，不得因旧密文失败，也不得重写密文。
    let (status, body) = call(
        &harness.admin,
        Req::patch("/admin/mcp/catalog/clickhouse-basic/install/clickhouse/config")
            .body(json!({"config": {"url": "https://ch.internal"}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["installation"]["config"]["url"], "https://ch.internal");
    assert_eq!(body["installation"]["secrets_set"]["password"], true);

    let after = harness
        .store
        .upstream("clickhouse")
        .expect("query")
        .expect("row");
    assert_eq!(
        after.env.get("JUMCP_CLICKHOUSE_PASSWORD"),
        Some(&sealed),
        "旧密文必须逐字节保留"
    );
    assert_eq!(
        after.env.get("JUMCP_CLICKHOUSE_URL").map(String::as_str),
        Some("https://ch.internal")
    );
}

#[tokio::test]
async fn install_rejects_overlong_secret_ciphertext() {
    let harness = BuiltinHarness::start().await;
    let mut body = install_body();
    body["config"]["password"] = json!(format!("enc:v1:{}", "a".repeat(4096)));
    let (status, body) = install(&harness, body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "MCP_VALIDATION_ERROR");
    assert_eq!(body["error"]["data"]["field"], "password");
    assert!(
        harness
            .store
            .upstream("clickhouse")
            .expect("query")
            .is_none(),
        "超长密文不得落库"
    );
}

#[tokio::test]
async fn transport_key_requires_admin_session() {
    let harness = BuiltinHarness::start().await;
    let (status, body) = call(
        &harness.admin,
        Req::get("/admin/transport-key").without_token(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["error"]["code"], "MCP_UNAUTHORIZED");
}

#[tokio::test]
async fn transport_key_missing_is_503() {
    let harness = BuiltinHarness::start().await;
    let admin = jumcp_server::admin::router(jumcp_server::admin::AdminState {
        service: harness.service.clone(),
        store: harness.store.clone(),
        transport_key: None,
    });
    let (status, body) = call(&admin, Req::get("/admin/transport-key")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["error"]["code"], "MCP_TRANSPORT_KEY_MISSING");
}

fn manual_upstream(id: &str) -> McpUpstream {
    McpUpstream {
        id: id.into(),
        name: "手工上游".into(),
        transport: "http".into(),
        url: "http://127.0.0.1:1/mcp".into(),
        command: String::new(),
        args: Vec::new(),
        env: BTreeMap::new(),
        headers: BTreeMap::new(),
        group: "db.read.core".into(),
        category: "custom".into(),
        read_only: true,
        enabled: true,
        timeout_ms: 30_000,
        catalog_id: None,
        catalog_version: None,
        created_at: String::new(),
        updated_at: String::new(),
    }
}

fn legacy_upstream(id: &str, dsn: &str) -> McpUpstream {
    let mut env = BTreeMap::new();
    env.insert("JUMCP_CLICKHOUSE_DSN".into(), dsn.to_string());
    env.insert(
        "JUMCP_CLICKHOUSE_ALLOWED_SCHEMAS".into(),
        "analytics".into(),
    );
    McpUpstream {
        id: id.into(),
        name: "旧安装".into(),
        transport: "stdio".into(),
        url: String::new(),
        command: "jumcp-mcp-clickhouse".into(),
        args: Vec::new(),
        env,
        headers: BTreeMap::new(),
        group: "db.read.core".into(),
        category: "custom".into(),
        read_only: true,
        enabled: true,
        timeout_ms: 30_000,
        catalog_id: Some("clickhouse-basic".into()),
        catalog_version: Some("0.0.1".into()),
        created_at: String::new(),
        updated_at: String::new(),
    }
}
