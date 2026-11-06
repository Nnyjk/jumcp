# 调研：DBX 的 Java Agent 如何带动多数据源接入

对象：`t8y2/dbx`（Rust + Tauri 数据库客户端，Apache-2.0）内置的 **Java Agent** 体系，以及它与 DBX MCP Server 的数据源组织方式。
本机对应物：`C:/Users/Nn/.dbx/`（agent 运行时与 MCP 二进制）、`C:/Users/Nn/AppData/Roaming/com.dbx.app/plugins/jdbc/`（通用 JDBC 插件）。

## 0. 先纠正一个前提

**DBX 的 "Java Agent" 不是 JVM 插桩 agent（不是 `-javaagent` / attach / JVMTI），而是 DBX 自己拉起的独立子进程。**
它不进入业务应用进程，也就不会"在运行中的 Java 应用里发现 DataSource Bean"；每个数据源由 DBX 配置（MCP 侧叫 *connection*），Agent 用 DBX 下发的连接参数自己再建一条 JDBC 连接。

硬证据：

- Agent JAR 的清单只有 `Main-Class`，**没有 `Premain-Class` / `Agent-Class` / `Can-Retransform-Classes`**：
  `C:/Users/Nn/.dbx/agents/drivers/dameng/agent.jar!/META-INF/MANIFEST.MF:1-4`（`Main-Class: com.dbx.agent.dameng.DamengAgent`）。
- Agent 源码树里没有 `premain` / `agentmain` / `Instrumentation` / `retransform` 任何一处（对 `agents/common/src/main/java/com/dbx/agent/**` 全量反编译后检索无命中；构建脚本 `agents/build.gradle:49-52` 只设 `archiveBaseName`，无 manifest 注入）。
- MCP 二进制 `C:/Users/Nn/.dbx/bin/dbx-mcp.exe`（Rust，36 MB）内检索不到 `javaagent` / `agentmain` / `Instrumentation`（唯一一次 `premain` 命中落在压缩 blob 内，非代码字符串）。
- 官方定位：*"Each agent runs as a standalone process and communicates with DBX via stdin/stdout JSON-RPC 2.0."* —— https://github.com/t8y2/dbx/blob/main/agents/README.md#L7
- *"Every agent is a standalone JVM process that … Talks to DBX over stdin/stdout JSON-RPC 2.0."* —— https://github.com/t8y2/dbx/blob/main/agents/docs/agent-authoring.md#L7-L15

因此本笔记回答的是"DBX 的 Java Agent 如何把**多个数据源**接进 MCP"，而不是"如何在业务 JVM 里发现 DataSource"。后者在 DBX 里不存在。

## 1. 机制总览

```mermaid
sequenceDiagram
    autonumber
    participant C as MCP 客户端<br/>(stdio / Streamable HTTP)
    participant M as DBX MCP Server<br/>(Rust 单进程)
    participant A as Agent 进程<br/>(java -jar dbx-agent-{type}.jar)
    participant D as 数据库

    Note over M,A: 首次用到该驱动时拉起子进程；<br/>stdin/stdout 双向管道 + JSON-RPC 2.0（按行）
    M->>A: 启动子进程，读取首行 {"ready":true}
    M->>A: handshake → protocolVersion / agentProtocolVersion / capabilities
    Note over M,A: 同一 (agent_key, 启动参数, env) 的多个连接<br/>共享这一个 Agent 进程
    M->>A: open_session {agentSessionId=uuid4, ...连接参数}
    A->>A: DriverManager.getConnection(jdbcUrl, props)<br/>→ HikariCP 池（按连接指纹分池）
    A->>D: 物理连接建立

    C->>M: dbx_execute_query {dbx_connection:{connection_id|connection_name}, sql}
    M->>M: 连接范围 / 工具白名单 / 权限级别 / 只读 / 生产保护 复核
    M->>A: execute_query {agentSessionId, sql, schema, maxRows, timeoutSecs}
    A->>D: Statement.execute（池内连接，非业务连接池）
    D-->>A: ResultSet
    A-->>M: JSON {columns, rows, truncated, timing}
    M-->>C: 文本 + structuredContent
```

DBX 自己的架构图（同样的意思）：

```
DBX Main Process (Rust/Tauri)
    │ stdin/stdout (JSON-RPC 2.0)
    ▼
agent / java -jar dbx-agent-{type}.jar
    │
    ▼
Native driver / JDBC → Database
```
—— https://github.com/t8y2/dbx/blob/main/agents/README.md#L150-L161

## 2. 七个问题逐条回答

### Q1 接入方式：不是 `-javaagent`，是"父进程拉起子进程 + stdio JSON-RPC"

- 方式：**独立子进程**。Rust 侧用 `stdin/stdout/stderr` 全管道启动：`command.args(...).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())` —— https://github.com/t8y2/dbx/blob/main/crates/dbx-driver-agent/src/agent_driver.rs#L4134-L4135 。Agent 侧就是 `System.in` 逐行读、`System.out` 逐行写 —— https://github.com/t8y2/dbx/blob/main/agents/common/src/main/java/com/dbx/agent/MultiSessionJsonRpcServer.java#L148-L160 。
- 协议自述：*"Agents can be Java JARs, native executables, or script runtimes as long as they speak the DBX stdin/stdout JSON-RPC protocol."* —— https://github.com/t8y2/dbx/blob/main/crates/dbx-driver-agent/src/agent_driver.rs#L2161-L2163
- **字节码增强：无**。目标不是 `javax.sql.DataSource`/`DriverManager`/`HikariCP`/`Druid`/`MyBatis`/`JdbcTemplate` 的插桩；Agent 是把这些库**当普通依赖使用**：`DriverManager.getConnection(...)` —— https://github.com/t8y2/dbx/blob/main/agents/common/src/main/java/com/dbx/agent/AbstractJdbcAgent.java#L654-L655 ，HikariCP 直接 shade 进 Agent JAR —— https://github.com/t8y2/dbx/blob/main/agents/README.md#L90 。
- 运行时要求：Java Agent 需要 **JRE 21 + 对应 Agent 包 + JDBC 驱动**三者齐备，DBX 负责下载/管理 JRE —— https://github.com/t8y2/dbx/blob/main/docs/content/docs/mcp.cn.mdx#L358-L360 、 https://github.com/t8y2/dbx/blob/main/agents/README.md#L69-L71 。本机状态：`C:/Users/Nn/.dbx/agents/state.json:1-30`（`installed_drivers.rabbitmq/vastbase/dameng`，`java_runtime.custom_java_path = C:\Program Files\Java\jdk21.0.8_9\bin\java.exe`）。
- 打包形态：shadow JAR `dbx-agent-<type>.jar`，清单写 `Agent-Label` + `Main-Class` —— https://github.com/t8y2/dbx/blob/main/agents/docs/agent-authoring.md#L148-L160 。本机实例：`C:/Users/Nn/.dbx/agents/drivers/dameng/agent.jar`、`C:/Users/Nn/.dbx/agents/download-cache/driver-dameng-base-0.1.102-.agent.jar.tar.zst`。

### Q2 多数据源的发现与命名：**不发现，靠配置 + 选择器**；标识 = connection_id（UUID）/ name，Agent 侧 = agentSessionId（UUID）

- DBX 的"多数据源"模型是 **connection（连接）**，存于 DBX 存储（`dbx.db`），字段含 `id`/`name`/`db_type`/`driver_profile`/`host`/`port`/`username`/`password`/`database`/`default_schema`/`read_only`/`is_production` —— https://github.com/t8y2/dbx/blob/main/crates/dbx-types/src/models/connection.rs#L155-L177 、 #L321-L325 。
- **枚举方式**：MCP 工具 `dbx_list_connections` 返回当前会话可见的连接；资源 `dbx://connections` 同源 —— https://github.com/t8y2/dbx/blob/main/crates/dbx-mcp/src/server.rs#L54-L57 、 https://github.com/t8y2/dbx/blob/main/docs/content/docs/mcp.cn.mdx#L125-L129 。
- **调用时如何选源**：所有连接型工具都带 `dbx_connection` 选择器（`connection_id` 或 `connection_name`），二者至少给一个，否则 `CONNECTION_NOT_FOUND` —— https://github.com/t8y2/dbx/blob/main/crates/dbx-mcp/src/server.rs#L4224-L4241 、 #L5324-L5341 。**工具名不随数据源变化**，数据源是参数而不是工具前缀。
- **命名规则**：`name` 全局唯一（大小写不敏感）—— https://github.com/t8y2/dbx/blob/main/crates/dbx-mcp/src/server.rs#L4152-L4154 ；`id` 是内部 UUID 串（连接增删改用 `Uuid::new_v4()`）—— https://github.com/t8y2/dbx/blob/main/crates/dbx-mcp/src/server.rs#L4252 。
- **Agent 侧命名**：每个连接在 Agent 进程里对应一个 **逻辑会话**，键 `agentSessionId`，由宿主生成 `uuid::Uuid::new_v4().simple()` —— https://github.com/t8y2/dbx/blob/main/crates/dbx-core/src/connection/mod.rs#L3951 ；Agent 端 `agentSessionId` 缺失即报错 —— https://github.com/t8y2/dbx/blob/main/agents/common/src/main/java/com/dbx/agent/MultiSessionJsonRpcServer.java#L442-L447 。
- **主库/从库/多租户**：DBX 没有"主从"概念，靠**作用域**表达多租户/多库：连接下再挂数据库范围（库 → 连接 → 分组 → 全局 逐级继承，越具体越优先，但连接只读/生产保护/账号权限是硬上限）—— https://github.com/t8y2/dbx/blob/main/docs/content/docs/mcp.cn.mdx#L450-L452 、 #L466 。默认库/schema 取连接配置的 `database` / `default_schema` —— https://github.com/t8y2/dbx/blob/main/crates/dbx-types/src/models/connection.rs#L174-L176 。
- 宿主侧还会用 `DBX_MCP_SCOPE_CONNECTION_ID(S)` / `_CONNECTION_NAME` / `_DATABASE` / `_SCHEMA` 限制会话可见范围 —— https://github.com/t8y2/dbx/blob/main/crates/dbx-mcp/src/server.rs#L1018-L1046 、 https://github.com/t8y2/dbx/blob/main/docs/content/docs/mcp.cn.mdx#L490-L494 。

### Q3 连接与元数据：Agent 自建 Hikari 池；元数据从 `DatabaseMetaData`/方言 SQL 读，缓存在宿主存储

- **连接怎么来**：宿主把连接参数（host/port/database/username/password/url_params/connection_string/jdbc_driver_class/jdbc_driver_paths/证书路径…）下发给 Agent，Agent 用 `DriverManager.getConnection(jdbcUrl, props)` 自己建连 —— https://github.com/t8y2/dbx/blob/main/agents/common/src/main/java/com/dbx/agent/ConnectParams.java （本地反编译同文件在 `C:/Users/Nn/.dbx/agents/drivers/dameng/agent.jar` 的 `com/dbx/agent/ConnectParams.class`）、 https://github.com/t8y2/dbx/blob/main/agents/common/src/main/java/com/dbx/agent/AbstractJdbcAgent.java#L654-L655 。
- **分池指纹**：连接身份 = `agentClass + driverClass + jdbcUrl + host + port + database + username + password + urlParams + connectionString + portExplicit`，取 SHA-256 作池键 —— https://github.com/t8y2/dbx/blob/main/agents/common/src/main/java/com/dbx/agent/AbstractJdbcAgent.java#L913-L925 、 https://github.com/t8y2/dbx/blob/main/agents/common/src/main/java/com/dbx/agent/JdbcConnectionPoolRegistry.java#L286-L292 。
- **池参数（默认值）**：maxPoolSize=8、minIdle=0、connectionTimeout=30s、validationTimeout=5s、idleTimeout=120s、maxLifetime=1800s、retire=60s、**metadataReserve=2**、**globalMaximumPhysicalConnections=32**、maxQuarantinedOperations=32 —— https://github.com/t8y2/dbx/blob/main/agents/common/src/main/java/com/dbx/agent/JdbcConnectionPoolRegistry.java#L43-L52 、 #L728-L808 。全部可用 `dbx.agent.jdbc.pool.*` / `DBX_AGENT_JDBC_POOL_*` 环境变量覆盖 —— 同上 #L730-L806 。
- **不与业务连接池抢占**：**根本没有共享**——Agent 不进业务进程，池是它自己的。但 DBX 在自己内部做了"元数据 vs 业务"隔离：`PoolEntry` 用两个信号量，`workloadPermits = maxPoolSize - metadataReserve`、`leasePermits = maxPoolSize` —— https://github.com/t8y2/dbx/blob/main/agents/common/src/main/java/com/dbx/agent/JdbcConnectionPoolRegistry.java#L896-L897 ；角色枚举 `WORKLOAD / METADATA` —— https://github.com/t8y2/dbx/blob/main/agents/common/src/main/java/com/dbx/agent/JdbcSessionRole.java 。宿主侧也按 session role 分池键 —— https://github.com/t8y2/dbx/blob/main/crates/dbx-core/src/connection/mod.rs#L6197-L6201 。
- **元数据来源**：标准 JDBC 走 `DatabaseMetaData`（`ConfiguredJdbcAgent` / `StandardJdbcMetadata`），PG 系走方言 SQL（`PostgresLikeAgent`），二者共享"生命周期 + 执行 + 分页 + 事务 + DDL 回退"基座 —— https://github.com/t8y2/dbx/blob/main/agents/docs/agent-authoring.md#L115-L124 。Schema 切换靠方言化 `setSchemaSQL`，由 `JdbcSchemaSwitcher` 执行并在语句后复位 —— https://github.com/t8y2/dbx/blob/main/agents/common/src/main/java/com/dbx/agent/AbstractJdbcAgent.java#L175-L180 。
- **元数据缓存（在宿主，不在 Agent）**：键 `object-meta:v1:<connection_id>:<database>` / `object-ddl:v1:...`，落在 DBX 存储；命中 DDL 类语句时失效 —— https://github.com/t8y2/dbx/blob/main/crates/dbx-core/src/query/object_cache.rs#L9-L12 、 #L31-L38 、 #L41-L63 。

### Q4 agent ↔ MCP server 的通道：本地 stdio 管道 + JSON-RPC 2.0（按行），无 socket/HTTP

- **通道形态**：父子进程的 stdin/stdout 管道，一行一个 JSON 对象；stderr 单独收集作诊断尾部 —— https://github.com/t8y2/dbx/blob/main/crates/dbx-driver-agent/src/agent_driver.rs#L2069-L2093 、 #L4231-L4249 。
- **握手**：
  1. 启动：Agent 先打印 `{"ready":true}`，宿主读到才继续；读不到就报错并重试一次 —— https://github.com/t8y2/dbx/blob/main/agents/common/src/main/java/com/dbx/agent/MultiSessionJsonRpcServer.java#L148-L151 、 https://github.com/t8y2/dbx/blob/main/crates/dbx-driver-agent/src/agent_driver.rs#L2093-L2105 、 #L2053-L2062 。
  2. 协议握手：`handshake` 返回 `{protocolVersion, agentProtocolVersion, capabilities}`；宿主记日志 `[agent] handshake complete: protocol=…, agent_protocol=…, capabilities=…`，失败则退回 legacy 协议 —— https://github.com/t8y2/dbx/blob/main/crates/dbx-driver-agent/src/agent_driver.rs#L3700-L3721 、 https://github.com/t8y2/dbx/blob/main/agents/common/src/main/java/com/dbx/agent/AgentProtocol.java#L468-L491 。
  3. 能力协商：当前 v2 默认能力 `connect/test_connection/metadata/query/paged_query/transaction/ddl/multi_session`，另有 `structured_error_v1`、`blob_bind_statements_v1`、`native_job_*`、`exact_metadata_identity_v1` 等 —— 见本机 `C:/Users/Nn/.dbx/agents/drivers/dameng/agent.jar!/agent-protocol-v2.json`（该 JSON 直接打包在 JAR 根目录）。
- **鉴权**：**通道上没有任何 token**——它就是父子进程的私有管道，信任边界 = 进程边界。真正的鉴权在 MCP 对外一侧（见 Q6）。
- **心跳**：**没有独立心跳**。存活靠 RPC 失败即断（`Agent runtime terminated` / `connection lost` / `end of stream`）；Agent 侧只有 60s 一次的维护任务 —— https://github.com/t8y2/dbx/blob/main/agents/common/src/main/java/com/dbx/agent/MultiSessionJsonRpcServer.java#L34 、 https://github.com/t8y2/dbx/blob/main/crates/dbx-driver-agent/src/agent_driver.rs#L4378-L4381 。
- **断线重连**：Agent 侧不重连；宿主侧在 `open_session` 失败时丢弃共享 runtime，下次重新拉进程（`forget_unused_runtime_after_failed_open`）—— https://github.com/t8y2/dbx/blob/main/crates/dbx-driver-agent/src/agent_runtime.rs#L366-L391 。宿主进程重启后 Agent 进程随之重启（JVM 无状态）。
- **取消**：`cancel_session {agentSessionId}` —— https://github.com/t8y2/dbx/blob/main/crates/dbx-driver-agent/src/agent_driver.rs#L421-L429 。

### Q5 执行面：SQL 在 **Agent 自己的池连接** 上执行；只读在**宿主**判定；行数/超时双重限制

- **在哪执行**：Agent 进程内、自己的 HikariCP 池连接上（不是业务应用的连接池）。`executeQuery` → `JdbcExecutor.current().execute(conn, sql, schema, setSchemaSQL, resetSchemaSQL, maxRows, fetchSize, timeoutSecs, …)` —— https://github.com/t8y2/dbx/blob/main/agents/common/src/main/java/com/dbx/agent/AbstractJdbcAgent.java#L213-L218 。执行用 `Statement.execute`（不区分 SELECT/其他），并对 `BEGIN/COMMIT/ROLLBACK` 做专门处理 —— https://github.com/t8y2/dbx/blob/main/agents/docs/agent-authoring.md#L67-L80 。
- **只读**：**Agent 侧完全不识别只读**（对 `readOnly|readonly|read_only` 检索 `agents/common` 源码零命中）。只读由宿主 Rust 侧判定：`SqlRisk{ReadOnly, Write, Ddl, Transaction}` —— https://github.com/t8y2/dbx/blob/main/crates/dbx-sql-core/src/sql_risk.rs#L17-L28 ；权限级别 `None < Read < Write < Full` —— https://github.com/t8y2/dbx/blob/main/crates/dbx-core/src/ai/mcp_access.rs#L64-L71 ；拒绝码 `MCP_READ_ONLY` / `CONNECTION_READ_ONLY` / `PRODUCTION_WRITE_BLOCKED` / `TOOL_BLOCKED` / `SCOPE_OUT_OF_POLICY` —— https://github.com/t8y2/dbx/blob/main/crates/dbx-mcp/src/server.rs#L1131-L1172 。
- **超时**：Agent 用 `statement.setQueryTimeout(timeoutSecs)` —— https://github.com/t8y2/dbx/blob/main/agents/common/src/main/java/com/dbx/agent/JdbcExecutor.java#L71 ；宿主还有 RPC 级超时（`Agent RPC call timed out at …`）与连接级超时。
- **行数限制**：Agent 侧 `stmt.setMaxRows(...)` + 读满即置 `truncated=true` —— https://github.com/t8y2/dbx/blob/main/agents/common/src/main/java/com/dbx/agent/JdbcExecutor.java#L272 、 #L362-L365 ，Agent 默认 `DEFAULT_MAX_ROWS=10000` —— 同上 #L27 ；宿主/MCP 侧再夹一次：`max_rows` 取值 1–1000、默认 100、**越界夹取不报错** —— https://github.com/t8y2/dbx/blob/main/docs/content/docs/mcp.cn.mdx#L191 。分页另有 `execute_query_page` / `fetch_query_page` / `close_query_session` 三段式游标 —— 见 `C:/Users/Nn/.dbx/agents/drivers/dameng/agent.jar!/agent-protocol-v2.json` 的 `commonMethods`。
- **写语句是否被拦截**：**在宿主拦截，不在 Agent 拦截**。高危 SQL / 破坏性操作走"人工确认"流程：确认时展示工具、目标与操作内容，批准只对本次调用生效，批准后还要重新校验权限 —— https://github.com/t8y2/dbx/blob/main/docs/content/docs/mcp.cn.mdx#L454-L458 。批处理里只要含一条写/DDL，整次调用在只读策略下失败 —— 同上 #L251 。
- **结果序列化**：`QueryResult{columns, columnTypes, rows, executionTimeMs, truncated}` 的 JSON；LOB/大字段可延迟读取并分块取（`read_large_value_chunk` / `release_large_value`）—— https://github.com/t8y2/dbx/blob/main/agents/common/src/main/java/com/dbx/agent/JdbcExecutor.java#L331-L370 、 `C:/Users/Nn/.dbx/agents/drivers/dameng/agent.jar!/agent-protocol-v2.json`（`blob_bind_statements_v1` / `read_large_value_chunk`）。

### Q6 安全边界：Agent 通道无鉴权（进程边界即信任边界），鉴权与只读全部在宿主侧

- **谁能连 Agent**：只有父进程（管道私有）。Agent 侧无 token、无来源校验。
- **谁能连 MCP Server**：stdio 形态靠进程归属；HTTP 形态用 **Bearer Token**（常量时间比较）+ Host/Origin allowlist —— https://github.com/t8y2/dbx/blob/main/crates/dbx-mcp/src/http_auth.rs#L14-L25 、 #L105-L118 ；Desktop 默认只监听 `127.0.0.1:5225/mcp` 并生成 Token —— https://github.com/t8y2/dbx/blob/main/docs/content/docs/mcp.cn.mdx#L305-L307 。Web 模式用 `DBX_WEB_MCP_TOKEN` / `DBX_WEB_MCP_TOKEN_FILE` + `DBX_WEB_MCP_ALLOWED_HOSTS/ORIGINS` —— 同上 #L370-L386 。
- **默认是否只读**：默认权限级别是 **Read**（`default_access: McpAccessLevel::Read`）—— https://github.com/t8y2/dbx/blob/main/crates/dbx-core/src/ai/mcp_access.rs#L306 ；并且可以用 `DBX_MCP_ALLOW_WRITES=0` 在本次运行强制只读（该变量**只能收紧不能放宽**）—— https://github.com/t8y2/dbx/blob/main/docs/content/docs/mcp.cn.mdx#L471-L471 。注意这条是 DBX 侧"收紧"开关，DBX 明确**不允许**用 `DBX_MCP_ALLOW_WRITES=1` 放宽中央策略。
- **白名单/黑名单**：工具有 allowlist（服务端每次调用强制，不只是客户端展示偏好）—— https://github.com/t8y2/dbx/blob/main/docs/content/docs/mcp.cn.mdx#L406-L406 ；连接有 allowlist（连接/分组/库三级可见性）—— 同上 #L440-L442 ；SQL 有风险分级（read-only / write / DDL / transaction）—— https://github.com/t8y2/dbx/blob/main/crates/dbx-sql-core/src/sql_risk.rs#L17-L28 ；Redis 命令另有安全分类 `REDIS_COMMAND_BLOCKED` —— https://github.com/t8y2/dbx/blob/main/crates/dbx-mcp/src/server.rs#L3762-L3780 。
- **敏感列脱敏**：**未找到**。全库与 Agent 源码中未见列级脱敏/掩码机制；只有"连接备注不得存密码"的提示 —— https://github.com/t8y2/dbx/blob/main/docs/content/docs/mcp.cn.mdx#L187-L187 。
- **审计**：有 **MCP 调用记录**，字段含时间、接入方式（stdio/HTTP）、客户端名与版本、工具、目标连接/库、判定级别、是否经确认、耗时、请求/响应摘要、成功/失败原因 —— https://github.com/t8y2/dbx/blob/main/docs/content/docs/mcp.cn.mdx#L462-L464 ；写入路径另有调用历史落库（`connection_id / sql / mcp_tool_name / executed_at`）—— https://github.com/t8y2/dbx/blob/main/crates/dbx-mcp/src/server.rs#L1601-L1650 。
- **凭据不外泄**：连接凭据由宿主持有，插件工具场景下"凭据由宿主从保存的连接生成 lifecycle 注入，永不经过模型或客户端" —— https://github.com/t8y2/dbx/blob/main/docs/content/docs/mcp.cn.mdx#L200-L200 。

### Q7 支持的数据源类型与限制：Agent 一族 + 原生驱动 + 通用 JDBC 插件；动态新增数据源**不需要重启**

- **Agent 家族**（本机只装了 3 个）：Java（JVM）Agent 覆盖 access/dameng/hbase 系/DB2/Hive/Trino/Snowflake/SAP HANA/Teradata/Vertica/Firebird/Exasol/Kingbase…；原生（Go/Rust）Agent 覆盖 cassandra/duckdb/hive-go/iotdb/oracle-go/kingbase-go/vastbase-go/tdengine/xugu/rabbitmq/rocketmq/zookeeper/etcd 等，**不需要 JRE** —— https://github.com/t8y2/dbx/blob/main/agents/README.md#L9-L67 、 #L95-L97 。本机：`C:/Users/Nn/.dbx/agents/state.json`（rabbitmq / vastbase / dameng）。
- **DBX 的明确建议**：能用成熟原生驱动就别用 Java/JDBC Agent（省 JVM 常驻内存与启动时间）—— https://github.com/t8y2/dbx/blob/main/agents/README.md#L95-L95 。
- **通用 JDBC 兜底**：独立 sidecar 插件 `plugins/jdbc`，manifest 声明 `protocol_version: 1`、`kind: external`、`database_type: jdbc` —— 本机 `C:/Users/Nn/AppData/Roaming/com.dbx.app/plugins/jdbc/manifest.json:1-16`（上游同文件 https://github.com/t8y2/dbx/blob/main/plugins/jdbc/manifest.json）。它用 Maven resolver 拉驱动（`bin/dbx-maven-resolver`），并支持 JDBCX（`io.github.jdbcx.WrappedDriver`）作为一等 profile；JDBCX 默认只放开 `help/var/version` 扩展，`shell/script/web/mcp` 等高权限扩展必须逐连接显式开启 —— https://github.com/t8y2/dbx/blob/main/plugins/jdbc/README.md#L38-L58 。同一插件进程也按 `(runtime_key, env.vars)` 复用，多连接各持一个逻辑会话（UUID v4）—— https://github.com/t8y2/dbx/blob/main/crates/dbx-plugin-runtime/src/plugins/jdbc_sessions.rs#L32-L80 。
- **连接池实现限制**：Agent 内置 HikariCP 7.1.0（shade 进 JAR），可用 `DBX_AGENT_JDBC_POOL_ENABLED=false` 回退到"一逻辑会话一物理连接"的旧行为 —— https://github.com/t8y2/dbx/blob/main/agents/README.md#L90-L91 、 https://github.com/t8y2/dbx/blob/main/agents/build.gradle#L62-L66 。**对业务侧连接池无要求**（因为不共享）。
- **动态新增数据源要不要重启**：**不要**。MCP 工具 `dbx_add_connection` / `dbx_duplicate_connection` / `dbx_remove_connection` 直接改 DBX 存储，下一次调用按需拉起对应 Agent 进程 —— https://github.com/t8y2/dbx/blob/main/crates/dbx-mcp/src/server.rs#L4148-L4182 、 #L4248-L4256 、 #L4309-L4362 。
- **进程复用粒度**：同一 driver 的多个连接**共享一个 Agent 进程**，进程指纹 = `agent_key + program + args + working_dir + env`（env 入指纹是为了隔离 Oracle 的 `NLS_LANG` 等进程级设置）—— https://github.com/t8y2/dbx/blob/main/crates/dbx-driver-agent/src/agent_runtime.rs#L413-L425 。同一进程内并发会话上限 **256**、请求线程池 **64** —— https://github.com/t8y2/dbx/blob/main/agents/common/src/main/java/com/dbx/agent/MultiSessionJsonRpcServer.java#L31-L33 、 #L247-L252 。若 Agent 不支持 `multi_session` 协议 v2，宿主回退为"每连接一个专属进程" —— https://github.com/t8y2/dbx/blob/main/crates/dbx-core/src/connection/mod.rs#L4000-L4004 、 https://github.com/t8y2/dbx/blob/main/crates/dbx-driver-agent/src/agent_driver.rs#L151-L161 。

## 3. 对我们可借鉴

我们的场景：Rust MCP server + 配置驱动 `[[datasources]]`（多数据源、每源独立 DSN 环境变量、只读/写账号分离、能力注册表）。

**可直接借鉴（设计层面，与 DBX 的 Java Agent 无关）**

1. **"工具名固定、数据源是参数"的选择器模型**。DBX 一律用 `dbx_connection {connection_id|connection_name}` 选源（https://github.com/t8y2/dbx/blob/main/crates/dbx-mcp/src/server.rs#L4224-L4241 ），工具清单不随数据源数量膨胀。直接对应我们的"能力注册表 + 数据源参数"，建议保持 `datasource` 参数而不是按数据源生成工具名。
2. **数据源标识分层**：`connection_id`（稳定 UUID，机器用） + `connection_name`（人类可读、全局唯一、大小写不敏感）双标识；Agent 侧再叠一层 `agentSessionId`（https://github.com/t8y2/dbx/blob/main/crates/dbx-core/src/connection/mod.rs#L3951 ）。我们的 `[[datasources]]` 应同时有稳定 id 与 name，并保证 name 唯一。
3. **资源式目录**：`dbx://connections`、`dbx://connections/{id}/databases`、`.../tables`、`.../table-schema`（https://github.com/t8y2/dbx/blob/main/crates/dbx-mcp/src/server.rs#L54-L57 ）。这套 URI 模板可以近乎照搬到我们的 MCP Resources，方便客户端做渐进发现而不占 tools/list 预算。
4. **元数据缓存键 = (数据源 id, 库)**，并用 DDL 分类做失效信号（https://github.com/t8y2/dbx/blob/main/crates/dbx-core/src/query/object_cache.rs#L31-L38 、 #L41-L63 ）。我们的每个源独立 DSN，缓存键必须带数据源维度，不能只按库名。
5. **元数据/业务资源隔离**：DBX 用 `metadataReserve=2` 给元数据查询预留物理连接额度（https://github.com/t8y2/dbx/blob/main/agents/common/src/main/java/com/dbx/agent/JdbcConnectionPoolRegistry.java#L896-L897 ）。我们的每个源若共用一个池，同样该给"看表结构"这类元数据操作留独立额度，别让长查询把元数据饿死。
6. **握手 + 能力协商**：即使我们不引入子进程，也值得借鉴"先握手声明协议版本与能力，再进入业务调用"的做法（https://github.com/t8y2/dbx/blob/main/agents/common/src/main/java/com/dbx/agent/AgentProtocol.java#L468-L491 ），以及"能力集可协商、按能力开关工具"。
7. **只读/写账号分离的落点**：DBX 把只读判定完全放在**宿主**，数据库账号权限是硬上限（https://github.com/t8y2/dbx/blob/main/docs/content/docs/mcp.cn.mdx#L402-L404 ）。我们已有"只读/写账号分离"，DBX 的做法是让**账号权限成为不可逾越的上限**，配置层只能收紧不能放宽——这是可以直接抄的策略语义。
8. **运行期只读开关**：`DBX_MCP_ALLOW_WRITES=0` 一次性把本次运行钉成只读，且明确"只能收紧不能放宽"（https://github.com/t8y2/dbx/blob/main/docs/content/docs/mcp.cn.mdx#L471 ）。对应我们的"部署期强制只读"开关语义。
9. **审计记录字段**：时间/接入方式/客户端名与版本/工具/目标数据源/判定级别/是否经确认/耗时/请求响应摘要（https://github.com/t8y2/dbx/blob/main/docs/content/docs/mcp.cn.mdx#L462-L464 ）。我们做 MCP 调用审计可以直接用这份字段表。

**不适用 / 不可照搬**

1. **"Java Agent" 这条路本身不适用**。DBX 的 Agent 是**为了兼容没有 Rust 客户端的厂商 JDBC 驱动**而存在的旁路进程，它付出的代价是：每源一个 JVM 常驻（DBX 自己都说"能不用就别用"，https://github.com/t8y2/dbx/blob/main/agents/README.md#L95 ）、需要额外装 JRE + 驱动 + Agent 三件套（https://github.com/t8y2/dbx/blob/main/docs/content/docs/mcp.cn.mdx#L358-L360 ）、驱动/Agent/JRE 三方版本耦合、Agent 崩溃或协议不兼容要整条链路降级（"does not support multi_session protocol v2" 回退到专属进程，https://github.com/t8y2/dbx/blob/main/crates/dbx-core/src/connection/mod.rs#L4000-L4004 ）。我们是 Rust server + 配置驱动 DSN，直接用 Rust 原生驱动 + 每源 DSN 环境变量，没有理由引入 JVM 旁路。
2. **字节码注入换"零配置"这条路我们根本不需要，也不能走**。任务描述里的"在运行中的 Java 应用里发现多个 DataSource"在 DBX 里不存在；真要实现就得接受：进程内信任边界（Agent 拿得到业务应用的一切，包括业务凭证与内存）、与业务 JVM 的升级强耦合（字节码增强随 JDK/框架版本碎）、崩溃直接影响业务应用、无法做独立鉴权与审计归属。我们的定位是"给 Agent 的数据面"，配置驱动是显式契约，比"猜"更可控。
3. **不要照搬 DBX 的"连接树 + 分组"权限模型**。DBX 是桌面客户端，权限树面向人（组/连接/库三级 + UI）；我们是服务端、面向 Agent 与租户，权限模型应是"租户 cap ≤ 角色默认 ≤ 请求声明"（已在 `notes/mcp_plan/research_dbx.md` 记录过差异）。DBX 的**继承顺序思想**可借鉴，**存储形态**不必照搬。
4. **不要照搬"状态化会话 + 显式事务 + USE/SET"**。DBX 允许会话固定连接、开事务、改 schema；我们走模板 + 只读 + 版本钉住，开这些等于开放 `USE`/`SET` 语义（该取舍已在 `notes/mcp_plan/research_dbx.md`「明确不采纳」记录）。
5. **不要照搬 DBX 的 `max_rows` 夹紧策略到 token 预算**。行数上限夹紧是合理的（少给几行并回显 effective）；但 token 预算必须显式失败或显式摘要，不能静默夹紧（DBX 没有 token 预算概念，这一层差异见 `notes/mcp_plan/research_dbx.md`）。

## 4. 未查清

以下问题我**没有找到直接证据**，不做推测填空：

1. **是否存在任何"attach 到运行中 JVM"的路径**（含商业/隐藏开关）。已查：本机 `C:/Users/Nn/.dbx/` 全量（`bin/`、`agents/`、`plugins/`）、`dbx-mcp.exe` 全字符串检索（`javaagent`/`agentmain`/`Instrumentation`/`jdwp`/`jattach` 均无命中）、反编译 `agents/common` 全部 164 个 `com/dbx/agent/**` 类（`premain`/`agentmain`/`Instrumentation`/`retransform` 零命中）、Agent JAR 的 `MANIFEST.MF`、上游 `agents/` 目录树（jsdelivr 列出的 1940 个文件中无任何 attach/premain 相关文件）。**未查**：GitHub 全库代码搜索（匿名 API 限额耗尽）、DBX 桌面端 asar 资源（本机未定位到桌面端安装目录，只有 `AppData/Local/com.dbx.app` 的 WebView2 缓存）。
2. **`agent_key`（db_type → agent 包）的完整映射表**。已知入口 `database_capabilities::agent_key(db_type, driver_profile)`（https://github.com/t8y2/dbx/blob/main/crates/dbx-driver-agent/src/agent_runtime.rs#L77-L79 ），但未逐项核对 `DatabaseType` 枚举与 Agent 包的对应表（`crates/dbx-core/src/database_capabilities.rs` 未下载）。
3. **Agent 进程池的完整生命周期**（何时因空闲回收、LRU 上限、跨 DBX 桌面端与 MCP 进程是否共享同一 runtime）。只看到 `stop_daemons` / `stop_daemon_by_key` / `restart_daemon_by_key`（https://github.com/t8y2/dbx/blob/main/crates/dbx-driver-agent/src/agent_runtime.rs#L85-L114 ）和"会话数归零则移除 runtime"（#L366-L391 ），未查到全局 LRU 阈值。
4. **元数据在 Agent 侧是否有二级缓存**。Agent 内有 `PostgresLikeAgent$TableAttributeCache`（本机 JAR 类名），但未读其源码确认缓存语义与失效条件；已确认的缓存是宿主侧 `object-meta:v1`。
5. **DBX 的"行数/单元"夹紧是否在 Agent 侧二次生效**：宿主 `max_rows` 1–1000 与 Agent 默认 `DEFAULT_MAX_ROWS=10000` 并存，两者谁最终生效（宿主传参覆盖，还是 Agent 上限更大导致宿主截断）未追到最终调用点。
6. **本地 jar 与上游 main 的版本差**：本机 dameng agent 为 `0.1.102`，上游 `main` 为最新源码；我引用的上游行号与本机 jar 行为一致的位置已逐条对照（如 `MultiSessionJsonRpcServer` 握手/会话逻辑），但**未做逐字节比对**。
7. **DBX 对"多租户库"（同一实例多 schema/多 catalog）的建模细节**：只看到库范围/子作用域的权限表达，未查到是否有"一连接多库"的显式模型（如 `AttachedDatabaseConfig` 只在 SQLite 语境下出现，与本题无关）。

## 5. 发行与安装形态（npm / Maven 侧）

- **MCP Server 走 npm**：包名 `@dbx-app/mcp-server`，版本 `0.4.139`，按平台分发原生二进制（`@dbx-app/mcp-win32-x64` 等）—— https://github.com/t8y2/dbx/blob/main/packages/mcp-server/package.json 。本机对应物是原生二进制而非 npm 包：`C:/Users/Nn/.dbx/bin/dbx-mcp.exe`（36 MB，Rust），版本文件 `C:/Users/Nn/.dbx/bin/.dbx-mcp-version` 内容 `0.4.136`。
- **JDBC 驱动走 Maven**：通用 JDBC 插件自带 `bin/dbx-maven-resolver`（以 `app.dbx.jdbc.maven.DbxMavenResolver` 为 main class 运行），从 Maven 解析驱动坐标；插件本体是 Gradle shadow JAR —— 本机 `C:/Users/Nn/AppData/Roaming/com.dbx.app/plugins/jdbc/bin/dbx-maven-resolver`（脚本内容可直接读到 `-cp "$DIR/../lib/dbx-jdbc-plugin.jar" app.dbx.jdbc.maven.DbxMavenResolver "$@"`）、上游 https://github.com/t8y2/dbx/blob/main/plugins/jdbc/README.md#L21-L37 （"DBX does not bundle Java or JDBC drivers"）。
- **Agent 包走 GitHub Release**：注册表 `https://github.com/t8y2/dbx/releases/download/agents-latest/agent-registry.json`，每个 Agent 一个 `.jar.tar.zst` —— 本机 `C:/Users/Nn/.dbx/agents/download-cache/driver-dameng-base-0.1.102-.agent.jar.tar.zst` 即该格式。

## 6. 取证方法（环境坑，供后续复用）

- `raw.githubusercontent.com` 与 `github.com/blob/...` 在本机被拒连（`ECONNRESET` / `ECONNREFUSED`），`cdn.jsdelivr.net` 同样被拒；**可用的镜像是 `fastly.jsdelivr.net/gh/<owner>/<repo>@main/<path>`**。
- `api.github.com` 匿名访问有 60 次/小时限额（本机 IP 已耗尽）；用 `Accept: application/vnd.github.raw` 可直接拿到文件原文，避开 base64。
- 仓库文件清单可用 `https://data.jsdelivr.com/v1/packages/gh/t8y2/dbx@main?structure=flat` 获取（本次返回 1940 条，是仓库全量的子集，**`agents/common/.../MultiSessionJsonRpcServer.java` 等文件不在该清单里但实际存在**，需直接按路径抓取）。
- 本机反编译用 IntelliJ 自带 Fernflower：`java -cp "<idea>/plugins/java-decompiler/lib/java-decompiler.jar" org.jetbrains.java.decompiler.main.decompiler.ConsoleDecompiler -dgs=true <in.jar> <outdir>`（输出是 `<outdir>/<in.jar>`，需再解一层）。
