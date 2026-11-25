use crate::support::fake_upstream::{spawn_fake, spawn_fake_named};
use axum::http::StatusCode;
use jumcp_core::registry::{CapabilityStore, McpUpstream};
use serde_json::{Value, json};

use crate::fixtures::*;

#[tokio::test]
async fn builtin_categories_are_exposed_and_filterable() {
    let h = harness();
    let token = issue_token(&h, "agent-categories", "analyst", &["db.read.core"]).await;
    let resp = mcp(&h.app, &token, "tools/list", json!({})).await;
    let tools = resp["result"]["tools"].as_array().expect("tools");
    let category_of = |name: &str| -> Value {
        tools
            .iter()
            .find(|t| t["name"] == json!(name))
            .map(|t| t["category"].clone())
            .unwrap_or(Value::Null)
    };
    assert_eq!(category_of("search_capabilities"), json!("basic"));
    assert_eq!(category_of("describe_capability"), json!("basic"));
    assert_eq!(category_of("list_groups"), json!("basic"));
    assert_eq!(category_of("db_run_template"), json!("basic"));

    // 自由的只读 SQL 工具已移除：任何 token 的 tools/list 都不出现它。
    assert_eq!(category_of("db_query_readonly"), Value::Null);

    let basic = call_tool(
        &h.app,
        &token,
        "search_capabilities",
        json!({"query":"", "category":"basic", "limit":50}),
    )
    .await;
    let ids: Vec<&str> = basic["result"]["structuredContent"]["results"]
        .as_array()
        .expect("hits")
        .iter()
        .filter_map(|h| h["id"].as_str())
        .collect();
    assert!(ids.contains(&"search_capabilities"));
    assert!(ids.contains(&"db_run_template"));
    assert!(!ids.contains(&"db_query_readonly"));

    // admin 组的工具对 analyst 不可见（分类不改变授权）。
    let admin = call_tool(
        &h.app,
        &token,
        "search_capabilities",
        json!({"query":"admin_", "category":"admin", "limit":50}),
    )
    .await;
    assert_eq!(admin["result"]["structuredContent"]["results"], json!([]));

    // analysis 归工具列表分析与变更提交审计，admin 只剩审计查询：都要 admin 组在 scope 内才可见，
    // 分类本身不放宽权限。
    let admin_token = issue_token(&h, "agent-category-admin", "admin", &["admin"]).await;
    let admin_resp = mcp(&h.app, &admin_token, "tools/list", json!({})).await;
    let admin_tools = admin_resp["result"]["tools"].as_array().expect("tools");
    let admin_category_of = |name: &str| -> Value {
        admin_tools
            .iter()
            .find(|t| t["name"] == json!(name))
            .map(|t| t["category"].clone())
            .unwrap_or(Value::Null)
    };
    for id in [
        "admin_review_queue",
        "admin_register_template",
        "admin_set_visibility",
    ] {
        assert_eq!(admin_category_of(id), json!("analysis"), "{id}");
    }
    assert_eq!(admin_category_of("admin_audit_queries"), json!("admin"));

    let analysis = call_tool(
        &h.app,
        &admin_token,
        "search_capabilities",
        json!({"query":"", "category":"analysis", "limit":50}),
    )
    .await;
    let analysis_ids: Vec<&str> = analysis["result"]["structuredContent"]["results"]
        .as_array()
        .expect("hits")
        .iter()
        .filter_map(|h| h["id"].as_str())
        .collect();
    for id in [
        "admin_review_queue",
        "admin_register_template",
        "admin_set_visibility",
    ] {
        assert!(analysis_ids.contains(&id), "{id} 应被 analysis 过滤命中");
    }
    assert!(!analysis_ids.contains(&"admin_audit_queries"));

    let admin_only = call_tool(
        &h.app,
        &admin_token,
        "search_capabilities",
        json!({"query":"", "category":"admin", "limit":50}),
    )
    .await;
    let admin_only_ids: Vec<&str> = admin_only["result"]["structuredContent"]["results"]
        .as_array()
        .expect("hits")
        .iter()
        .filter_map(|h| h["id"].as_str())
        .collect();
    assert_eq!(admin_only_ids, vec!["admin_audit_queries"]);
}

#[tokio::test]
async fn upstream_tools_follow_the_token_group_scope() {
    let h = harness();
    let (url, _fake) = spawn_fake(false).await;
    assert_eq!(
        create_upstream(&h, upstream_body("analytics", &url))
            .await
            .0,
        StatusCode::OK
    );
    let allowed = issue_token(&h, "agent-scope-full", "analyst", &["db.read.analytics"]).await;
    let core_only = issue_token(&h, "agent-scope-core", "core-only", &["db.read.core"]).await;

    assert!(
        tool_names(&h.app, &allowed)
            .await
            .contains(&"analytics__echo".to_string())
    );
    let names = tool_names(&h.app, &core_only).await;
    assert!(!names.iter().any(|n| n.starts_with("analytics__")));
    let resp = mcp(
        &h.app,
        &core_only,
        "tools/call",
        json!({"name":"analytics__echo","arguments":{}}),
    )
    .await;
    assert_eq!(
        resp["error"]["code"],
        json!(-32602),
        "越界组的调用名必须与不存在同构: {resp}"
    );
}

/// 注册表里的 managed capability 与上游暴露名撞名：整台上游拒绝，不静默去重。
fn publish_managed_capability(store: &CapabilityStore, id: &str) {
    store
        .register(
            id,
            "tool",
            "db.read.analytics",
            "read",
            json!({"description": id}),
        )
        .expect("register");
    store.submit_for_review(id, "alice").expect("submit");
    store
        .promote_to_published(id, "ap-pub", "bob")
        .expect("publish");
    store
        .set_visibility(id, Some(true), Some(true), "alice")
        .expect("visibility");
}

#[tokio::test]
async fn upstream_conflicts_with_managed_capability_are_rejected() {
    let h = harness();
    publish_managed_capability(&h.store, "xy__y");
    let (url, _fake) = spawn_fake_named(&["y"]).await;
    assert_eq!(
        create_upstream(&h, upstream_body("xy", &url)).await.0,
        StatusCode::OK
    );

    let (status, refreshed) = call(
        &h.app,
        "POST",
        "/admin/upstreams/xy/refresh",
        Some(ALICE_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(refreshed["status"], json!("error"), "{refreshed}");
    assert!(
        refreshed["status_message"]
            .as_str()
            .is_some_and(|m| m.contains("注册表能力重名")),
        "{refreshed}"
    );
    assert_eq!(refreshed["tool_count"], json!(0));

    // tools/list 只保留 managed 的那一份，不出现重复 id。
    let token = issue_token(
        &h,
        "agent-managed-conflict",
        "analyst",
        &["db.read.analytics"],
    )
    .await;
    let names = tool_names(&h.app, &token).await;
    assert_eq!(
        names.iter().filter(|n| *n == "xy__y").count(),
        1,
        "{names:?}"
    );
}

#[tokio::test]
async fn admin_write_tools_declare_non_readonly_annotations() {
    let h = harness();
    let token = issue_token(&h, "agent-annotations", "admin", &["admin"]).await;
    let resp = mcp(&h.app, &token, "tools/list", json!({})).await;
    let tools = resp["result"]["tools"].as_array().expect("tools");
    let annotations_of = |name: &str| -> Value {
        tools
            .iter()
            .find(|t| t["name"] == json!(name))
            .map(|t| t["annotations"].clone())
            .unwrap_or(Value::Null)
    };
    for write_tool in ["admin_register_template", "admin_set_visibility"] {
        let annotations = annotations_of(write_tool);
        assert_eq!(annotations["readOnlyHint"], json!(false), "{write_tool}");
        assert_eq!(annotations["destructiveHint"], json!(false), "{write_tool}");
        assert_eq!(annotations["idempotentHint"], json!(false), "{write_tool}");
        assert_eq!(annotations["openWorldHint"], json!(false), "{write_tool}");
    }
    for read_tool in ["admin_review_queue", "admin_audit_queries"] {
        let annotations = annotations_of(read_tool);
        assert_eq!(annotations["readOnlyHint"], json!(true), "{read_tool}");
        assert_eq!(annotations["destructiveHint"], json!(false), "{read_tool}");
    }
}

#[tokio::test]
async fn upstream_annotations_default_by_catalog_origin() {
    let h = harness_with(false);
    let (manual_url, _manual) = spawn_fake_named(&["plain"]).await;
    let (builtin_url, _builtin) = spawn_fake_named(&["plain"]).await;
    assert_eq!(
        create_upstream(&h, upstream_body("manual", &manual_url))
            .await
            .0,
        StatusCode::OK
    );
    // 内置包安装出来的行：catalog_id 让投影按只读处理（实现随仓库发布，可审计）。
    let def = McpUpstream {
        id: "pack".into(),
        name: "内置包上游".into(),
        transport: "http".into(),
        url: builtin_url.clone(),
        command: String::new(),
        args: Vec::new(),
        env: Default::default(),
        headers: Default::default(),
        category: "custom".into(),
        enabled: true,
        timeout_ms: 5000,
        catalog_id: Some("clickhouse-basic".into()),
        catalog_version: Some("0.0.1".into()),
        created_at: String::new(),
        updated_at: String::new(),
    };
    h.store.insert_upstream(&def, &[], "alice").expect("insert");
    let (granted, error) = set_exposure(&h, DEFAULT_GROUP, json!({"pack": true}), json!({})).await;
    assert_eq!(granted, StatusCode::OK, "{error}");
    let (status, _) = call(
        &h.app,
        "POST",
        "/admin/upstreams/pack/refresh",
        Some(ALICE_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let token = issue_token(
        &h,
        "agent-risk-annotations",
        "analyst",
        &["db.read.analytics"],
    )
    .await;
    let resp = mcp(&h.app, &token, "tools/list", json!({})).await;
    let tools = resp["result"]["tools"].as_array().expect("tools");
    let annotations_of = |name: &str| -> Value {
        tools
            .iter()
            .find(|t| t["name"] == json!(name))
            .map(|t| t["annotations"].clone())
            .unwrap_or(Value::Null)
    };

    // 内置包没声明注解 → 按只读投影。
    let builtin = annotations_of("pack__plain");
    assert_eq!(builtin["readOnlyHint"], json!(true));
    assert_eq!(builtin["idempotentHint"], json!(true));

    // 手工接入的外部 MCP 没声明注解 → 协议最保守缺省值，绝不出现 readOnlyHint=true。
    let manual = annotations_of("manual__plain");
    assert_eq!(manual["readOnlyHint"], json!(false));
    assert_eq!(manual["destructiveHint"], json!(true));
    assert_eq!(manual["idempotentHint"], json!(false));
    assert_eq!(manual["openWorldHint"], json!(true));
}
