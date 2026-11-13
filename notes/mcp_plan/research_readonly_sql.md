# 功能点：只读 SQL 执行面

问题：给模型一个能执行 SQL 的入口，如何保证"只读"不是口头约定。

## 候选方案

| 方案 | 做法 | 关键点 |
|---|---|---|
| Postgres MCP Pro（crystaldba） | 多 access mode（unrestricted / restricted 只读等）；只读依赖 `default_transaction_read_only` + 每个查询包只读事务，**并解析 SQL** 阻断 `COMMIT`/`ROLLBACK` 后开新写事务的逃逸 | 解析不是唯一保证，但与数据库级只读叠加。[github.com/crystaldba/postgres-mcp](https://github.com/crystaldba/postgres-mcp) |
| Bytebase DBHub | 工具级 `readonly` 开关，仅允许 SELECT/SHOW/DESCRIBE/EXPLAIN；`max_rows` 限制返回行数；单二进制覆盖 6 种数据库 | 面向 token 效率的极简工具集。[github.com/bytebase/dbhub](https://github.com/bytebase/dbhub) |
| SQLGlot | 纯 Python、零依赖 SQL 解析/改写/优化，30+ 方言，可 AST 遍历做语句分类 | 生态里被 SQLMesh、Superset 采用；可直接做 §4.3.4 结构变体白名单与附录 B 黑名单的落地基础。[sqlglot.com](https://sqlglot.com/) |
| 运维侧加固（实践总结） | 只读角色 + 仅 SELECT 授权（最好只授视图）+ `statement_timeout` / `idle_in_transaction_session_timeout` + 指向只读副本 | 逃逸 `SET default_transaction_read_only = off` 仍会被权限拒绝。[hikmahtechnologies.com](https://hikmahtechnologies.com/blog/postgres-mcp-server) |

## 契合度与取舍

- **采纳（分层，而非单点）**：与 Postgres MCP Pro 的结论一致——数据库级只读是主保证，SQL 解析是防逃逸的补充。本计划 §4.3.1 的 L1 只读账号 / L2 只读事务 / L3 AST 白名单 / L4 行级与 token 上限即该结构的强化版，且明确 L1 不可被 L3 替代。
- **采纳（DBHub 的 `readonly` 语义）**：工具级开关 + 白名单语句类型 + `max_rows` 三件套直接对应 `db_query_readonly` 的对外契约。
- **采纳（SQLGlot 作为实现基础）**：附录 B 的 AST 黑名单与 §4.3.4 的结构变体白名单需要可靠解析器；成熟解析器优于自写正则。
- **放弃（AST 作为唯一保证）**：生态实现同样不把解析当唯一防线——可写 CTE、`nextval`、`set_config` 等都会漏；反过来，只有工具级 `readonly` 标志也不够（DBHub 的做法不阻断可写 CTE）。
- **放弃（多 access mode 暴露给模型）**：access mode 是部署期配置，不进入工具入参——模型不能自己请求写权限。本服务不提供业务数据写面，因此也不存在"审批后的写工具"（`decisions_write_scope.md`）。

## 坑：运行期 `SET` 与连接池

- 运行期 `SET default_transaction_read_only = on` 不是安全边界：可被 `COMMIT; SET ... = off; ...` 链式绕过，且在 PgBouncer 等池化环境下会污染共享 socket，导致其他服务的写请求异常失败（Google MCP Toolbox 明确把这类做法列为"软锁"）。
- 正确位置是**连接/角色级**：DSN 锁参数（Cloud SQL `cloudsql_session_read_only=locked`、AlloyDB 同款）、或 `ALTER ROLE ... SET default_transaction_read_only = on` + 仅 SELECT 授权；这两者都无法被会话内语句覆盖。
- 只读部署应当**不注册写工具**（Toolbox 的 tool suppression），而不是注册后靠策略拒绝——目录本身是最强的默认拒绝。

来源：[MCP Toolbox Read-Only Tools](https://mcp-toolbox.dev/documentation/configuration/security/read-only/)。

## 对本计划的具体约束

- `db_query_readonly` 必须显式声明语句白名单（SELECT/SHOW/DESCRIBE/EXPLAIN），并与 AST 校验、数据库只读事务三层同时生效。
- 超时与行数上限是**执行期参数**（statement_timeout + `max_rows`），不是提示词约束。
- 模板 SQL 的预编译与结构变体校验使用同一解析器，避免"校验一套语法、执行另一套"。
