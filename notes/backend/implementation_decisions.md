# 后端实现决策与坑（Rust）

配套：`docs/plan/hjzl-mcp-server-preview.md`、`docs/plan/implementation-plan.md`。

## 已定决策

- **未采用 rmcp**（官方 Rust MCP SDK）：其 API 随版本漂移，且本服务要求 stdio 与 streamable-http 共用同一条 async 调用链；手写 JSON-RPC 协议层更可控，代价是自行维护 initialize / tools / 错误码。
- **只读超时用事务内 `set_config('statement_timeout', $1, true)`**：`SET LOCAL statement_timeout = $N` 在 PG 不接受绑定参数；两者都只在事务内生效、随 rollback 失效，不会污染 PgBouncer 的共享 socket。
- **命名占位符只在注册期改写**：`:name` → `$n` 在模板编译时一次性完成；`db_query_readonly` 只接受位置参数数组，遇到命名占位符直接拒绝，运行期不做任何字符串替换。
- **限制与预算语义分离**：行数/单格字符超限夹紧并回显（`effective_max_rows` / `next_offset`）；token 预算超限显式失败或进摘要模式。二者混用等于静默降级。
- **能力指针行可更新，历史不可改**：`capabilities` 行指向当前版本；`capability_versions` 由 append-only trigger 保护。不是靠禁止指针更新来保证"编辑即新版本"。
- **管理面身份来自凭证，而非请求头**：`HJZL_ADMIN_TOKENS="id:token,id:token"`，token 常量时间比较后派生 actor id；`X-Admin-Actor` 仅作一致性校验（不一致 400），审计主体恒用派生 id。四眼因此不可伪造：`capabilities.submitted_by` 记录提审人，publish 时与批准人比较。
- **前端不持有管理面凭证**：`VITE_*` 是编译期内联，任何凭证都会随构建产物公开。dev 由 Vite 代理在 `proxyReq` 注入 `Authorization`（token 只存在于 Node 进程环境，`HJZL_DEV_ADMIN_TOKEN`）；生产由网关/BFF 注入或改用会话 Cookie；构建期守卫在 production 模式检测到 `VITE_ADMIN_*` 直接终止构建。
- **数组参数本轮不启用**：`array<T>` 在注册期拒绝（`MCP_TEMPLATE_INVALID` / `rule=array_param_unsupported`），因为 `$n::T[]` 的 ANY 展开尚未实现——不留"注册放行、首次调用必炸"的中间态。
- **null 参数必须显式 cast**：`Value::Null` 只能按 text OID 绑定，`$1 IS NULL OR col = $1` 作用在 int/date 上会得到 `operator does not exist`。绑定前校验：null 且占位符无 `::type` → `MCP_NULL_PARAM_NEEDS_CAST`（提示 `$1::int`）。

## 坑

- sqlparser 与 sqlglot 差异：MySQL 可执行注释 `/*! ... */`、`INTO OUTFILE` 在 AST 层不可见，必须在字符/词法层拦截；`DO $$ ... $$`、`LOCK IN SHARE MODE` 解析失败需兜底；Postgres 的 `?` 会被当成 JSONB 运算符。
- Postgres 美元引号必须自行识别，否则字符串里的分号会误切语句；`#` 只在 MySQL/MariaDB 是注释，在 Postgres 里是运算符，误判会截短被校验语句。
- `shape_rows` 的 Rust 签名用 `usize`：负数夹紧无从表达，故未移植 Python 的负数用例（0→1 下界仍覆盖）。
- git 基线提交前需忽略 `**/data/*.sqlite3*`：运行时审计库默认落在 `data/audit.sqlite3`，`*.db` 规则匹配不到。
- 管理面在 stdio 模式下不启用；HTTP 模式未配置 `HJZL_ADMIN_TOKENS` 时进程拒绝启动（fail-closed）。
