# 数据库连接层替代方案：能力矩阵（采集事实，不含架构结论）

采集时间：2026-11-10。只读采集，未改动代码/配置。

## 0. 核验方法与证据来源

| 渠道 | 用途 | 说明 |
|---|---|---|
| `https://crates.io/api/v1/crates/{name}` 与 `/crates/{name}/{version}` | 版本、License、发布时间、MSRV、feature 列表 | JSON 原文，字段 `newest_version` / `max_stable_version` / `versions[].license` / `versions[].features` |
| `git ls-remote --tags --refs <repo>` | 最新 tag（避开 GitHub API 限流） | 只读网络查询 |
| `https://codeload.github.com/googleapis/genai-toolbox/tar.gz/refs/heads/main` | Toolbox 源码快照 | 解包到本机临时目录后读源码，行号即该快照行号（`main` 分支，2026-11-10 拉取） |
| `https://codeload.github.com/bytebase/dbhub/tar.gz/refs/heads/main` | DBHub 源码快照 | 同上（`package.json` version=1.6.0） |
| `https://ungh.cc/repos/<owner>/<repo>` | star / 最近 push 时间 | 第三方镜像 GitHub 仓库元数据 |
| `https://raw.githubusercontent.com/...` 与 `https://trino.io/docs/current/...` | README / 官方文档正文 | Trino 文档当前版本为 **484** |

未从 GitHub REST API 取得 release 资产列表（限流 403），因此「release 资产形态」相关项标注为未核验。

---

## 1. Rust 数据库访问库

### 1.1 基础信息

| crate | 最新版 | 发布日期 | License | 仓库 | MSRV | 维护状态（star / 最近 push） |
|---|---|---|---|---|---|---|
| `sqlx` | **0.9.0** | 2026-05-21 | MIT OR Apache-2.0 | github.com/launchbadge/sqlx | 1.94.0 | 17637 star / 2026-10-31 |
| `clickhouse`（官方客户端） | **0.15.3** | 2026-10-13 | MIT OR Apache-2.0 | github.com/ClickHouse/clickhouse-rs | 1.89.0 | 570 star / 2026-11-07 |
| `oracle` | **0.6.3** | 2025-01-02 | UPL-1.0/Apache-2.0（crates.io 原文写法） | github.com/kubo/rust-oracle | 1.60.0 | 229 star / 最近 push **2025-03-23**（低速） |
| `odbc-api` | **30.0.0** | 2026-11-07 | MIT | github.com/pacman82/odbc-api | 未标注 | 188 star / 2026-11-08 |
| `arrow-odbc` | **27.0.0** | 2026-11-07 | MIT | github.com/pacman82/arrow-odbc | 未标注（edition 2024） | 同上仓库作者 / 2026-11 |
| `connectorx` | **0.4.6** | 2026-09-18 | MIT | github.com/sfu-db/connector-x | 未标注 | 2659 star / 2026-11-02 |
| `adbc_core` / `adbc_driver_manager` / `adbc_ffi` | **0.25.0** | 2026-10-26 | Apache-2.0 | github.com/apache/arrow-adbc | 1.85 | 636 star / 2026-11-06 |
| `tiberius`（MSSQL/TDS） | **0.13.0** | 2026-09-25 | MIT/Apache-2.0 | github.com/prisma/tiberius | 1.88 | 448 star / 2026-09-25 |

补充事实：
- `sqlx` 下载量 185,637,622；`sqlx-core`/`sqlx-mysql`/`sqlx-postgres`/`sqlx-sqlite` 同为 0.9.0（2026-05-21）。
- crate 名 `adbc` 与 `arrow-adbc` 在 crates.io **不存在**（404）；Rust 侧对应物是 `adbc_core` / `adbc_driver_manager` / `adbc_ffi`。
- `clickhouse` 是 ClickHouse 官方 crate（描述原文 "Official Rust client for ClickHouse DB"）。

### 1.2 能力对比

| crate | 支持引擎 | 绑定参数方式 | 连接池 | 异步/阻塞 | 结果类型 | 只读能力 |
|---|---|---|---|---|---|---|
| `sqlx` 0.9.0 | Postgres、MySQL、MariaDB（同一 `mysql` feature）、SQLite、`Any` 运行期切换 | PG `$1`、MySQL `?`；`query(...).bind()`；编译期校验宏 `query!` | 有（`sqlx::Pool` / `PgPoolOptions` 等） | 异步（tokio / async-std / actix，feature 选定） | 行 → `FromRow`/derive 映射；无原生 Arrow | 无内建只读模式（需 `SET TRANSACTION READ ONLY` 等 SQL 层手段）【推断：仓库未检索到只读开关】 |
| `clickhouse` 0.15.3 | ClickHouse | `?` 占位 + `.bind()`；`?fields` 为字段列表占位；`query_raw()` 关闭占位解析 | 无独立池对象；README 明确「复用/克隆 client 以复用连接池」 | 异步（HTTP transport） | serde 结构体行（`RowBinaryWithNamesAndTypes`/`RowBinary`） | 无只读开关（未核验） |
| `oracle` 0.6.3 | Oracle | 位置绑定 `:1`/`:2`，`conn.execute(sql, &[&v])`；`prepare` 后可多次 `stmt.execute(&[..])` | 无内建池（未核验外部池适配） | **同步/阻塞** | `Row` 取值 API | 无只读模式 |
| `odbc-api` 30.0.0 | 任意 ODBC 驱动（引擎取决于驱动） | `cursor.execute(("a", 1))` 元组参数（arrow-odbc README:48-50 示例） | 无内建池（未核验） | **同步/阻塞**；Cargo.toml features 未提供 async 开关 | ODBC 列读取 API | 取决于驱动/连接属性 |
| `arrow-odbc` 27.0.0 | 任意 ODBC 驱动 | 同上（透传 odbc-api 参数元组） | 无 | **同步/阻塞**（批次迭代器） | Arrow `RecordBatch`，`OdbcReaderBuilder::with_max_bytes_per_batch` | 取决于驱动 |
| `connectorx` 0.4.6 | Postgres、MySQL、MariaDB、SQLite、Redshift、ClickHouse、SQL Server、Azure SQL、Oracle、BigQuery、Trino（README "Sources" 节） | **无参数绑定**：API 只接 SQL 字符串 | **无池**；按 `partition_on` 分片并发取数 | 同步 API，内部多线程 | Arrow（arrow2）→ pandas/pyarrow/polars；并行流式 | 只读定位（取数工具），无强制机制 |
| `adbc_core` 等 0.25.0 | 依赖被加载的 ADBC C 驱动 | Arrow 参数（批量 bind），不是文本占位符 | 无（driver manager 管理连接） | 同步 trait API（`Statement::execute`），异步需自封装【推断】 | Arrow | 取决于驱动 |
| `tiberius` 0.13.0 | SQL Server（TDS） | `@P1` 位置命名参数 + `&[&dyn ToSql]` | **明确非目标**（README「Non-goals: Connection pooling」，建议 bb8/mobc/deadpool；`bb8-tiberius` 0.16.0，2025-01-08） | 异步 | `Row` 流 | 无 |

### 1.3 关键判定点（可直接引用的原文）

**sqlx 的 MSSQL：不支持。**
- crates.io `sqlx` 0.9.0 的 `features` 只有 `postgres` / `mysql` / `sqlite` / `any`（+ `sqlite-bundled` 等），**没有 `mssql`**。
- `sqlx-mssql` 在 crates.io 只有占位版 `0.0.0`（2019-11-27），无 0.9 发布。
- README（`launchbadge/sqlx` main，第 75 行）："MSSQL was supported prior to version 0.7, but has been removed pending a full rewrite of the driver as part of our SQLx Pro initiative."；同文件第 188 行仍列有 `mssql` feature 条目，**与 crates.io 实际 feature 表冲突，属过期文档**。
- `any` feature 的依赖只含 `sqlx-mysql?` / `sqlx-postgres?` / `sqlx-sqlite?` → `Any` 只代理 PG/MySQL/SQLite。

**oracle crate 需要 Oracle Client（OCI）。**
- README "Run-time Requirements: Oracle client 11.2 or later. See ODPI-C installation document."；实现基于 ODPI-C。
- 最新 tag 仅到 `v0.6.3`，仓库最近 push 2025-03-23；README 已出现 "0.7.0 ~" 的 MSRV 表述但无对应发布 → 活跃度低。

**odb-api / arrow-odbc 是阻塞式。**
- `odbc-api` `Cargo.toml` 的 `[features]` 只含 `narrow` / `wide` / `odbc_version_3_5` / `odbc_version_3_80` 一类，无 async feature。
- `arrow-odbc` README:46-60 使用同步 `execute("SELECT * FROM MyTable", parameters)?` + `for batch in arrow_record_batches`。

**connectorx 的输出与并行。**
- README：`cx.read_sql(dsn, "SELECT ...")`，可选 `partition_on="l_orderkey", partition_num=10` 分片并发；Destinations = Pandas / PyArrow / Modin / Dask / Polars。**接口不接受绑定参数**，也不暴露池。

---

## 2. MCP Toolbox for Databases（googleapis/genai-toolbox）

### 2.1 项目事实

| 项 | 值 | 证据 |
|---|---|---|
| 最新 tag | **v1.16.0** | `git ls-remote --tags` 排序后末位为 v1.16.0（v1.13.0…v1.16.0） |
| License | **Apache-2.0** | 仓库根 `LICENSE` 首行 "Apache License Version 2.0" |
| star | **16,828** | ungh.cc |
| 最近 commit（push） | **2026-11-07T02:51:35Z** | ungh.cc |
| 实现语言/部署 | Go；**单二进制**（linux/amd64、darwin/amd64+arm64、windows/amd64+arm64），另有 Docker 镜像、Homebrew、`npx @toolbox-sdk/server` | README「Install & Run」节（约 251-395 行） |
| MCP 传输 | `/mcp`（JSON-RPC），另有 `/sse` 路由（`internal/server/mcp.go:372,378`）；`--stdio` 走 STDIO | `internal/server/server.go:756`、`internal/cmd/flags.go:71` |

### 2.2 `sources` 精确名称（47 个 SourceType 常量）

```
alloydb-admin, alloydb-postgres, arcadedb, bigquery, bigtable, cassandra,
clickhouse, cloud-gemini-data-analytics, cloud-healthcare, cloud-logging-admin,
cloud-monitoring, cloud-sql-admin, cloud-sql-mssql, cloud-sql-mysql,
cloud-sql-postgres, cloud-storage, cockroachdb, couchbase, datalineage, dataplex,
dataproc, dgraph, elasticsearch, falkordb, firebird, firestore, http, looker,
mindsdb, mongodb, mssql, mysql, neo4j, oceanbase, oracle, postgres, redis,
scylladb, serverless-spark, singlestore, snowflake, spanner, sqlite, tidb, trino,
valkey, yugabytedb
```
（由 `internal/sources/*/*.go` 中 `SourceType string = "..."` 提取；源目录共 57 个条目，含 `sources.go`/`dialect.go`/`util.go` 等非数据源文件。）

- **达梦 / 金仓 / 虚谷：无。** 全树搜索 `dameng` / `kingbase` / `xugu` / `highgo` / `gbase` 无命中。
- **Oracle：有 `oracle`**，但驱动是 `github.com/godror/godror`（`internal/sources/oracle/oracle.go:14` 注释 "OCI driver"，`:340-341` 选择 `godror`）→ **运行时需要 Oracle Client（OCI）**。

### 2.3 tools 种类（与题目点名的对照）

| 问题 | 结论 | 证据（`internal/tools/**` `resourceType` 常量） |
|---|---|---|
| `postgres-sql` | **存在** | `postgres/postgressql/postgressql.go:31` |
| `mysql-sql` | **存在** | `mysql/mysqlsql/mysqlsql.go:30` |
| `mssql-sql` | **存在** | `mssql/mssqlsql/mssqlsql.go:31` |
| `sqlite-sql` | **存在** | `sqlite/sqlitesql/sqlitesql.go:30` |
| `oracle-sql` | **存在** | `oracle/oraclesql/oraclesql.go:18` |
| `clickhouse-sql` | **存在**（常量名 `sqlType` 而非 `resourceType`） | `clickhouse/clickhousesql/clickhousesql.go:29` |
| `trino-sql` | **存在** | `trino/trinosql/trinosql.go:29` |
| `*-execute-sql` | **存在**：`postgres-execute-sql`、`mysql-execute-sql`、`mssql-execute-sql`、`sqlite-execute-sql`、`oracle-execute-sql`、`clickhouse-execute-sql`、`trino-execute-sql`、`cockroachdb-execute-sql`、`singlestore-execute-sql`、`snowflake-execute-sql`、`spanner-execute-sql`、`tidb-execute-sql`、`oceanbase-execute-sql`、`firebird-execute-sql`、`mindsdb-execute-sql`、`bigquery-execute-sql`、`arcadedb-execute-sql` | 对应 `*/…executesql/*.go` |
| 工具总数 | **299** 个已注册 tool 类型（按 `tools.Register(...)` 的实参常量解析后去重） | 全量 `internal/tools/**` 注册调用计数 |

### 2.4 参数化：`*-sql` 支持 `$1`，`*-execute-sql` 不支持

- `postgres-sql` 配置字段（`postgressql.go:52-59`）：`statement`（必填）+ `parameters` + `templateParameters`。
- 执行路径（`postgressql.go:92-113`）：`ResolveTemplateParams` 处理模板参数 → `parameters.GetParams` → `newParams.AsSlice()` → `source.RunSQL(ctx, newStatement, sliceParams)`。
- 数据源侧（`internal/sources/postgres/postgres.go:116-118`）：`s.PostgresPool().Query(ctx, statement, params...)` → 即 **pgx 原生 `$1` 占位符**。
- `postgres-execute-sql`（`postgresexecutesql.go:67-69,93,103`）：唯一入参是 `sql` 字符串，执行时 `source.RunSQL(ctx, sql, nil)` → **无绑定参数，`$1` 不适用**（MySQL 侧同理为裸 SQL 文本）。
- 结论：**参数化能力只在「预定义 SQL 模板 + 声明参数」的 `*-sql` 工具上成立；`*-execute-sql` 是裸 SQL 文本通道。**

### 2.5 非 MCP 的 HTTP API（存在，但默认关闭）

| 事实 | 证据 |
|---|---|
| 路由：`GET /api/tool/{toolName}`、`POST /api/tool/{toolName}/invoke`、`GET /api/toolset`、`GET /api/toolset/{toolsetName}` | `internal/server/api.go:43-48` |
| 默认 **关闭**：未开 `--enable-api` 时 `/api/*` 返回 410 + "the /api native endpoints are disabled by default. Please use the standard /mcp JSON-RPC endpoint" | `internal/server/server.go:757-768` |
| 开关：`--enable-api`（"Enable the /api endpoint."） | `internal/cmd/flags.go:73` |
| 官方称其为 **legacy** HTTP API，且与 MCP Auth 互斥 | `internal/cmd/root.go:452`（"MCP Auth cannot be enabled together with the legacy HTTP API (--enable-api)"） |

### 2.6 只读 / 行数上限 / 连接池 / 认证

| 能力 | 事实 | 证据 |
|---|---|---|
| source 级 `readOnly` 字段 | **仅云托管变体与部分源提供**：`alloydb-postgres`、`alloydb-admin`、`cloud-sql-postgres`、`cloud-sql-mysql` 等；`cockroachdb` 另有 `read_only_mode` | `alloydbpg/alloydb_pg.go:63,102`；`cloudsqlpg/cloud_sql_pg.go:178-181`；`cockroachdb/cockroachdb.go:297` |
| 普通源 `IsReadOnly()` | `postgres` / `mysql` / `mssql` / `sqlite` / `oracle` / `clickhouse` / `trino` / `cockroachdb` / `tidb` / `oceanbase` / `singlestore` / `firebird` 全部 `return false` | 各 `internal/sources/<db>/<db>.go` 的 `IsReadOnly()` |
| 只读如何生效 | 会话级引擎锁（如 `alloydb_session_read_only=locked`、`cloudsql_session_read_only=locked`、MySQL `read_only_connection:true`），**不是**每查询拦截 | `alloydbpg/alloydb_pg.go:202-205`；`cloudsqlmysql/cloud_sql_mysql.go:217` |
| `execute-sql` 的注解 | 仅当 source `IsReadOnly()` 为真时把注解改写为 `readOnlyHint: true` / `destructiveHint: false` | `postgresexecutesql.go:126-140` |
| 行数上限 | **无全局上限**；唯一实现是 `cockroachdb` 的 `maxRowLimit`（默认 1000，注解 "MCP requirement: limit query results"，且 SQL 已含 LIMIT 时跳过改写） | `cockroachdb/cockroachdb.go:55,87,403-420` |
| 连接池 | postgres 用 `pgxpool.Pool`（`postgres.go:112`）；oracle/mssql 用 `database/sql`（`oracle.go:358`、`mssql.go:198`）；无对外 pool 大小配置项 | 同上 |
| 认证 | 内置 authServices：`google`（OAuth）与 `generic`；另有 MCP OAuth Protected Resource Metadata 端点 | `internal/auth/{generic,google}` 目录；`internal/server/server.go:709-748` |
| 动态加载 | `--disable-reload` 可关；默认 `tools.yaml` 热重载 | `internal/cmd/flags.go:121` |

---

## 3. DBHub（bytebase/dbhub）

| 项 | 值 | 证据 |
|---|---|---|
| 版本 | **1.6.0**（`package.json`） | 源码快照 `package.json`；最新 tag `v1.6.0` |
| License | **MIT** | `package.json` `license` 字段 + 仓库 `LICENSE` |
| star / 最近 push | 3,811 / 2026-11-08T06:29:04Z | ungh.cc |
| 运行时 | Node **>= 22.5.0**；`bin: dbhub` | `package.json` `engines`/`bin` |
| 支持引擎 | **PostgreSQL、MySQL、MariaDB、SQL Server、Oracle、SQLite** | README 第 46 行；连接器目录 `src/connectors/{postgres,mysql,mariadb,sqlserver,oracle,sqlite}` |
| 驱动依赖 | 全部为 **optionalDependencies**（`pg`、`mysql2`、`mariadb`、`mssql`、`oracledb`）；Oracle 走 `oracledb`（thin/thick 取决于驱动配置，未核验是否需要 Instant Client） | `package.json` |
| 参数绑定 | **无**：`execute_sql` 唯一入参是 `sql` 字符串（"SQL to execute (multiple statements separated by ;)"） | `src/tools/execute-sql.ts:19-21` |
| readonly | **按工具配置** `[[tools]] readonly = true`；由关键字分类器 + 数据库自身只读模式双重执行；**CLI `--readonly` 已废弃并报错** | `src/tools/execute-sql.ts:5,54-66`；`src/config/env.ts:30-39`；`docs/tools/execute-sql.mdx` |
| max_rows | **按工具配置** `[[tools]] max_rows`（仅 execute_sql 允许），连接器层用 `SQLRowLimiter.applyMaxRowsWithTruncationProbe` 改写并探测截断 | `src/config/toml-loader.ts:212-229`；`src/connectors/interface.ts:129` |
| 连接池 | 每 source `pool_max_connections`（默认 10，范围 1-1000） | `dbhub.toml.example` 注释；`src/config/toml-loader.ts:620-632` |
| MCP | STDIO（`serveStdio`）与 HTTP（`app.all("/mcp")`） | `src/server.ts:309,382` |
| 非 MCP HTTP | 只有 `/healthz`、`/api/sources`、`/api/sources/:id`、`/api/requests`（均为只读元数据，**没有 SQL 调用端点**） | `src/server.ts:269-292`；`src/api/openapi.yaml` 仅定义 `/api/sources*` |
| 其它 | 支持 SSH 隧道（`ssh2`/`ssh-config`）；另有 `search_objects`、`explain_sql`、`health_check` 内建工具 | `src/tools/` 目录；`package.json` 依赖 |

> 与既有笔记的差异：`research_db_tool_servers.md` 写的「每连接 `readonly`/`max_rows`」在 **1.6.0** 已改为 **按工具（`[[tools]]`）配置**，且 `--readonly` 命令行开关会直接报错退出。以本节为准。

---

## 4. Trino

| 项 | 事实 | 证据 |
|---|---|---|
| 文档版本 | **484** | trino.io/docs/current |
| License | **Apache-2.0** | 仓库 `LICENSE` 首行 |
| star / 最近 push | 13,488 / 2026-11-09 | ungh.cc |
| 部署依赖 | JVM 集群：**1 coordinator + N worker**（亦可 Docker / Helm）；无单机嵌入式形态 | trino.io/docs/current/installation.html |
| 连接器列表（37） | BigQuery、Black Hole、Cassandra、**ClickHouse**、Delta Lake、Druid、DuckDB、Elasticsearch、Exasol、Faker、Google Sheets、Hive、Hudi、Iceberg、Ignite、JMX、Kafka、Lakehouse、Loki、**MariaDB**、Memory、MongoDB、**MySQL**、OpenSearch、**Oracle**、Pinot、**PostgreSQL**、Prometheus、Redis、Redshift、**SQL Server**、SingleStore、Snowflake、System、TPC-DS、TPC-H、Thrift | trino.io/docs/current/connector.html |
| 预处理语句 | **提供会话级 `PREPARE name FROM <stmt>` + `EXECUTE ... USING`**，语句内用 `?` 表示参数；JDBC 默认 `explicitPrepare=true`，即 `PREPARE`+`EXECUTE`，可改 `EXECUTE IMMEDIATE` | trino.io/docs/current/sql/prepare.html；`/client/jdbc.html`（explicitPrepare） |
| 只读 | 通过 access control（如 file-based system access control）限制，非连接级 `readonly` 参数【部分未核验：未逐条读 access control 文档】 | — |
| 行数/资源限制 | 资源组提供 `hardConcurrencyLimit`、`maxQueued`、`softMemoryLimit`；**未见 `query.max-rows` 这一配置项**【未核验：properties 页为 JS 渲染，静态抓取无正文】 | trino.io/docs/current/admin/resource-groups.html |
| 达梦/金仓/虚谷 | **无连接器** | 上述 37 项列表内无 |

---

## 5. 未核验清单（明确标注）

1. `clickhouse` crate 是否提供「行数上限 / query settings」的 Rust 侧 API（未读全量源码；可用 SQL `LIMIT` 或 `SETTINGS` 兜底）。
2. `odbc-api` / `arrow-odbc` / `oracle` / `adbc_driver_manager` 是否有官方或社区连接池适配（仅确认 `bb8-tiberius 0.16.0` 存在）。
3. ADBC **驱动全集**：仓库内 `c/driver` 仅含 `flightsql`、`postgresql`、`sqlite`（+ `driver_manager`/`framework`/`common`）；Snowflake、BigQuery、DuckDB、Trino 等驱动由外部维护，未逐个核验版本。
4. Toolbox 与 DBHub 的 GitHub Release **资产清单/发布日期**（GitHub REST API 403 限流；版本号来自 tag 与 `package.json`）。
5. Toolbox `--enable-api` 的 API 是否为公开稳定契约：源码仅以 `legacy HTTP API` 表述，未见版本化/兼容承诺文档。
6. Trino `query.max-rows` 是否存在（静态抓取失败；`resource-groups` 确认的是并发/排队/内存限制，不是行数）。
7. DBHub Oracle 连接是否需要 Oracle Instant Client（`oracledb` 的 thin 模式可免，但未读其连接器实现细节）。

---

## 6. 证据索引

本地（本次采集快照，均为只读解包）：
- Toolbox main 分支源码：临时目录 `internal/sources/*`、`internal/tools/**`、`internal/server/{server.go,api.go,mcp.go}`、`internal/cmd/{flags.go,root.go}`、`LICENSE`、`README.md`。
- DBHub 1.6.0 源码：`package.json`、`src/tools/execute-sql.ts`、`src/connectors/interface.ts`、`src/config/{env.ts,toml-loader.ts}`、`src/server.ts`、`src/api/openapi.yaml`、`docs/tools/execute-sql.mdx`、`dbhub.toml.example`。

线上：
- crates.io API：`/api/v1/crates/{sqlx,sqlx-core,clickhouse,oracle,odbc-api,arrow-odbc,connectorx,adbc_core,adbc_driver_manager,adbc_ffi,tiberius,sqlx-mssql}`（版本/License/日期/features）。
- README 原文：`raw.githubusercontent.com/{launchbadge/sqlx/main,kubo/rust-oracle/master,ClickHouse/clickhouse-rs/master,sfu-db/connector-x/main,pacman82/arrow-odbc/main,apache/arrow-adbc/main,prisma/tiberius/master}/README.md`。
- 标签：`git ls-remote --tags github.com/{googleapis/genai-toolbox,kubo/rust-oracle,bytebase/dbhub}`。
- 文档：`trino.io/docs/current/{connector.html,sql/prepare.html,client/jdbc.html,admin/resource-groups.html,installation.html}`；`arrow.apache.org/adbc/current/installation.html`。

## 7. 关键数字速览

| 数字 | 含义 |
|---|---|
| 0.9.0 | sqlx 当前版；**无 MSSQL**（`sqlx-mssql` 停在 0.0.0/2019） |
| 47 | Toolbox 数据源类型数（无达梦/金仓/虚谷） |
| 299 | Toolbox 已注册 tool 类型数（去重） |
| 18 / 16 | Toolbox 中「模板型 `*-sql`」18 个（bigquery、bigtable、clickhouse、cockroachdb、firebird、mindsdb、mssql、mysql、oceanbase、oracle、postgres、singlestore、snowflake、spanner、sqlite、tidb、trino、looker-query-sql）；「裸 SQL 型 `*-execute-sql`」16 个（含 arcadedb） |
| 1.6.0 | DBHub 当前版；`readonly`/`max_rows` 已改为**按工具**配置，`--readonly` CLI 失效 |
| 6 | DBHub 支持引擎数（PG/MySQL/MariaDB/MSSQL/Oracle/SQLite） |
| 37 | Trino 连接器数；**支持 `PREPARE` + `?` 参数** |
| 30.0.0 / 27.0.0 | odbc-api / arrow-odbc 当前版，均 **阻塞式**、MIT |
| 0.25.0 | ADBC Rust 绑定当前版，Apache-2.0，需外部 C 驱动 |
