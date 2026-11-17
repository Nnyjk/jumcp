//! 可安装 MCP 包：目录（Discover）→ 安装 → 卸载。
//!
//! 覆盖点：目录页的本机状态（stdio 开关 / 二进制是否存在 / 已安装列表）、安装写出的
//! 上游配置（env、包坐标、默认 id/name）、必填与冲突的错误语义，以及卸载只能删本包的安装。
//!
//! 二进制存在性走 `JUMCP_MCP_BUNDLED_BIN_DIR` 指向的临时目录（占位文件），不往 target
//! 目录写东西；该变量是进程级的，所以本文件的用例统一用 [`StagedBundled`] 串行。

use crate::fixtures::*;
use axum::http::StatusCode;
use serde_json::{Value, json};
use std::ffi::OsString;
use std::sync::{Mutex, MutexGuard};

/// 本文件所有用例都会改 `JUMCP_MCP_BUNDLED_BIN_DIR`，必须互斥执行。
static ENV_LOCK: Mutex<()> = Mutex::new(());

struct StagedBundled {
    _lock: MutexGuard<'static, ()>,
    _dir: tempfile::TempDir,
    previous_dir: Option<OsString>,
    previous_path: Option<OsString>,
}

impl StagedBundled {
    /// `present = true` 时在临时目录放一个占位可执行文件，等价于"随包分发已就位"。
    fn new(present: bool) -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        if present {
            let name = if cfg!(windows) {
                "jumcp-mcp-clickhouse.exe"
            } else {
                "jumcp-mcp-clickhouse"
            };
            std::fs::write(dir.path().join(name), b"placeholder").expect("placeholder");
        }
        let previous_dir = std::env::var_os("JUMCP_MCP_BUNDLED_BIN_DIR");
        let previous_path = std::env::var_os("PATH");
        // SAFETY: 同一进程内的环境改动都在 ENV_LOCK 下进行；本测试二进制里的其它用例
        // 不走 PATH 起进程（fake 上游是进程内 axum）。
        unsafe {
            std::env::set_var("JUMCP_MCP_BUNDLED_BIN_DIR", dir.path());
            // PATH 也换成同一个空目录：`cargo test` 会把 target/debug 放进 PATH，
            // 开发机上编过连接器时"二进制缺失"就无法验证了。三层查找全部落空才是真的缺失。
            std::env::set_var("PATH", dir.path());
        }
        Self {
            _lock: lock,
            _dir: dir,
            previous_dir,
            previous_path,
        }
    }
}

impl Drop for StagedBundled {
    fn drop(&mut self) {
        // SAFETY: 仍持有 ENV_LOCK，见 `new`。
        unsafe {
            match self.previous_dir.take() {
                Some(value) => std::env::set_var("JUMCP_MCP_BUNDLED_BIN_DIR", value),
                None => std::env::remove_var("JUMCP_MCP_BUNDLED_BIN_DIR"),
            }
            match self.previous_path.take() {
                Some(value) => std::env::set_var("PATH", value),
                None => std::env::remove_var("PATH"),
            }
        }
    }
}

async fn install(h: &Harness, id: &str, body: Value) -> (StatusCode, Value) {
    call(
        &h.app,
        "POST",
        &format!("/admin/mcp/catalog/{id}/install"),
        Some(ALICE_TOKEN),
        Some(body),
    )
    .await
}

fn dsn(value: &str) -> Value {
    json!({"group": "db.read.analytics", "config": {"dsn": value}})
}

#[tokio::test]
async fn catalog_reports_host_state_and_installations() {
    let staged = StagedBundled::new(true);
    let h = harness_with(true, false);

    let (status, body) = call(&h.app, "GET", "/admin/mcp/catalog", Some(ALICE_TOKEN), None).await;
    assert_eq!(status, StatusCode::OK, "目录读取失败: {body}");
    let item = &body["items"][0];
    assert_eq!(item["id"], json!("clickhouse-basic"));
    assert_eq!(item["name"], json!("ClickHouse 基础查询"));
    assert_eq!(item["version"], json!("0.1.0"));
    assert_eq!(item["transport"], json!("stdio"));
    assert_eq!(item["requires_stdio"], json!(true));
    assert_eq!(item["stdio_enabled"], json!(true));
    assert_eq!(item["binary_present"], json!(true));
    assert_eq!(item["default_upstream_id"], json!("clickhouse"));
    assert_eq!(item["installations"], json!([]));
    assert_eq!(item["tools"].as_array().map(Vec::len), Some(3));
    let field = &item["fields"][0];
    assert_eq!(field["key"], json!("dsn"));
    assert_eq!(field["kind"], json!("password"));
    assert_eq!(field["required"], json!(true));
    assert_eq!(field["secret"], json!(true));
    // env 是实现细节（写进哪个变量由服务端决定），不下发到前端。
    assert!(field.get("env").is_none(), "fields 不得暴露 env: {field}");

    let allowed = &item["fields"][1];
    assert_eq!(allowed["key"], json!("allowed_schemas"));
    assert_eq!(allowed["label"], json!("允许的库（逗号分隔）"));
    assert_eq!(allowed["kind"], json!("text"));
    assert_eq!(allowed["required"], json!(false));
    assert_eq!(allowed["secret"], json!(false));
    assert_eq!(allowed["placeholder"], json!("analytics,ods"));
    assert!(
        allowed.get("env").is_none(),
        "fields 不得暴露 env: {allowed}"
    );
    // 控件形态只有实际有载体的几种：没有候选列表就不该出现 select。
    let kinds: Vec<&str> = item["fields"]
        .as_array()
        .expect("fields")
        .iter()
        .map(|field| field["kind"].as_str().expect("kind"))
        .collect();
    assert_eq!(kinds, ["password", "text"]);

    let (status, created) = install(&h, "clickhouse-basic", dsn("http://h:1/db")).await;
    assert_eq!(status, StatusCode::OK, "安装失败: {created}");

    let (status, body) = call(&h.app, "GET", "/admin/mcp/catalog", Some(ALICE_TOKEN), None).await;
    assert_eq!(status, StatusCode::OK);
    let installed = &body["items"][0]["installations"][0];
    assert_eq!(installed["upstream_id"], json!("clickhouse"));
    assert_eq!(installed["name"], json!("ClickHouse 基础查询"));
    assert_eq!(installed["group"], json!("db.read.analytics"));
    assert_eq!(installed["catalog_version"], json!("0.1.0"));
    assert_eq!(installed["status"], json!("unknown"), "未探测就不该报可达");
    assert_eq!(installed["tool_count"], json!(0));

    // 开关关闭时同一台主机显示"不可安装"，但已安装记录仍要看得到。
    drop(staged);
    let closed = harness_with(false, false);
    let (status, body) = call(
        &closed.app,
        "GET",
        "/admin/mcp/catalog",
        Some(ALICE_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["items"][0]["stdio_enabled"], json!(false));
}

#[tokio::test]
async fn install_writes_package_configuration_and_masks_secrets() {
    let _staged = StagedBundled::new(true);
    let h = harness_with(true, false);
    let secret_dsn = "http://reader:s3cret@10.0.0.5:8123/analytics";

    let (status, body) = install(&h, "clickhouse-basic", dsn(secret_dsn)).await;
    assert_eq!(status, StatusCode::OK, "安装失败: {body}");
    let upstream = &body["upstream"];
    // 未给 upstream_id/name：用包定义里的默认值。
    assert_eq!(upstream["id"], json!("clickhouse"));
    assert_eq!(upstream["name"], json!("ClickHouse 基础查询"));
    assert_eq!(upstream["transport"], json!("stdio"));
    assert_eq!(upstream["command"], json!("jumcp-mcp-clickhouse"));
    assert_eq!(upstream["args"], json!([]));
    assert_eq!(upstream["url"], Value::Null);
    assert_eq!(upstream["category"], json!("custom"));
    assert_eq!(upstream["read_only"], json!(true));
    assert_eq!(upstream["enabled"], json!(true));
    assert_eq!(upstream["timeout_ms"], json!(30000));
    assert_eq!(upstream["group"], json!("db.read.analytics"));
    assert_eq!(upstream["catalog_id"], json!("clickhouse-basic"));
    assert_eq!(upstream["catalog_version"], json!("0.1.0"));
    // 输出层脱敏：DSN 也是凭据。
    assert_eq!(upstream["env"]["JUMCP_CLICKHOUSE_DSN"], json!("***"));
    // 未填 allowed_schemas 时不写空串键。
    assert_eq!(
        upstream["env"],
        json!({"JUMCP_CLICKHOUSE_DSN": "***"}),
        "未配置的可选字段不得留下空串 env"
    );

    // 安装返回体与 GET /admin/upstreams/{id} 同一结构（前端复用同一个渲染）。
    let (status, item) = call(
        &h.app,
        "GET",
        "/admin/upstreams/clickhouse",
        Some(ALICE_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["upstream"], item, "安装返回体必须与单项查询一致");

    // 落库是真实 DSN，脱敏只发生在输出层。
    let stored = h.store.upstream("clickhouse").expect("store").expect("row");
    assert_eq!(stored.env["JUMCP_CLICKHOUSE_DSN"], secret_dsn);
    assert_eq!(stored.catalog_id.as_deref(), Some("clickhouse-basic"));
    assert_eq!(stored.catalog_version.as_deref(), Some("0.1.0"));

    // 覆盖 id/name/enabled 时按请求落库。
    let (status, renamed) = install(
        &h,
        "clickhouse-basic",
        json!({
            "upstream_id": "clickhouse-alt",
            "name": "备用 ClickHouse",
            "group": "db.read.core",
            "enabled": false,
            "config": {"dsn": "http://h:2/db"}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "安装失败: {renamed}");
    assert_eq!(renamed["upstream"]["id"], json!("clickhouse-alt"));
    assert_eq!(renamed["upstream"]["name"], json!("备用 ClickHouse"));
    assert_eq!(renamed["upstream"]["enabled"], json!(false));
    assert_eq!(renamed["upstream"]["group"], json!("db.read.core"));

    // 组必须存在于有效策略。
    let (status, error) = install(
        &h,
        "clickhouse-basic",
        json!({"upstream_id": "clickhouse-bad-group", "group": "no.such.group", "config": {"dsn": "http://h:3/db"}}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert_eq!(error["error"]["code"], json!("MCP_UPSTREAM_INVALID"));
}

#[tokio::test]
async fn install_rejects_missing_required_field_without_row() {
    let _staged = StagedBundled::new(true);
    let h = harness_with(true, false);

    // 空白等同未填：必填字段不接受"看起来有值"的空串。
    let (status, error) = install(&h, "clickhouse-basic", dsn("   ")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert_eq!(error["error"]["code"], json!("MCP_VALIDATION_ERROR"));
    let message = error["error"]["message"].as_str().expect("message");
    assert!(
        message.contains("ClickHouse DSN"),
        "报错要指名字段 label: {message}"
    );
    assert_eq!(error["error"]["data"]["field"], json!("dsn"));
    assert!(
        h.store.upstream("clickhouse").expect("store").is_none(),
        "校验失败不得建行"
    );

    // group 也是必填：缺了就明确说是 group。
    let (status, error) = install(
        &h,
        "clickhouse-basic",
        json!({"config": {"dsn": "http://h:1/db"}}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"]["code"], json!("MCP_VALIDATION_ERROR"));
    assert!(
        error["error"]["message"]
            .as_str()
            .expect("message")
            .contains("group"),
        "报错要指明 group: {error}"
    );
    assert!(h.store.upstream("clickhouse").expect("store").is_none());
}

#[tokio::test]
async fn install_writes_allowed_schemas_only_when_present() {
    let _staged = StagedBundled::new(true);
    let h = harness_with(true, false);

    let (status, body) = install(&h, "clickhouse-basic", dsn("http://h:1/db")).await;
    assert_eq!(status, StatusCode::OK, "安装失败: {body}");
    let stored = h.store.upstream("clickhouse").expect("store").expect("row");
    assert_eq!(
        stored.env.keys().collect::<Vec<_>>(),
        vec!["JUMCP_CLICKHOUSE_DSN"],
        "未填可选字段时不得写空串 env"
    );

    let (status, body) = install(
        &h,
        "clickhouse-basic",
        json!({
            "upstream_id": "clickhouse-scoped",
            "group": "db.read.analytics",
            "config": {"dsn": "http://h:2/db", "allowed_schemas": "analytics,ods"}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "安装失败: {body}");
    let stored = h
        .store
        .upstream("clickhouse-scoped")
        .expect("store")
        .expect("row");
    assert_eq!(
        stored.env["JUMCP_CLICKHOUSE_ALLOWED_SCHEMAS"],
        "analytics,ods"
    );
}

#[tokio::test]
async fn install_rejects_unknown_config_keys() {
    let _staged = StagedBundled::new(true);
    let h = harness_with(true, false);

    let (status, error) = install(
        &h,
        "clickhouse-basic",
        json!({
            "group": "db.read.analytics",
            "config": {"dsn": "http://h:1/db", "foo": "bar"}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert_eq!(error["error"]["code"], json!("MCP_VALIDATION_ERROR"));
    assert_eq!(error["error"]["message"], json!("未知配置字段：foo"));
    assert_eq!(error["error"]["data"]["field"], json!("foo"));
    assert_eq!(error["error"]["data"]["unknown"], json!(["foo"]));
    assert!(
        h.store.upstream("clickhouse").expect("store").is_none(),
        "未知配置键不得建行"
    );

    // 大小写笔误同样是未知键：静默丢弃会让管理员以为白名单已经生效。
    let (status, error) = install(
        &h,
        "clickhouse-basic",
        json!({
            "group": "db.read.analytics",
            "config": {"dsn": "http://h:1/db", "Allowed_Schemas": "analytics"}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert_eq!(error["error"]["code"], json!("MCP_VALIDATION_ERROR"));
    assert_eq!(
        error["error"]["message"],
        json!("未知配置字段：Allowed_Schemas")
    );
    assert_eq!(
        error["error"]["data"]["unknown"],
        json!(["Allowed_Schemas"])
    );
    assert!(h.store.upstream("clickhouse").expect("store").is_none());
}

#[tokio::test]
async fn install_missing_required_field_precedes_unknown_keys() {
    let _staged = StagedBundled::new(true);
    let h = harness_with(true, false);

    // 门禁顺序：缺必填字段先于未知键报出（管理员先补 dsn，再谈其它）。
    let (status, error) = install(
        &h,
        "clickhouse-basic",
        json!({
            "group": "db.read.analytics",
            "config": {"foo": "bar"}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert_eq!(error["error"]["code"], json!("MCP_VALIDATION_ERROR"));
    assert_eq!(error["error"]["data"]["field"], json!("dsn"));
}

#[tokio::test]
async fn install_gates_on_stdio_switch_then_binary() {
    // 二进制缺失：即使开关打开也必须拒绝，且不建行。
    let _staged = StagedBundled::new(false);
    let h = harness_with(true, false);
    let (status, error) = install(&h, "clickhouse-basic", dsn("http://h:1/db")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert_eq!(error["error"]["code"], json!("MCP_UPSTREAM_BINARY_MISSING"));
    assert!(
        h.store.upstream("clickhouse").expect("store").is_none(),
        "缺二进制不得建行"
    );

    // 开关关闭优先报开关：连必填字段一起缺时也必须是开关错误。否则管理员会去补 dsn，
    // 真正挡住安装的却是部署事实。
    let closed = harness_with(false, false);
    let (status, error) = install(
        &closed,
        "clickhouse-basic",
        json!({"group": "db.read.analytics"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert_eq!(error["error"]["code"], json!("MCP_UPSTREAM_STDIO_DISABLED"));
    assert!(
        error["error"]["message"]
            .as_str()
            .expect("message")
            .contains("JUMCP_MCP_UPSTREAM_STDIO"),
        "报错要说清怎么打开: {error}"
    );
    let (status, error) = install(&closed, "clickhouse-basic", dsn("http://h:1/db")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert_eq!(error["error"]["code"], json!("MCP_UPSTREAM_STDIO_DISABLED"));
    assert!(
        closed
            .store
            .upstream("clickhouse")
            .expect("store")
            .is_none(),
        "开关关闭不得建行"
    );

    // 目录不存在：404，且与"上游不存在"区分开。
    let (status, error) = install(&closed, "no-such-package", dsn("http://h:1/db")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{error}");
    assert_eq!(error["error"]["code"], json!("MCP_CATALOG_NOT_FOUND"));
}

#[tokio::test]
async fn install_conflicts_on_duplicate_upstream_id() {
    let _staged = StagedBundled::new(true);
    let h = harness_with(true, false);

    let (status, _) = install(&h, "clickhouse-basic", dsn("http://h:1/db")).await;
    assert_eq!(status, StatusCode::OK);
    let (status, error) = install(&h, "clickhouse-basic", dsn("http://h:2/db")).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["error"]["code"], json!("MCP_UPSTREAM_CONFLICT"));

    // 冲突不得覆盖已有配置。
    let stored = h.store.upstream("clickhouse").expect("store").expect("row");
    assert_eq!(stored.env["JUMCP_CLICKHOUSE_DSN"], "http://h:1/db");
}

#[tokio::test]
async fn uninstall_only_deletes_this_packages_installation() {
    let _staged = StagedBundled::new(true);
    let h = harness_with(true, false);
    let (status, _) = install(&h, "clickhouse-basic", dsn("http://h:1/db")).await;
    assert_eq!(status, StatusCode::OK);

    // 手工创建的上游不归任何包：卸载入口不能变成"删任意上游"的后门。
    let (status, manual) =
        create_upstream(&h, upstream_body("manual-one", "http://127.0.0.1:9/mcp")).await;
    assert_eq!(status, StatusCode::OK, "{manual}");
    let (status, error) = call(
        &h.app,
        "DELETE",
        "/admin/mcp/catalog/clickhouse-basic/install/manual-one",
        Some(ALICE_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{error}");
    assert_eq!(error["error"]["code"], json!("MCP_UPSTREAM_NOT_FOUND"));
    assert!(
        h.store.upstream("manual-one").expect("store").is_some(),
        "非本包安装的上游必须留着"
    );

    // 包不存在：404，不能靠猜测路径删东西。
    let (status, error) = call(
        &h.app,
        "DELETE",
        "/admin/mcp/catalog/no-such-package/install/clickhouse",
        Some(ALICE_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error["error"]["code"], json!("MCP_CATALOG_NOT_FOUND"));

    let (status, deleted) = call(
        &h.app,
        "DELETE",
        "/admin/mcp/catalog/clickhouse-basic/install/clickhouse",
        Some(ALICE_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{deleted}");
    assert_eq!(deleted, json!({"deleted": true}));
    assert!(h.store.upstream("clickhouse").expect("store").is_none());
    assert!(
        h.store.upstream("manual-one").expect("store").is_some(),
        "卸载只影响本包的安装"
    );

    // 再删一次：已经不存在。
    let (status, error) = call(
        &h.app,
        "DELETE",
        "/admin/mcp/catalog/clickhouse-basic/install/clickhouse",
        Some(ALICE_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error["error"]["code"], json!("MCP_UPSTREAM_NOT_FOUND"));
}

#[tokio::test]
async fn put_keeps_package_coordinates() {
    let _staged = StagedBundled::new(true);
    let h = harness_with(true, false);
    let (status, _) = install(&h, "clickhouse-basic", dsn("http://h:1/db")).await;
    assert_eq!(status, StatusCode::OK);

    // 普通编辑不能抹掉安装来源：否则上游"孤儿化"，卸载入口再也删不掉它。
    let (status, updated) = call(
        &h.app,
        "PUT",
        "/admin/upstreams/clickhouse",
        Some(ALICE_TOKEN),
        Some(json!({
            "name": "改过的名字",
            "transport": "stdio",
            "url": Value::Null,
            "command": "jumcp-mcp-clickhouse",
            "args": [],
            "env": {"JUMCP_CLICKHOUSE_DSN": "***"},
            "group": "db.read.analytics",
            "enabled": true,
            "timeout_ms": 30000
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["name"], json!("改过的名字"));
    assert_eq!(updated["catalog_id"], json!("clickhouse-basic"));
    assert_eq!(updated["catalog_version"], json!("0.1.0"));
    // 脱敏占位符不能写库。
    let stored = h.store.upstream("clickhouse").expect("store").expect("row");
    assert_eq!(stored.env["JUMCP_CLICKHOUSE_DSN"], "http://h:1/db");
}
