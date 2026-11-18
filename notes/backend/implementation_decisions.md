# 后端实现决策与坑（Rust）

配套：`../mcp_plan/architecture_preview.md`、`../mcp_plan/implementation_plan.md`（历史设计记录，不作为当前契约）。

## 已定决策

- **未采用 rmcp**（官方 Rust MCP SDK）：其 API 随版本漂移，且本服务要求 stdio 与 streamable-http 共用同一条 async 调用链；手写 JSON-RPC 协议层更可控，代价是自行维护 initialize / tools / 错误码。
- **只读超时用事务内 `set_config('statement_timeout', $1, true)`**：`SET LOCAL statement_timeout = $N` 在 PG 不接受绑定参数；两者都只在事务内生效、随 rollback 失效，不会污染 PgBouncer 的共享 socket。
- **命名占位符只在注册期改写**：`:name` → `$n` 在模板编译时一次性完成；`db_query_readonly` 只接受位置参数数组，遇到命名占位符直接拒绝，运行期不做任何字符串替换。
- **限制与预算语义分离**：行数/单格字符超限夹紧并回显（`effective_max_rows` / `next_offset`）；token 预算超限显式失败或进摘要模式。二者混用等于静默降级。
- **能力指针行可更新，历史不可改**：`capabilities` 行指向当前版本；`capability_versions` 由 append-only trigger 保护。不是靠禁止指针更新来保证"编辑即新版本"。
- **管理面身份来自凭证，而非请求头**：`JUMCP_ADMIN_TOKENS="id:token,id:token"`，token 常量时间比较后派生 actor id；`X-Admin-Actor` 仅作一致性校验（不一致 400），审计主体恒用派生 id。主体因此不可伪造：`capabilities.submitted_by` 记录提审人，publish 时把批准人（token 派生）写入 `approved_by` 与审计 `approval`。
- **前端不持有管理面凭证**：`VITE_*` 是编译期内联，任何凭证都会随构建产物公开。管理台改为**登录页 + HttpOnly 会话 cookie**：用户把 `JUMCP_ADMIN_TOKENS` 里的某个 token 一次性 `POST /admin/login` 交给后端，此后同源请求自动带 cookie（`SameSite=Strict`，非 GET 校验同源）。**没有网关/BFF，也没有 Vite 注入**；dev 期 Vite 只做纯转发（`JUMCP_DEV_ADMIN_TARGET` / `JUMCP_DEV_API_TARGET` 两个地址变量）；构建期守卫在 production 模式检测到 `VITE_ADMIN_*` 直接终止构建。
- **数组参数本轮不启用**：`array<T>` 在注册期拒绝（`MCP_TEMPLATE_INVALID` / `rule=array_param_unsupported`），因为 `$n::T[]` 的 ANY 展开尚未实现——不留"注册放行、首次调用必炸"的中间态。
- **null 参数必须显式 cast**：`Value::Null` 只能按 text OID 绑定，`$1 IS NULL OR col = $1` 作用在 int/date 上会得到 `operator does not exist`。绑定前校验：null 且占位符无 `::type` → `MCP_NULL_PARAM_NEEDS_CAST`（提示 `$1::int`）。

## 2026-11：MCP 接入令牌改为平台生成（DB 持久化）

- **单一鉴权分支**：`/mcp` 只查 registry 的 `access_tokens`（sha256 命中 + `active`）；环境变量静态 token 只在启动引导时导入一次，之后不再参与鉴权。集成测试若直接构造 `Service`/`http_router`，必须显式 seed（`import_bootstrap_tokens` 或 `create_access_token`），不要恢复第二套生产分支。
- **组是硬上限**：token 的 `groups` 同时是默认组与 allowlist，`resolve_scope` 不再回落到角色 `default_groups`；header/`_meta` 只能收窄。否则"按 token 配置收窄可见工具"会被角色默认组悄悄放大。
- **内置工具是独立 allowlist**：只按组过滤 managed 能力不够——`search_capabilities` / `describe_capability` / `list_groups` / `db_query_readonly` / `db_run_template` 是 builtin，恒在可见集里。`EffectiveScope.allowed_builtin_tools` 必须同时约束 `tools/list` 与 `tools/call`；`admin_*` 再加 `admin` 组双重判定。
- **审计来源**：`ActorRef.token_id`/`source` 是可选键（serde `skip_serializing_if`），旧行反序列化不受影响。管理面 `source="admin-api"`；MCP token 用 token 配置的 `source` 与 `id`。store 写 `admin_change` 通过 `IntoAuditOrigin`（`&str` 仅 id，`&Actor` 带全量来源）统一构造，避免各调用点各拼一份。
- **撤销不删行**：`enabled=0` + `revoked_at`，重复撤销 409 且不写第二条审计；组删除必须同时看 `capability_count` 与 `token_count`。
- **明文只出现一次**：库里只有 sha256；审计与 HTTP item 只写 `token_prefix`。`AccessTokenRecord` 故意不 derive `Debug`（避免 hash 进日志）。

## 2026-11：组/角色 code + name 双轨

- **code 是唯一权限标识**：`capability.group`、`token.groups`、`role.default_groups`、`scope.groups`、审计 `object_id`/`actor.role` 全部存 code；`name` 只是中文展示名。`resolve_scope` 只按 `policy.group_codes()` 判定，用中文展示名申明组会得到 `MCP_GROUP_FORBIDDEN`。
- **配置兼容在反序列化里做**：`TenantPolicy` 走 `TenantPolicyWire`，`[[groups]]` 只有 `name` 时把它当 code、展示名回退 code；角色缺 `name` 回退 code。运行期结构永远是 `GroupDef { code, name }`，不保留第二套“name 即 code”分支。
- **管理组旧库迁移**：`groups.name` 列继续存 code，新增 `groups.display_name`（`ensure_column`，`TEXT NOT NULL DEFAULT ''`）。读取时空串回退 code；写入/编辑只改 `display_name` 与数值/角色列，主键不动。
- **PUT 不接受 code**：路径即 code；body 出现 `code` 直接 400，避免“改 code”被静默忽略后权限标识漂移。审计 object_id 仍是 code，`changes` 额外记录 `name`（展示名）。
- **过滤在合并结果上做**：`GET /admin/groups` 先合并配置组+管理组，再按 `q`（code/name 小写包含）、`source`（policy|managed，非法值 400）、`role`（角色 code 精确）过滤，`total` 为过滤后数量。

## 坑

- sqlparser 与 sqlglot 差异：MySQL 可执行注释 `/*! ... */`、`INTO OUTFILE` 在 AST 层不可见，必须在字符/词法层拦截；`DO $$ ... $$`、`LOCK IN SHARE MODE` 解析失败需兜底；Postgres 的 `?` 会被当成 JSONB 运算符。
- Postgres 美元引号必须自行识别，否则字符串里的分号会误切语句；`#` 只在 MySQL/MariaDB 是注释，在 Postgres 里是运算符，误判会截短被校验语句。
- `shape_rows` 的 Rust 签名用 `usize`：负数夹紧无从表达，故未移植 Python 的负数用例（0→1 下界仍覆盖）。
- git 基线提交前需忽略 `**/data/*.sqlite3*`：运行时审计库默认落在 `data/audit.sqlite3`，`*.db` 规则匹配不到。
- 管理面在 stdio 模式下不启用；HTTP 模式未配置 `JUMCP_ADMIN_TOKENS` 时进程拒绝启动（fail-closed）。
- `access_tokens` 的列表列可能因旧库 `ensure_column` 补列得到空串：`groups_json` / `builtin_tools_json` 解析时空串按空集处理，否则一行坏数据会让整张列表 500。
- 过期时间比较必须解析 RFC3339 后比 `DateTime<Utc>`；字符串字典序在带不同时区偏移时会错。解析失败按过期处理（fail-closed）。
- 组删除的 token 引用计数与删除必须在同一把连接锁内完成；不要在持锁时调用会再次加锁的 store 方法（死锁）。
- 后端默认监听 `0.0.0.0`（容器/WSL 可访问）；本机健康检查与脚本仍用 `127.0.0.1`。
