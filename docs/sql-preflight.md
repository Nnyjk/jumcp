# 第三方 SQL 前置拦截（sql preflight）

jumcp 聚合的上游 MCP server 都是**第三方实现**：它们收到什么 SQL、会不会自己校验，
本服务既不能假设也不能依赖。因此「恶意注入」与「无限制的大范围查询」必须在 jumcp
这一层被挡下——它是这些调用链上唯一由我们控制的收口点。

本文是当前有效契约；实现见 `backend/crates/jumcp-server/src/sql_guard.rs`。

## 1. 拦截点

唯一入口是 `mcp/dispatch/upstream.rs` 的 `tool_upstream`，顺序固定为：

```
tools/call
  → allowlist（tool_allowed / matches_upstream）        ── 拒绝 → policy_decision + isError
  → SQL 前置拦截（sql_guard::check）                     ── 拒绝 → policy_decision + isError  ← 本文
  → manager.resolve（建连 + tools/list）
  → manager.call（真正转发）
  → tool_call 审计（只有真正发生的调用才落台账）
```

为什么在 `resolve` 之前：`resolve` 会建立上游连接并拉取 `tools/list`。被拒的调用连
「上游有哪些工具」都不该去问——第三方上游看到的每一次握手都值得省掉。

「这是不是 SQL 调用」的判定只看入参本身：`arguments` 是对象，且 `sql` / `query` /
`statement` 里存在字符串值（优先取第一个非空；三个都给空白串时按「声明了 SQL 却为空」
处理，命中 `empty`）。其它字段（`table`、`filter`、`database`…）不参与判定：它们的语义由
各上游自定义，jumcp 无权替它猜。

命中拦截后：

- **不调用上游**，也**不写 `tool_call`** 台账（被拒的调用不是一次调用）；
- 写一条 `policy_decision` 审计（`action=deny`、`outcome=denied`、`error_code=MCP_SQL_BLOCKED`），
  `data.detail` 里带 `surface=mcp/sql-preflight`、`rule`、`tool`、`upstream_id`、`sql`（前 200 字符）。

## 2. 规则表

规则名是**稳定标识**：管理台与告警按 `rule` 聚合，不解析文案。判定顺序即优先级
（先命中先返回）；全部判定基于词法，不做 AST 解析——第三方工具的 SQL 方言不可静态确定。

| rule | 触发条件 |
|---|---|
| `empty` | trim 后为空 |
| `comment` | 含 `--` 或 `/*`（注释是最常见的注入隐藏/绕过载体） |
| `not_select` | 首个关键字不是 `SELECT` / `WITH`（大小写不敏感，允许前置括号） |
| `multiple_statements` | 去掉字符串字面量与结尾分号后仍含 `;` |
| `write_statement` | 出现 `INSERT`/`UPDATE`/`DELETE`/`ALTER`/`DROP`/`CREATE`/`TRUNCATE`/`RENAME`/`GRANT`/`REVOKE`/`ATTACH`/`DETACH`/`OPTIMIZE`/`SYSTEM`/`KILL`/`SET`（词边界；`db.x` 限定名里的库名不算） |
| `system_table` | 引用 `system.` 前缀的表 |
| `bypass_function` | 调用 `dictGet` / `dictGetOrDefault` / `joinGet`（绕过表/库白名单的取值通道） |
| `schema_out_of_scope` | `FROM` / `JOIN` 引用区出现 `db.table` 且 `db` 不在白名单（白名单为空则跳过该规则） |
| `limit_too_large` | 存在 `LIMIT n` 且 `n > 1000`（`LIMIT offset, n` 取第二个数字） |
| `unbounded` | 既无 `LIMIT` 也无 `WHERE`，且不是纯聚合查询 |

细节：

- **字符串字面量先被抹成空白**。`WHERE name = 'a; DROP TABLE t'` 里的分号与关键字是数据，
  不是语句结构；不抹就会误杀合法查询，也会让真正的注入藏进字面量里漏判。
- **纯聚合**指 `count/sum/avg/min/max/uniq/quantile` 且 SELECT 列表不是 `*`——聚合只返回
  一行，不构成"把整张表带回去"。`WHERE` 视为范围收敛，所以点查（`WHERE id = 1`）不需要 LIMIT。
- 库白名单只看 `FROM`/`JOIN` 的引用区：`SELECT` 列表和 `WHERE` 里的 `t.column` 是别名引用，
  按它们判库名会把正常查询误杀。

## 3. 错误形状

拦截回的是 **MCP 工具结果**（`result` 里 `isError=true`），不是 JSON-RPC error：

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "content": [
      {
        "type": "text",
        "text": "{\"error\":{\"code\":\"MCP_SQL_BLOCKED\",\"message\":\"查询既无 LIMIT 也无 WHERE，会全表/全范围扫描。\",\"data\":{\"rule\":\"unbounded\",\"hint\":\"加上 LIMIT（≤1000）或 WHERE 过滤条件后重试。\",\"upstream_id\":\"clickhouse\",\"tool\":\"clickhouse__query\"}}}"
      }
    ],
    "structuredContent": {
      "error": {
        "code": "MCP_SQL_BLOCKED",
        "message": "查询既无 LIMIT 也无 WHERE，会全表/全范围扫描。",
        "data": {
          "rule": "unbounded",
          "hint": "加上 LIMIT（≤1000）或 WHERE 过滤条件后重试。",
          "upstream_id": "clickhouse",
          "tool": "clickhouse__query"
        }
      }
    },
    "isError": true
  }
}
```

对应的审计事件（`event_type=policy_decision`）：

```json
{
  "event_type": "policy_decision",
  "object_type": "request",
  "object_id": "clickhouse__query",
  "action": "deny",
  "outcome": "denied",
  "error_code": "MCP_SQL_BLOCKED",
  "error_message": "查询既无 LIMIT 也无 WHERE，会全表/全范围扫描。",
  "correlation_id": null,
  "data": {
    "detail": {
      "surface": "mcp/sql-preflight",
      "rule": "unbounded",
      "tool": "clickhouse__query",
      "upstream_id": "clickhouse",
      "sql": "SELECT * FROM env_air.air_point_info",
      "hint": "加上 LIMIT（≤1000）或 WHERE 过滤条件后重试。"
    }
  }
}
```

### 为什么用 `isError` 而不是 JSON-RPC error

`isError` 是**模型可见**的结果：agent 能读到 `message` 与 `hint`，据此改写查询后重试；
JSON-RPC error 属于传输/协议层失败，多数客户端只把它当"调用挂了"展示或重试，模型拿不到
可操作的修正信息。这里的拦截是**业务判定**（查询不合规），不是协议错误，因此走结果通道；
同时 `rule` 稳定，便于治理面按规则统计拦截量。

## 4. 配置

| 项 | 来源 | 默认 | 语义 |
|---|---|---|---|
| `allowed_schemas` | env `JUMCP_SQL_ALLOWED_SCHEMAS`（逗号分隔） | 空 | 允许 `FROM`/`JOIN` 引用的库；空 = 不做库范围限定（`schema_out_of_scope` 整条规则跳过）。去空白、丢空项、大小写不敏感去重 |
| `max_limit` | 代码常量 `1000` | 1000 | 单次查询允许的最大 `LIMIT` |

`max_limit` 不开放配置：它是"一次调用能带走多少行"的安全承诺，可配置就等于把它降级成
运维便利项；要改必须走代码评审。配置装配在 `McpServer::from_arc`（由 `GuardConfig::from_env()`
构造），`McpServer::with_sql_guard(Some(cfg))` 替换，`None` 关闭拦截（仅排障/对照测试用）。

## 5. 与 `jumcp-sqlguard` 的分工

`jumcp-sqlguard` 是**已知方言**的 AST 只读校验（控制字符、可执行注释、行锁、危险函数黑名单、
schema 越界），服务于"我们知道这条 SQL 要去哪个数据源"的路径；本文这层面对的是**方言未知**的
第三方工具，因此只用词法规则，并且规则集、错误码、LIMIT 上限都与本层契约一一对应。
两层的拒绝对象不同，不要合并：AST 校验替不了这里的"上游收口"，词法网关也不该假装能替
已知方言做深度校验。
