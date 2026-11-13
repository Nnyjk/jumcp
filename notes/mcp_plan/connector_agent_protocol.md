# Connector agent 协议与边界（ClickHouse）

结论：数据库执行面从「host 进程内直连驱动」改为「host 拉起独立子进程，按行 JSON-RPC 通信」。
agent 不是 MCP server，只做连接、绑定、执行、取数；治理与整形全部留在 host。

## 1. 拓扑

```text
MCP client
  → jumcp-server（鉴权 / 组 / 预算 / L4 / database 收窄 / 整形 / sql_hash / 审计）
      → stdin/stdout 按行 JSON-RPC
          → jumcp-connector-clickhouse（clickhouse crate 0.15，HTTP）
              → ClickHouse
```

- 子进程 stdin/stdout 只承载协议帧；host 即使自身使用 stdio MCP 传输也不受影响（独立管道）。
- 一个 host 进程只维护一个 agent 进程，请求串行化；断链/超时后杀死，下一次调用重新拉起。
- agent 环境里剔除 `JUMCP_ADMIN_TOKENS` / `JUMCP_AGENT_TOKENS`；DSN 只通过 `JUMCP_CLICKHOUSE_DSN` 注入。

## 2. 协议

传输：每行一个 JSON 对象，JSON-RPC 2.0；请求带 `id`，响应必须回同一个 `id`。
协议版本：`1`，`initialize` 必须回同一版本，否则 host 拒绝启动。

### initialize

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocol_version":1}}
```

```json
{"jsonrpc":"2.0","id":1,"result":{
  "protocol_version":1,
  "agent_version":"0.1.0",
  "server_version":"25.5.1.2782",
  "database":"default"
}}
```

握手会真连一次 ClickHouse（`SELECT version(), currentDatabase()`）；失败即拒绝。

### query

```json
{"jsonrpc":"2.0","id":2,"method":"query","params":{
  "sql":"SELECT number FROM system.numbers LIMIT 10",
  "params":[],
  "max_rows":3,
  "database":"default",
  "timeout_ms":15000
}}
```

```json
{"jsonrpc":"2.0","id":2,"result":{
  "columns":[{"name":"number","type":"UInt64"}],
  "rows":[["0"],["1"],["2"]],
  "row_count":3,
  "truncated":true,
  "elapsed_ms":4
}}
```

- `rows` 是 JSONCompact 的二维数组；`columns` 保留 ClickHouse 类型名。
- 64 位整数在 JSONCompact 里是字符串（默认 `output_format_json_quote_64bit_integers=1`），
  `Decimal` 是 JSON number；DateTime 为服务器时区的字符串。要保留类型语义以 `columns` 为准。
- agent 先按 `max_rows` 截断再返回；`truncated` 表示原始结果超过上限。host 仍会再按
  `cell_char_limit` / `cell_char_offset` 整形并回显 `effective_max_rows` / `next_offsets`。

### shutdown

```json
{"jsonrpc":"2.0","id":3,"method":"shutdown"}
```

回 `{"ok":true}` 后进程退出。

## 3. 错误码

| JSON-RPC code | `data.code` | 语义 |
|---|---|---|
| -32700 | `MCP_VALIDATION_ERROR` | 帧不是合法 JSON |
| -32601 | — | 未知方法 |
| -32602 | `MCP_VALIDATION_ERROR` / `MCP_PARAM_STYLE_UNSUPPORTED` | 入参不合法、非标量参数 |
| -32001 | `MCP_STORE_ERROR` | ClickHouse 明确返回的失败（语法、权限、未知表等） |
| -32002 | `MCP_EXEC_TIMEOUT` | 超时；`data.outcome=unknown`，调用方不得重试 |
| -32003 | `MCP_CONFIG_ERROR` | DSN 解析/进程配置错误 |

host 的映射：`outcome=unknown` 原样保留到 `McpError.data.outcome`；`mcp.rs` 据此把审计标成
`unknown`，并且不重试、不重放。

## 4. 只读与防爆

agent 对每条查询通过请求 URL 参数下发：

- `readonly=1` — ClickHouse 查询级只读；DDL/DML 当场 `READONLY` 拒绝。
- `max_execution_time` — 由 `timeout_ms` 换算，块级软限制。
- `max_result_rows = max_rows + 1`、`result_overflow_mode=break`、`max_block_size = max_rows + 1`、
  `max_threads=1` — 把 `break` 的溢出限制在约一个块内；精确行数仍由 host 应用层截断。
- 查询级 `SETTINGS` 子句被 L4 拒绝（`MCP_SQL_REJECTED` / `reason=settings_clause`）：否则
  `SETTINGS readonly=0` 或 `max_result_rows=0` 会在同一请求里覆盖上述 URL 参数。
- 客户端 `tokio::time::timeout(timeout_ms + 1s)` 是权威超时边界（`max_execution_time` 在函数内阻塞时不会及时中断）。

L1 只读账号仍是唯一强保证；以上只是纵深防御。部署时必须验证该账号没有写权限。

L4 另在 `jumcp-sqlguard` 的词法层按「函数名 + `(`」拦截 ClickHouse 外部访问/执行函数
（`url`/`file`/`s3`/`remote`/`jdbc`/`mysql`/`postgresql`/`sqlite`/`mongodb`/`redis`/`odbc`/`hdfs`/
`azureBlobStorage`/`gcs`/`iceberg`/`deltaLake`/`hudi`/`executable`/`format`/`input`/`clusterAllReplicas`/`distributed`/`zookeeper`/`arrowFlight`），
`numbers`/`system.*`/`values`/`null` 不受影响。当前联调环境的 `default` 是超级用户，
`readonly=1` 不阻止这些读取外部资源的函数；生产必须换成只读账号。

残留风险：函数黑名单只按名字拦截调用，无法覆盖**已存在的** `jdbc`/`mysql` 等外部表引擎、
`dictGet` 读取配置字典、`system.query_log` 等系统目录读取；这些路径只能靠 L1 只读账号权限与
`allowed_schemas` 白名单收敛。`default` 超级用户仅用于当前联调环境。

## 5. 参数绑定

- host 侧：`db_query_readonly` 只接受标量位置数组，按 SQL 中 `?` 的顺序绑定；占位符计数与
  clickhouse crate 的解析口径一致（扫描所有 `?`，`??` 视为字面 `?`，字面量里的 `?` 也必须转义）。
- agent 侧：`ClickHouseQuery.bind(serde_json::Value)` 映射成 Rust 标量后交给 crate；
  **crate 的 `?` 绑定是客户端转义后拼接 SQL 文本，不是服务端参数**。因此：
  - 值不能是数组/对象（显式拒绝，不做隐式字符串化）；
  - 禁止在日志/审计里打印绑定后的 SQL 或参数值；
  - `sql_hash` 只覆盖 SQL 文本：`db_query_readonly` 用 `sha256_hex(q.sql)`（绑定前，不含 params）；
    模板运行审计用 `params_hash`（渲染后命名参数的规范化哈希），不是 `sql_hash`。两者都不含参数值。
- 服务端 `{name:Type}` 需要调用方给出精确类型，本轮不用；模板统一在编译期把 `:name` 改写为 `?`。

## 6. 配置

| 项 | 说明 |
|---|---|
| `[[datasources]] engine` | 只接受 `"clickhouse"`；其它值启动即拒绝 |
| `dsn_env` | 环境变量名；值为 `http://user:pass@host:port[/database]`，只存 env |
| `allowed_schemas` | ClickHouse **database 白名单**；空数组 = 不限制 |
| `JUMCP_CLICKHOUSE_AGENT_BIN` | 可选，覆盖 agent 二进制路径 |
| agent 解析顺序 | 环境变量 → host 可执行文件同目录 → `target/debug` 兄弟目录（cargo test）→ `PATH` |

`database` 入参：省略时用 DSN 路径中的库（无路径则是 `default`）；给出时必须落在白名单内。
白名单非空时，连「省略入参后的实际执行库」也要检查——不能靠省略参数绕过。

## 7. 失败语义

| 场景 | host 行为 |
|---|---|
| agent 启动失败 / 握手失败 | 启动直接失败（fail-closed），错误带数据源名 |
| 请求超时 | 杀死 agent，`MCP_EXEC_TIMEOUT` + `outcome=unknown`，不重试 |
| stdout EOF / 响应 id 不匹配 / 非法 JSON | 杀死 agent，`MCP_STORE_ERROR` + `outcome=unknown` |
| ClickHouse 明确报错 | 原样返回 `MCP_STORE_ERROR`，agent 保持可用 |
| 进程崩溃 | 下一次调用重新拉起；崩溃前的请求不重放 |
| stderr | 只保留最近 32 行并脱敏 URL authority，附在致命错误 `data.stderr_tail` |

## 8. 非目标

- 不做多引擎抽象；新增引擎必须先定协议与只读语义，再决定是否复用本 agent 形态。
- 不在 agent 内做 L4、预算、审计或模板；这些留在 host，避免第二治理面。
- 不做跨请求会话状态（`session_id`、临时表、事务）；每条查询独立。
- 不把 agent 暴露为 MCP server 或 HTTP 服务。
