//! 可安装 MCP 包的静态目录：随二进制分发的"包定义"，安装即生成一条已配置的上游记录。
//!
//! 为什么目录写在代码里、而不是从任意 URL 拉清单：安装动作会把 `command/args` 写进 stdio
//! 上游，等于把"以服务进程身份执行某个命令"交出去。目录随版本发布、与
//! `jumcp-mcp-<name>` 兄弟二进制同源，安装只填参数，不引入任何新的可执行文件来源。
//!
//! 安装产物仍是 [`McpUpstream`](super::McpUpstream)（同一套校验/审计/删除路径），
//! `catalog_id`/`catalog_version` 只回答"这条上游由哪个包装出来"，不改变运行时语义。

/// 字段输入形态：只影响管理台控件与对外 JSON 表达，不参与服务端校验。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    Password,
    Number,
}

impl FieldKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Password => "password",
            Self::Number => "number",
        }
    }
}

/// 包的一个配置字段：安装时值写进上游 env 的 `env` 变量。
pub struct InstallableField {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: FieldKind,
    /// 必填：缺值/空串即安装失败（错误信息按 `label` 指名字段）。
    pub required: bool,
    /// 敏感值：管理面回读一律脱敏成 `***`（上游侧脱敏只看 env 键名，这里只表达字段语义）。
    pub secret: bool,
    pub default: Option<&'static str>,
    pub placeholder: Option<&'static str>,
    pub help: Option<&'static str>,
    pub env: &'static str,
}

/// 包声明的工具：安装前后都只用于展示（真实工具清单仍由上游 `tools/list` 决定）。
pub struct InstallableTool {
    pub name: &'static str,
    pub summary: &'static str,
}

/// 一个可安装包：安装 = 按 `fields` 组装一条 stdio 上游。
pub struct InstallablePackage {
    pub id: &'static str,
    pub name: &'static str,
    pub summary: &'static str,
    pub version: &'static str,
    pub tags: &'static [&'static str],
    pub transport: &'static str,
    pub command: &'static str,
    pub args: &'static [&'static str],
    /// 安装时未指定 `upstream_id` 的默认值（与手动注册同一套 id 门禁）。
    pub default_upstream_id: &'static str,
    pub fields: &'static [InstallableField],
    pub tools: &'static [InstallableTool],
}

const CLICKHOUSE_FIELDS: &[InstallableField] = &[
    InstallableField {
        key: "dsn",
        label: "ClickHouse DSN",
        kind: FieldKind::Password,
        required: true,
        secret: true,
        default: None,
        placeholder: Some("http://default:密码@127.0.0.1:8123/default"),
        help: Some("格式 http(s)://用户名:密码@主机:端口/数据库；密码可留空。"),
        env: "JUMCP_CLICKHOUSE_DSN",
    },
    InstallableField {
        key: "allowed_schemas",
        label: "允许的库（逗号分隔）",
        kind: FieldKind::Text,
        required: false,
        secret: false,
        default: None,
        placeholder: Some("analytics,ods"),
        help: Some(
            "留空表示不限制；带库名的表必须落在其中，未指定库的查询钉到列表首个库；库名按实际大小写填写，比较不区分大小写；建议配合只读账号。",
        ),
        env: "JUMCP_CLICKHOUSE_ALLOWED_SCHEMAS",
    },
];

const CLICKHOUSE_TOOLS: &[InstallableTool] = &[
    InstallableTool {
        name: "query",
        summary: "执行单条只读 SQL，返回列与行。",
    },
    InstallableTool {
        name: "list_databases",
        summary: "列出当前账号可见的数据库。",
    },
    InstallableTool {
        name: "list_tables",
        summary: "列出指定数据库下的表。",
    },
];

/// 全部可安装包。顺序即管理台展示顺序；新增包只追加，不改已有 id（已安装行按 id 归属）。
const PACKAGES: &[InstallablePackage] = &[InstallablePackage {
    id: "clickhouse-basic",
    name: "ClickHouse 基础查询",
    summary: "连接 ClickHouse，提供只读 SQL 查询、数据库与表枚举工具。",
    version: "0.1.0",
    tags: &["database", "clickhouse"],
    transport: "stdio",
    command: "jumcp-mcp-clickhouse",
    args: &[],
    default_upstream_id: "clickhouse",
    fields: CLICKHOUSE_FIELDS,
    tools: CLICKHOUSE_TOOLS,
}];

pub fn catalog() -> &'static [InstallablePackage] {
    PACKAGES
}

pub fn package(id: &str) -> Option<&'static InstallablePackage> {
    PACKAGES.iter().find(|package| package.id == id)
}
