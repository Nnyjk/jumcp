# 后端实现决策与坑（Rust）

配套：`docs/plan/hjzl-mcp-server-preview.md`、`docs/plan/implementation-plan.md`。

## 已定决策

- **未采用 rmcp**（官方 Rust MCP SDK）：其 API 随版本漂移，且本服务要求 stdio 与 streamable-http 共用同一条 async 调用链；手写 JSON-RPC 协议层更可控，代价是自行维护 initialize / tools / 错误码。
- **只读超时用事务内 `set_config('statement_timeout', $1, true)`**：`SET LOCAL statement_timeout = $N` 在 PG 不接受绑定参数；两者都只在事务内生效、随 rollback 失效，不会污染 PgBouncer 的共享 socket。
- **命名占位符只在注册期改写**：`:name` → `$n` 在模板编译时一次性完成；`db_query_readonly` 只接受位置参数数组，遇到命名占位符直接拒绝，运行期不做任何字符串替换。
- **限制与预算语义分离**：行数/单格字符超限夹紧并回显（`effective_max_rows` / `next_offset`）；token 预算超限显式失败或进摘要模式。二者混用等于静默降级。
- **能力指针行可更新，历史不可改**：`capabilities` 行指向当前版本；`capability_versions` 由 append-only trigger 保护。不是靠禁止指针更新来保证"编辑即新版本"。

## 坑

- sqlparser 与 sqlglot 差异：MySQL 可执行注释 `/*! ... */`、`INTO OUTFILE` 在 AST 层不可见，必须在字符/词法层拦截；`DO $$ ... $$`、`LOCK IN SHARE MODE` 解析失败需兜底；Postgres 的 `?` 会被当成 JSONB 运算符。
- Postgres 美元引号必须自行识别，否则字符串里的分号会误切语句；`#` 只在 MySQL/MariaDB 是注释，在 Postgres 里是运算符，误判会截短被校验语句。
- `shape_rows` 的 Rust 签名用 `usize`：负数夹紧无从表达，故未移植 Python 的负数用例（0→1 下界仍覆盖）。
- git 基线提交前需忽略 `**/data/*.sqlite3*`：运行时审计库默认落在 `data/audit.sqlite3`，`*.db` 规则匹配不到。
- 管理面在 stdio 模式下不启用；HTTP 模式未配置 `HJZL_ADMIN_TOKEN` 时进程拒绝启动（fail-closed）。
