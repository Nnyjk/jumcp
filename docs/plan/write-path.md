# 写路径（write path）：prepare / apply 契约与实施设计

本文件是 `docs/plan/hjzl-mcp-server-preview.md` §4.4.6 的展开：主文档只保留对外契约摘要，字段级、错误码、校验分层、开关与配额以本文件为准。读路径（L1–L6，§4.3.1）不受本文件影响。

## 0. 结论与现状锚点

1. 写能力建模为 `kind ∈ {template, endpoint}` ∧ `risk = write`；**不新增任何 MCP 工具**，prepare/apply 是同一调用入口的两种 `phase`。
2. `prepare` 无副作用，只做校验、落 `write_intent`、签发一次性令牌；`apply` 消费令牌后执行，执行前重新判定权限（撤销优先）。
3. 写能力默认关闭：五级开关（全局 → 租户 → 组 → 数据源 → 能力）同时打开，且角色授权成立、会话未显式关闭，才允许写。发现层任一开关关闭 → 能力不可见（`MCP_CAPABILITY_NOT_FOUND`）；已可见能力在运行期开关（数据源/角色/会话）关闭或审批失效时 → `MCP_WRITE_DISABLED`。两种都是拒绝，不降级为只读执行。
4. 写账号与只读账号是**两个不同的 DSN 环境变量**；写账号启动自检要求「DML 可用、DDL/超管不可用」，失败即拒绝启动。
5. 一次能力调用只允许一条 DML 语句、一个事务；无多语句、无跨系统事务、无分布式提交。`COMMIT` 之后不提供回滚。
6. 写操作产生三类审计事件 `write_intent` / `write_apply` / `write_failed`，进入现有单表哈希链（`backend/crates/hjzl-core/src/audit.rs:12-21`），链结构不变。

写侧代码触点全部落在现有只读实现之外，不复用只读校验入口：

| 现状事实 | 位置 |
|---|---|
| 只读校验只接受单条 `SELECT`/`WITH`，根节点非查询即拒 | `backend/crates/hjzl-sqlguard/src/validate.rs:115-136` |
| 只读黑名单是固定函数表 + 前缀族 | `backend/crates/hjzl-sqlguard/src/validate.rs:24-40` |
| 只读校验只做语法级判定，唯一强保证在数据源账号 | `backend/crates/hjzl-sqlguard/src/lib.rs:1-8` |
| 模板注册硬拒 `risk != read` | `backend/crates/hjzl-core/src/template.rs:250-254` |
| 模板注册期渲染后仍用只读校验 | `backend/crates/hjzl-core/src/template.rs:671-674` |
| 数据源只有只读 DSN 与 `readonly` 标记 | `backend/crates/hjzl-core/src/config.rs:12-23` |
| 只读连接在 DSN 上追加锁参数 | `backend/crates/hjzl-server/src/pg_executor.rs:22-25` |
| 只读执行恒 `rollback`，不提交 | `backend/crates/hjzl-server/src/pg_executor.rs:120-175` |
| 只读自检：`default_transaction_read_only=on` 且 `CREATE TEMP TABLE` 必须失败 | `backend/crates/hjzl-server/src/pg_executor.rs:90-118` |
| 所有工具共用一份 `readOnlyHint=true` 注解 | `backend/crates/hjzl-server/src/mcp.rs:16-17` |
| 审计事件类型是闭集，未知类型直接拒绝 | `backend/crates/hjzl-core/src/audit.rs:12-21`、`audit.rs:111-116` |
| 四眼只比 `submitted_by != approved_by`，批准人身份取自 token | `backend/crates/hjzl-core/src/registry.rs:242-250`、`backend/crates/hjzl-server/src/admin.rs:498-520` |
| 管理台写操作按钮为占位文案「未接通」 | `admin-web/src/components/PageShell.tsx:77-86`、`Sidebar.tsx:69`、`pages/ReviewQueuePage.tsx:60` |

## 1. 建模与调用面

### 1.1 能力定义扩展

写能力在现有 `TemplateDef`（`backend/crates/hjzl-core/src/template.rs:147-180`）上新增 `write` 段；`deny_unknown_fields` 要求同步扩展结构体。

```yaml
template_id: order.mark_paid
version: 7
group: db.write.orders
risk: write
description: "按订单号把订单置为已支付；并发安全，需带期望版本"
sql: |
  UPDATE public.orders
     SET status = $2, paid_at = now(), version = version + 1
   WHERE id = $1 AND version = $3
  RETURNING id, version
params:
  - { name: order_id, type: string, required: true }
  - { name: status,   type: enum, required: true, enum: [paid, refunded] }
  - { name: expected_row_version, type: int, required: true }
write:
  enabled: false                  # 能力级开关，默认 false；开启需审批
  operation: update               # insert | update | delete | upsert，注册期与 AST 反推结果核对
  target_tables: [public.orders]  # 只允许写这些表
  read_tables: []                 # 语句内 SELECT 子查询可引用的表；与 target_tables 取并为白名单
  write_columns: [status, paid_at, version]
  returning_columns: [id, version]
  server_time_columns: [paid_at]  # 允许 `col = now()` 的列
  max_affected_rows: 1            # ≤ 数据源写面上限
  require_where: true             # update/delete 必须 true
  optimistic_lock: { column: version, param: expected_row_version }
  idempotency: key_required       # key_required | natural
  preview: estimate               # estimate | count
  approval: { mode: human, calls_per_approval: 1 }
constraints:
  allowed_schemas: [public]
  statement_timeout_ms: 5000
```

注册期规则：

- `risk=write` 时必须存在 `write` 段，`write.enabled=false` 时只允许 `status=draft`（可审核，不发布）。
- 打开 `write.enabled` 是治理动作：记 `admin_change`，走四眼审批，与 `status` 发布分开；发布只决定可见性，不能替代写开关。
- `write.write_columns` 与数据源 `write.forbidden_write_columns` 求交必须为空，否则拒绝注册（`MCP_TEMPLATE_INVALID`）。
- `write.optimistic_lock` 声明后，SQL 中必须能静态找到 `column = <param>` 形式的谓词，否则拒绝注册。
- `write.operation` 必须与 AST 反推的根节点一致；`auto_limit` / `result_cache_ttl_s` 对写能力无效（写不缓存，`result_cache_ttl_s` 强制 0）。
- 定义哈希 `definition_hash` 覆盖 `write` 段，改动即新版本（沿用 `registry.rs:351-404`）。

### 1.2 调用面：每个 kind 仍只有一个出口

| kind | 调用面 | prepare/apply 形态 |
|---|---|---|
| `template` | 常驻 `db_run_template` | `phase` 入参，无新工具 |
| `endpoint` | 常驻 `invoke_capability` | 同上 |
| `tool` | 客户端 `tools/call` | **写能力不采用**：无法承载服务端 prepare 阶段 |

**禁止**新增 `db_run_write`、`db_execute`、`prepare_call`、`apply_call` 等第二出口。理由：版本钉住、参数校验、审计、令牌、开关都在同一分发链上，第二出口必然分叉（主文档 §2 原则 9）。

`db_run_template` 入参（在 `backend/crates/hjzl-server/src/mcp.rs:45-53` 现有 `template_id` / `params` / `version` 基础上追加）：

| 参数 | 类型 | 必填 | 语义 |
|---|---|---|---|
| `template_id` | string | 是 | 能力 id |
| `version` | int | 否 | 会话钉住版本，冲突返回 `MCP_VERSION_CONFLICT` |
| `params` | object | 是 | 逻辑参数；prepare 与 apply 必须完全一致 |
| `phase` | `prepare` \| `apply` | 否 | 写能力缺省 = `prepare`；读能力传入即 `MCP_VALIDATION_ERROR` |
| `confirm_token` | string | apply 时必填 | prepare 返回的一次性令牌 |
| `idempotency_key` | string | 视能力声明 | 1–128 字符；`idempotency: key_required` 时必填 |

`invoke_capability` 追加同一组 `phase` / `confirm_token` / `idempotency_key`；读 endpoint 不接受这三个参数。

### 1.3 注解

现有入口共用静态 `readOnlyHint=true`（`backend/crates/hjzl-server/src/mcp.rs:16-17`），写能力开启后会变成错误声明：

| 场景 | `readOnlyHint` | `destructiveHint` | `idempotentHint` | `openWorldHint` |
|---|---|---|---|---|
| 会话写开关关闭（默认） | `true` | `false` | `true` | `false` |
| 会话写开关开启，入口工具 | `false` | `true` | `false` | `false` |
| 单个写能力（`describe_capability` 返回） | `false` | `INSERT` 为 `false`，其余 `true` | `key_required` 或 `natural` 为 `true` | `endpoint` 写为 `true` |

注解按会话 `tools/list` 投影，与 §4.2 的目录投影同源；不得出现"注解说只读、实际会写"。`risk` 与注解冲突时以更保守者为准（主文档 §3.3）。

### 1.4 endpoint 写

`executor.type=http` 的写能力：

- `method ∈ {POST, PUT, PATCH, DELETE}`，注册期声明；`DELETE` 必须带路径 id 参数，禁止无目标批量删除。
- 仅允许注册期声明的 base URL + 路径模板；禁止参数指定绝对 URL、跨 host 重定向（§4.3.8）。
- `params_schema` 即请求体 schema；`idempotency_key` 透传为 `Idempotency-Key` 请求头。
- 2xx → `ok:true`；4xx → `ok:false` 且永不重试；5xx/超时仅在 `idempotent=true` 且带幂等键时可重试 1 次。
- `executor.type=kb` 不开放写；`template` 在 `invoke_capability` 内继续返回 `MCP_USE_DB_RUN_TEMPLATE`。

## 2. 协议

### 2.1 prepare

```json
{
  "template_id": "order.mark_paid",
  "version": 7,
  "params": { "order_id": "O-20261106-001", "status": "paid", "expected_row_version": 3 },
  "idempotency_key": "01J9Z8Q7X6W5V4"
}
```

返回（`db_run_template` 的 tool result 内容，非 JSON-RPC error）：

```json
{
  "phase": "prepare",
  "intent_id": "wi_01J9Z8R1",
  "status": "prepared",
  "confirm_token": "wt_9f2c1a...",
  "expires_at": "2026-11-06T10:20:00Z",
  "summary": {
    "capability_id": "order.mark_paid",
    "version": 7,
    "risk": "write",
    "datasource": "core-pg",
    "schemas": ["public"],
    "operation": "update",
    "tables": ["public.orders"],
    "columns": ["status", "paid_at", "version"],
    "update_columns": ["status", "paid_at", "version"],
    "predicate": "id = $1 AND version = $3",
    "params_hash": "sha256:2b0f...",
    "affected_rows_estimate": 1,
    "affected_rows_exact": null,
    "preview": "estimate",
    "approval": { "required": true, "mode": "human", "approvers_needed": 1 }
  },
  "next": "等待管理台批准后，以 phase=apply 与相同 params 重发"
}
```

- prepare 只允许读：执行 AST 校验 + 生成 `EXPLAIN`（`preview: estimate`）或 `SELECT count(*)`（`preview: count`），不执行 DML。
- `affected_rows_estimate` 取自 EXPLAIN 计划行数，标注 `preview: estimate`；不得对外表述为精确值。`preview: count` 受同一 `statement_timeout_ms` 约束，超时降级为 estimate 并在 `summary.preview` 回显，不失败。
- **summary 不回传参数值**；参数值仅供管理台审批视图使用，加密留存（§7.3）。
- `status` 是意图状态（§2.4）：`prepared`（待批准）或 `approved`（策略自动批准）；值来自意图表，不由调用方指定。
- `write_intent` 审计事件必须在令牌落库与返回**之前**落盘；令牌可先预生成（内存中算好哈希与 id），审计失败则丢弃、不返回。审计写失败即 prepare 失败（fail-closed）。

### 2.2 apply

```json
{
  "template_id": "order.mark_paid",
  "version": 7,
  "params": { "order_id": "O-20261106-001", "status": "paid", "expected_row_version": 3 },
  "phase": "apply",
  "confirm_token": "wt_9f2c1a...",
  "idempotency_key": "01J9Z8Q7X6W5V4"
}
```

apply 严格按序：

1. 重算 `params_hash`，与令牌记录比对；不一致 → `MCP_CONFIRM_TOKEN_INVALID`（参数变化即失效）。
2. 逐项比对令牌记录中的 `tenant` / `actor_id` / `session_id` / `capability_id` / `version`；任一不符即拒绝，码同上，`data.reason` 区分（跨租户、跨会话、跨人、版本漂移）。
3. 原子消费令牌：`UPDATE write_tokens SET used_at=? WHERE token_hash=? AND used_at IS NULL AND expires_at>?`，影响行数必须为 1，否则 `MCP_CONFIRM_TOKEN_INVALID`（`data.reason=replayed|expired`）。claim 只负责单次与过期；绑定字段已在第 2 步比对，不做隐式过滤，避免"查不到就当成过期"。
4. 重新判定：可见性、`callable`、角色授权、写开关矩阵、审批状态与有效期、配额。任一失败即拒绝，**已撤销的权限立即生效**。
5. 幂等键检查（§5.3）。
6. 执行单语句事务（§5.1），`affected_rows` 上限后检查在提交前（§4.2）。
7. 提交成功后写 `write_apply` 审计，再返回结果；提交失败或连接中断 → `write_failed` + `MCP_WRITE_OUTCOME_UNKNOWN`。

返回：

```json
{
  "phase": "apply",
  "intent_id": "wi_01J9Z8R1",
  "capability_id": "order.mark_paid",
  "version": 7,
  "ok": true,
  "affected_rows": 1,
  "returning": [{ "id": "O-20261106-001", "version": 4 }],
  "replayed": false,
  "elapsed_ms": 42,
  "token_id": "wt_9f2c1a"
}
```

`replayed=true` 出现在同幂等键重放（§5.3），此时 `elapsed_ms` 为首次执行耗时，另加 `applied_at`。

### 2.3 令牌

令牌是服务端存储的不透明串（`wt_` + 32 字节随机），不是自包含 JWT：吊销、单次消费、审批关联都需要服务端状态。表：

```sql
CREATE TABLE write_tokens (
  token_hash TEXT PRIMARY KEY,      -- SHA-256(token)，不存明文
  intent_id TEXT NOT NULL,
  tenant TEXT NOT NULL,
  actor_id TEXT NOT NULL,
  session_id TEXT NOT NULL,
  capability_id TEXT NOT NULL,
  version INTEGER NOT NULL,
  params_hash TEXT NOT NULL,
  approval_id TEXT NOT NULL DEFAULT '',
  issued_at TEXT NOT NULL,
  expires_at TEXT NOT NULL,
  used_at TEXT
);
```

| 属性 | 取值 |
|---|---|
| TTL | 300s（可配 60–900s） |
| 使用次数 | 1 次（原子 claim） |
| 绑定 | `tenant + actor_id + session_id + capability_id + version + params_hash` 六元组 |
| 吊销 | 管理台按 `intent_id` 批量置失效；下一次 apply 立即拒绝 |
| 存储 | 只存令牌哈希；明文只在 prepare 响应中出现一次 |

### 2.4 意图状态机

意图表（与令牌表同库；密文仅供审批视图使用）：

```sql
CREATE TABLE write_intents (
  intent_id TEXT PRIMARY KEY,
  tenant TEXT NOT NULL,
  actor_id TEXT NOT NULL,
  session_id TEXT NOT NULL,
  capability_id TEXT NOT NULL,
  version INTEGER NOT NULL,
  risk TEXT NOT NULL,
  datasource TEXT NOT NULL,
  operation TEXT NOT NULL,
  params_hash TEXT NOT NULL,
  params_ciphertext TEXT NOT NULL,     -- AES-GCM；完成后按留存期删除
  summary_json TEXT NOT NULL,
  approval_id TEXT NOT NULL DEFAULT '',
  approved_by TEXT NOT NULL DEFAULT '',
  approved_at TEXT NOT NULL DEFAULT '',
  calls_approved INTEGER NOT NULL DEFAULT 1,
  calls_used INTEGER NOT NULL DEFAULT 0,
  state TEXT NOT NULL,
  created_at TEXT NOT NULL,
  expires_at TEXT NOT NULL
);
```

| 状态 | 进入条件 | 允许的下一步 |
|---|---|---|
| `prepared` | prepare 通过校验、`write_intent` 已落盘 | `approved` / `rejected` / `expired` |
| `approved` | 自动策略命中，或管理台批准且四眼通过 | `applied` / `failed` / `unknown` / `expired` |
| `rejected` | 审批拒绝 | 终态（可重新 prepare） |
| `expired` | 令牌 TTL 或审批 TTL 到期 | 终态 |
| `applied` | 提交成功 | 终态 |
| `failed` | 事务内失败或提交前拒绝 | 终态（可重新 prepare） |
| `unknown` | 提交结果不可判定 | 终态；只能靠幂等键查询或人工核对 |

`approval.calls_per_approval > 1` 时，一次批准可支撑 N 次 apply（N ≤ 10，`sensitive`/`admin` 固定 1），每次 apply 需要独立意图与令牌，`approval.calls_used` 原子递增。

### 2.5 错误码

新增（写侧专用）：

| 码 | 触发 | `data` | 可重试 |
|---|---|---|---|
| `MCP_WRITE_DISABLED` | 运行期开关关闭（数据源/角色/会话）、时间窗外、目标系统不在允许清单 | `{reason, level, target?}` | 否 |
| `MCP_WRITE_REJECTED` | 语句形状/WHERE/列/函数/字面量/表越界 | `{reason, detail}` | 否 |
| `MCP_WRITE_LIMIT_EXCEEDED` | `affected_rows > max_affected_rows`（已回滚） | `{affected_rows, max_affected_rows}` | 否 |
| `MCP_WRITE_QUOTA_EXCEEDED` | 笔数、并发、日累计行数超限 | `{scope, limit, retry_after_ms}` | 是（退避后） |
| `MCP_WRITE_APPROVAL_REQUIRED` | 未批准、批准过期、批准人=提交人、需 DBA 二次签核未签 | `{intent_id, reason}` | 否（需人工） |
| `MCP_WRITE_OUTCOME_UNKNOWN` | 提交阶段连接中断/超时 | `{intent_id, token_id}` | 否（禁止重试） |
| `MCP_IDEMPOTENCY_CONFLICT` | 同键异参，或同键仍在执行中 | `{idempotency_key_hash, reason: params_mismatch\|in_flight}` | 仅 `in_flight` 可延后重试 |
| `MCP_ROW_VERSION_MISMATCH` | 声明乐观锁且 `affected_rows = 0` | `{column, expected, affected_rows: 0}` | 否（需重读） |

复用既有码，不造同义码：`MCP_MULTI_STATEMENT`、`MCP_CONFIRM_TOKEN_INVALID`、`MCP_VERSION_CONFLICT`、`MCP_CAPABILITY_NOT_FOUND`、`MCP_SCHEMA_OUT_OF_SCOPE`、`MCP_EXEC_TIMEOUT`、`MCP_VALIDATION_ERROR`、`MCP_BUDGET_EXCEEDED`、`MCP_USE_DB_RUN_TEMPLATE`（均在 `backend/crates/hjzl-core/src/errors.rs:6-33` 与 `backend/crates/hjzl-sqlguard/src/error.rs:8-14` 登记）。

## 3. 风险分级与动作矩阵

| 维度 | `read` | `write` | `sensitive` | `admin` |
|---|---|---|---|---|
| 典型能力 | 只读模板/查询 | 单表 INSERT/UPDATE/DELETE | 涉及 PII/资金/权限相关列 | TRUNCATE、DDL、GRANT、账号 |
| 本期是否开放 | 已开放 | 阶段五开放，默认关 | 阶段五之后单独评估，默认关 | **不开放**（注册期拒绝；运行期 `MCP_WRITE_REJECTED`） |
| 批审 | 无 | 生产：人工；staging 可策略自动 | 人工 + 可选 DBA 二次签核 | — |
| 令牌 | 无 | 1 次 | 1 次 | — |
| 每次批准覆盖调用数 | — | 默认 1，可配 ≤10 | 固定 1 | — |
| 单次影响行数 | 不限（读） | ≤ `max_affected_rows`（默认 100，上限 1000） | ≤ 1（按主键定位） | — |
| 语句面 | SELECT/WITH | 单条 INSERT/UPDATE/DELETE | 同 write，且列级白名单更窄 | — |
| 执行器 | `pg` / `mysql` / `clickhouse` | 阶段五仅 `pg` | 仅 `pg` | — |
| 数据源 | 只读 DSN | 写 DSN | 写 DSN（独立实例优先） | — |
| 审计 | `template_run` / `tool_call` | `write_intent` / `write_apply` / `write_failed` | 同 write，审批视图访问也记审计 | — |

`sensitive` 的能力定义与 `write` 相同，差异全部落在上表（审批强度、行数上限、可选 DBA 签核、可写列范围）。

## 4. SQL 写侧校验：L7–L12

读侧 L1–L6 的编号与语义不变（主文档 §4.3.1）。写侧用同一序列续编，映射关系：L7 对应读侧 L1+L2（账号与连接权限），L11 对应 L3（资源上限），L12 对应 L6（审计），L8–L10 是写特有。

L7 见 §6.2，L11 见 §5.1 与 §6.3，L12 见 §7 与 §8；§4.1–§4.5 只展开 L8–L10 的 SQL 规则。

| 编号 | 名称 | 施加位置 | 失败码 | 强度 |
|---|---|---|---|---|
| L7 | 写账号与权限边界 | 数据源配置 + 启动自检 | 拒绝启动（`MCP_CONFIG_ERROR`） | 强（唯一强保证） |
| L8 | 语句形状 | 注册期 + 每次 apply | `MCP_WRITE_REJECTED` / `MCP_MULTI_STATEMENT` | 强 |
| L9 | 影响范围（WHERE、行数、列与函数白名单） | 注册期 + 每次 apply | `MCP_WRITE_REJECTED` / `MCP_WRITE_LIMIT_EXCEEDED` | 强 |
| L10 | 参数与值绑定 | 注册期 + 每次 apply | `MCP_WRITE_REJECTED` / `MCP_VALIDATION_ERROR` | 强 |
| L11 | 事务、锁与超时 | 执行器 | `MCP_EXEC_TIMEOUT` / `MCP_WRITE_OUTCOME_UNKNOWN` | 强 |
| L12 | 审计与审批 | 治理链 | `MCP_WRITE_APPROVAL_REQUIRED` / `MCP_AUDIT_ERROR` | 事后可追溯 |

### 4.1 L8 语句形状

- 只允许一条语句；`;` 拼接与多语句一律 `MCP_MULTI_STATEMENT`（复用 `lex::split_statements`，不在词法层新写实现）。
- 根节点只允许 `INSERT` / `UPDATE` / `DELETE`；`MERGE`、`TRUNCATE`、`CREATE`/`ALTER`/`DROP`、`GRANT`/`REVOKE`、`COPY`、`CALL`、`DO`、`VACUUM`、`LOCK`、`SET` 全部拒绝。
- 允许 `WITH`，但 CTE 体内只允许 `SELECT`；CTE 内含 DML 或 `RETURNING` 一律拒绝。
- 允许 `INSERT ... VALUES` 与 `INSERT ... SELECT`；`SELECT` 子查询引用的表必须在 `write.read_tables` 内。
- 拒绝 `UPDATE ... FROM` 与 `DELETE ... USING`（多表形态在本期不开放，避免行数判定与锁范围失控）。
- `INSERT ... ON CONFLICT DO UPDATE` 归为 `upsert`：必须声明 `operation: upsert` 且带显式 `conflict_target`；按 UPDATE 计影响行数。
- `UPDATE`/`DELETE` 的目标表必须唯一，且与 `write.target_tables` 完全一致（不接受同 schema 下的别名歧义）。
- 拒绝 `RETURNING *`；只允许 `returning_columns` 中声明的列与字面量，不允许函数调用。

实现位置：新增 `backend/crates/hjzl-sqlguard/src/write.rs`，导出 `validate_write(sql, dialect, specs) -> WriteValidated`，**不改** `validate()` 的语义与返回类型（读路径继续只接受查询）。注册期的 `validate_rendered`（`template.rs:671-674`）按 `risk` 分流到新函数。

### 4.2 L9 影响范围

- `UPDATE`/`DELETE` 必须带 `WHERE`；`write.require_where` 在注册期只接受 `true`，传 `false` 即 `MCP_TEMPLATE_INVALID`。
- 退化谓词拒绝：`WHERE 1=1`、`WHERE true`、`WHERE $1 = $1` 等不引用任何目标表列的谓词 → `MCP_WRITE_REJECTED`，`reason: degenerate_where`。
- 赋值的列 ⊆ `write.write_columns`；未声明的列出现即拒绝（`reason: column_not_declared`）。
- `write.write_columns` ∩ 数据源 `write.forbidden_write_columns` 必须为空（注册期强制）；执行期再查一次，防止数据源配置收紧后能力未重审。
- 行数上限采用**事务内后检查 + 整体回滚**，不截断：
  1. 执行 DML，取 `affected_rows`；
  2. 若 `affected_rows > min(能力上限, 数据源上限)` → `ROLLBACK`，返回 `MCP_WRITE_LIMIT_EXCEEDED`，附实际行数；
  3. 否则 `COMMIT`。
  理由：`UPDATE ... LIMIT` 非标准 SQL，子查询截断需要二次排序语义且与 `RETURNING` 结果不一致；截断会让调用方在"部分成功"上继续推理，比拒绝更危险。`SET LOCAL` 级上限不可用（PG 无该 GUC），所以只能后检查；检查在提交前，数据不会真正落库。
- 执行期 `affected_rows` 与 `RETURNING` 行数不一致（触发器改写）时，以驱动返回的 `affected_rows` 为准，并记录 `data.returning_rows` 供核对。

### 4.3 L10 参数与值

- `INSERT ... VALUES` 与 `UPDATE ... SET` 的值表达式只允许下列四类，其余（字面量、子查询、任意函数）一律 `MCP_WRITE_REJECTED`：
  1. `$n` 或 `$n::type`；
  2. `DEFAULT` / `NULL`；
  3. 目标列自增：`col = col + 1` / `col = col - 1`，仅当 `col` 是 `optimistic_lock.column` 或显式声明在 `write.counter_columns`；
  4. 服务端时间：`now()` / `CURRENT_TIMESTAMP`，仅当 `col` 在 `write.server_time_columns`。
- 谓词中允许字面量（模板常量），但字面量不得与 `$n` 混用来"拼出"值。
- 所有 `$n` 必须由参数数组提供，个数严格匹配（复用 `check_placeholders`，`backend/crates/hjzl-server/src/executor.rs:175-249`）；命名占位符在渲染期已改写为 `$n`，执行期不再接受。
- 值类型按 `params[].type` 校验；`null` 必须有显式 `::cast`（复用 `MCP_NULL_PARAM_NEEDS_CAST` 规则）。
- 字符串参数按 `max_len` 截断**不允许**：超长即 `MCP_VALIDATION_ERROR`。
- 语句中出现 `nextval(...)` 视为隐式写序列，禁止；序列值由数据库默认值承担。

### 4.4 写侧函数黑名单

在读侧黑名单（`validate.rs:24-40`）之上追加：

```text
nextval, setval, currval, set_config, pg_notify, pg_cancel_backend,
pg_terminate_backend, pg_reload_conf, pg_rotate_logfile, pg_switch_wal,
lo_import, lo_export, lo_unlink, pg_read_file, pg_read_binary_file,
pg_ls_dir, dblink*, pg_sleep*, pg_advisory_lock*, current_setting
```

- 前缀族匹配保留 `dblink` / `pg_sleep` / `pg_advisory_lock`。
- `current_setting` 禁用于写侧：避免模板把会话 GUC（如 `search_path`、`role`）读回后用于后续判断。
- 黑名单是纵深防御，不是保证；L7 的账号权限才是。Postgres MCP Pro 0.3.0 的 restricted-mode 绕过（`RangeFunction` 节点未走函数名校验，可执行 `pg_read_file`）说明 AST 白名单永远可能漏项，账号权限必须独立成立。

### 4.5 施加时机

| 时机 | 做什么 | 失败后果 |
|---|---|---|
| 注册 / 发布新版本 | 用样本参数渲染 → L8/L9/L10 全量校验 → `EXPLAIN` 预检 → 记录 `write_shape_hash` | 拒绝入库（`MCP_TEMPLATE_INVALID`） |
| prepare | 用真实参数渲染 → 同上校验 → 生成摘要 | 拒绝（`MCP_WRITE_REJECTED`） |
| apply | 对渲染后 SQL **再跑一次** L8–L10（防止变体矩阵与旧版本绕过） | 拒绝，令牌作废 |

注册期校验的是"某个变体"，执行期校验的是"这一次渲染结果"；两者不可互相替代。

## 5. 事务、并发与幂等

### 5.1 原子性与提交顺序

- 一次 apply = 一个显式事务 = 一条 DML。
- 事务内顺序：`BEGIN` → `SET LOCAL statement_timeout` / `lock_timeout`（用 `set_config(..., true)`，与只读执行器一致，见 `pg_executor.rs:137-146`）→ 执行 DML → 行数后检查 → `COMMIT`。
- **先提交、后返回**：`RETURNING` 结果在内存中暂存，`COMMIT` 成功后才组装响应。提交失败时禁止返回任何行——那会让调用方以为写成功。
- 提交阶段连接中断 → `MCP_WRITE_OUTCOME_UNKNOWN`，禁止自动重试，禁止换连接重放；只能按幂等键查询或人工核对。
- `statement_timeout` 命中时事务已回滚，可判定"未写"；返回 `MCP_EXEC_TIMEOUT` 且 `data.retryable=true`（带幂等键可重试）。
- `lock_timeout` 命中同理（默认 2000ms），避免长事务持锁拖垮在线业务。

### 5.2 多语句与存储过程

- 多语句禁止（L8）。
- 存储过程/函数调用禁止（`CALL` 在 L8 拒绝；DML 内调用函数受 L10 与黑名单约束）。
- 触发器副作用不可由本服务判定：写账号只授予目标表 DML，触发器的跨表写入由 DBA 在建设写账号时确认（D7）；能力注册必须记录 `write.trigger_reviewed_by`。

### 5.3 幂等键

```sql
CREATE TABLE write_idempotency (
  tenant TEXT NOT NULL,
  actor_id TEXT NOT NULL,
  capability_id TEXT NOT NULL,
  key_hash TEXT NOT NULL,        -- SHA-256(idempotency_key)
  params_hash TEXT NOT NULL,
  intent_id TEXT NOT NULL,
  state TEXT NOT NULL,           -- pending | applied | failed | unknown
  result_json TEXT NOT NULL DEFAULT '',
  created_at TEXT NOT NULL,
  expires_at TEXT NOT NULL,
  PRIMARY KEY (tenant, actor_id, capability_id, key_hash)
);
```

- 作用域含 `actor_id`：不同主体用同一字符串不会互相读到结果。
- 键**不含能力版本**：版本升级后同键同参重放仍返回首次结果，避免"升级后重试导致重复写"；参数变化仍按 `params_hash` 拒绝。
- 语义：
  - 不存在 → 插入 `pending`，继续执行；提交后更新为 `applied` + 结果。
  - 存在且 `params_hash` 相同且 `state=applied` → 直接返回首次结果，`replayed:true`，不再执行 DML；本次令牌仍按 §2.2 消费（单次使用，避免令牌复用）。
  - 存在且 `params_hash` 不同 → `MCP_IDEMPOTENCY_CONFLICT`。
  - 存在且 `state=pending` 且未超过 `statement_timeout + lock_timeout` → `MCP_IDEMPOTENCY_CONFLICT`，`reason: in_flight`，`retryable: true`（等首次结束后重试，不排队）。
  - 存在且 `state=pending` 且已超时 → 转为 `unknown`，返回 `MCP_WRITE_OUTCOME_UNKNOWN`。
- TTL 24h（可配 1–168h）；到期清理任务删除，不参与哈希链。
- `idempotency: natural` 的能力不要求幂等键：仅当能力是"幂等赋值"（同参数重复执行结果一致，如 `SET status` 且带乐观锁）时允许；注册期由审核人确认。

### 5.4 乐观锁

- 能力声明 `optimistic_lock: {column, param}` 后，执行期校验渲染 SQL 中存在 `column = $n`（`$n` 对应声明参数），否则拒绝。
- `affected_rows = 0` 且声明了乐观锁 → `MCP_ROW_VERSION_MISMATCH`。**不**自动重读、不自动重试：重试会变成"无条件写入"。
- `affected_rows > 1` 对声明乐观锁的能力视为异常 → 回滚 + `MCP_WRITE_LIMIT_EXCEEDED`。

### 5.5 重复提交检测

- 窗口 60s，键 = `(tenant, actor_id, capability_id, version, params_hash, session_id)`。
- 窗口内存在未消费令牌 → prepare 返回同一令牌，`summary.dedupe: "reused"`，不新建意图。
- 窗口内已消费且同幂等键 → 返回重放结果（§5.3）。
- 窗口内已消费且无幂等键（`idempotency: natural`）→ 允许新令牌，但 `summary.duplicate_suspect: true` 并计入指标。

### 5.6 并发上限

每个数据源写事务并发 ≤ 2（可配 1–4），用信号量实现；拿不到许可立即返回 `MCP_WRITE_QUOTA_EXCEEDED`（`scope: "concurrency"`），**不排队**：排队的写会与审批时看到的行版本脱节。

## 6. 数据源与账号

### 6.1 配置

```toml
[[datasources]]
name = "core-pg"
dsn_env = "HJZL_DS_CORE_RO_DSN"          # 只读账号
write_dsn_env = "HJZL_DS_CORE_RW_DSN"    # 写账号；为空表示不允许写
engine = "postgres"
readonly = true                          # 只描述只读连接：DSN 锁必须生效
allowed_schemas = ["public"]
statement_timeout_ms = 15000

[datasources.write]
enabled = false
allowed_schemas = ["public"]
statement_timeout_ms = 5000
lock_timeout_ms = 2000
max_affected_rows = 100
max_write_per_minute = 30
max_rows_per_day = 50000
max_concurrent_transactions = 2
forbidden_write_columns = ["user_phone", "id_card", "password_hash", "role", "is_deleted"]
write_probe = { schema = "public", table = "__hjzl_write_probe" }
```

校验规则（扩展 `DatasourceRef::validate`，`backend/crates/hjzl-core/src/config.rs:37-63`）：

- `write_dsn_env` 非空时禁止包含 `=`/`://`/`/`（与 `dsn_env` 同规则）。
- `write_dsn_env != dsn_env`：同一环境变量意味着只读锁与写权限共用凭证，直接拒绝启动。
- `statement_timeout_ms`（写）≤ 数据源只读值，且 ≤ 30000；`lock_timeout_ms` < `statement_timeout_ms`。
- `write.enabled=true` 但 `write_dsn_env` 为空 → 配置错误。
- `primary_datasource()`（`config.rs:364-370`）继续只选只读数据源；写数据源按 `name` 解析，不复用该函数。

### 6.2 写账号启动自检（L7）

启用写能力的数据源在启动时连接写 DSN 并按序检查，任一失败即拒绝启动（fail-closed）：

| 编号 | 检查 | 期望 | 失败含义 |
|---|---|---|---|
| W1 | `SHOW default_transaction_read_only` / `transaction_read_only` | `off` | 写连接被误配成只读 |
| W2 | `SELECT rolsuper, rolcreatedb, rolcreaterole FROM pg_roles WHERE rolname = current_user` | 全 `false` | 账号过大 |
| W3 | `SELECT has_schema_privilege(current_user, $1, 'CREATE')` for each `write.allowed_schemas` | `false` | 可建表/可 DDL |
| W4 | `SELECT has_table_privilege(current_user, $probe, 'INSERT,UPDATE,DELETE')` | `true` | 写账号连探针表都写不了 |
| W5 | 事务内 `INSERT` → `UPDATE` → `DELETE` 探针行，`affected_rows` 均为 1，然后 `ROLLBACK` | 全部成功 | 写链路不可用 |
| W6 | 事务内 `CREATE TABLE <probe_schema>.__hjzl_ddl_probe(x int)` | 必须失败（`42501`） | 账号有 DDL 权限 |
| W7 | `SELECT count(*) FROM pg_trigger WHERE tgrelid IN (target_tables...) AND NOT tgisinternal` | 结果与注册时 `trigger_reviewed` 一致（仅告警，不阻断） | 目标表触发器变更需重审 |

只读自检（`pg_executor.rs:90-118`）保持不变：只读账号下 `CREATE TEMP TABLE` 必须失败。两套自检针对两套凭证，互不替代。

### 6.3 连接池与超时

- 写池与读池完全独立：`max_connections = max_concurrent_transactions + 1`，`acquire_timeout = 1000ms`。
- 写池 DSN **不得**经过 `readonly_dsn()`（`pg_executor.rs:22-25`）；该函数会追加只读锁参数，写池复用它会导致写全部失败，或诱导开发者删掉锁参数而污染读池。
- 连接建立后固定 `search_path`（按 `write.allowed_schemas` 顺序），禁止运行期 `SET ROLE`。
- `statement_timeout_ms` 按能力 → 数据源 → 默认 5000 取最小值；`lock_timeout_ms` 固定来自数据源配置，能力不可放大。

### 6.4 方言边界

阶段五只开放 PostgreSQL。MySQL 的 `affected_rows` 默认是"变化行数"而非"匹配行数"（需 `CLIENT_FOUND_ROWS`），且无 `RETURNING`；ClickHouse 无事务、mutation 异步，无法满足"提交前检查行数"。两者在写路径进入前必须补齐单独的能力矩阵，不在本文件承诺。

## 7. 审计

### 7.1 事件

在 `EVENT_TYPES`（`backend/crates/hjzl-core/src/audit.rs:12-21`）中新增三个类型；`admin_change` 的必填字段规则不变（`audit.rs:22`）。

| 事件 | 时机 | 关键字段 |
|---|---|---|
| `write_intent` | prepare 校验通过、令牌落库前 | `intent_id, capability_id, kind, version, risk, params_hash, param_names, params_redacted, datasource, schemas, tables, operation, columns, predicate, affected_rows_estimate, preview, token_id, requester{actor_id, role, session_id}, approval_required, expires_at` |
| `write_apply` | 提交成功后 | `intent_id, token_id, approval_id, approver, approver_role, affected_rows, returning_rows, elapsed_ms, statement_hash, replayed, datasource` |
| `write_failed` | 任意失败或未知 | `intent_id, token_id, error_code, reason, phase ∈ {prepare, execute, commit}, affected_rows, rolled_back, elapsed_ms, retryable` |

- `param_names` 只记列名与类型；`params_redacted` 是对"哪些参数被脱敏"的布尔映射，**不含值**。
- 提交结果未知时 `write_failed` 必须带 `phase: "commit"`、`retryable: false`。
- `write_intent` 与 `write_apply` 必须能按 `intent_id` 一一对应，除此之外不允许出现"有 apply 无 intent"的记录；每日链校验作业（`audit.rs:174-195`）之外增加这条业务一致性检查。

### 7.2 谁批准

`write_apply` 记录 `approval{id, approver_id, approver_role, approved_at, four_eyes: true, dba_signed: bool}`；批准人身份只取 admin token 派生值，不采信请求体（沿用 `backend/crates/hjzl-server/src/admin.rs:498-520` 的做法）。

### 7.3 参数值

- 审计表只存哈希与列名（与只读路径一致：日志/审计不含参数明文）。
- 审批视图需要看到"要改什么"，因此 `write_intents` 表额外存 `params_ciphertext`（AES-GCM，密钥来自 `HJZL_MCP_WRITE_PARAM_KEY`），访问审批详情时解密，访问行为写 `policy_decision` 审计。
- 留存：意图完成后 7 天删除密文（可配 1–30 天），哈希与事件永久保留在审计链上。密钥轮换与留存期属 D11。

## 8. 审批与授权

| 项 | 规则 |
|---|---|
| 谁能写 | 角色 allowlist `policy.write_roles`；角色必须同时具备组可见性与 `callable` |
| 谁能批 | `policy.approver_roles`；且与提交人不同（四眼） |
| 四眼判定点 | apply 前重新判定一次（不只是 prepare 时）；批准人从 token 派生 |
| DBA 二次签核 | `write.require_dba_signoff=true` 时需 `policy.dba_roles` 中另一人批准；默认关（D12） |
| 批准有效期 | 3600s（可配 300–86400s）；过期即 `MCP_WRITE_APPROVAL_REQUIRED` |
| 一次批准覆盖 | `calls_per_approval`，默认 1；`sensitive` 固定 1 |
| 时间窗 | `write.window = "Mon-Fri 09:00-19:00 Asia/Shanghai"`，窗外 `MCP_WRITE_DISABLED reason=outside_window` |
| 自助低风险 | 仅 `env=staging` ∧ `risk=write` ∧ 单表主键定位 ∧ `max_affected_rows=1` 时允许策略自动批准；生产永不自动批 |
| 撤销优先 | 组/角色/能力在 prepare 与 apply 之间被撤，apply 立即失败；令牌不构成授权 |
| 写权限撤销后的清理 | 允许执行声明为 `cleanup` 的能力（用于回滚本次变更），其余写继续拒绝 |

## 9. 开关矩阵

任一级未开即拒绝，全部默认关：

| 级别 | 配置键 | 默认 | 关闭时行为 |
|---|---|---|---|
| 全局 | `HJZL_MCP_WRITE_ENABLED` / `settings.write_enabled` | `false` | 写能力不进入可见集；`draft` 仍可入库审核 |
| 租户 | `policy.write_enabled` | `false` | 同上，`level=tenant` |
| 组 | `[[groups]] write_enabled` | `false` | 该组写能力不可发现、不可调用 |
| 数据源 | `[datasources.write] enabled` | `false` | 能力可见，调用即 `MCP_WRITE_DISABLED level=datasource`（写 DSN 不加载） |
| 能力 | `write.enabled` | `false` | 该能力不进入可见集 |
| 角色 | `policy.write_roles` | 空 | 能力可见，调用即 `MCP_WRITE_DISABLED level=role` |
| 会话 | `initialize._meta.mcp.write` | 继承角色 | 仅可关闭（`false`）；关闭后调用 `MCP_WRITE_DISABLED level=session`；传 `true` 不能提权（§4.1.1 只做收敛） |

生效顺序：全局 → 租户 → 组 → 数据源 → 能力 → 角色 → 会话 → 审批状态 → 配额。全局/租户/组/能力决定"是否可见"，关闭即 not-found（不泄露写能力存在性）；数据源/角色/会话决定"可见但能否执行"，关闭即 `MCP_WRITE_DISABLED`。全部为逻辑与；任一读取失败（配置缺失、注册表不可用）按关闭处理。`admin` 类语句（TRUNCATE/DDL/GRANT）在注册期即拒绝（`MCP_TEMPLATE_INVALID`），运行期出现按 `MCP_WRITE_REJECTED` 拒绝。

## 10. 限流与配额

| 配额 | 默认 | 码 | 说明 |
|---|---|---|---|
| 租户笔数 | 60/min | `MCP_WRITE_QUOTA_EXCEEDED` `scope=tenant` | 令牌桶，按 prepare 计 |
| 会话笔数 | 10/min，50/会话 | 同上 `scope=session` | 防单会话失控 |
| 能力笔数 | 20/min | 同上 `scope=capability` | 热能力保护 |
| 数据源笔数 | 30/min | 同上 `scope=datasource` | 与 DB 承载能力挂钩 |
| 日累计影响行数 | 50000/租户 | 同上 `scope=rows_per_day` | 按 `write_apply.affected_rows` 累计 |
| 并发写事务 | 2/数据源 | 同上 `scope=concurrency` | 信号量，不排队 |

拒绝优先于排队；`retry_after_ms` 给出建议退避。配额计数在 prepare 时预占，失败或过期归还。

## 11. 失败与重试

| 分类 | 例子 | 是否已写 | 重试 | 码 |
|---|---|---|---|---|
| 前置拒绝 | 开关、授权、参数、形状、配额 | 否 | 修正后再来 | `MCP_WRITE_DISABLED` / `MCP_WRITE_REJECTED` / `MCP_WRITE_QUOTA_EXCEEDED` |
| 令牌/审批 | 过期、重放、参数变化、未批 | 否 | 重新 prepare / 等审批 | `MCP_CONFIRM_TOKEN_INVALID` / `MCP_WRITE_APPROVAL_REQUIRED` |
| 事务内失败 | 约束冲突、死锁、`lock_timeout`、`statement_timeout` | 否（已回滚） | 带幂等键可重试（死锁/超时） | `MCP_EXEC_TIMEOUT` / 驱动错误 |
| 行数超限 | `affected_rows > max` | 否（回滚） | 修正参数或调小范围 | `MCP_WRITE_LIMIT_EXCEEDED` |
| 乐观锁未命中 | `affected_rows=0` | 否 | 重读后重新 prepare | `MCP_ROW_VERSION_MISMATCH` |
| 提交未知 | 提交阶段断连/超时 | **未知** | **禁止重试** | `MCP_WRITE_OUTCOME_UNKNOWN` |
| 提交成功但响应丢失 | 调用方超时 | 是 | 用幂等键查询 | 重放返回 `replayed:true` |
| 外部系统写 | HTTP 5xx / 超时 | 视 `idempotent` | 仅幂等键 + `idempotent=true` 重试 1 次 | 统一 envelope `ok:false` |

- 回滚：提交后不存在回滚；对外表述必须是"补偿"而不是"回滚"。
- 补偿边界：仅当**一个能力只写一个系统**时才能定义补偿动作；不实现跨系统两阶段提交（2PC）。需要业务一致性时，由目标服务在其本地事务内写 outbox，本服务只负责把写请求送达并记录结果。
- 本服务自身不建 outbox：它不拥有业务库；若未来需要"先落本地再转发"，那是新的执行器类型，需回写 §3.3 契约。

## 12. 管理台

| 页面 | 内容 | 依赖接口 |
|---|---|---|
| 写意图审核 | 队列：意图摘要、参数（解密视图）、影响行数估计、目标表/列、申请人与时间；操作：批准 / 拒绝 / 转 DBA 签核 | `GET /admin/write-intents`、`POST /admin/write-intents/{id}/approve`、`.../reject` |
| 意图详情 | 状态机时间线、令牌状态、apply 结果、`affected_rows`、错误码、审计事件链接 | `GET /admin/write-intents/{id}` |
| 能力审核 | 写能力新增字段 diff（`write_columns`、`max_affected_rows`、`approval.mode`）；高风险变更强制重审 | 现有 `GET /admin/capabilities` 扩展字段 |
| 审计追溯 | 按 `intent_id` 串起 `write_intent → write_apply/write_failed` | 现有 `GET /admin/audit-events` + `intent_id` 过滤 |

- UI 仍不直连库，全部经 admin API（`backend/crates/hjzl-server/src/admin.rs:52-67`）。
- 批准动作必须带 `approval_id`，批准人取自 token；请求体自述只做一致性校验。
- 上线时同步替换三处「未接通」文案（`admin-web/src/components/PageShell.tsx:77-86`、`Sidebar.tsx:69`、`pages/ReviewQueuePage.tsx:60`）；在管理台接口可用之前，文案保持现状，不得提前宣称已接通。

## 13. 指标与验收口径

| 指标 | 口径 | 目标 |
|---|---|---|
| 写成功率 | `write_apply{outcome=ok} / (ok + failed)`，剔除 `replayed` 与前置拒绝 | ≥99%（不含调用方参数错误） |
| 拒绝原因分布 | `write_failed` 按 `error_code` + `reason` 分组 / 全部 `write_intent` | 无单一原因 >40%（参数错误除外） |
| affected_rows 分布 | 按能力分桶 p50/p95/p99/max；>阈值（默认 10× 能力均值）单独告警 | p99 不超过能力上限；7 日内 p95 上升 >20% 告警 |
| prepare→apply 比率 | 已消费令牌数 / 新建意图数（按能力、7 日滚动） | 0.5–0.95；<0.3 视为模型误用或审批阻塞 |
| prepare→apply 延迟 | 拆分 `approval_wait_ms` 与 `exec_wait_ms`，取 p95 | 审批 p95 ≤ 4h；执行 p95 ≤ 500ms |
| 令牌异常 | 重放/过期/跨会话计数 | 重放攻击类 = 0；过期占比 <5% |
| 幂等重放率 | `replayed=true / apply 总数` | 5%–20%（反映客户端重试行为） |
| 误写率 | 24h 内被人工修正/补偿的意图数 ÷ apply 成功数 | <0.1% |
| 四眼违规 | 同人批准计数 | 0 |
| 审计一致性 | 有 apply 无 intent 的记录数 | 0 |

误写率的数据源：管理台"标记误写"操作 + 补偿能力执行记录；两者都必须关联 `intent_id`，否则不计入分子。

## 14. 与既有契约的冲突点

下表列出写路径与既有契约的张力点、冲突原因与定稿处理（已落在对应文档/代码约束中）；实现时以"定稿处理"列为准。

| # | 契约点 | 冲突 | 定稿处理 |
|---|---|---|---|
| C1 | preview §1.3 非目标 | 「本期不注册写能力」会让阶段五无从开工 | 限定为「阶段一–四不注册；阶段五按 §4.4.6 开放，默认关」 |
| C2 | preview §5.1 预算预留 | 若 prepare/apply 是独立工具，需加常驻 600 | 不新增常驻工具；两个入口各加 3 个入参，描述增量按实测复核，峰值上界不变 |
| C3 | preview §4.4.6 调用期确认 | 独立 `prepare_call` / `apply_call` 与 §2 原则 9「每 kind 唯一调用面」冲突 | 改为同一入口的 `phase`；`prepare` 无副作用，不构成第二出口 |
| C4 | preview §3.3 注解段 | 入口工具统一 `readOnlyHint=true`，写开启后注解失真 | 注解按会话投影；能力级注解由 `describe_capability` 返回（§1.3） |
| C5 | preview §4.3.1 L1「唯一强只读保证」 | 写面没有对应的强保证层 | 保留 L1–L6 读侧；新增 L7–L12 写侧，L7 为写面唯一强保证（§4） |
| C6 | preview §7 启动自检 | 只检查只读账号写失败 | 并列增加写账号自检 W1–W7（§6.2），任一失败拒绝启动 |
| C7 | preview §8 阶段五 | 「启用写路径须重算常驻预算」未说明结论 | 明确「不新增常驻工具；启用时复核入参与描述增量并附实测」 |
| C8 | `config.rs:18-20` 的 `readonly` | 与写能力语义纠缠，易被读成「只读数据源不能写」 | `readonly` 只描述只读连接；写面由 `write_dsn_env` + `write.enabled` 独立控制 |
| C9 | `template.rs:250-254` 注册期拒绝 `risk != read` | 写模板无法入库 | 按部署开关判定：`write_enabled=false` 时维持拒绝；开启后放行 `risk=write`，`sensitive` 另需开关 |
| C10 | `admin-web` 三处「未接通」文案 | 阶段五上线后过期 | 与 W5.7 同步替换；接口未可用前保持现状 |
| C11 | `audit.rs:12-21` 事件闭集 | 写事件无法写入 | 追加 `write_intent` / `write_apply` / `write_failed` |
| C12 | preview §9 指标表 | 只有读侧与治理指标 | 追加 §13 的写侧指标行 |
| C13 | preview §6.3 失败路径表 | 缺写侧失败分类 | 追加写侧错误码行（§2.5、§11） |

不新增协议语义、不新增 MCP 方法；`write_*` 是服务端内部事件与错误码，不改变 JSON-RPC 结构。

## 15. 待决策（D4–D12）

| # | 决策 | 建议默认 | 不决策的后果 |
|---|---|---|---|
| D4 | 写权限授予方式：角色 allowlist 由谁维护、是否需要申请表 | 角色 allowlist 落在 `policy.toml` + admin API，按组授权，不按人 | 只能按人开白名单，离职/转岗后权限长期残留，审计无法回答"谁被授权" |
| D5 | 是否需要工单/变更单联动（如生产写必须挂变更单号） | 生产写要求 `change_ticket` 参数，格式由平台定；staging 不需要 | 生产写无法与变更管理对账，事后追责只能靠审计时间线推测 |
| D6 | 审计保留期：`write_*` 与 `admin_change` 是否同为 7 年 | 7 年，与 `admin_change` 一致；密文参数 7 天 | 保留期过短会在合规审计时缺证；过长则存储与密钥管理成本无人认领 |
| D7 | 写账号来源：谁创建、如何轮换、是否独立实例 | 平台 DBA 创建，按数据源一账号，90 天轮换；高价值库独立实例 | 写账号可能被复用为通用账号；轮换无 owner 时只能等事故驱动 |
| D8 | 允许写的目标系统清单（数据源/API 白名单） | 首批仅内部订单库 staging + 一个只写 API；清单入配置 | 任意注册的写能力都可能指向未评估系统，等于没有边界 |
| D9 | PII 列写保护：黑名单由谁定、是否允许能力级豁免 | 数据源级硬黑名单不可豁免；能力级只能加严 | 敏感列被写能力覆盖后，只读侧的列黑名单形同虚设 |
| D10 | 生产写是否首批开放 | 不开放；阶段五只到 staging 与内测租户 | 生产写与审批组织未就绪时上线，等于把误写风险直接暴露给业务 |
| D11 | 审批视图参数留存：加密方式、密钥归属与留存期 | AES-GCM，密钥在 KMS/环境变量，留存 7 天 | 不留存则审批人看不到要改什么，只能盲批；长期明文留存则审计面变成 PII 仓库 |
| D12 | 二次签核责任人：是否需要独立 DBA 角色与 SLA | 需要 `dba_roles`，SLA 4h；默认关闭 | 高风险写只有单人批准，四眼退化为形式 |

## 16. 代码落点

| 位置 | 改动 |
|---|---|
| `backend/crates/hjzl-sqlguard/src/write.rs`（新增） | `WriteOp`、`validate_write`、列/函数/表白名单、`RETURNING` 校验、退化 WHERE 判定 |
| `backend/crates/hjzl-sqlguard/src/lib.rs` | 导出写校验；`validate()` 语义不动 |
| `backend/crates/hjzl-core/src/template.rs` | `TemplateDef.write` 结构；注册期按 `risk` 分流校验；`PreparedQuery` 增加 `write_shape_hash` |
| `backend/crates/hjzl-core/src/config.rs` | `DatasourceRef.write` 段与校验；`Settings.write_enabled` |
| `backend/crates/hjzl-core/src/registry.rs` | `write_tokens`、`write_intents`、`write_idempotency` 三表与状态迁移；批准/吊销 |
| `backend/crates/hjzl-core/src/audit.rs` | 三个事件类型入闭集 |
| `backend/crates/hjzl-core/src/errors.rs` | 8 个新错误码与默认文案 |
| `backend/crates/hjzl-server/src/mcp.rs` | `phase` / `confirm_token` / `idempotency_key` 解析；注解按会话投影；写分发 |
| `backend/crates/hjzl-server/src/pg_write.rs`（新增） | 写池、自检 W1–W7、单语句事务、行数后检查、提交未知处理 |
| `backend/crates/hjzl-server/src/admin.rs` | 写意图列表/详情/批准/拒绝；`intent_id` 审计过滤 |
| `backend/crates/hjzl-server/tests/write_*.rs`（新增） | 形状矩阵、令牌、四眼、幂等、超限回滚、提交未知 |
| `admin-web/src/pages/WriteIntentPage.tsx`（新增） | 审核与详情页；替换「未接通」文案 |
