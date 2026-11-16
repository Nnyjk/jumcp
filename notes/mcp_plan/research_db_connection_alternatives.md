# 数据库连接层：可替代自研的开源方案

问题：连接、驱动、连接池、查询执行这四件事，哪些能直接复用开源实现，哪些必须自研。

结论先行：

- **主流关系库不需要自研驱动层**：`sqlx`（PG/MySQL/MariaDB/SQLite）+ `tiberius`（SQL Server）+ 官方 `clickhouse` crate 已覆盖；连接池、绑定参数、流式取数都是现成的。
- **长尾/JDBC-only 引擎没有成熟的纯 Rust 方案**，唯一通用且带绑定参数的标准桥是 **ODBC**（`odbc-api`），不是 JDBC。达梦/金仓/虚谷/DB2/Oracle/SQL Server 都能走 ODBC，但驱动质量与只读语义按厂商分裂。
- **MCP Toolbox 与 DBHub 都不能替代 `db_query_readonly` 的执行后端**：两者都没有“按调用传 `max_rows` + 精确 `truncated` + 结构化行”的契约；Toolbox 的引擎级只读只覆盖 GCP 托管源，DBHub 只覆盖 PG/MySQL/MariaDB/SQLite。可作为长尾模板的临时后端做 POC，不宜进主链路。
- **Trino 只在真需要跨源联邦时成立**；它没有达梦/金仓/虚谷/DB2 连接器，且引入 JVM 集群与另一套 SQL 方言。
- **通用开源 JDBC 桥不存在**（AceQL 是 BUSL + 收费；clickhouse-jdbc-bridge 已归档且只服务 ClickHouse；ADBC 的 JDBC Adapter 只能被 JVM 客户端使用）。JDBC-only 引擎若 ODBC 走不通，只能自建一个极窄的 Java sidecar，并明确它只做 prepare/bind/execute/fetch。

## 1. 评估口径

基准是现有执行契约，而不是“能不能连上库”：

- `db_query_readonly(sql, params?, max_rows, max_tokens, cell_char_limit, cell_char_offset, allowed_schemas, database?)`：位置参数绑定、单条只读语句、行数夹紧并回显 `effective_max_rows/truncated`、单元格续读、token 预算、`sql_hash`、审计。
- `db_run_template(template_id, version?, params)`：命名参数 → 绑定占位符、版本钉住、发布期 EXPLAIN、结果缓存键、schema 白名单。
- L1 只读账号是强保证；L2 连接/事务级只读是强保证；L4 AST 是纵深防御，不是保证（`architecture_preview.md` §2、§4.3.1）。

一个方案只有同时满足 **绑定参数 + 每次调用可下压的行数上限 + 可判定的 `truncated` + 结构化行 + 可按引擎建立只读会话**，才可能直接承接 `db_query_readonly`。只满足其中一部分的，只能承接模板。

事实截止 2026-11-10；版本号来自 crates.io / npm / 官方文档。

## 2. 总表

| 方案 | 形态 | 引擎覆盖 | 参数绑定 | 只读强保证 | 行数控制 | 结果结构 | 部署依赖 | License | 适用场景 |
|---|---|---|---|---|---|---|---|---|---|
| **sqlx 0.9.0** | Rust 库 | PostgreSQL、MySQL、MariaDB、SQLite | ✅ `$1`/`?`，编译期/运行期均可 | ❌ 无内建；靠 DSN 选项、只读账号、事务语句 | ❌ 需自行 `LIMIT n+1` 或流式取数 | 类型化 `Row`，需逐引擎解码 | 纯 Rust | MIT OR Apache-2.0 | 主流 OLTP 的首选驱动层 |
| **tiberius 0.13.0** | Rust 库 | SQL Server | ✅ `@p1` | ❌ 无内建（SQL Server 无只读事务） | ❌ 应用层 | 类型化 `Row` | 纯 Rust，异步 | MIT/Apache-2.0 | SQL Server 的唯一纯 Rust 路径 |
| **clickhouse 0.15.3** | Rust 库 | ClickHouse | ✅ `?`（服务端 query parameter） | ❌ 无内建；用服务端 `readonly=1` 用户设置 | ❌ 应用层 | 类型化 struct/serde；无动态列 | HTTP，纯 Rust | MIT OR Apache-2.0 | ClickHouse 分析查询 |
| **oracle 0.6.3** | Rust 库 | Oracle | ✅ 位置/命名绑定 | ⚠️ 可自行 `SET TRANSACTION READ ONLY` | ❌ 应用层 | 类型化 | **Oracle Instant Client**；同步阻塞 | UPL-1.0/Apache-2.0 | 已有 Instant Client 的 Oracle 接入 |
| **odbc-api 30.0.0** | Rust 库 / 标准桥 | 任何有 ODBC 驱动的库：SQL Server、Oracle、DB2、达梦、金仓、虚谷、PG、MySQL、SQLite | ✅ `?` + prepared statement | ❌ 无内建；靠驱动/账号/会话语句 | ⚠️ 应用层；ODBC `SQL_ATTR_MAX_ROWS` 驱动支持不一（未核验） | 列缓冲/文本，需类型映射 | 系统 ODBC 管理器 + 厂商驱动 | MIT | 长尾引擎的最大公约数 |
| **arrow-odbc 27.0.0** | Rust 库 | 同 ODBC | ✅ 参数元组 | ❌ 无内建 | ⚠️ 只能控制 batch 大小，不控总行数 | Arrow RecordBatch | 同上 | MIT | 分析型批量取数，不是小结果 MCP |
| **connectorx 0.4.6** | Rust 库 | PG、MySQL、MSSQL、Oracle、BigQuery、Trino、ClickHouse、SQLite、CSV | ❌ 只接受 SQL 文本 | ⚠️ 读取路径仅 SELECT，但不是安全边界 | ❌ | Arrow/DataFrame | 编译期驱动，部分 C 依赖 | MIT | 数据导出/分析，不适合交互查询 |
| **ADBC 0.25.0** | 标准 + Rust 客户端 | ADBC 驱动：PG、MySQL/MariaDB、MSSQL、ClickHouse、Trino、BigQuery、Snowflake、SQLite、FlightSQL 等 | ✅ prepare + bind（Arrow 参数 batch） | ❌ 无内建 | ❌ 应用层 | Arrow | 需分发 C 驱动 `.so/.dll` + manifest | Apache-2.0 | Arrow 生态/列式分析；无 Oracle/DB2/达梦 |
| **MCP Toolbox 1.16.0** | 服务（Go 单二进制） | 约 50 个集成：PG、MySQL、MariaDB、MSSQL、Oracle、ClickHouse、Trino、SQLite、BigQuery、Spanner、Cloud SQL、AlloyDB… | ✅ 位置参数（按引擎用 `$1`/`?`/`@p1`/`:1`）；`templateParameters` 可插标识符（有注入风险） | ⚠️ 仅 Cloud SQL/AlloyDB/BigQuery 有引擎级锁；自建源无 `readOnly` 字段 | ❌ 无通用行数上限（`execute-sql` 无；个别 list 工具有 `limit` 参数） | JSON 数组，外层再包一层字符串 | 单二进制/Docker | Apache-2.0 | GCP 托管库；或长尾模板的临时执行器 |
| **DBHub 1.6.0** | 服务（Node ≥22.5） | PG、MySQL、MariaDB、SQL Server、Oracle、SQLite | ✅ 自定义工具位置参数；`execute_sql` 本身无参数 | ⚠️ `execute_sql` 在 PG/MySQL/MariaDB/SQLite 有引擎级只读；SQL Server/Oracle 仅关键字分类；自定义工具不强制只读 | ✅ 配置级 `max_rows`（SELECT，probe 行判 `truncated`） | JSON 字符串放在 `content[].text` | Node 运行时 | MIT | 多引擎探索；模板型只读 |
| **Trino 484** | 服务（JVM 集群） | 37 个连接器：PG、MySQL、MariaDB、SQL Server、Oracle、ClickHouse、BigQuery、Snowflake…；**无 DB2/达梦/金仓/虚谷** | ✅ `PREPARE ... ?` + `EXECUTE`；JDBC `explicitPrepare` 默认 true | ✅ 系统级 `read-only` access control 拒绝写 | ⚠️ `query.max-*` 限制资源，不按行数；客户端需自行停取 | 分页 JSON（客户端解析） | Java 25 + coordinator/worker | Apache-2.0 | 真跨源联邦 |
| **JDBC 桥（AceQL / Linkis / ADBC JDBC）** | 服务/JVM 库 | 任意 JDBC 驱动（达梦/金仓/虚谷/Oracle/DB2…） | ✅ AceQL 支持 `PreparedStatement`；ADBC JDBC Adapter 支持 | ⚠️ 取决于实现 | ⚠️ 取决于实现 | 取决于实现 | JVM + 厂商 JAR | AceQL BUSL-1.1（收费）；Linkis Apache-2.0；ADBC Adapter Apache-2.0 | 仅当 ODBC 不可用时 |
| **DBX（对照）** | 服务/MCP | 100+ 引擎（含达梦/金仓/虚谷） | ❌ 无绑定参数 | ❌ 无 DB 级只读事务 | ⚠️ 结构化路径固定 100 行 | Markdown 文本；batch 有结构化 | 二进制/JRE 21 + Agent | Apache-2.0 | 仅探索型、≤100 行、无参数 |

> “只读强保证”一列只描述产品自身提供的能力；部署时仍必须叠加 L1 只读账号。

## 3. Rust 原生库逐项

### 3.1 sqlx 0.9.0

- 支持引擎：PostgreSQL、MySQL、MariaDB、SQLite（`README.md` 第 74 行；crate 描述同）。
- **MSSQL 已移除**：0.7.0 起删除 `mssql` feature，计划放入商业版 SQLx Pro（`CHANGELOG.md` 0.7.0 节：“The MSSQL driver has been removed…”）。因此 `sqlx::Any` 也不能覆盖 SQL Server。
- 绑定参数：`$n`（PG）、`?`（MySQL/SQLite）；运行期 `query_with` + `Arguments`。
- 连接池：`Pool`/`PoolOptions`，按引擎独立配置；`after_connect` 可做会话初始化。
- 只读：无内建开关。PG 可用 DSN `options=-c default_transaction_read_only=on`（现实现 `READONLY_OPTIONS`）；MySQL 需 `SET SESSION TRANSACTION READ ONLY` 或 `START TRANSACTION READ ONLY`；SQLite 用 `PRAGMA query_only=ON`。都可被 `COMMIT; SET ...=off` 绕过，不能替代 L1。
- 结果：类型化 `Row`；动态 JSON 化必须逐引擎写 `decode_cell` 分支（现 `pg_executor.rs:240-313` 已是这种形态），跨引擎无法共用一套解码。
- 异步：tokio/async-std/smol；MSRV **1.94.0**（crate Cargo.toml）。
- 维护：0.9.0 发布于 2026-05-21，持续活跃。
- 结论：PG/MySQL/MariaDB/SQLite 直接复用；不要把它当多引擎统一抽象。

### 3.2 SQL Server：tiberius 0.13.0

- 纯 Rust、异步，`@p1` 绑定；配合 `bb8-tiberius 0.16.0` 或 `deadpool-tiberius 0.1.10` 做池。
- **SQL Server 没有 PostgreSQL 那种事务级/会话级只读开关**。`ApplicationIntent=ReadOnly` 只是路由提示，不是拒绝写的保证。L2 在这一引擎上不可实现，只能靠 L1（只读登录 + 仅 SELECT/视图授权）+ L4。
- 引入代价：新增一套行/列解码与错误码映射；`sqlx` 的 `PgRow` 经验不能复用。
- 结论：需要 SQL Server 时用 tiberius，不要为了“统一”退回 ODBC；ODBC 只在同时要覆盖 DB2/达梦等长尾时才有价值。

### 3.3 ClickHouse：clickhouse 0.15.3

- 官方 crate（ClickHouse/clickhouse-rs），HTTP 传输，`Client` 内含 HTTP pool；`Client::query(...).bind(...)` 用 `?` 占位，落到服务端 query parameter。
- 只读：crate 无开关；应使用服务端只读用户或 `readonly=1` 设置。ClickHouse 无事务，L2 的“只读事务”语义不适用。
- 结果：以 serde 反序列化到具体 struct；动态列/未知 schema 需要额外处理，和现有 `rows: Vec<Vec<Value>>` 整形方式不同。
- 结论：分析查询直接用；不要试图把它塞进 sqlx 的统一 Row 抽象。

### 3.4 Oracle：oracle 0.6.3

- 基于 ODPI-C，需要 **Oracle Instant Client 11.2+**；同步阻塞，必须在 `spawn_blocking` 里跑。
- 自带 `Pool`/`PoolBuilder`（`src/pool.rs`），支持位置/命名绑定。
- 只读：可在事务里 `SET TRANSACTION READ ONLY`（Oracle 支持），但这要执行器自己发；crate 不提供。
- 维护：0.6.3 发布于 2025-01-02，仓库最后推送 2025-03，活跃度低。
- 结论：能用，但引入运行时分发（Instant Client）与阻塞线程；若同一部署还要覆盖 DB2/达梦，ODBC 更划算。

### 3.5 ODBC：odbc-api 30.0.0 / arrow-odbc 27.0.0

- `odbc-api`：ODBC 3.80 的安全封装，`Connection::execute(sql, params)`、`prepare` + `execute(params)`，`?` 占位；同步阻塞。
- `arrow-odbc`：同一作者，在 `odbc-api` 之上把结果读成 Arrow RecordBatch；`execute(sql, parameters, timeout)` 支持参数元组；只适合批量读，不适合小结果集 MCP。
- 覆盖：Windows 自带 ODBC 管理器；Linux/macOS 需 unixODBC。厂商驱动决定一切：SQL Server/Oracle/DB2/达梦/金仓/虚谷 都各自提供 ODBC 驱动。
- 只读：无内建。Oracle 可 `SET TRANSACTION READ ONLY`；SQL Server 无；达梦/金仓需按厂商语法确认（未核验）。ODBC 的 `SQL_ATTR_MAX_ROWS` 可限制返回行，但驱动支持程度不一（未核验）。
- 结论：**长尾引擎的最大公约数**。代价是阻塞线程、按驱动做类型映射、驱动安装/版本耦合；收益是一套代码覆盖最多引擎，且始终有绑定参数。

### 3.6 ADBC / ConnectorX

- ADBC Rust 客户端 `adbc_core`/`adbc_driver_manager` 0.25.0 支持 `prepare` + `bind(RecordBatch)` + `execute`，结果原生 Arrow。
- 驱动清单见 `arrow.apache.org/adbc/current/driver/index.html`：BigQuery、ClickHouse、DuckDB、MSSQL、MySQL/MariaDB、PostgreSQL、Presto、Trino、Snowflake、SQLite、FlightSQL 等；**没有 Oracle、DB2、达梦、金仓、虚谷**。
- 需要分发/安装 C 驱动 `.so/.dll` 与 manifest；驱动注册表由第三方（Columnar）托管，供应链要单独评估。
- ConnectorX 只接受 SQL 文本的 `queries: &[CXQuery]`，没有绑定参数入口；定位是“高速只读导出到 Arrow/DataFrame”，不是交互执行器。
- 结论：如果未来要做列式分析/大结果导出，ADBC 值得单独评估；它不解决本项目的 MCP 行结果与长尾引擎问题。

**元数据、异步模型与引入代价**

| 库 | 元数据 | 异步模型 | 引入代价 |
|---|---|---|---|
| sqlx | `Column`/`TypeInfo`；按引擎各自一套，无统一元数据 | tokio/async-std/smol | 同 crate 多 feature；MSRV 1.94；逐引擎解码 |
| tiberius | `Column`/`TypeInfo` | 原生 async | 新依赖 + 外部池 + 解码；无 L2 |
| clickhouse | `RowBinaryWithNamesAndTypes` 带列名/类型；面向 struct | tokio HTTP | 新解码层；动态列能力弱 |
| oracle | `Rows::column_info()` / `ColumnInfo` | 同步阻塞，需 `spawn_blocking` | Instant Client + 阻塞线程 + 解码 |
| odbc-api | `ResultSetMetadata`（列名/类型/显示长度） | 同步阻塞 | 驱动管理 + 逐驱动类型映射 |
| arrow-odbc | Arrow `Schema` | 同步阻塞 | 同上 + Arrow 依赖 |
| connectorx | 查询前 schema/count 探测 | 内部线程池 | 编译期驱动；只读导出定位 |
| ADBC | `get_objects`/`get_table_schema`/`get_info` | 同步（Rust 绑定） | C 驱动分发 + Arrow 转换 |

维护活跃度（crates.io 发布时间）：clickhouse 0.15.3 = 2026-10、odbc-api 30.0.0 = 2026-11、arrow-odbc 27.0.0 = 2026-11、connectorx 0.4.6 = 2026-09、adbc_core 0.25.0 = 2026-10、tiberius 0.13.0 = 2026-09，均活跃；**oracle 0.6.3 = 2025-01，是唯一明显滞后的**。

## 4. MCP Toolbox for Databases（googleapis/mcp-toolbox）

### 4.1 事实

- 版本 1.16.0（文档页脚 2026-11-05 release），Apache-2.0，16.8k stars，单 Go 二进制/Docker。
- 源类型约 50 个（`docs/en/integrations/` 目录）：PG、MySQL、MariaDB、MSSQL、Oracle、ClickHouse、Trino、SQLite、Firebird、BigQuery、Spanner、Cloud SQL、AlloyDB、Snowflake、SingleStore、OceanBase、TiDB 等；**无 DB2、达梦、金仓、虚谷**。
- 工具类型：每个源有 `*-sql`（预定义 statement + 命名参数）与 `*-execute-sql`（入参只有 `sql`）。官方对 `postgres-execute-sql` 的说明是 “intended for developer assistant workflows with human-in-the-loop and shouldn’t be used for production agents”。源码 `internal/tools/postgres/postgressql/postgressql.go`：`statement` + `parameters`，`$1..$n` 按位置绑定，`templateParameters` 在 prepare 前做标识符/片段替换。
- 参数类型：`string`、`integer`、`float`、`boolean`、`array`、`map`；约束有 `allowedValues`（支持正则）、`excludedValues`、`minValue/maxValue`、`escape`；没有 `date`/`timestamp`/`enum` 独立类型。
- 结果：`RunSQL` 返回 `[]any` 的 ordered map（行对象数组）；原生 API 再 `json.Marshal` 成字符串放进 `{"result": "..."}`（`internal/server/api.go` 的 `toolInvokeHandler`）。
- 只读：`readOnly: true` 只在托管源上有定义。`docs/en/integrations/cloud-sql-pg/source.md` 明确它注入 `cloudsql_session_read_only=locked`；而 `postgres`/`mysql`/`mariadb`/`mssql`/`oracle`/`clickhouse`/`trino`/`sqlite` 的 `source.md` 没有该字段。也就是说：**自建库上 Toolbox 没有引擎级只读**，只能靠只读账号或 `queryParams`（PG 的 `options=-c default_transaction_read_only=on`）。
- 行数：`*-sql`/`*-execute-sql` 的工具定义里没有 `max_rows`/`maxRows`；`postgres-execute-sql` 只有 `sql` 一个参数（个别 `list-*` 工具自带 `limit` 参数，不是通用机制）。行数只能写在 statement 里。
- 连接池：PG 用 `pgxpool`，源码只设置 `DefaultQueryExecMode` 与 `ConnectTimeout`，没有 `MaxConns` 等池参数；MySQL 用 Go `database/sql` 默认池。文档也未提供池配置项。
- 调用入口：MCP Streamable HTTP `POST /mcp`、`/mcp/{toolset}`；另有原生 REST `POST /api/tool/{toolName}/invoke`，但**默认关闭**（`internal/server/api.go` 返回 410：“/api native endpoints are disabled by default. Please use the standard /mcp JSON-RPC endpoint”），需 `--enable-api`；该 flag 未出现在 1.16 的 CLI reference 中，且与 MCP Auth 互斥（`cmd/root.go` 报错）。
- 认证：MCP 侧支持 Google Sign-In / 通用 OIDC（`documentation/configuration/authentication/`），工具可声明 `authRequired`/`authServices`，并支持 authorized invocation 与 secure parameters；原生 `/api` 也读取 `Authorization` 头（仅用于 BigQuery 等客户端凭据）。服务本身没有多租户/按 agent 的身份模型，内部调用应放私网并叠加独立 token/网络策略。
- 注解：数据库工具默认用 `NewDestructiveAnnotations()`（`ReadOnlyHint=false, DestructiveHint=true`），必须显式覆盖。

### 4.2 作为“本项目前门 → Toolbox 执行后端”的判定

技术上可以：本项目按 Toolbox 的 `POST /api/tool/{name}/invoke`（开 `--enable-api`）或 `/mcp` 的 `tools/call` 调用，参数以 JSON 传入，绑定由 Toolbox 完成。治理面（身份、组、预算、版本、审计）全部留在本项目，不把 `tools/list` 暴露给 agent。

但五个契约缺口使它不能承接 `db_query_readonly`：

1. **任意 SQL 没有参数入口**：`*-execute-sql` 只接受 `sql`，不能同时传 `params`；`*-sql` 是注册期固定的 statement，属于模板而不是自由查询。要绑定参数就必须预注册工具，这已经越出 `db_query_readonly` 的语义。
2. **没有 per-call 行数上限**。本项目的 `max_rows` 是调用参数，且要精确返回 `truncated/effective_max_rows`。Toolbox 只能把 `LIMIT` 写死在 statement 里；对任意用户 SQL 无法下压，且返回后要全量计数才能判截断。
3. **没有结构化结果契约**。原生 API 返回 `{"result":"<JSON 字符串>"}`；MCP 返回 content 文本。行对象可以解析，但没有列类型、没有 token/字符预算、没有精确截断元信息。
4. **自建库没有 L2**。`readOnly` 只覆盖 Cloud SQL/AlloyDB/BigQuery；PG/MySQL/Oracle/MSSQL/ClickHouse/Trino 只能靠账号或 DSN 参数。Oracle/SQL Server 的只读会话仍需自建。
5. **配置不可变性与版本语义缺失**。`tools.yaml` 热重载（push/poll），没有版本号、没有 draft/published、没有审批；`db_run_template` 的 `(template_id, version)` 钉住与 EXPLAIN 预检无法映射。

结论：**不作为通用执行后端，不作为模板注册表**。可保留为“GCP 托管源或长尾引擎的临时只读查询后端”，前置条件是：只读账号、SQL 内自带 `LIMIT`、上游仍然做 L4/行数/token/审计；且必须钉住 Toolbox 版本与启动参数。它更适合作为 `tools.yaml` 结构、只读分层思路的参考（现有 `research_db_tool_servers.md` 已采纳）。

## 5. Bytebase DBHub

### 5.1 事实

- `@bytebase/dbhub` 1.6.0，MIT，3.8k stars，Node ≥22.5（使用内置 `node:sqlite`）。
- 引擎：PostgreSQL、MySQL、MariaDB、SQL Server、Oracle、SQLite（`src/connectors/` 目录与 README）。
- 工具：`execute_sql`、`search_objects`，可选 `explain_sql`、`health_check`；`dbhub.toml` 可定义自定义工具（预定义 `statement` + 类型化 `parameters`）。
- 参数绑定：自定义工具按引擎用 `$1`（PG）、`?`（MySQL/MariaDB/SQLite）、`@p1`（SQL Server）、`:1`（Oracle）；类型 `string/integer/float/boolean/array`，支持 `allowed_values`、`default`、`required`。`execute_sql` 本身只有 `sql` 文本，没有参数。
- 只读：
  - `execute_sql` 的 `readonly=true`：先过只读关键字白名单，再走引擎级机制。源码 `dist/chunk-*.js` 仅实现 PG `BEGIN READ ONLY`、SQLite `PRAGMA query_only=ON`、MySQL/MariaDB `SET SESSION TRANSACTION READ ONLY; START TRANSACTION READ ONLY`。**SQL Server / Oracle 没有引擎级实现**，只有关键字分类。
  - 自定义工具**不能**设置 `readonly`，配置校验会直接报错；其 SQL 只做分类并写入 `readOnlyHint`，不阻断写。
- 行数：`max_rows` 是 `[[tools]]` 配置项（仅 `execute_sql`），不是调用参数；SELECT 取 `max_rows` 与 SQL 内 `LIMIT/TOP` 的较小值，多取一行判 `truncated`。
- 连接池：`pool_max_connections` 仅 PostgreSQL/MySQL（每进程每源，默认 10，1–1000）；`query_timeout` 服务端 + 客户端兜底；`[sources.session]` 可下发会话设置（PG 作为 startup options，PgBouncer 可能丢弃）。
- 传输：stdio 与 `--transport http`；结果为 `content[].text` 里的 JSON 字符串，无 `structuredContent`。
- 配置热重载：TOML 变更后重连/重建；没有不可变版本语义。

### 5.2 判定

- 比 Toolbox 更接近本项目：有类型化自定义工具、有 `max_rows`、有 `truncated`、有 PG/MySQL 的引擎级只读、MIT、部署轻。
- 但仍不能承接 `db_query_readonly`：`execute_sql` 无绑定参数，自定义工具是预注册 statement（不是任意 SQL）；`max_rows` 不能按调用下压；SQL Server/Oracle 没有 L2；结果需要从 `content[].text` 二次解析。
- 也不能承接 `db_run_template` 的完整语义：自定义工具不强制只读、没有版本/审批/EXPLAIN 预检/缓存键；热重载与版本钉住冲突。
- 结论：**只作为探索型只读查询的参考实现**（尤其是 `max_rows` 的 probe 行与 `truncated` 设计）；不进主链路。

## 6. Trino 484

### 6.1 事实

- Apache-2.0，13.5k stars；文档版本 484。
- 连接器 37 个（`trino.io/docs/current/connector.html`）：BigQuery、Cassandra、ClickHouse、Delta Lake、Druid、DuckDB、Elasticsearch、Exasol、Hive、Iceberg、Kafka、MariaDB、MongoDB、MySQL、Oracle、PostgreSQL、Redshift、SingleStore、Snowflake、SQL Server、TPC-DS/TPC-H 等。**没有 DB2、达梦、金仓、虚谷**。
- 预编译：`PREPARE name FROM SELECT ... ?`，`EXECUTE name USING ...`；参数用 `?`（`sql/prepare.html`）。JDBC 驱动 `explicitPrepare` 默认 true，即 `PREPARE` + `EXECUTE`，`false` 时用 `EXECUTE IMMEDIATE`（`client/jdbc.html`）。
- 只读：系统访问控制 `read-only`，拒绝写数据/元数据（`security/built-in-system-access-control.html`）。
- 资源限制：`query.max-run-time`、`query.max-execution-time`、`query.max-scan-physical-bytes`、`query.max-output-data-size` 等（`admin/properties-query-management.html`），是资源/字节/时间限制，不是行数上限。
- 部署：Java **25**（最低 25.0.1），coordinator + workers，`-Xmx` 建议 16G 起（`installation/deployment.html`）。
- Rust 客户端：`trino-rust-client 0.12.0`（MIT，2026-08-19，70 万下载）通过 `X-Trino-Prepared-Statement` 头支持 `PREPARE/EXECUTE`；`prusto 0.5.2` 是更老的 Presto 客户端。

### 6.2 判定

- 作为“多源连接层”只在**确实需要跨源 join/联邦**时成立；它的价值是统一 SQL 与跨 catalog 查询，不是驱动复用。
- 代价：JVM 集群（至少 coordinator + worker）、每个源一个 catalog、另一套 SQL 方言（`db_run_template` 的 PG/MySQL 语法要改）、跨 catalog join 通常要 shuffle 数据而非下推、资源限制不等于行数限制。
- 长尾引擎不可达：达梦/金仓/虚谷/DB2 没有连接器；Oracle/SQL Server 有，但只读与参数化仍受连接器能力约束（未逐一核验）。
- 结论：**不作为默认连接层**；仅当出现“跨多个主流源联合分析”的明确需求时单独立项。

## 7. JDBC / ODBC 标准桥

### 7.1 ODBC：可用，且是首选标准桥

- `odbc-api` 直接调 ODBC C API；Windows 自带管理器，Linux 需 unixODBC；厂商提供驱动。
- 绑定参数是 ODBC 核心能力（`?` + `SQLBindParameter`），`odbc-api` 的 `execute`/`prepare` 已封装；结果按列缓冲读取，需自己做类型映射与行截断。
- 覆盖目标：SQL Server、Oracle、DB2、达梦、金仓、虚谷、Access、甚至 PG/MySQL/SQLite。它是“一个执行器覆盖长尾”的唯一现实选项。
- 限制：同步阻塞；驱动安装/版本/位数耦合；字符集与 Unicode 依驱动；`SQLCancel`/超时行为依驱动；只读语义依引擎（Oracle 可 `SET TRANSACTION READ ONLY`，SQL Server 无）。

### 7.2 JDBC：没有成熟的通用开源桥

核验到的候选都不满足“通用 + 绑定参数 + 可嵌入 Rust + 宽松许可”：

| 候选 | 结论 |
|---|---|
| **AceQL HTTP**（kawansoft/aceql-http） | 支持 `PreparedStatement`、连接池、SQL firewall；但许可证是 **BUSL-1.1**（源码可见、生产使用按数据库数量收费），不是 OSI 开源。 |
| **ClickHouse/clickhouse-jdbc-bridge** | Apache-2.0，但**2025-10 已归档**；定位是 ClickHouse 的 `jdbc()` 表函数代理，不是通用查询网关。 |
| **Apache Linkis** | Apache-2.0，3.4k stars；支持 JDBC 引擎与参数化，但它是完整计算中间件（Eureka、Manager、引擎插件、资源治理），不是薄桥；引入成本高于自研一个窄 sidecar。 |
| **Apache Kyuubi** | Apache-2.0，面向 Spark/Flink/Trino 的 SQL 网关，不是任意 JDBC 数据库。 |
| **ADBC JDBC Adapter** | Apache-2.0，官方 `adbc-driver-jdbc`；但它是 **JVM 库**，只能被 Java/Scala/Kotlin 客户端使用，Rust 侧无法直接加载。 |
| **jdbc-rs（crates.io `jdbc` 0.1.0）** | 3.2k 下载、2023 年后无更新；不具生产可用性。 |

结论：**不存在可直接复用的通用开源 JDBC 桥**。若某引擎只有 JDBC 驱动、ODBC 不可用，最小自建边界是：

> 另一条可选路径是用 `jni`/`j4rs` 在 Rust 进程内嵌 JVM 直接调 JDBC。它省掉 sidecar 进程，但把 JVM 生命周期、崩溃隔离、类路径/驱动加载、线程附着都变成自研代码，且仍然要自己实现 prepare/bind/fetch 契约；不推荐作为首选。

1. 一个 Java sidecar 进程，只暴露 `connect / prepare / bind / execute / fetch(n) / cancel / close` 六个动词；
2. 参数按 JSON 类型映射到 JDBC `setObject`，禁止把值拼进 SQL；
3. 结果返回 `columns[] + rows[][]`（或 Arrow IPC），由 Rust 侧整形；
4. 只读由 sidecar 在连接上执行引擎对应语句（Oracle `SET TRANSACTION READ ONLY`；SQL Server 无，只能靠账号）；
5. 进程生命周期、鉴权、审计由本项目管理；sidecar 不暴露 MCP。

这条路的成本约等于“自己实现一个 DBHub 的 JDBC 子集”，只应在 ODBC 被明确否决后启动。

## 8. DBX 窄化（对照，不重复调研）

既有结论（`notes/mcp_plan/research_dbx_delegation.md`、`dbx_delegation_inventory.md`；对象为 `dbx-mcp 0.4.141`）：

- 无绑定参数：MCP/CLI/核心 API 只接受 SQL 文本；
- 结构化结果 ≤100 行：`dbx_execute_batch` 固定 `BATCH_MAX_ROWS=100` 且无 `max_rows`；`dbx_execute_query` 有 `max_rows` 但只回 Markdown；
- 无 per-agent 身份：单 workspace、HTTP 单 Bearer Token；
- 无哈希链审计：只有本地 `history` 表；
- 只读是应用层语句分级，不是 DB 会话/事务级锁。

因此 DBX 只能作为“无参数、≤100 行、单引擎、探索型”的受管执行后端，不能承接 `db_query_readonly` 或模板。本笔记不再展开。

## 9. 按目标引擎分档的结论

### 9.1 主流（PG / MySQL / SQL Server / ClickHouse）

| 引擎 | 首选 | 理由 |
|---|---|---|
| PostgreSQL | **sqlx 0.9**（现实现） | 纯 Rust、绑定、池、DSN 只读锁都已落地；`pg_executor.rs` 可直接复用。 |
| MySQL / MariaDB | **sqlx 0.9** | 同一依赖加 feature；`?` 占位与现有 `Dialect::supports_question_placeholder` 对齐；只读用只读账号 + `SET SESSION TRANSACTION READ ONLY`。 |
| SQL Server | **tiberius 0.13 + bb8/deadpool** | sqlx 无 MSSQL；tiberius 纯 Rust 异步。注意 SQL Server 无只读事务，L2 不可得。 |
| ClickHouse | **clickhouse 0.15.3** | 官方 crate、HTTP、绑定；无事务，只读用服务端只读用户。 |

这一档不需要任何外部服务，也不需要 Toolbox/DBHub/Trino。

### 9.2 国内 / JDBC-only（达梦 / 金仓 / 虚谷 / Oracle / DB2）

| 引擎 | 首选 | 事实与限制 |
|---|---|---|
| Oracle | `oracle 0.6.3` 或 ODBC | 原生 crate 需 Instant Client；ODBC 需 Oracle ODBC 驱动。两者都可 `SET TRANSACTION READ ONLY`。 |
| DB2 | **ODBC**（`odbc-api`）/ `ibm_db 1.0.8` | 无成熟纯 Rust 协议驱动；IBM 官方 `ibm_db` 依赖 DB2 CLI 驱动（未核验其是否绕过 ODBC 层）。 |
| 达梦 DM | **ODBC** | crates.io `dameng 0.1.0`（295 下载）、`dameng-helper 0.2.4`（2022）均不具生产可用性；达梦提供 JDBC/ODBC 驱动。 |
| 金仓 Kingbase | **ODBC 优先；sqlx PG 需实测** | 金仓是 PG 系，理论可复用 PG wire protocol，但驱动/版本/语法兼容未核验；JDBC/ODBC 驱动由厂商提供。 |
| 虚谷 Vastbase | **ODBC 优先；sqlx PG 需实测** | 同金仓，PG/OpenGauss 系；未核验。 |

这一档的结论：

- **ODBC 是唯一“一套代码覆盖全部”的方案**；`odbc-api` 提供绑定参数，满足模板与只读查询的基本条件。
- 纯 Rust 原生驱动只在 Oracle 有可接受选项；达梦/金仓/虚谷没有。
- L4 方言：`sqlparser 0.63` 已有 `MsSqlDialect`、`OracleDialect`；`jumcp-sqlguard/src/dialect.rs` 目前只接了 7 种，需要为 MSSQL/Oracle 增加映射，达梦/金仓/虚谷只能用 `GenericDialect` 近似（或按 Oracle/PG 方言），这是**弱校验**，必须明说。
- 只读：Oracle 可用 `SET TRANSACTION READ ONLY`；达梦是否支持未核验；SQL Server 无；DB2 的 `SET TRANSACTION READ ONLY` 支持未核验。无论如何 L1 必须先行。

### 9.3 GCP / 云托管

| 目标 | 首选 | 理由 |
|---|---|---|
| Cloud SQL (PG/MySQL/SQL Server) | Cloud SQL Auth Proxy + 对应原生驱动 | 代理负责连接与 IAM；驱动层用 sqlx/tiberius。 |
| AlloyDB | 同 Cloud SQL（PG 协议） | `alloydb_session_read_only=locked` 可作 L2。 |
| BigQuery | `google-cloud-bigquery 0.19.0` 或 ADBC BigQuery | 官方 Rust 客户端已稳定；无事务，只读靠 IAM + dry-run/授权。 |
| Spanner | `google-cloud-spanner 0.34.8-preview` | 官方 crate 仍标 preview；生产需评估。 |
| 统一入口 | **Toolbox** 的 GCP 托管源 | 它是唯一在这些源上提供引擎级只读锁（`readOnly: true`）的开源方案；但仅限 GCP 托管，且无行数上限。 |

结论：GCP 场景可以“Toolbox 作为执行后端 + 本项目治理前门”，但只用于**模板化、SQL 内已带 LIMIT** 的查询；`db_query_readonly` 仍应走本项目执行器（Cloud SQL 用 PG/MySQL 原生驱动即可）。

### 9.4 多源联邦

- Trino 是唯一成熟的开源联邦层；但它没有达梦/金仓/虚谷/DB2 连接器，且引入 JVM 集群与另一套方言。
- 若联邦范围只在 PG/MySQL/SQL Server/Oracle/ClickHouse/BigQuery 等已有连接器内，可评估 Trino；否则不成立。
- 结论：**不为“多源”而引入 Trino**；本项目的多源应停留在“同一执行器按 datasource 路由”，联邦查询留给未来的独立能力。

## 10. 推荐路线

### 10.1 最小改动（覆盖主流四类引擎）

1. 保留 `PgExecutor` 不动。
2. 新增 `MySqlExecutor` / `SqliteExecutor`，复用 `sqlx` 依赖（增加 `mysql`、`sqlite` features），沿用 `QueryExecutor` 与 `shape_rows`。
   - 占位符：模板层的 `:name` 按引擎改写为 `?`；`db_query_readonly` 的 `params` 仍只接受位置数组。
   - 只读：只读账号 + `SET SESSION TRANSACTION READ ONLY`（MySQL）/`PRAGMA query_only=ON`（SQLite）；启动自检写一条 DDL 必须失败。
3. 新增 `ClickHouseExecutor`，用官方 `clickhouse` crate；只读用服务端只读用户；结果解码按列类型映射到 `Value`。
4. 新增 `MsSqlExecutor`，用 `tiberius` + `bb8`；明确记录“SQL Server 无 L2，只有 L1 + L4”。
5. `jumcp-sqlguard::Dialect` 增加 `MsSql`（`MsSqlDialect`）；不需要为 MySQL/ClickHouse 新增（已有）。

代价：每引擎一个模块 + 一套类型解码；约 3 个新依赖（`sqlx` features 不算新依赖、`clickhouse`、`tiberius`、`bb8`）。收益：主流引擎全部有绑定参数、池、结构化结果，且完全在进程内，审计/行数/token 契约不变。

### 10.2 最大覆盖（再加长尾）

在 10.1 之上增加 `OdbcExecutor`：

- 一个执行器 + 一个 `OdbcDialectProfile`（连接串、占位符、只读语句、标识符引用、类型映射）。
- 覆盖 Oracle/DB2/达梦/金仓/虚谷/SQL Server/Access；遇到已有原生执行器的引擎可优先走原生，ODBC 作为兜底。
- 阻塞调用放 `tokio::task::spawn_blocking`；每个 datasource 一个连接池（`r2d2` 或自管 `Vec<Connection>`），池大小可配。
- 只读：按引擎发 `SET TRANSACTION READ ONLY`（Oracle 可用；达梦未核验）；SQL Server 只靠账号；启动自检写一条 `CREATE TABLE` 必须失败。
- 行数：优先 `SQL_ATTR_MAX_ROWS`（若驱动支持），否则 `LIMIT`/`FETCH FIRST` 包裹；仍要 probe 行判 `truncated`。

这是“一个进程覆盖全部目标引擎”的上限；再往下就只能自建 JDBC sidecar。

### 10.3 必须自研的部分

以下能力没有任何候选库/服务能替，必须留在本项目：

| 能力 | 为什么不能外包 |
|---|---|
| 命名参数 → 位置占位符的改写与校验 | 模板用 `:name`，各引擎用 `$n`/`?`/`@p1`/`:1`；改写必须与 AST 校验使用同一套解析，否则“校验一套、执行另一套”。 |
| `max_rows` 夹紧 + 精确 `truncated` | 需要“取 n+1 行”或驱动级 max rows；Toolbox/DBHub/Trino 都不提供 per-call 语义。 |
| `cell_char_limit` / `next_offset` 续读 | 只有本项目的结果整形层有这个契约。 |
| `max_tokens` 预算与摘要 | 生态工具只做行数/字节，不做 token。 |
| 只读分层（L1/L2/L4） | 外部产品的只读是产品级开关，不按 datasource/引擎组合表达；SQL Server 等引擎根本没有 L2。 |
| 版本钉住、EXPLAIN 预检、缓存键 | Toolbox/DBHub 配置热重载，没有不可变版本。 |
| 哈希链审计与 `sql_hash` | 外部工具只有本地日志；DBX 已确认无哈希链。 |
| schema 白名单与 `MCP_SCHEMA_OUT_OF_SCOPE` | 生态工具没有这个语义。 |

### 10.4 坑

1. **不要把 `sqlx` 当统一抽象**：MSSQL 已移除；`Any` 只覆盖 PG/MySQL/SQLite；动态解码必须逐引擎写。
2. **`fetch_all` 后再截断不是行数控制**：现 `pg_executor.rs` 的 `run` 用 `fetch_all`，`shape_rows` 在内存里截断；大结果会先全量进内存。新增执行器应改为 `LIMIT n+1` 或流式 `fetch`，并把这一点作为 POC 验收项。
3. **MySQL 的 `?` 与 PG 的 `?` 语义不同**：PG 的 `?` 是 jsonb 运算符，`Dialect::supports_question_placeholder` 已处理解析层；绑定层仍要按引擎区分。
4. **Oracle Instant Client**：运行时依赖、位数、`LD_LIBRARY_PATH`/`PATH`、容器镜像体积；`oracle` crate 是同步的，必须 `spawn_blocking`。
5. **ODBC 驱动是厂商资产**：达梦/金仓/虚谷的 Linux 驱动版本与数据库版本耦合；Unicode/字符集、`SQLCancel`、`SQL_ATTR_MAX_ROWS` 支持度都要逐驱动验证。
6. **SQL Server 没有只读事务**：`ApplicationIntent=ReadOnly` 不是写保护；不要把 L2 写进文档承诺。
7. **Toolbox 的 `templateParameters` 是注入面**：官方文档自己标注“more vulnerable to SQL injections”；本项目的 `column_ref` 必须继续用白名单 + 标识符引用，不能直接映射过去。
8. **Toolbox 原生 API 是隐藏入口**：`--enable-api` 不在 CLI reference 中，且与 MCP Auth 互斥；依赖它等于依赖未文档化行为。
9. **DBHub 自定义工具不强制只读**：只设置 `readOnlyHint` 注解；`readonly` 字段在自定义工具上是启动错误。
10. **Trino 不是驱动层**：JVM 25、coordinator/worker、catalog 配置、方言迁移、跨源 join 的 shuffle 成本，都会把“省一个执行器”变成“多运维一个集群”。

## 11. 对现有 `db_query_readonly` / `db_run_template` 契约的影响

### 11.1 `db_query_readonly`

- **可以替换的部分**：连接、池、驱动、绑定参数、类型解码。替换后请求/响应 JSON 形状不变。
- **不能替换的部分**：`max_rows`（1–1000 夹紧）、`cell_char_limit`（1–4000）、`cell_char_offset`/`next_offset`、`max_tokens`、`truncated`、`effective_max_rows`、`sql_hash`、`allowed_schemas`、单语句 L4。任何外部后端都会把这部分推回上游；上游仍要 parse/计数/整形，等于没有减少自研量。
- **只读**：外部后端最多提供“产品级只读”，不能按 datasource 表达 L1/L2；且 SQL Server 等引擎无 L2。结论：只读分层必须留在本项目，外部后端只能作为“执行通道”。
- **审计**：外部后端的日志不是哈希链，且可能记录参数（Toolbox 在 debug 日志打印 `invocation params`）。若引入，必须只把外部当无状态执行器，审计以本项目请求为准，并关闭外部详细日志。

### 11.2 `db_run_template`

- **可以替换的部分**：预定义 SQL 的绑定执行。Toolbox 的 `parameters` 与 DBHub 的自定义工具都能做位置绑定；`$1`/`?`/`@p1`/`:1` 的差异可在注册期按引擎生成。
- **不能替换的部分**：
  - `(template_id, version)` 会话钉住：两者都是热重载配置，没有版本对象；
  - 发布期 EXPLAIN 与 `preflight` 字段：Toolbox/DBHub 没有；
  - 结果缓存键 `(template_id, version, normalized_params_hash)`：外部工具不暴露；
  - `constraints.allowed_schemas` / `forbidden_columns` / `auto_limit`：需要上游改写/校验；
  - `column_ref` 白名单标识符：Toolbox `templateParameters` 的转义弱于本项目的 enum + quote。
- 结论：模板可以“借声明结构”，但执行仍应在本项目；若一定要外包，必须把每个模板版本物化成一份不可变配置并重启外部服务，代价高于自研。

## 12. POC 建议（不实施）

### POC：`OdbcExecutor` 可行性验证

目标：确认“一个 ODBC 执行器覆盖 SQL Server + Oracle + 达梦”能否满足 `db_query_readonly` 的契约，并与原生 `PgExecutor` 的输出逐字段对齐。

范围：

- 新增一个仅用于 spike 的 `OdbcExecutor`（不合并），实现 `QueryExecutor`；
- 连接串来自环境变量；只读账号；
- 引擎矩阵：SQL Server（ODBC Driver 18）、Oracle（Instant Client ODBC）、达梦（厂商 ODBC 驱动，若环境可得）。

验收项：

1. **绑定参数**：`$1` 位置参数（或模板改写的 `?`）绑定到 `?`，SQL 文本中不出现参数值；注入字符串（`' OR 1=1 --`）作为值传入不改变语义。
2. **行数**：`max_rows=3` 返回 3 行且 `truncated=true`；表恰好 3 行时 `truncated=false`；`max_rows=1000` 被夹紧；实现不得先全量取回再截断（用 100 万行表观察内存/首包时间）。
3. **单元格**：`cell_char_limit=200` 截断并给 `next_offset`；按 offset 续读能拼回原值；中文/emoji 按字符而非字节截断。
4. **超时**：`statement_timeout_ms` 到点后查询被取消，返回 `MCP_TIMEOUT`，连接不被污染；下一请求正常。
5. **只读**：只读账号下 `CREATE TABLE`/`INSERT` 失败；若引擎支持 `SET TRANSACTION READ ONLY`，在该会话下写也失败；`COMMIT; SET ...=off; ...` 链式输入被 L4 拒绝。
6. **类型**：`NUMBER(38,10)`/`DECIMAL` 不丢精度（JSON 数字或字符串的取舍要固定）；`DATE`/`TIMESTAMP WITH TIME ZONE` 带时区；`CLOB`/`BLOB` 截断与续读；`NULL` 与空串区分。
7. **错误映射**：连接失败、权限拒绝、语法错误、超时、结果集过大分别映射到现有 `McpError` 类别；不泄露 DSN 密码。
8. **审计**：每次调用有 `sql_hash`、`elapsed_ms`、`row_count`；参数值不进入日志。
9. **平台**：Windows 与 Linux 各跑一遍；Unicode DSN/密码；驱动缺失时启动期 fail-closed。
10. **对比**：同一 SQL 在 ODBC 与原生执行器上的 `rows`、`truncated`、`effective_max_rows` 完全一致。

否决红线（任一成立即放弃 ODBC 路线）：

- 驱动不支持绑定参数，只能字符串拼接；
- 驱动无法取消/超时，长查询会一直占用连接；
- 只能返回格式化文本，或 `NUMBER`/`DECIMAL` 精度丢失且无法恢复；
- 每查询需要 GUI/交互式授权，或驱动许可证不允许随服务部署；
- 只读无法在账号或会话层建立，且 L4 无法覆盖该方言；
- 驱动在目标 OS 上不可获得，或与数据库版本强绑定到无法升级。

可选对照实验（不是 POC 主体）：用 DBHub 1.6.0 的 `dbhub.toml` 自定义工具跑同一组 Oracle/SQL Server 查询，验证 `max_rows` + `truncated` + 参数绑定；预期结论是“模板可用、`db_query_readonly` 不可用”，用于验证第 5 节的判断。

## 13. 证据与未核验

### 13.1 版本与来源

| 事实 | 来源 |
|---|---|
| sqlx 0.9.0；支持 PG/MySQL/MariaDB/SQLite；MSSQL 自 0.7.0 移除；MSRV 1.94 | crates.io `sqlx`；crate `README.md:74`；`CHANGELOG.md` 0.7.0 节；`Cargo.toml` `rust-version` |
| tiberius 0.13.0；bb8-tiberius 0.16.0；deadpool-tiberius 0.1.10 | crates.io |
| clickhouse 0.15.3；官方 crate；`Client` 内含 HTTP pool；`Query::bind` 绑定 `?` | crates.io；crate `README.md`；`src/lib.rs:51`、`src/query.rs:57-71` |
| oracle 0.6.3；ODPI-C；Instant Client 11.2+；`PoolBuilder` | crates.io；crate `README.md`；`src/pool.rs:295-417` |
| odbc-api 30.0.0；ODBC 3.80；`execute`/`prepare` 参数绑定 | crates.io；crate `README.md`；`src/connection.rs:162`、`src/prepared.rs:51` |
| arrow-odbc 27.0.0；Arrow RecordBatch；`execute(sql, parameters, timeout)` | crates.io；crate `README.md`；`src/reader/odbc_reader.rs:45-52` |
| connectorx 0.4.6；源：PG/MySQL/MSSQL/Oracle/BigQuery/Trino/ClickHouse/SQLite/CSV；无绑定参数 | crates.io；`Cargo.toml` `[features]`；`src/lib.rs` 示例（`queries: &[CXQuery]`） |
| adbc_core 0.25.0；`prepare`/`bind`/`execute`；驱动清单 | crates.io；`src/sync.rs:421-485`；`arrow.apache.org/adbc/current/driver/index.html` |
| Toolbox 1.16.0；Apache-2.0；50 个集成；`readOnly` 仅托管源；原生 API 默认 410 | `mcp-toolbox.dev` 文档页脚；`docs/en/integrations/`；`internal/sources/postgres/postgres.go`；`internal/server/api.go`；`internal/server/server.go`；`cmd/root.go` |
| Toolbox 默认注解 `ReadOnlyHint=false, DestructiveHint=true` | `internal/tools/tools.go` `NewDestructiveAnnotations` |
| DBHub 1.6.0；MIT；Node ≥22.5；引擎/参数/只读/行数 | npm `@bytebase/dbhub`；`dbhub.ai/tools/execute-sql.md`、`dbhub.ai/config/toml.md`、`dbhub.ai/tools/custom-tools.md`；包内 `dist/chunk-*.js` |
| Trino 484；37 连接器；无 DB2/达梦/金仓/虚谷；`PREPARE`/`EXECUTE`；read-only access control；Java 25 | `trino.io/docs/current/connector.html`；`sql/prepare.html`；`client/jdbc.html`；`security/built-in-system-access-control.html`；`installation/deployment.html` |
| Trino Rust 客户端 0.12.0（MIT）支持 prepared statement 头 | crates.io；`trino-rust-client` `src/client.rs:196-203,345` |
| JDBC 桥：AceQL BUSL；clickhouse-jdbc-bridge 归档 2025-10；ADBC JDBC Adapter 仅 JVM；Linkis 为中间件 | GitHub 仓库元数据；ADBC drivers 页 “Java/JVM” 表 |
| DBX 约束 | `notes/mcp_plan/research_dbx_delegation.md`、`dbx_delegation_inventory.md` |
| 现实现：sqlx PG、`fetch_all` 后 `shape_rows`、`READONLY_OPTIONS`、`QueryParams` 夹紧 | `backend/crates/jumcp-server/src/pg_executor.rs:19,120-205,240-313`；`executor.rs:31-100`；`backend/crates/jumcp-sqlguard/src/dialect.rs` |

### 13.2 参考链接

- sqlx：<https://crates.io/crates/sqlx>、<https://github.com/launchbadge/sqlx/blob/main/CHANGELOG.md>
- tiberius：<https://crates.io/crates/tiberius>
- clickhouse：<https://crates.io/crates/clickhouse>、<https://github.com/ClickHouse/clickhouse-rs>
- oracle：<https://crates.io/crates/oracle>、<https://github.com/kubo/rust-oracle>
- odbc-api：<https://crates.io/crates/odbc-api>、<https://github.com/pacman82/odbc-api>
- arrow-odbc：<https://crates.io/crates/arrow-odbc>、<https://github.com/pacman82/arrow-odbc>
- connectorx：<https://crates.io/crates/connectorx>、<https://github.com/sfu-db/connector-x>
- ADBC：<https://crates.io/crates/adbc_core>、<https://arrow.apache.org/adbc/current/driver/index.html>
- Toolbox：<https://mcp-toolbox.dev/documentation/configuration/security/read-only/>、<https://mcp-toolbox.dev/documentation/configuration/tools/>、<https://mcp-toolbox.dev/reference/cli/>、<https://github.com/googleapis/mcp-toolbox>
- DBHub：<https://dbhub.ai/tools/execute-sql>、<https://dbhub.ai/config/toml>、<https://dbhub.ai/tools/custom-tools>、<https://github.com/bytebase/dbhub>
- Trino：<https://trino.io/docs/current/connector.html>、<https://trino.io/docs/current/sql/prepare.html>、<https://trino.io/docs/current/security/built-in-system-access-control.html>
- AceQL：<https://github.com/kawansoft/aceql-http>；clickhouse-jdbc-bridge：<https://github.com/ClickHouse/clickhouse-jdbc-bridge>；Linkis：<https://github.com/apache/linkis>
- 现有实现：`backend/crates/jumcp-server/src/pg_executor.rs`、`backend/crates/jumcp-server/src/executor.rs`、`backend/crates/jumcp-sqlguard/src/dialect.rs`
- DBX：`notes/mcp_plan/research_dbx_delegation.md`、`notes/mcp_plan/dbx_delegation_inventory.md`

### 13.3 未核验

- 金仓/虚谷与 PostgreSQL wire protocol 的兼容程度（sqlx 能否直接连通、系统表/权限/占位符差异）。
- 达梦/金仓/虚谷 ODBC 驱动在目标 Linux/Windows 上的可得性、位数、Unicode、`SQLCancel`、`SQL_ATTR_MAX_ROWS` 支持。
- 达梦/DB2 是否支持 `SET TRANSACTION READ ONLY` 及其对 DDL 的阻断程度。
- Toolbox `--enable-api` 在 1.16 的实际可用性与后续版本稳定性；原生 API 对数据库工具的完整响应（只读了 handler 代码，未运行）。
- Toolbox 各源自带的池参数（文档未列；源码只见 pgxpool 默认值，MySQL 未逐行核验）。
- DBHub `--transport http` 下“每请求新建 server”对连接池生命周期的影响。
- DBHub SQL Server/Oracle 是否存在未在 `dist` 中出现的只读实现（源码 grep 未见）。
- ADBC 各驱动（尤其 MSSQL/Trino/ClickHouse）的生产成熟度与参数绑定一致性。
- Trino 跨 catalog join 的下推行为与实际性能；Oracle/SQL Server 连接器的只读/参数能力。
- `oracle` crate 在 Oracle 19c/21c/23ai + Instant Client 版本组合下的稳定性。
- `clickhouse` crate 对未知列类型/Nullable/LowCardinality 的动态解码能力（其定位是类型化 struct）。
- `google-cloud-spanner 0.34.8-preview` 的生产可用性。
