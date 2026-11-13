# DBX 委托调研：可核验量化清单

采集时间：2026-11-10。只读采集，未改代码/配置，未提交。
本文件只放事实与表格；架构结论由另一路负责。文中 `[未核验]` 表示本次未取得直接证据。

> 历史快照（2026-11-11 加注）：本清单采集于审计 v1 时期。其中「8 类 `event_type`」「`admin_change` 强制 before/after/diff/approval_id 四字段」「`verify_chain`」以及 PG executor 相关条目已被后续实现取代（现状见 `docs/audit-openness.md`）。原文保留，不再逐条更新。

---

## A. 本项目数据库面

### A1. 数据库相关代码规模（`wc -l`，commit `73c87bc`）

| 文件 | 行数 |
| --- | ---: |
| `backend/crates/jumcp-server/src/executor.rs` | 551 |
| `backend/crates/jumcp-server/src/pg_executor.rs` | 394 |
| `backend/crates/jumcp-server/src/influx_executor.rs` | 686 |
| `backend/crates/jumcp-server/src/influxql.rs` | 668 |
| `backend/crates/jumcp-server/src/mcp.rs` | 1429 |
| `backend/crates/jumcp-server/src/main.rs` | 232 |
| `backend/crates/jumcp-server/src/service.rs` | 441 |
| `backend/crates/jumcp-core/src/config.rs` | 734 |
| `backend/crates/jumcp-core/src/registry.rs` | 1133 |
| `backend/crates/jumcp-core/src/template.rs` | 1696 |
| `backend/crates/jumcp-core/src/audit.rs` | 472 |
| `backend/crates/jumcp-core/src/budget.rs` | 241 |
| **合计** | **8677** |

另有独立 crate `backend/crates/jumcp-sqlguard`（只读 SQL 校验 + 结果整形）：`dialect.rs` 114、`error.rs` 39、`lex.rs` 555、`lib.rs` 22、`shape.rs` 111、`validate.rs` 633，合计 **1474** 行。

### A2. 当前对外 tool 清单

静态来源：`backend/crates/jumcp-server/src/mcp.rs:36-76`（内置 5 个）、`mcp.rs:88-122`（admin 4 个，仅 `scope.groups` 含 `admin` 时可见/可调）。运行时 `tools/list` 还会按可见能力平铺（`flatten_threshold`，默认 40，`JUMCP_MCP_FLATTEN_THRESHOLD=0` 关闭平铺）。

| 工具名 | 说明（代码原文） |
| --- | --- |
| `search_capabilities` | 统一检索入口：返回候选列表，不含完整 schema |
| `describe_capability` | 按 id 取完整 schema（只说明怎么调用，不构成调用通道） |
| `list_groups` | 组索引：只含组名与计数，不含定义 |
| `db_run_template` | 模板入口：注册期已校验的固定查询路径（版本钉住 + 结果缓存） |
| `db_query_readonly` | 通用只读入口：只读查询校验 → 预算 → 只读执行 → 整形 → 审计 |
| `admin_register_template` | 登记模板草案（status=draft；发布必须走人工审核、填写外部审批单号的管理面） |
| `admin_review_queue` | 审核队列：draft/review 两类能力（只读） |
| `admin_audit_queries` | 审计查询（只读，limit 上限 200） |
| `admin_set_visibility` | 改 discoverable/callable（写 admin_change；不改定义、不触发重新审核） |

`db_query_readonly` 完整 `inputSchema`（`mcp.rs:61-74`，与代码逐字一致；`annotations` 统一为 `{"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false}`）：

```json
{
  "type": "object",
  "properties": {
    "sql": { "type": "string", "description": "PostgreSQL 为 SQL，InfluxDB 为 InfluxQL" },
    "params": { "type": "array", "description": "位置参数（按 $1..$n 顺序绑定）；InfluxQL 不支持" },
    "max_rows": { "type": "integer", "description": "默认 100，上限 1000" },
    "max_tokens": { "type": "integer" },
    "cell_char_limit": { "type": "integer", "description": "默认 200，上限 4000" },
    "cell_char_offset": { "type": "integer" },
    "allowed_schemas": { "type": "array", "description": "只能是连接声明的子集" },
    "database": { "type": "string", "description": "InfluxDB 数据库名，engine=influxdb 时 SELECT/SHOW 必填；PostgreSQL 拒绝该参数" }
  },
  "required": ["sql"]
}
```

`db_run_template` schema：`{template_id(required), params(object), version(integer)}`。
`db_query_readonly` 返回字段（`pg_executor.rs:198-210`）：`rows`、`row_count`、`truncated`、`tokens`、`next_offsets[]`、`elapsed_ms` 等。

### A3. 引擎、只读校验/执行路径、配置、环境变量、测试

#### 引擎矩阵

| engine | 配置是否接受 | 启动是否实现 | 只读校验/执行路径 |
| --- | --- | --- | --- |
| `postgres`（默认） | 是（`config.rs:50-58`） | 是（`main.rs:66-93`） | DSN 追加 `options=-c default_transaction_read_only=on`（`pg_executor.rs:18`）；启动自检：`SHOW default_transaction_read_only` 必须 `on`，且 `CREATE TEMP TABLE __jumcp_probe` 必须失败，否则拒绝启动（`pg_executor.rs:88-119`）；每查询单事务 + `set_config('statement_timeout', …, true)`，永远 rollback（`pg_executor.rs:120-197`）；连接池 `max_connections=5`；SQL 过 `jumcp_sqlguard::validate`（单语句、根节点 Select/Union/Subquery、附录 B 函数黑名单、schema 白名单、控制字符/可执行注释/`INTO OUTFILE` 词法拦截，`sqlguard/src/validate.rs`）；发布期 `EXPLAIN` 预检（`pg_executor.rs:170-187`） |
| `influxdb` | 是 | 是（`main.rs:94-107`） | InfluxQL 专用校验器（`influxql.rs`）：单语句、只读根、禁用关键词/`SHOW` 目标白名单、禁三段式 measurement、`SHOW ... ON` 与请求 database 一致、database 按 `allowed_schemas` 约束；`params` 与未声明 database 被拒（`influx_executor.rs` 测试 `influx_executor_rejects_params_and_unscoped_database`）；userinfo 只用于请求、不进入 URL 日志 |
| `mysql` | 是 | **否**：启动直接 `bail!("… 尚未实现（当前支持 postgres/influxdb）")`（`main.rs:104-109`） | sqlguard 层有 MySQL/MariaDB 方言解析能力（`sqlguard/src/dialect.rs`），但无执行器 |
| `clickhouse` | 是 | **否**（同上） | sqlguard 层有 ClickHouse 方言解析能力，无执行器 |

约束：`main.rs:39-46` 只允许一个 `readonly=true` 数据源（多于一个直接拒绝启动）；`main.rs:47-57` InfluxDB 主数据源禁止 `templates_file`。

#### `DatasourceRef` 全字段（`config.rs:11-26`）

| 字段 | 类型 | 默认 | 语义 |
| --- | --- | --- | --- |
| `name` | String | 必填 | 数据源名 |
| `dsn_env` | String | 必填 | 存放 DSN 的环境变量名；含 `=`/`://`/`/` 直接拒绝（`config.rs:43-49`） |
| `engine` | String | `postgres` | `postgres` / `mysql` / `clickhouse` / `influxdb`（`config.rs:50-58`） |
| `readonly` | bool | `true` | 主数据源选择依据（第一个 readonly） |
| `allowed_schemas` | Vec<String> | `[]` | 连接级 schema 白名单（请求只能是子集）；InfluxDB 下为 database 白名单，空=不限制 |
| `statement_timeout_ms` | i64 | `15000` | 必须为正 |

#### 环境变量

| 变量 | 默认/取值 | 用途 |
| --- | --- | --- |
| `JUMCP_MCP_CONFIG_FILE` | `config/policy.toml` | 策略 + `[[datasources]]` |
| `JUMCP_MCP_TRANSPORT` | `stdio` | `stdio` / `streamable-http` |
| `JUMCP_MCP_HOST` / `JUMCP_MCP_PORT` | `127.0.0.1` / `8000` | HTTP 监听 |
| `JUMCP_MCP_AUDIT_PATH` | `data/audit.sqlite3` | 审计 SQLite；HTTP 模式为空则拒绝启动（`main.rs:207-218`） |
| `JUMCP_MCP_REGISTRY_PATH` | `data/registry.sqlite3` | 能力/模板注册表 |
| `JUMCP_MCP_TEMPLATES_FILE` | 空 | 启动导入模板 JSON |
| `JUMCP_MCP_FLATTEN_THRESHOLD` | `40` | 平铺阈值，0=关闭平铺 |
| `JUMCP_MCP_GROUPS_MODE` | `strict` | `strict` / `intersect` |
| `JUMCP_MCP_BUDGET_OVERFLOW` | `fail` | `fail` / `truncate` |
| `JUMCP_ADMIN_TOKENS` | 空 | 管理面 `id:token,...`；HTTP 模式为空拒绝启动 |
| `JUMCP_AGENT_TOKENS` | 空 | `/mcp` `agent_id:role:token,...`；为空时 `/mcp` 一律 `MCP_UNAUTHORIZED` |
| `RUST_LOG` | `info` | 日志级别 |
| `JUMCP_TEST_PG_DSN` / `JUMCP_TEST_INFLUX_DSN` | 未设置 | 真实库 `#[ignore]` 测试 |
| `JUMCP_DEV_ADMIN_TOKEN` / `JUMCP_DEV_API_TARGET` / `JUMCP_DEV_PG_DSN` / `JUMCP_DEV_INFLUX_DSN` | 开发脚本注入 | `scripts/dev.ps1` / `dev.sh` |

`config/policy.toml` 当前**没有** `[[datasources]]` 段（实际数据源由 `scripts/dev.ps1 -PgDsn/-InfluxDsn` 生成 `.dev-data/policy.toml`）；角色 3 个（analyst/executor/admin），组 7 个，`tenant_budget_cap=2000`，`allow_intersect=false`。

#### 数据库相关测试（按文件统计，共 67 个）

| 文件 | 数量 | 测试名 |
| --- | ---: | --- |
| `jumcp-server/src/executor.rs` | 12 | default_is_not_implemented, params_defaults_and_clamps, allowed_schemas_only_narrow, database_parses_only_non_empty_strings, default_validate_keeps_pg_semantics_and_rejects_database, object_params_rejected_as_named_style, placeholder_count_matches, null_param_without_cast_is_rejected, null_param_with_explicit_cast_is_allowed, non_null_params_need_no_cast, placeholders_in_literals_and_comments_are_ignored, named_placeholder_rejected_but_casts_and_literals_allowed |
| `jumcp-server/src/pg_executor.rs` | 5 | readonly_dsn_appends_options, config_validation_rejects_bad_dsn_with_datasource_name, bind_rejects_non_scalar, pg_real_null_param_with_cast_executes*, pg_real_self_check*（*需 `JUMCP_TEST_PG_DSN`，`#[ignore]`） |
| `jumcp-server/src/influx_executor.rs` | 9 | parses_userinfo_and_keeps_it_out_of_urls, parse_errors_never_echo_credentials, parses_standard_response_with_multiple_series, empty_and_error_responses, row_budget_is_shared_across_series, exhausted_budget_keeps_later_series_with_empty_rows, cell_limit_and_offset_apply_per_series, influx_executor_rejects_params_and_unscoped_database, influx_real_show_databases* |
| `jumcp-server/src/influxql.rs` | 10 | accepts_common_read_queries, accepts_schema_introspection_show_queries, rejects_empty_and_multi_statement, rejects_non_read_roots_and_forbidden_keywords, rejects_disallowed_show_targets, rejects_three_part_measurement_references, show_on_must_match_requested_database, database_is_required_where_needed_and_scoped, keywords_inside_literals_regex_and_comments_are_ignored, unclosed_literals_are_rejected |
| `jumcp-sqlguard/tests/shape.rs` | 12 | rows_truncated_to_max_rows, rows_not_truncated_when_within_limit, max_rows_zero_still_returns_one_row, long_string_cell_truncated_with_next_offset, cell_exactly_at_limit_not_truncated, next_offset_continues_from_offset, offset_beyond_cell_length_returns_empty_and_no_offset, non_string_cells_produce_no_offsets, mixed_row_only_string_cell_reports_offset, offsets_are_per_cell_coordinates, offsets_only_for_kept_rows, empty_rows |
| `jumcp-sqlguard/tests/validate.rs` | 19 | control_character_rejected, ordinary_whitespace_accepted, into_outfile_rejected, schema_out_of_scope, schema_in_scope_accepted, unqualified_table_uses_dialect_default_schema, unqualified_table_out_of_scope_when_default_not_allowed, no_allowed_schemas_disables_scope_check, mysql_unqualified_table_resolves_to_connection_database, mysql_qualified_table_out_of_scope, cte_alias_is_not_counted_as_table, tables_are_deduplicated_in_order, two_level_qualified_table_requires_both_parts_allowed, validated_statement_fields, has_params_false_without_placeholders, mysql_session_variable_is_not_a_param, union_kind, subquery_kind, parse_error_rejected |

### A4. 本项目已有治理能力（按代码/文档事实，不含优劣判断）

| # | 能力 | 证据 |
| --- | --- | --- |
| 1 | 只读账号 + 只读会话双保险（L1/L3），启动自检失败即非零退出 | `pg_executor.rs:18-119`；`README.md` 边界段 |
| 2 | AST 级只读校验（L4）：单语句、只读根节点、可写 CTE/行锁/`SELECT INTO` 拒绝、函数黑名单（`pg_read_file`/`lo_import`/`nextval`/`dblink*` 等）、schema 越界拒绝 | `jumcp-sqlguard/src/validate.rs:19-41`、`validate()` |
| 3 | 词法层绕过拦截：控制字符、可执行注释、`INTO OUTFILE`；注释/字面量中的关键词与占位符不误判 | `validate.rs`、`lex.rs`，测试 `into_outfile_rejected` 等 |
| 4 | 参数化：仅位置参数 `$1..$n`，命名参数拒绝；占位符数量核对；NULL 参数必须显式 cast | `executor.rs:110-130`、`pg_executor.rs:120-140` 及测试 |
| 5 | 连接级 schema 白名单只收敛不放宽；请求 `allowed_schemas` 只能是子集 | `executor.rs:66-93`；测试 `allowed_schemas_only_narrow` |
| 6 | 语句超时：事务级 `set_config('statement_timeout', …, true)`，事务恒 rollback，不泄漏到 PgBouncer 共享连接 | `pg_executor.rs:130-140` |
| 7 | 结果整形：`max_rows` 默认 100/上限 1000、`cell_char_limit` 默认 200/上限 4000、`cell_char_offset` 续读，超限夹紧并回显 `truncated`/`next_offsets` | `executor.rs:37-52`、`sqlguard/src/shape.rs:19-30` |
| 8 | Token 预算：`estimate_tokens`=canonical JSON 字节/4；会话级 schema/结果/峰值上限；溢出默认 `fail`（`truncate` 需显式开启）；`max_tokens` 超限返回 `MCP_BUDGET_EXCEEDED` | `budget.rs`、`mcp.rs:922-937`、`config.rs:389-414` |
| 9 | 审计：SQLite 单表 + SHA-256 哈希链 + trigger 禁 UPDATE/DELETE（append-only）；8 类 `event_type`；`admin_change` 强制 before/after/diff/approval_id 四字段；提供 `verify_chain` | `audit.rs:13-31,37-93,172-198` |
| 10 | 能力注册表：版本化（`get_version`）、`draft → review → published`（`submit_for_review` 与 `promote_to_published`/`publish_version` 分离）、`reject_review`、`deprecate`、`review_queue`、`set_visibility`（discoverable/callable 双布尔） | `registry.rs:146-685` |
| 11 | 模板：参数规格/类型约束、编译期校验、版本钉住、发布前 `EXPLAIN` 预检（仅 PG） | `template.rs:61-226,397-459,1172-1210`；`pg_executor.rs:170-187` |
| 12 | RBAC 与身份：角色→默认组→默认预算；`JUMCP_AGENT_TOKENS` 派生 per-request actor，未知 token 一律 `MCP_UNAUTHORIZED`；`admin_*` 工具仅在含 `admin` 组时可见且调用前复核 | `config.rs:91-125,346-470`、`mcp.rs:88-122`、`entry.rs`（`EffectiveScope`） |
| 13 | 工具暴露预算/渐进发现：平铺阈值 + `search_capabilities`/`describe_capability`/`list_groups` 元工具三件套 | `mcp.rs:33-76`、`config.rs:409-411` |
| 14 | 双传输：stdio 逐行 JSON-RPC 与 streamable-http `POST /mcp` 共用调用链；HTTP 模式缺审计路径/管理 token 即拒绝启动 | `main.rs:1-2,160-232` |
| 15 | DSN 不落配置：只存环境变量名，启动时读取，缺失即显式报错 | `config.rs:12-15,38-49,68-75` |
| 16 | InfluxQL 专用校验器（非 SQL 复用的独立语法层），含 database 作用域与 `SHOW` 目标白名单 | `influxql.rs` |

---

## B. DBX 上游与本机

### B1. 版本与指纹

| 项 | 值 |
| --- | --- |
| 本机 MCP 二进制 | `C:\Users\Nn\.dbx\bin\dbx-mcp.exe` |
| 版本 | `0.4.141`（`--version` 输出 `dbx-mcp 0.4.141`；同目录 `.dbx-mcp-version` 内容 `0.4.141`） |
| 大小 | 36,153,344 字节（34.5 MiB） |
| SHA-256 | `f0143efdab97097c0f39486178d9f99c3b918137188d82da6b3ac62f3c2cc6c1` |
| 文件类型 | `PE32+ executable for MS Windows 6.00 (console), x86-64, 6 sections` |
| 桌面版 | `0.6.68-beta.20261107.4`（HKCU Uninstall 注册项 `DisplayName=DBX`）；`D:\softs\DBX\dbx.exe` 89,493,392 字节 |
| 桌面安装体积 | `D:\softs\DBX` 86 MB（仅 `dbx.exe` + `uninstall.exe` 可见） |
| 上游最新 release | `v0.6.69-beta.20261108.1`（2026-11-08T19:52:26Z，桌面）；`packages-v0.4.141`（2026-11-08T05:29:07Z，MCP 包） |

未输出任何 token/密码/连接串；DBX 配置文件与 `dbx.db` 仅做存在性/表名/列名检查。

### B2. CLI 表面与依赖

`--help` 不被接受：`Error: "unknown DBX MCP argument: --help"`（`runtime.rs` 对未知参数直接报错）。实际支持（`crates/dbx-mcp/src/runtime.rs:43-90`、`main.rs:14-30`）：

| 形式 | 说明 |
| --- | --- |
| `dbx-mcp --version` | 打印 `dbx-mcp <版本>` 后退出 |
| `dbx-mcp --authorize-keychain` | macOS 钥匙串授权（仅该参数，其他参数组合报 Usage） |
| `dbx-mcp --verify-keychain-access` | 校验后台钥匙串访问（`credentials.rs:2`） |
| `--stdio` / `--http` / `--transport <v>` | 传输选择；默认 `stdio`；`http`=`streamable-http` |
| `--http-host` / `--http-port` / `--http-path` | 默认 `127.0.0.1` / `5225` / `/mcp` |
| `--http-allow-remote` | 非回环绑定必须同时设 `DBX_MCP_HTTP_ALLOW_REMOTE=1` 且带此参数 |

启动依赖：单个静态 Rust 二进制（PE32+），stdio 模式无需 Node；原生路径（PG/MySQL/SQLite/Redis/MongoDB 等）不需要 JRE。JDBC/Agent 数据库另需 DBX 安装的驱动与 JRE（`packages/mcp-server/README.md` 本地原生模式/Agent 段）。HTTP 模式的 Bearer token 必须由 `DBX_MCP_HTTP_TOKEN` 或 `DBX_MCP_HTTP_TOKEN_FILE` 提供；本次**仅确认其存在性要求，未读取任何值**。

### B3. 上游仓库（`t8y2/dbx`，GitHub API）

| 项 | 值 |
| --- | --- |
| License | Apache-2.0 |
| Stars / Forks / Open issues | 29,756 / 2,709 / 1,697 |
| 创建时间 / 最近 push | 2026-04-29 / 2026-11-10T01:18:10Z |
| 默认分支 / 主页 | `main` / https://dbxio.com |
| 仓库体积（API `size`） | 338,150 KB |

#### 工具总数与分组（`crates/dbx-mcp/src/server.rs`，`#[tool(` 共 **50** 个）

| 分组 | 数量 | 工具名 |
| --- | ---: | --- |
| 核心数据面 | 25 | dbx_list_connections, dbx_list_databases, dbx_list_tables, dbx_describe_table, dbx_list_routines, dbx_get_routine_source, dbx_get_schema_context, dbx_execute_query, dbx_execute_batch, dbx_open_session, dbx_begin_transaction, dbx_commit_transaction, dbx_rollback_transaction, dbx_close_session, dbx_execute_redis_command, dbx_peek_messages, dbx_send_message, dbx_add_connection, dbx_duplicate_connection, dbx_remove_connection, dbx_open_table, dbx_execute_and_show, dbx_salesforce_current_user, dbx_salesforce_prepare_write, dbx_salesforce_apply_write |
| Nacos | 14 | list_namespaces, list_configs, get_config, config_history, publish_config, search_configs, rollback_config, delete_config, list_services, get_service, list_instances, update_instance, register_instance, deregister_instance（均 `dbx_nacos_*`） |
| Consul | 8 | kv_list, kv_get, kv_put, kv_delete, list_services, list_nodes, service_health, set_maintenance（均 `dbx_consul_*`） |
| 插件元工具 | 3 | dbx_plugin_list, dbx_plugin_tools, dbx_plugin_call |

文档口径差异（事实记录）：`packages/mcp-server/README.md` 正文写 "**18 MCP tools**"，同文件工具表实列 22 行；`docs/content/docs/mcp.cn.mdx` 主表 25 个、Nacos/Consul 分级表 22 个（含级别列：只读/读写/高危）。源码 50 个（含编译特性/产品门控）。另有 4 个 MCP Resources：`dbx://connections`、`dbx://connections/{id}/databases`、`.../tables{?database,schema}`、`.../table-schema{?database,schema,table}`。

#### `dbx_execute_query` inputSchema 与输出形状

入参结构（`server.rs:108-115` `ConnectionSelector` + `395-426` `ExecuteQueryRequest`）：

| 字段 | 类型 | 说明（上游 schemars 描述） |
| --- | --- | --- |
| `connection_id` | string? | DBX 连接 ID |
| `connection_name` | string? | DBX 连接名 |
| `database` | string? | 数据库名 |
| `sql` | string | 要执行的 SQL/MongoDB shell/SOQL |
| `session_id` | string? | 来自 `dbx_open_session`，固定连接并保留 USE/SET/临时表 |
| `cell_char_offset` | integer? | 每个字符串单元的起始字符偏移（默认 0，最大 1,000,000） |
| `cell_char_limit` | integer? | 每个字符串单元最大字符数（默认 200，最大 4000） |
| `max_rows` | integer? | 行数上限（默认 100，最大 1000，越界夹紧；仅 SQL 连接） |

关键注解（`server.rs:2644-2650`）：`annotations(read_only_hint = false, destructive_hint = true, open_world_hint = false)`。

输出形状：**非结构化 JSON，而是 Markdown 文本表格**（`crates/dbx-core/src/ai/agent_tools.rs:1458-1510` `format_query_result_as_text`）：
- 无列（命令结果）：`Query executed. N row(s) affected.`；
- 有列：`| col | … |` 表头 + `|---|---|` 分隔行 + 逐行 `| v | … |`（NULL 渲染 `NULL`，字符串按 `cell_window` 截断）；
- 截断提示：`... (showing N rows; ...)`，区分“达到行上限”与“驱动侧体积上限”两种原因；
- 统计行：`(N rows, Xms)`。
`dbx_execute_batch` 每条语句一个结果，`use_transaction` 时为单个合并结果；批量工具另带 `structuredContent.results[]`，详见 C1。普通 `dbx_execute_query` 不产出 `structuredContent`，详见 C2。

#### 连接类型/驱动来源

- `plugins/connection-types/*.yaml` 共 **89** 个类型描述文件（含 mysql、postgresql、sqlite、clickhouse、influxdb、mongodb、redis、dameng、kingbase、oracle、sqlserver、duckdb、jdbc、nacos、consul、mq/kafka 等）。
- 编译期由 `crates/dbx-types/build.rs` 汇总生成连接类型注册表（校验 `schemaVersion=1`、`dbType`/`rustVariant`/`order` 唯一）；运行时 `database_manifest.rs` 读取 `OUT_DIR/database_manifest.json`，提供 `default_port`/`agent_key`/`dialect_name`/`supports_capability` 等。
- 运行模式枚举（`database_manifest.rs:14-20`）：`native` / `file` / `agent` / `external`；`is_agent_runtime()` 区分 JDBC/Agent 路径。
- 仓库描述口径为 “100+ databases”，本地类型描述文件实测 89 个。

#### 插件 API / 挂载机制：**存在**

| 项 | 事实 | 证据路径 |
| --- | --- | --- |
| 插件清单贡献 | `manifest.json` 的 `mcp` 贡献需声明 `external_tools: true` 才会暴露到外部 MCP 面；`ai_tools: false` 可禁止进入内置 AI 面 | `docs/content/docs/plugin-development.cn.mdx`（插件 manifest / 为 DBX AI 助手提供工具两节） |
| Sidecar 协议 | JSON-RPC v2 over `stdio-jsonl`（默认）或 `stdio-framed`；方法 `plugin/initialize`、`objects/list` 等 | 同上 “原生 Sidecar 和 RPC” |
| 工具桥接方法 | `mcp/tools` 返回 `{tools:[{name,description,inputSchema,annotations?}]}`；`mcp/call` 接收 `{tool,arguments,lifecycle}` 返回 `CallToolResult` | 同上 |
| 外部 MCP 暴露名 | `dbx_<插件前缀>__<工具名>`（前缀取插件 id 末段，如 `io.dbx.ssh` → `dbx_ssh__*`）；schema 移除 `connectionId`/`connectionName`，改由 `dbx_connection` 选择已保存连接；凭据由宿主注入 | `docs/content/docs/mcp.cn.mdx`（插件工具节）、`crates/dbx-mcp/src/plugin_tools.rs:1-110` |
| 上限与发现模式 | `MAX_TOTAL_PLUGIN_TOOLS = 512`；`DBX_MCP_PLUGIN_TOOLS=flat|lazy|both`，`lazy` 只暴露 3 个元工具；兼容模式 ≤40 平铺 | `plugin_tools.rs:31`、`mcp.cn.mdx`（懒发现节） |
| 确认策略 | 只有声明 `annotations.readOnlyHint=true` 的插件工具直接执行，其余默认需桌面人工确认（5 分钟超时视为拒绝） | `plugin-development.cn.mdx` |
| 静态工具注册 | rmcp `#[tool_router]` + 50 个编译期 `#[tool(...)]` 方法，非运行期动态注册 | `crates/dbx-mcp/src/server.rs:1671-` |
| 挂载外部 MCP server（通用） | **未发现**：未见“把任意第三方 MCP server 挂到 dbx 下”的机制；插件是唯一扩展通道 | 全库 grep `plugin`/`mount`；本次未发现 `mcpServers` 桥接源码 |

### B4. 本机 DBX 安装体积与数据

| 路径 | 大小/内容 |
| --- | --- |
| `C:\Users\Nn\.dbx` 总计 | **55 MB** |
| `…\.dbx\bin` | 35 MB（`dbx-mcp.exe` 34.5 MB + `.dbx-mcp-version` 8 B） |
| `…\.dbx\agents` | 21 MB |
| `…\.dbx\agents\drivers` | 3 个驱动：dameng 2.6 MB、rabbitmq 5.8 MB、vastbase 7.8 MB |
| `…\.dbx\agents\download-cache` | 2 个 jar 包 tar.zst：`driver-dameng-base-0.1.103-.agent.jar.tar.zst` 2.4 MB、`driver-rabbitmq-base-0.1.18-.agent.exe.tar.zst` 2.1 MB |
| `…\.dbx\agents\state.json` | 659 B；含 `jre_versions.21 = 21.0.12+kerberos.ec.2`、`java_runtime.mode = custom`、`custom_java_path = C:\Program Files\Java\jdk21.0.8_9\bin\java…`、3 个 `installed_drivers`（dameng 0.1.103 / rabbitmq 0.1.18 / vastbase 0.1.57，jre=21） |
| 捆绑 JRE | **无**（`.dbx` 内无 JRE 目录）；使用外部 JDK，`C:\Program Files\Java\jdk21.0.8_9` 共 **329 MB** |
| `C:\Users\Nn\AppData\Roaming\com.dbx.app\dbx.db` | 1.8 MB（另 `-shm` 32 KB、`-wal` 3.9 MB） |
| `%APPDATA%\com.dbx.app` 其他 | `plugins\`、`database-backups\`、`dbx-secret-migration-<uuid>\`、`.window-state.json`、`mcp-bridge-port`(5 B) |

`dbx.db` 表清单（`sqlite3` 只读、仅表名，共 **24** 张）：
`ai_config, ai_configs, ai_conversations, ai_provider_configs, ai_runs, app_settings, app_state, connection_secrets, connections, data_migrations, history, mq_token_records, prompt_templates, saved_sql_files, saved_sql_folders, schema_cache, sidebar_layout, state_store, tab_runtime_cache, table_vgroups, task_run_items, task_runs, transfer_run_details, tunnel_profiles`。

关键表列名（仅列名，未 SELECT 任何凭据值）：
- `connections`（2 列）：`id, config_json`
- `connection_secrets`（4 列）：`connection_id, key, secret, secret_enc`
- `history`（20 列）：`id, connection_id, connection_name, database, sql_text, executed_at, execution_time_ms, success, error, activity_kind, operation, target, affected_rows, rollback_sql, details_json, source, mcp_tool_name, mcp_request_json, mcp_response_json, mcp_session_id`

### B5. 权限/审计/鉴权/存储的证据

| 项 | 事实 | 证据 |
| --- | --- | --- |
| 默认权限级别 | `McpAccessPolicy::default().default_access = McpAccessLevel::Read`；级别排序 `None < Read < Write < Full` | `crates/dbx-core/src/ai/mcp_access.rs:67-95, 302-313` |
| 权限回退顺序 | 单库 → 连接 → 最近分组 → 上级分组 → 全局默认；连接只读、生产库保护、数据库范围、数据库账号权限、连接 allowlist 是硬上限，不能被更具体设置放宽 | `docs/content/docs/mcp.cn.mdx`（权限管理 / 权限按最具体范围继承两节） |
| 只读语义与开关 | 权限表三档：只读（查询+元数据）/ 数据读写（普通增删改、MongoDB 修改、Redis 写入）/ 完全访问（大范围增删、DDL、TRUNCATE、FLUSH*）；`DBX_MCP_ALLOW_WRITES=0/false` 仅作兼容，保持“未配置策略为只读” | `mcp.cn.mdx`（安全和环境变量节）、`packages/mcp-server/README.md` 环境变量表 |
| 高危确认 | 可选“高危工具调用执行前请求人工确认”；权限不足直接拒绝，完全访问也逐次确认；拒绝/超时/无桌面时**不执行**；批准后复查权限；仅适用桌面确认（Web 不提供） | `mcp.cn.mdx`（调用安全节） |
| 审计/调用历史 | 桌面 **设置 → MCP → 接入方式 → 查看 MCP 调用记录**：时间、stdio/HTTP、客户端名/版本、工具、目标连接/库、判定级别、是否确认、耗时、请求/响应摘要、成功或失败原因；有独立保留与清理设置；落 `history` 表（含 `mcp_tool_name/mcp_request_json/mcp_response_json/mcp_session_id`） | `mcp.cn.mdx`（MCP 调用记录节）、本机 `dbx.db` `history` 表列名 |
| 哈希链/防篡改审计 | **未发现**：未见 append-only 触发器、prev_hash/hash 链、`verify_chain` 类实现 | 本次全库 grep `audit`/`hash chain` 未取得 |
| HTTP 鉴权 | Streamable HTTP 必须 Bearer token（`DBX_MCP_HTTP_TOKEN` 或 `..._TOKEN_FILE`，二者互斥，空值报错）；token 常量时间比较；Host 白名单；Origin 白名单/回环豁免；非回环绑定需同时 `DBX_MCP_HTTP_ALLOW_REMOTE=1` 与 `--http-allow-remote`，且必须配 `DBX_MCP_HTTP_ALLOWED_HOSTS` 与 `..._ALLOWED_ORIGINS`；默认仅 `127.0.0.1:5225/mcp`；GET/DELETE 返回 405，仅接受 POST | `crates/dbx-mcp/src/http_auth.rs:1-120`、`runtime.rs:92-140`、`mcp.cn.mdx`（HTTP 服务节） |
| stdio 鉴权 | 无 token（本机进程信任模型）；连接/工具作用域仍按策略复核 | `runtime.rs:85-88`（stdio 直接返回，无 auth 配置） |
| 连接存储位置 | SQLite `dbx.db`，默认 macOS `~/Library/Application Support/com.dbx.app/dbx.db`、Linux `~/.local/share/com.dbx.app/dbx.db`、Windows `%APPDATA%\com.dbx.app\dbx.db`；`DBX_DATA_DIR` 可覆盖（Windows 便携版为 `DBX.exe` 同级 `data`） | `crates/dbx-mcp/src/paths.rs:5-17`、`packages/mcp-server/README.md` 本地原生模式 |
| 凭据存储 | `connection_secrets` 表存在 `secret` 与 `secret_enc` 两列（加密列）；macOS 另有钥匙串授权/校验入口；**存储实现细节未核验** | 本机 `dbx.db` 列名；`credentials.rs`（`--authorize-keychain`/`--verify-keychain-access`） |
| 工具/连接 allowlist | 工具 allowlist 与连接 allowlist 在**每次调用**服务端强制，客户端缓存旧列表也会被拒；插件白名单支持 `dbx_<前缀>__<工具>`、`dbx_<前缀>__*`、`dbx_*__*` | `mcp.cn.mdx`（安全与环境变量 / 插件工具节） |

---

## C. 补充核验（二次采集，2026-11-10）

源码路径均为上游 `t8y2/dbx@main`；行号取自本次下载的原始文件。`dbx_execute_batch` / `dbx_execute_query` 的入参结构位于 `crates/dbx-mcp/src/server.rs`。

### C1. `dbx_execute_batch` 完整入参字段与返回形状

入参结构 `ExecuteBatchQueryRequest`（`server.rs:625-657`）：

| 字段 | 类型 | 行号 | 说明 |
| --- | --- | --- | --- |
| `connection_id` | string? | 627-628（flatten `ConnectionSelector`，`server.rs:108-115`） | 连接选择 |
| `connection_name` | string? | 同上 | 连接选择 |
| `cell_char_offset` | integer? | 629-634（flatten `CellWindowArgs`，`server.rs:606-617`） | 每条语句渲染的字符串单元起始偏移（默认 0，最大 1,000,000） |
| `cell_char_limit` | integer? | 同上 | 每条语句渲染的字符串单元最大字符数（默认 200，最大 4000） |
| `database` | string? | 637 | 数据库名 |
| `sql` | string | 641 | **单个字符串脚本**，不是语句数组；由方言感知 splitter 拆分（分号在字符串/注释/存储过程体内不拆） |
| `session_id` | string? | 646 | 固定到 `dbx_open_session` 的会话连接 |
| `continue_on_error` | boolean? | 651 | 语句失败后继续；连接级失败始终停止；默认 false |
| `use_transaction` | boolean? | 656 | 多语句脚本整体 `BEGIN…COMMIT`，返回单个合并结果；不能与 `session_id`/`continue_on_error` 同用；MySQL 家族 DDL 脚本拒绝 |

- **不含 `max_rows`**，**不含 `params`**（批量工具无 SQL 绑定参数入参）。
- 单条语句行数上限：硬编码 `const BATCH_MAX_ROWS: usize = 100`（`server.rs:6225-6228`）；会话事务批 `lease.batch(…, Some(BATCH_MAX_ROWS), …)`（`server.rs:3302`），普通批 `QueryExecutionOptions { max_rows: Some(BATCH_MAX_ROWS) }`（`server.rs:3370`），渲染截断提示同样取该常量（`server.rs:6298, 6315`）。**调用方无法设置/提高单条结果行数上限**；只能靠语句自身 `LIMIT` 收窄。`cell_char_offset` / `cell_char_limit` 由调用方设置。

返回：`CallToolResult` = 文本（`format_batch_results` 渲染的 Markdown，`server.rs:6285`）+ `structured_content`：

| 路径 | `structuredContent` 结构 | 行号 |
| --- | --- | --- |
| 会话内事务批（`session.transaction_owner`） | `{ session_id, transaction_state, transaction_outcome, results }` | `server.rs:3347-3353` |
| 普通/自动提交批（含 `use_transaction`） | `{ results }` | `server.rs:3426-3431` |

`results[]` 每项为 `BatchStatementResult`（`crates/dbx-mcp/src/backend.rs:136-155`）：
- `#[serde(flatten)] result: dbx_core::db::QueryResult`；
- `execution_error`（false 时省略）、`statement_index`、`error_message`、`merged`（false 时省略）、`transaction_state`、`transaction_outcome`。

`QueryResult` 序列化字段（`crates/dbx-types/src/types.rs:636-691` 定义；自定义 `Serialize` 见 `700-760` 起）：
`columns`、`column_types`、`column_sortables`、`rows`、`affected_rows`、`execution_time_ms`、`truncated`、`session_id`、`has_more` 恒在；`spatial_columns`、`spatial_values`、`raw_key_cells`、`server_execute_time_us`、`query_timings_ms`、`messages` 按非空/Some 输出。

### C2. `dbx_execute_query` 的返回路径

- 普通路径：`self.backend.execute_agent_tool(connection, &database, "execute_query", arguments, permissions)`（`server.rs:3021-3022`）返回 `dbx_core::agent_events::ToolResult`（只有 `content: String` 与 `is_error`），随后 `agent_result(result)`（`server.rs:3036`）。
- `agent_result`（`server.rs:6344-6350`）：成功时 `text(result.content)`；`text()`（`server.rs:6146-6148`）只构造一个 text content block，**不设置 `structured_content`**。
- 因此普通 `dbx_execute_query` 的 `structuredContent` **不存在/为空**（rmcp 的 `Option` 为 `None`，不序列化），行列等只能从 Markdown 文本解析。全文件 `structured_content` 赋值点仅：`3348`、`3521`、`3691`、`4635`、`6166`、`6177`，其中 3348/3431 属于 batch，6166/6177 属于 MySQL 显式事务结果。
- 例外：native MySQL 显式事务会话路径 `transaction_success_result`（`server.rs:6160-6172`）会设置 `{session_id, transaction_state, transaction_outcome, result}`；失败路径 `transaction_failure_result`（`server.rs:6175-6185`）设置 `{session_id, transaction_state, transaction_outcome, mysql_code, error}`。测试断言见 `server.rs:11869-11870`。

### C3. PG 连接参数：`url_params` / `connection_string`，以及 `dbx_add_connection` 的 `password`

`ConnectionConfig`（`crates/dbx-types/src/models/connection.rs:156-334`）相关字段：

| 字段 | 行号 | 事实 |
| --- | --- | --- |
| `url_params: Option<String>` | 167 | 连接 URL 查询串；PG 分支会拼接进 URL |
| `connection_string: Option<String>` | 253 | 显式连接串；PG 原生 URL 分支**不使用**（Mongo 使用：`1551-1559`；JDBC 方言提示使用：`730`） |
| `host` / `port` / `username` / `password` / `database` | 170-174 | PG URL 的其余组成部分 |
| `init_script: Option<String>` | 197-200 | 连接建立后执行的 SQL，注释标注 **DuckDB only for now** |

PG URL 构造链（`crates/dbx-types/src/models/connection.rs`）：
1. `connection_url_with_host`（`1483` 起）：`let params = self.normalized_url_params();`（`1489`）。
2. PG/Redshift 分支（`1515-1523`）：`postgres://{username}:{password}@{host}:{port}{db_part}?{params}`；`db_part` 来自 `database`。**不读 `connection_string`**。
3. `normalized_url_params`（`1856-1876`）：PG/Redshift 走 `normalize_postgres_url_params(value, self.ssl)`（`1870`）。
4. `normalize_postgres_url_params`（`2470-2569`）：未知 key 原样保留（`2512-2514`）；`schema`/`currentSchema`、`timezone`/`time_zone` 转成 `options=-c …`；若已有 `options`，把缺的 `-c search_path=…` / `-c TimeZone=…` 合并进同一 `options`（`2534-2550`）；缺 `sslmode` 时补 `sslmode=prefer|require`（`2526-2530`）。
5. 校验 `validate_native_url_params`（`1900-1906`）→ `validate_postgres_url_params`（`2571-2590`）：只校验 `stringtype` 取值（仅允许 `unspecified`/`varchar`），**其他 key 不拦**。
6. 驱动侧：`pg_url_has_timezone_setting`（`crates/dbx-driver-postgres/src/postgres.rs:3852-3874`）显式解析 URL 的 `options` 参数（用于避免覆盖用户时区设置），说明 `options` 随 URL 进入 PG 驱动。

结论（事实层）：连接配置的 `url_params` 可以把 `options=-c default_transaction_read_only=on` 送进 PG 连接 URL；值需 URL 编码（如 `options=-c%20default_transaction_read_only%3Don`）。若同时配置 `schema`/`timezone`，会与已有 `options` **合并**（`2534-2550`），不是覆盖。该只读选项是否被 DBX 驱动/会话层再次改写：**未核验**（仅确认驱动会读取 `options`，未见其重置 `default_transaction_read_only`）。

`dbx_add_connection` 入参 `AddConnectionRequest`（`server.rs:450-472`）：

| 字段 | 行号 | 说明 |
| --- | --- | --- |
| `name` / `db_type` / `host` / `port` | 451-455 | 必填/可选基础字段 |
| `username` / `password` | 457-459 | **`password` 是普通工具入参**（`String`，默认空），模型可见、可传值 |
| `database` / `ssl` / `driver_profile` | 461-465 | 连接参数 |
| `external_config` | 466-471 | 方言专用外部配置，注释明确为 Nacos 配置；**无 `url_params` / `connection_string` 入参** |

`request.password` 直接传给 `new_connection_config_with_external_config(...)`（`server.rs:4172-4184`）。因此经 MCP 的 `dbx_add_connection` **不能**设置 PG 的 `url_params`/`connection_string`；要写 `url_params` 需通过 DBX 桌面端的连接配置（该字段属于持久化 `ConnectionConfig`）。密码落库位置：`dbx.db` 的 `connection_secrets(connection_id,key,secret,secret_enc)`；本次未读取任何凭据值。

---

## 未发现 / 未核验

- 未发现 DBX 提供通用“挂载任意外部 MCP server”的机制（仅插件 Sidecar 扩展通道）。
- 未发现 DBX 具备哈希链式防篡改审计或审计链校验；仅有 UI 调用记录 + `history` 表。
- 未核验 `secret_enc` 的具体加密算法与钥匙串/DPAPI 绑定方式（按要求未读取凭据值）。
- 未核验 DBX 桌面端安装目录内是否有额外运行时（安装目录仅见 `dbx.exe`/`uninstall.exe`，WebView2 由系统提供）。
- 未启动长期 MCP 服务；本地 `dbx-mcp.exe` 仅执行 `--version`/`--help`（后者返回未知参数错误）。
