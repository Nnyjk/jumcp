//! 发现面契约（W6）：单一可见集 = builtin ∪ 注册表可见行；管理面发布后无需重启即可见。
//!
//! 装配真实 `CapabilityStore`（`:memory:`）+ `TemplateRegistry`，只把执行器换成 fake，
//! 因此这里验证的是"注册表 → 发现面"这条真实链路，而不是内存目录的替身。

use jumcp_core::config::{DatasourceRef, GroupDef, RoleDef, Settings, TenantPolicy};
use jumcp_core::entry::EffectiveScope;
use jumcp_core::registry::CapabilityStore;
use jumcp_core::template::TemplateRegistry;
use jumcp_server::executor::{ExecFuture, QueryExecutor, QueryParams};
use jumcp_server::mcp::McpServer;
use jumcp_server::service::{RESIDENT_TOOL_NAMES, Service};
use serde_json::{Value, json};
use std::sync::Arc;

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
            name: "analyst".into(),
            default_groups: vec!["db.read.core".into(), "db.read.analytics".into()],
            default_budget: 2000,
        },
    );
    Settings {
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
                    code: "db.admin.schema".into(),
                    name: "库表管理".into(),
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
            name: "main-pg".into(),
            dsn_env: "JUMCP_TEST_MAIN_DSN".into(),
            engine: "clickhouse".into(),
            readonly: true,
            allowed_schemas: vec!["public".into()],
            statement_timeout_ms: 5000,
        }],
        ..Default::default()
    }
}

fn scope() -> EffectiveScope {
    EffectiveScope {
        groups: vec!["db.read.core".into(), "db.read.analytics".into()],
        budget: 2000,
        degraded: false,
        source: "default".into(),
        mode: "strict".into(),
        allowed_builtin_tools: None,
    }
}

struct Harness {
    server: McpServer,
    registry: Arc<TemplateRegistry>,
    store: Arc<CapabilityStore>,
}

/// 服务在注册任何能力**之前**装配好：后续发布只有"每次请求现查注册表"才可能被看到。
fn harness() -> Harness {
    let store = Arc::new(CapabilityStore::open(":memory:", None).expect("store"));
    let registry = Arc::new(TemplateRegistry::new(store.clone()));
    let service = Service::new(settings())
        .with_executor(Arc::new(FakeExecutor))
        .with_scope(scope())
        .with_templates(registry.clone());
    Harness {
        server: McpServer::new(service),
        registry,
        store,
    }
}

fn template_value() -> Value {
    json!({
        "template_id": "order.daily_summary",
        "group": "db.read.analytics",
        "risk": "read",
        "description": "按日汇总订单",
        "tags": ["order", "daily"],
        "sql": "SELECT day FROM public.orders WHERE created_at >= :start_date AND created_at < :end_date",
        "params": [
            {"name":"start_date","type":"timestamp","required":true,"nullable":false},
            {"name":"end_date","type":"timestamp","required":true,"nullable":false}
        ],
        "constraints": {
            "allowed_schemas": ["public"],
            "auto_limit": 100,
            "max_result_tokens": 1500,
            "statement_timeout_ms": 5000
        },
        "owner": "data-platform"
    })
}

async fn search(server: &McpServer, query: &str) -> Vec<String> {
    let resp = server
        .handle(&json!({
            "jsonrpc":"2.0","id":1,"method":"tools/call",
            "params":{"name":"search_capabilities","arguments":{"query":query,"limit":50}}
        }))
        .await;
    resp["result"]["content"]
        .as_array()
        .map(|hits| {
            hits.iter()
                .filter_map(|h| h["id"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

async fn describe(server: &McpServer, id: &str) -> Value {
    server
        .handle(&json!({
            "jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"describe_capability","arguments":{"capability_id":id}}
        }))
        .await
}

async fn list_groups(server: &McpServer) -> Vec<String> {
    let resp = server
        .handle(&json!({
            "jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{"name":"list_groups","arguments":{}}
        }))
        .await;
    resp["result"]["content"]
        .as_array()
        .map(|groups| {
            groups
                .iter()
                .filter_map(|g| g["group"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

async fn tool_names(server: &McpServer) -> Vec<String> {
    let resp = server
        .handle(&json!({"jsonrpc":"2.0","id":4,"method":"tools/list"}))
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

async fn run_template(server: &McpServer, template_id: &str) -> Value {
    server
        .handle(&json!({
            "jsonrpc":"2.0","id":5,"method":"tools/call",
            "params":{"name":"db_run_template","arguments":{
                "template_id":template_id,
                "params":{"start_date":"2026-11-01","end_date":"2026-11-02"}
            }}
        }))
        .await
}

fn err_code(resp: &Value) -> String {
    resp["result"]["content"]["error"]["code"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// 管理面：注册 → 提审 → 发布 → 开放（与 admin.rs 调的是同一批 store 方法）。
fn publish_through_admin_surface(h: &Harness, id: &str, discoverable: bool) {
    h.registry
        .set_visibility(id, Some(discoverable), Some(true), "alice")
        .expect("set_visibility");
}

#[tokio::test]
async fn published_template_becomes_visible_without_restart() {
    let h = harness();
    // 基线：注册表为空，连"模板"这四个字都搜不到业务能力。
    assert!(search(&h.server, "order.daily_summary").await.is_empty());

    h.registry
        .register(&template_value())
        .expect("register draft");
    // draft 不可见（§4.4 审核前不暴露）。
    assert!(
        search(&h.server, "order.daily_summary").await.is_empty(),
        "draft 能力不得出现在发现面"
    );
    assert_eq!(
        err_code(&describe(&h.server, "order.daily_summary").await),
        "MCP_CAPABILITY_NOT_FOUND"
    );

    h.registry
        .submit_for_review("order.daily_summary", "alice")
        .expect("submit");
    h.registry
        .publish("order.daily_summary", "ap-pub", "bob")
        .expect("publish");
    // published 但 discoverable=0（register 的初值）：仍不可见——两个布尔各管一段。
    assert!(
        search(&h.server, "order.daily_summary").await.is_empty(),
        "published 但未开放 discoverable 的能力不得可见"
    );

    publish_through_admin_surface(&h, "order.daily_summary", true);
    // 同一进程、同一个 McpServer，没有任何重建：下一个请求就能搜到。
    let ids = search(&h.server, "order.daily_summary").await;
    assert_eq!(ids, vec!["order.daily_summary".to_string()]);

    let described = describe(&h.server, "order.daily_summary").await;
    assert_eq!(described["result"]["isError"], json!(false));
    let content = &described["result"]["content"];
    assert_eq!(content["kind"], json!("template"));
    assert_eq!(content["source"], json!("managed"));
    assert_eq!(content["group"], json!("db.read.analytics"));
    let schema = &content["params_schema"];
    assert_eq!(schema["type"], json!("object"));
    assert_eq!(schema["properties"]["start_date"]["type"], json!("string"));
    assert_eq!(
        schema["properties"]["start_date"]["format"],
        json!("date-time")
    );
    assert_eq!(
        schema["required"],
        json!(["start_date", "end_date"]),
        "声明为 required 的参数必须进 required 列表"
    );

    // 发布新版本（只改说明，SQL 未变）：无需重启，下一次 describe 就是新定义。
    let mut v2 = template_value();
    v2["description"] = json!("按日汇总订单（v2）");
    let receipt = h.registry.preflight(&v2).expect("preflight");
    h.registry
        .publish_version(
            "order.daily_summary",
            &v2,
            "ap-v2",
            "alice",
            "bob",
            &receipt,
        )
        .expect("publish_version");
    assert_eq!(
        h.store
            .get("order.daily_summary")
            .expect("store")
            .expect("record")
            .version,
        2
    );
    let republished = describe(&h.server, "order.daily_summary").await;
    assert_eq!(
        republished["result"]["content"]["one_line"],
        json!("按日汇总订单（v2）"),
        "发布新版本后下一个请求必须看到新定义"
    );

    // 可调用：db_run_template 与发现面同一判定。
    let ran = run_template(&h.server, "order.daily_summary").await;
    assert_eq!(ran["result"]["isError"], json!(false), "{ran}");
    assert_eq!(ran["result"]["cache"], json!("miss"));

    // 下架（同时关闭可发现与可调用）：同一进程内立刻从发现面与调用面消失。
    h.registry
        .set_visibility("order.daily_summary", Some(false), Some(false), "alice")
        .expect("hide");
    assert!(
        search(&h.server, "order.daily_summary").await.is_empty(),
        "改可见性后下一个请求必须看不到"
    );
    assert_eq!(
        err_code(&describe(&h.server, "order.daily_summary").await),
        "MCP_CAPABILITY_NOT_FOUND"
    );
    assert_eq!(
        err_code(&run_template(&h.server, "order.daily_summary").await),
        "MCP_CAPABILITY_NOT_FOUND"
    );
}

/// §4.4.1 / D3：`published ∧ discoverable=false ∧ callable=true` 是合法的系统直调状态——
/// 不可见不等于不可调用；只有 callable=false 才是"停用调用"。
#[tokio::test]
async fn hidden_but_callable_template_stays_callable() {
    let h = harness();
    let id = "order.daily_summary";
    h.registry.register(&template_value()).expect("register");
    h.registry.submit_for_review(id, "alice").expect("submit");
    h.registry.publish(id, "ap-pub", "bob").expect("publish");
    h.registry
        .set_visibility(id, Some(false), Some(true), "alice")
        .expect("visibility");

    assert!(search(&h.server, id).await.is_empty(), "不可发现");
    assert_eq!(
        err_code(&describe(&h.server, id).await),
        "MCP_CAPABILITY_NOT_FOUND"
    );
    let ran = run_template(&h.server, id).await;
    assert_eq!(
        ran["result"]["isError"],
        json!(false),
        "可调用性不得依赖 discoverable：{ran}"
    );

    // 停用调用（callable=false）后必须拒绝，且与不存在同构。
    h.registry
        .set_visibility(id, None, Some(false), "alice")
        .expect("visibility");
    assert_eq!(
        err_code(&run_template(&h.server, id).await),
        "MCP_CAPABILITY_NOT_FOUND"
    );
}

#[tokio::test]
async fn draft_capability_is_invisible_to_discovery() {
    let h = harness();
    h.store
        .register(
            "cap.draft_only",
            "tool",
            "db.read.core",
            "read",
            json!({"description":"草案能力"}),
        )
        .expect("register");

    assert!(search(&h.server, "cap.draft_only").await.is_empty());
    assert_eq!(
        err_code(&describe(&h.server, "cap.draft_only").await),
        "MCP_CAPABILITY_NOT_FOUND"
    );
}

#[tokio::test]
async fn published_capability_outside_scope_is_invisible() {
    let h = harness();
    h.store
        .register(
            "cap.admin_only",
            "tool",
            "db.admin.schema",
            "read",
            json!({"description":"管理组能力"}),
        )
        .expect("register");
    h.store
        .submit_for_review("cap.admin_only", "alice")
        .expect("submit");
    h.store
        .promote_to_published("cap.admin_only", "ap-pub", "bob")
        .expect("publish");
    h.store
        .set_visibility("cap.admin_only", Some(true), Some(true), "alice")
        .expect("visibility");

    // 已发布 + 可发现，但组不在 scope 内：搜不到、describe 同构为不存在、组索引不出现。
    assert!(search(&h.server, "cap.admin_only").await.is_empty());
    assert_eq!(
        err_code(&describe(&h.server, "cap.admin_only").await),
        "MCP_CAPABILITY_NOT_FOUND"
    );
    let groups = list_groups(&h.server).await;
    assert!(
        !groups.contains(&"db.admin.schema".to_string()),
        "越界组不得出现在组索引：{groups:?}"
    );

    // db_run_template 与发现面同一判定：越界组的模板即使已发布已开放，也调不到。
    let mut admin_template = template_value();
    admin_template["template_id"] = json!("order.admin_only");
    admin_template["group"] = json!("db.admin.schema");
    h.registry.register(&admin_template).expect("register");
    h.registry
        .submit_for_review("order.admin_only", "alice")
        .expect("submit");
    h.registry
        .publish("order.admin_only", "ap-pub", "bob")
        .expect("publish");
    publish_through_admin_surface(&h, "order.admin_only", true);
    assert!(search(&h.server, "order.admin_only").await.is_empty());
    assert_eq!(
        err_code(&run_template(&h.server, "order.admin_only").await),
        "MCP_CAPABILITY_NOT_FOUND",
        "越界组的模板不得可调用"
    );
}

#[tokio::test]
async fn builtin_ids_reject_every_maintenance_write() {
    let h = harness();
    // register 用内置 id 必须被拒：否则维护通道能影子覆盖内置工具定义。
    let e = h
        .store
        .register(
            "db_query_readonly",
            "tool",
            "db.read.core",
            "read",
            json!({"description":"影子覆盖"}),
        )
        .unwrap_err();
    assert_eq!(e.code, "MCP_BUILTIN_IMMUTABLE");

    // 模板层（registry）同样不能对内置 id 改可见性。
    let e = h
        .registry
        .set_visibility("db_run_template", Some(false), None, "alice")
        .unwrap_err();
    assert_eq!(e.code, "MCP_BUILTIN_IMMUTABLE");

    // 内置能力仍然可被检索/描述：拒绝写不等于拒绝读。
    let described = describe(&h.server, "db_query_readonly").await;
    assert_eq!(described["result"]["content"]["source"], json!("builtin"));
}

#[tokio::test]
async fn tools_list_keeps_one_copy_of_each_builtin() {
    let h = harness();
    let names = tool_names(&h.server).await;
    assert_eq!(
        names.len(),
        RESIDENT_TOOL_NAMES.len(),
        "空注册表时 tools/list 只应有常驻工具：{names:?}"
    );
    for builtin in RESIDENT_TOOL_NAMES {
        assert_eq!(
            names.iter().filter(|n| n.as_str() == *builtin).count(),
            1,
            "{builtin} 不得重复出现：{names:?}"
        );
    }

    // describe 内置工具：schema 与 tools/list 同源（tool_definitions），且标记 builtin。
    let described = describe(&h.server, "db_query_readonly").await;
    assert_eq!(described["result"]["isError"], json!(false));
    let content = &described["result"]["content"];
    assert_eq!(content["source"], json!("builtin"));
    assert_eq!(content["kind"], json!("tool"));
    assert_eq!(
        content["params_schema"]["properties"]["sql"]["type"],
        json!("string")
    );

    // 常驻工具不是业务组：即使可见集里只有 5 个 builtin，组索引也必须为空。
    let groups = list_groups(&h.server).await;
    assert!(
        !groups.contains(&"basic".to_string()),
        "list_groups 不得出现 basic 伪组：{groups:?}"
    );
    assert!(groups.is_empty(), "空注册表的组索引应为空：{groups:?}");
}

#[tokio::test]
async fn endpoint_capability_projection_never_panics_on_missing_schema() {
    let h = harness();
    for (id, definition) in [
        ("cap.endpoint_bare", json!({"description":"裸端点"})),
        (
            "cap.endpoint_declared",
            json!({"description":"声明端点","inputSchema":{"type":"object","properties":{"day":{"type":"string"}}}}),
        ),
    ] {
        h.store
            .register(id, "endpoint", "db.read.core", "read", definition)
            .expect("register endpoint");
        h.store.submit_for_review(id, "alice").expect("submit");
        h.store
            .promote_to_published(id, "ap-pub", "bob")
            .expect("publish");
        h.store
            .set_visibility(id, Some(true), Some(true), "alice")
            .expect("visibility");
    }

    let bare = describe(&h.server, "cap.endpoint_bare").await;
    assert_eq!(bare["result"]["isError"], json!(false));
    assert_eq!(
        bare["result"]["content"]["params_schema"],
        json!({"type": "object", "properties": {}}),
        "无 schema 的 endpoint 退化为空对象 schema（不 panic）"
    );
    assert_eq!(bare["result"]["content"]["one_line"], json!("裸端点"));

    let declared = describe(&h.server, "cap.endpoint_declared").await;
    assert_eq!(
        declared["result"]["content"]["params_schema"]["properties"]["day"]["type"],
        json!("string"),
        "定义自带的 inputSchema 必须原样使用"
    );
}
