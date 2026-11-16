//! 平台 access token 契约测试：创建 / 列表 / 撤销 / options / 组引用闭环，
//! 以及"token 配置 → /mcp tools/list 与 tools/call 同口径"的端到端验证。
//!
//! 全部走真实 axum 链路（admin router + /mcp），执行器替换为 fake，其余是生产实现。

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use jumcp_core::audit::{ActorRef, AuditEvent, AuditLog};
use jumcp_core::config::{AdminTokens, DatasourceRef, GroupDef, RoleDef, Settings, TenantPolicy};
use jumcp_core::entry::EffectiveScope;
use jumcp_core::registry::{ADMIN_CAPABILITY_IDS, BUILTIN_CAPABILITY_IDS, CapabilityStore};
use jumcp_core::template::TemplateRegistry;
use jumcp_server::admin::AdminState;
use jumcp_server::executor::{ExecFuture, QueryExecutor, QueryParams};
use jumcp_server::mcp::{McpServer, http_router};
use jumcp_server::service::Service;
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

const ALICE_TOKEN: &str = "tok_alice_000000000";
const BOB_TOKEN: &str = "tok_bob_00000000000";

struct FakeExecutor;

impl QueryExecutor for FakeExecutor {
    fn execute<'a>(&'a self, _scope: &'a EffectiveScope, _q: &'a QueryParams) -> ExecFuture<'a> {
        Box::pin(async {
            Ok(json!({
                "rows": [["2026-11-05", 3]],
                "row_count": 1,
                "truncated": false,
                "effective_max_rows": 100,
                "elapsed_ms": 1,
                "tokens": 3
            }))
        })
    }
}

fn settings() -> Settings {
    let mut roles = std::collections::HashMap::new();
    roles.insert(
        "analyst".into(),
        RoleDef {
            name: "数据分析".into(),
            default_groups: vec!["db.read.core".into(), "db.read.analytics".into()],
            default_budget: 2000,
        },
    );
    roles.insert(
        "admin".into(),
        RoleDef {
            name: "管理员".into(),
            default_groups: vec!["admin".into()],
            default_budget: 2500,
        },
    );
    Settings {
        admin_tokens: AdminTokens::parse(&format!("alice:{ALICE_TOKEN},bob:{BOB_TOKEN}"))
            .expect("admin tokens"),
        policy: TenantPolicy {
            groups: vec![
                GroupDef {
                    code: "db.read.core".into(),
                    name: "核心数据只读".into(),
                    priority: 10,
                    min_budget: 800,
                },
                GroupDef {
                    code: "db.read.analytics".into(),
                    name: "分析数据只读".into(),
                    priority: 8,
                    min_budget: 800,
                },
                GroupDef {
                    code: "admin".into(),
                    name: "管理员通道".into(),
                    priority: 1,
                    min_budget: 800,
                },
            ],
            roles,
            tenant_budget_cap: 2000,
            allow_intersect: false,
        },
        budget_overflow: "fail".into(),
        datasources: vec![DatasourceRef {
            name: "main".into(),
            dsn_env: "JUMCP_TEST_MAIN_DSN".into(),
            engine: "clickhouse".into(),
            readonly: true,
            allowed_schemas: vec![],
            statement_timeout_ms: 5000,
        }],
        ..Default::default()
    }
}

struct Harness {
    app: Router,
    store: Arc<CapabilityStore>,
    audit: Arc<AuditLog>,
}

fn harness() -> Harness {
    let audit = Arc::new(AuditLog::open(":memory:").expect("audit"));
    let store = Arc::new(CapabilityStore::open(":memory:", Some(audit.clone())).expect("store"));
    let registry = Arc::new(TemplateRegistry::new(store.clone()));
    let service = Arc::new(
        Service::new(settings())
            .with_executor(Arc::new(FakeExecutor))
            .with_audit(audit.clone())
            .with_templates(registry)
            .with_scope(EffectiveScope {
                groups: vec!["db.read.core".into()],
                budget: 2000,
                degraded: false,
                source: "default".into(),
                mode: "strict".into(),
                allowed_builtin_tools: None,
            }),
    );
    let admin = AdminState {
        service: service.clone(),
        store: store.clone(),
        tokens: settings().admin_tokens,
    };
    let app = jumcp_server::admin::router(admin)
        .merge(http_router(Arc::new(McpServer::from_arc(service))));
    Harness { app, store, audit }
}

/// 发布一条可见可调的能力（store 直调，管理面发布路径已有独立测试）。
fn publish_capability(h: &Harness, id: &str, group: &str) {
    h.store
        .register(id, "tool", group, "read", json!({"description": id}))
        .expect("register");
    h.store.submit_for_review(id, "alice").expect("submit");
    h.store
        .promote_to_published(id, "ap-pub", "bob")
        .expect("publish");
    h.store
        .set_visibility(id, Some(true), Some(true), "alice")
        .expect("visibility");
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    for (key, value) in headers {
        builder = builder.header(*key, *value);
    }
    let body = match body {
        Some(v) => Body::from(v.to_string()),
        None => Body::empty(),
    };
    let request = builder
        .header(header::CONTENT_TYPE, "application/json")
        .body(body)
        .expect("request");
    let response = app.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    let value: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("json body")
    };
    (status, value)
}

fn rpc(method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","id":1,"method":method,"params":params})
}

async fn mcp(app: &Router, token: Option<&str>, body: Value, headers: &[(&str, &str)]) -> Value {
    let (status, value) = call(app, "POST", "/mcp", token, headers, Some(body)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "MCP 入口应回 200 + JSON-RPC: {value}"
    );
    value
}

async fn create_token(app: &Router, body: Value) -> (StatusCode, Value) {
    call(
        app,
        "POST",
        "/admin/tokens",
        Some(ALICE_TOKEN),
        &[("x-admin-actor", "alice")],
        Some(body),
    )
    .await
}

fn base_token_body() -> Value {
    json!({
        "name": "分析型只读",
        "role": "analyst",
        "groups": ["db.read.core"],
        "builtin_tools": ["search_capabilities", "describe_capability"],
        "source": "agent",
        "caller": "agent-analyst",
        "expires_at": null
    })
}

async fn tool_names(app: &Router, token: &str) -> Vec<String> {
    let resp = mcp(app, Some(token), rpc("tools/list", json!({})), &[]).await;
    resp["result"]["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|t| t["name"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn err_code(resp: &Value) -> String {
    resp["result"]["content"]["error"]["code"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// 创建 → 列表 → 审计 → 明文只出现一次；item 永不包含明文/hash。
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

/// admin_* 的双重判定：没有 admin 组时创建即 400；有 admin 组 + allowlist 才可见可调。
#[tokio::test]
async fn admin_tools_require_group_and_allowlist() {
    let h = harness();
    let mut admin_body = base_token_body();
    admin_body["name"] = json!("维护通道");
    admin_body["role"] = json!("admin");
    admin_body["groups"] = json!(["admin", "db.read.core"]);
    admin_body["builtin_tools"] = json!(["admin_review_queue", "list_groups"]);
    let (status, created) = create_token(&h.app, admin_body).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let plaintext = created["token"].as_str().unwrap().to_string();
    let names = tool_names(&h.app, &plaintext).await;
    assert!(
        names.contains(&"admin_review_queue".to_string()),
        "{names:?}"
    );
    assert!(names.contains(&"list_groups".to_string()), "{names:?}");
    assert!(
        !names.contains(&"admin_audit_queries".to_string()),
        "allowlist 外的 admin 工具不得出现: {names:?}"
    );

    // admin 组在 scope 内 + allowlist 命中 → 可调。
    let ok = mcp(
        &h.app,
        Some(&plaintext),
        rpc(
            "tools/call",
            json!({"name": "admin_review_queue", "arguments": {}}),
        ),
        &[],
    )
    .await;
    assert_eq!(ok["result"]["isError"], json!(false), "{ok}");

    // header 收窄掉 admin 组：即使 allowlist 命中也不可见、不可调。
    let narrowed = tool_names_with_groups(&h.app, &plaintext, "db.read.core").await;
    assert!(
        !narrowed.contains(&"admin_review_queue".to_string()),
        "{narrowed:?}"
    );
    let denied = mcp(
        &h.app,
        Some(&plaintext),
        rpc(
            "tools/call",
            json!({"name": "admin_review_queue", "arguments": {}}),
        ),
        &[("x-mcp-tool-groups", "db.read.core")],
    )
    .await;
    assert_eq!(err_code(&denied), "MCP_CAPABILITY_NOT_FOUND", "{denied}");

    // header 不能扩大 token 组：声明 analytics 越界。
    let escalated = mcp(
        &h.app,
        Some(&plaintext),
        rpc("tools/list", json!({})),
        &[("x-mcp-tool-groups", "db.read.analytics")],
    )
    .await;
    assert_eq!(
        escalated["error"]["data"]["mcp_code"],
        json!("MCP_GROUP_FORBIDDEN"),
        "{escalated}"
    );
}

async fn tool_names_with_groups(app: &Router, token: &str, groups: &str) -> Vec<String> {
    let resp = mcp(
        app,
        Some(token),
        rpc("tools/list", json!({})),
        &[("x-mcp-tool-groups", groups)],
    )
    .await;
    resp["result"]["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|t| t["name"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// 组闭环：GET /admin/groups 带 token_count；被 token 引用的组删除返回 409 且带两个计数。
#[tokio::test]
async fn group_token_count_and_delete_guard() {
    let h = harness();
    let (status, group) = call(
        &h.app,
        "POST",
        "/admin/groups",
        Some(ALICE_TOKEN),
        &[("x-admin-actor", "alice")],
        Some(
            json!({"code": "db.read.tmp", "name": "临时只读组", "priority": 5, "min_budget": 800, "roles": ["analyst"]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{group}");

    let mut body = base_token_body();
    body["groups"] = json!(["db.read.tmp"]);
    let (status, created) = create_token(&h.app, body).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");

    let (status, listed) = call(&h.app, "GET", "/admin/groups", Some(ALICE_TOKEN), &[], None).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let item = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["code"] == json!("db.read.tmp"))
        .expect("组必须出现");
    assert_eq!(item["token_count"], json!(1));

    let (status, blocked) = call(
        &h.app,
        "DELETE",
        "/admin/groups/db.read.tmp",
        Some(ALICE_TOKEN),
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{blocked}");
    assert_eq!(blocked["error"]["code"], json!("MCP_GROUP_IN_USE"));
    assert_eq!(blocked["error"]["data"]["capability_count"], json!(0));
    assert_eq!(blocked["error"]["data"]["token_count"], json!(1));

    // 撤销后引用归零，删除恢复可行。
    let id = created["item"]["id"].as_str().unwrap().to_string();
    let (status, _) = call(
        &h.app,
        "POST",
        &format!("/admin/tokens/{id}/revoke"),
        Some(ALICE_TOKEN),
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, deleted) = call(
        &h.app,
        "DELETE",
        "/admin/groups/db.read.tmp",
        Some(ALICE_TOKEN),
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{deleted}");
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
    // 角色按 code 稳定排序；name 是中文展示名，不再是 code 的重复。
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
}

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

/// `initialize._meta.mcp.groups` 对 token scope 同样只能收窄，不能扩到 token 未授予的组。
#[tokio::test]
async fn meta_groups_can_only_narrow_token_scope() {
    let h = harness();
    publish_capability(&h, "cap.core", "db.read.core");
    publish_capability(&h, "cap.analytics", "db.read.analytics");
    let mut body = base_token_body();
    body["groups"] = json!(["db.read.core", "db.read.analytics"]);
    let (status, created) = create_token(&h.app, body).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let plaintext = created["token"].as_str().unwrap().to_string();

    let init = mcp(
        &h.app,
        Some(&plaintext),
        rpc(
            "initialize",
            json!({"_meta": {"mcp": {"groups": ["db.read.core"]}}}),
        ),
        &[],
    )
    .await;
    assert_eq!(init["error"], Value::Null, "{init}");
    let names = tool_names(&h.app, &plaintext).await;
    assert!(names.contains(&"cap.core".to_string()), "{names:?}");
    assert!(
        !names.contains(&"cap.analytics".to_string()),
        "_meta 未声明的组不得出现: {names:?}"
    );

    // 越界 _meta：strict 直接拒绝（不会裁成 token 组）。
    let escalated = mcp(
        &h.app,
        Some(&plaintext),
        rpc(
            "initialize",
            json!({"_meta": {"mcp": {"groups": ["db.read.sensitive"]}}}),
        ),
        &[],
    )
    .await;
    assert_eq!(
        escalated["error"]["data"]["mcp_code"],
        json!("MCP_GROUP_FORBIDDEN"),
        "{escalated}"
    );
}

/// 调用明细与审计过滤都要带出 token 来源：`actor.source/token_id` 可选、旧事件不匹配。
#[tokio::test]
async fn call_details_and_audit_filter_carry_token_origin() {
    let h = harness();
    // 先写一条"改造前"的 tool_call（actor 没有 token_id/source）。
    let legacy = h
        .audit
        .append(AuditEvent {
            tenant: "default".into(),
            actor: ActorRef {
                actor_type: "agent".into(),
                id: "legacy-agent".into(),
                role: "analyst".into(),
                token_id: None,
                source: None,
            },
            event_type: "tool_call".into(),
            payload: json!({
                "object_type": "tool",
                "object_id": "admin_review_queue",
                "action": "invoke",
                "outcome": "ok",
                "data": {"arguments": {}}
            }),
            session_id: "default".into(),
            ts: String::new(),
            event_id: String::new(),
        })
        .expect("legacy event");

    let mut body = base_token_body();
    body["role"] = json!("admin");
    body["groups"] = json!(["admin"]);
    body["builtin_tools"] = json!(["admin_review_queue"]);
    let (status, created) = create_token(&h.app, body).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let plaintext = created["token"].as_str().unwrap().to_string();
    let token_id = created["item"]["id"].as_str().unwrap().to_string();

    let call_resp = mcp(
        &h.app,
        Some(&plaintext),
        rpc(
            "tools/call",
            json!({"name": "admin_review_queue", "arguments": {}}),
        ),
        &[],
    )
    .await;
    assert_eq!(call_resp["result"]["isError"], json!(false), "{call_resp}");

    // 审计查询：token_id 精确匹配到本次 token 的事件，旧事件不出现。
    let (status, filtered) = call(
        &h.app,
        "GET",
        &format!("/admin/audit-events?token_id={token_id}&limit=200"),
        Some(ALICE_TOKEN),
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{filtered}");
    let events = filtered["events"].as_array().expect("events");
    assert!(!events.is_empty(), "token 调用必须能被 token_id 过滤到");
    assert!(
        events
            .iter()
            .all(|e| e["actor"]["token_id"] == json!(token_id)
                && e["actor"]["source"] == json!("agent")),
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .all(|e| e["event_id"] != json!(legacy.event_id)),
        "旧事件（无 token_id）不得被 token_id 过滤命中"
    );

    let (status, none) = call(
        &h.app,
        "GET",
        "/admin/audit-events?token_id=tok_does_not_exist",
        Some(ALICE_TOKEN),
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{none}");
    assert_eq!(none["total"], json!(0));

    // 工具调用明细：新调用的 actor 带 source/token_id，旧调用只有 id/role。
    let (status, calls) = call(
        &h.app,
        "GET",
        "/admin/tools/admin_review_queue/calls?window_days=1&limit=50",
        Some(ALICE_TOKEN),
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{calls}");
    let rows = calls["calls"].as_array().expect("calls");
    let new_call = rows
        .iter()
        .find(|c| c["actor"]["token_id"] == json!(token_id))
        .expect("新调用应带 token_id");
    assert_eq!(new_call["actor"]["source"], json!("agent"));
    assert_eq!(new_call["actor"]["id"], json!("agent-analyst"));
    let old_call = rows
        .iter()
        .find(|c| c["event_id"] == json!(legacy.event_id))
        .expect("旧调用仍在明细里");
    assert!(
        old_call["actor"].get("token_id").is_none(),
        "旧事件不得凭空出现 token_id: {old_call}"
    );
    assert!(old_call["actor"].get("source").is_none());
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
    let deletes: Vec<&jumcp_core::audit::Envelope> = events
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
