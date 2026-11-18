//! 三个 MCP 工具的定义与实现：`query` / `list_databases` / `list_tables`。
//!
//! 名字用短名：上游 router 对外暴露成 `<server_id>__<tool>`，这里再带前缀会重复。
//!
//! 只读语义三层不削弱：
//! - 用户 SQL 必过 [`jumcp_sqlguard::validate`]（ClickHouse 方言 + `allowed_schemas`）；
//! - 执行走 connector（`readonly=1` 等设置在 agent 内固定，本 crate 无法关掉）；
//! - `system.*` 内的目录查询由本服务自己写死，用户输入只作为**绑定参数**进入，绝不拼接。
//!
//! 无库名前缀的表名在 ClickHouse 方言下无法静态判定落在哪个库（见
//! `jumcp_sqlguard::Dialect::is_connection_scoped`），因此 `query` 在 `allowed_schemas`
//! 非空且调用方未指定 `database` 时，把本次请求的会话库钉到白名单首项——否则 DSN 默认库
//! 会绕过白名单。
//!
//! 例外说明：`list_databases` / `list_tables` 的 SQL 是服务端常量，不含用户输入，
//! 因此不再跑 sqlguard——否则 `allowed_schemas` 非空时连 `system.tables` 都会被判越界，
//! 目录类工具会直接不可用。用户输入只以参数形式传入（`?` 占位符）；
//! 结果的库名一律按 `allowed_schemas` 收窄（见 [`Server::in_scope`]），越界 schema 不会出现在输出里。

use jumcp_connector_protocol::QueryRequest;
use jumcp_sqlguard::{Dialect, validate};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Instant;

use crate::Server;
use crate::executor::ToolError;

/// 查询工具名。
pub const TOOL_QUERY: &str = "query";
/// 库清单工具名。
pub const TOOL_LIST_DATABASES: &str = "list_databases";
/// 表清单工具名。
pub const TOOL_LIST_TABLES: &str = "list_tables";

/// 工具清单里的三处短名。
pub const TOOL_NAMES: [&str; 3] = [TOOL_QUERY, TOOL_LIST_DATABASES, TOOL_LIST_TABLES];

/// `max_rows` 缺省值。
pub const MAX_ROWS_DEFAULT: i64 = 200;
/// `max_rows` 上限（与 `jumcp-sqlguard` 的行数上限一致）。
pub const MAX_ROWS_CEILING: i64 = 1000;
/// 单格字符上限缺省值。
pub const CELL_CHAR_LIMIT_DEFAULT: i64 = 2000;
/// 单格字符上限（与 `jumcp-sqlguard` 的夹紧上限一致）。
pub const CELL_CHAR_LIMIT_CEILING: i64 = 4000;
/// 单格续读偏移上限（与 `jumcp-sqlguard` 的夹紧上限一致）。
pub const CELL_CHAR_OFFSET_CEILING: i64 = 1_000_000;
/// 单条查询超时上限（毫秒）：再大的入参也不让一条查询瘫住会话。
pub const TIMEOUT_CEILING_MS: i64 = 300_000;
/// 目录类工具的行数上限（system 表很小；取上限即可）。
const CATALOG_MAX_ROWS: i64 = MAX_ROWS_CEILING;

/// `tools/list` 的工具定义。
pub fn definitions() -> Vec<Value> {
    vec![
        json!({
            "name": TOOL_QUERY,
            "description": "执行单条只读 ClickHouse SQL（SELECT/UNION/WITH），返回列、行与截断信息。\
        多语句、写操作、外部访问函数与越界 schema 会被服务端拒绝。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "sql": {
                        "type": "string",
                        "description": "单条只读 SQL（ClickHouse 方言）；`?` 为位置参数占位符，按 params 顺序绑定"
                    },
                    "params": {
                        "type": "array",
                        "items": {},
                        "description": "按 SQL 中 `?` 出现顺序绑定的标量参数（bool/number/string/null）"
                    },
                    "max_rows": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_ROWS_CEILING,
                        "default": MAX_ROWS_DEFAULT,
                        "description": "返回行数上限，缺省 200，夹紧到 1..1000"
                    },
                    "database": {
                        "type": "string",
                        "description": "本次查询的默认库（无库名前缀的表落在这里）；不给时：配置了允许的库则用其中第一个，否则用 DSN 指定的库。显式给值必须落在允许的库内"
                    },
                    "cell_char_limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": CELL_CHAR_LIMIT_CEILING,
                        "default": CELL_CHAR_LIMIT_DEFAULT,
                        "description": "单个字符串单元的字符上限，缺省 2000，夹紧到 1..4000"
                    },
                    "cell_char_offset": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "长值续读起点（配合 next_offsets 翻页），缺省 0"
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "本次查询超时（毫秒）；缺省取服务端 JUMCP_CLICKHOUSE_TIMEOUT_MS"
                    }
                },
                "required": ["sql"]
            }
        }),
        json!({
            "name": TOOL_LIST_DATABASES,
            "description": "列出当前 ClickHouse 实例可见的数据库名（只读目录查询）；`truncated` 表示结果被行数上限截断。",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }),
        json!({
            "name": TOOL_LIST_TABLES,
            "description": "列出表及其引擎（只读目录查询）。可按库过滤与按表名 LIKE 过滤；\
        过滤值以绑定参数传入，不会拼进 SQL；`truncated` 表示结果被行数上限截断。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "database": {
                        "type": "string",
                        "description": "只看该库；不给则跨所有可见库"
                    },
                    "like": {
                        "type": "string",
                        "description": "表名 LIKE 模式，例如 %order%；不给则不过滤"
                    }
                }
            }
        }),
    ]
}

/// `query` 入参。
#[derive(Debug, Default, Deserialize)]
pub struct QueryArgs {
    /// 用户 SQL（必填）。
    pub sql: String,
    /// 位置参数。
    #[serde(default)]
    pub params: Vec<Value>,
    /// 行数上限。
    #[serde(default)]
    pub max_rows: Option<i64>,
    /// 默认库。
    #[serde(default)]
    pub database: Option<String>,
    /// 单格字符上限。
    #[serde(default)]
    pub cell_char_limit: Option<i64>,
    /// 单格续读起点。
    #[serde(default)]
    pub cell_char_offset: Option<i64>,
    /// 超时（毫秒）。
    #[serde(default)]
    pub timeout_ms: Option<i64>,
}

/// `list_tables` 入参。
#[derive(Debug, Default, Deserialize)]
pub struct ListTablesArgs {
    /// 库名过滤。
    #[serde(default)]
    pub database: Option<String>,
    /// 表名 LIKE 模式。
    #[serde(default)]
    pub like: Option<String>,
}

/// 行数上限夹紧：缺省 200，越界夹到 1..1000。
pub fn clamp_max_rows(value: Option<i64>) -> i64 {
    value.unwrap_or(MAX_ROWS_DEFAULT).clamp(1, MAX_ROWS_CEILING)
}

/// 单格字符上限夹紧：缺省 2000，越界夹到 1..4000。
pub fn clamp_cell_char_limit(value: Option<i64>) -> usize {
    value
        .unwrap_or(CELL_CHAR_LIMIT_DEFAULT)
        .clamp(1, CELL_CHAR_LIMIT_CEILING) as usize
}

/// 单格续读起点夹紧：缺省 0，上限 100 万。
pub fn clamp_cell_char_offset(value: Option<i64>) -> usize {
    value.unwrap_or(0).clamp(0, CELL_CHAR_OFFSET_CEILING) as usize
}

/// 超时解析：入参 > 0 时生效（上限 5 分钟），否则用服务端缺省值。
pub fn resolve_timeout_ms(value: Option<i64>, fallback: i64) -> i64 {
    match value {
        Some(value) if value > 0 => value.min(TIMEOUT_CEILING_MS),
        _ => fallback,
    }
}

/// `database` 入参归一化：空白视为「不给」。
fn normalize_database(value: Option<String>) -> Option<String> {
    value
        .map(|database| database.trim().to_string())
        .filter(|database| !database.is_empty())
}

impl Server {
    /// 库名是否落在白名单内（`allowed_schemas` 为空表示不限制）。
    ///
    /// 比较不区分 ASCII 大小写，与 `jumcp_sqlguard` 的 schema 判定保持一致：否则运营者填
    /// `Analytics` 时 `analytics.t` 能过 sqlguard，目录输出和显式 `database` 参数却会被误拦。
    /// 目录工具在 SQL 侧不跑 sqlguard（见模块注释），因此也必须在这里按白名单收窄，
    /// 否则 `list_tables` 会把越界 schema 的表名一起吐出来。
    fn in_scope(&self, database: &str) -> bool {
        self.allowed_schemas().is_empty()
            || self
                .allowed_schemas()
                .iter()
                .any(|item| item.eq_ignore_ascii_case(database))
    }

    /// `query`：L4 校验 → 执行 → 整形。
    pub(crate) async fn call_query(&self, args: QueryArgs) -> Result<Value, ToolError> {
        let sql = args.sql.trim();
        if sql.is_empty() {
            return Err(ToolError::new(
                "MCP_SQL_REJECTED",
                "sql 不能为空",
                json!({"reason": "empty_sql"}),
            ));
        }
        // L4：单语句、根节点为查询、函数黑名单、schema 范围。
        validate(sql, Dialect::ClickHouse, self.allowed_schemas())
            .map_err(ToolError::from_guard)?;

        // 会话默认库同样是 schema 范围的一部分：allowed_schemas 非空时不得借 database 参数越界
        // （ClickHouse 的无前缀表名落在会话库，sqlguard 无法静态判定）。调用方没指定库时钉到
        // 白名单首项，否则请求会落到 DSN 默认库，白名单形同虚设。
        let database = match normalize_database(args.database) {
            Some(database) => {
                if !self.in_scope(&database) {
                    return Err(ToolError::schema_out_of_scope(
                        &database,
                        self.allowed_schemas(),
                    ));
                }
                Some(database)
            }
            None => self.allowed_schemas().first().cloned(),
        };

        let max_rows = clamp_max_rows(args.max_rows);
        let cell_char_limit = clamp_cell_char_limit(args.cell_char_limit);
        let cell_char_offset = clamp_cell_char_offset(args.cell_char_offset);
        let request = QueryRequest {
            sql: sql.to_string(),
            params: args.params,
            max_rows,
            database,
            timeout_ms: resolve_timeout_ms(args.timeout_ms, self.timeout_ms()),
        };

        let started = Instant::now();
        let result = self
            .executor()
            .query(&request)
            .await
            .map_err(ToolError::from_agent)?;

        let shaped = jumcp_sqlguard::shape_rows(
            &result.rows,
            max_rows as usize,
            cell_char_limit,
            cell_char_offset,
        );
        let next_offsets: Vec<Value> = shaped
            .next_offsets
            .iter()
            .map(|(row, column, next_offset)| {
                json!({"row": row, "column": column, "next_offset": next_offset})
            })
            .collect();

        Ok(json!({
            "columns": result.columns,
            "rows": shaped.rows,
            "row_count": shaped.row_count,
            "truncated": shaped.truncated || result.truncated,
            "elapsed_ms": started.elapsed().as_millis() as i64,
            "effective_max_rows": shaped.effective_max_rows,
            "next_offsets": next_offsets
        }))
    }

    /// `list_databases`：`system.databases` 目录查询。
    pub(crate) async fn call_list_databases(&self) -> Result<Value, ToolError> {
        let request = QueryRequest {
            sql: "SELECT name FROM system.databases ORDER BY name".to_string(),
            params: Vec::new(),
            max_rows: CATALOG_MAX_ROWS,
            database: None,
            timeout_ms: self.timeout_ms(),
        };
        let result = self
            .executor()
            .query(&request)
            .await
            .map_err(ToolError::from_agent)?;
        let databases: Vec<Value> = result
            .rows
            .iter()
            .filter_map(|row| row.first())
            .filter(|name| {
                name.as_str()
                    .is_some_and(|database| self.in_scope(database))
            })
            .cloned()
            .collect();
        Ok(json!({
            "databases": databases,
            "truncated": result.truncated
        }))
    }

    /// `list_tables`：`system.tables` 目录查询，过滤条件全部参数绑定。
    pub(crate) async fn call_list_tables(&self, args: ListTablesArgs) -> Result<Value, ToolError> {
        let database = normalize_database(args.database);
        if let Some(database) = &database
            && !self.in_scope(database)
        {
            return Err(ToolError::schema_out_of_scope(
                database,
                self.allowed_schemas(),
            ));
        }
        let like = args
            .like
            .map(|pattern| pattern.trim().to_string())
            .filter(|pattern| !pattern.is_empty());

        // 空串代表「该条件不生效」：占位符只做绑定，用户输入永远不会进入 SQL 文本。
        let database_filter = database.clone().unwrap_or_default();
        let like_filter = like.unwrap_or_default();
        let request = QueryRequest {
            sql: "SELECT database, name, engine FROM system.tables \
                  WHERE (? = '' OR database = ?) AND (? = '' OR name LIKE ?) \
                  ORDER BY database, name"
                .to_string(),
            params: vec![
                Value::String(database_filter.clone()),
                Value::String(database_filter),
                Value::String(like_filter.clone()),
                Value::String(like_filter),
            ],
            max_rows: CATALOG_MAX_ROWS,
            database: None,
            timeout_ms: self.timeout_ms(),
        };
        let result = self
            .executor()
            .query(&request)
            .await
            .map_err(ToolError::from_agent)?;
        let tables: Vec<Value> = result
            .rows
            .iter()
            .filter(|row| {
                row.first()
                    .and_then(Value::as_str)
                    .is_some_and(|database| self.in_scope(database))
            })
            .map(|row| {
                json!({
                    "database": cell(row, 0),
                    "name": cell(row, 1),
                    "engine": cell(row, 2)
                })
            })
            .collect();
        Ok(json!({
            "database": database.map(Value::String).unwrap_or(Value::Null),
            "tables": tables,
            "truncated": result.truncated
        }))
    }
}

/// 取一行里的第 `index` 个单元；缺列时为 `null`。
fn cell(row: &[Value], index: usize) -> Value {
    row.get(index).cloned().unwrap_or(Value::Null)
}
