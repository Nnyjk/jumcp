# 审计开放化：当前契约

本文件记录**当前实现**的审计事件契约、管理面接口字段与统计口径。字段名与取值以代码为准；库中旧格式事件按「旧行边界」处理，不参与本契约。

## 1. 事件信封

```
{ event_id, ts, tenant, actor{type,id,role}, session_id,
  event_type, payload, prev_hash, hash, surface }
```

| 字段 | 说明 |
|---|---|
| `event_id` | 32 位无连字符 UUID，追加时生成 |
| `ts` | RFC3339，UTC，纳秒或微秒小数位；管理台按本地时区显示到毫秒并标注 UTC 偏移，同一秒内的事件可据此排序 |
| `actor` | `type`∈{`admin`,`agent`}，`id` 为凭证派生主体（admin 由 `JUMCP_ADMIN_TOKENS` 派生，agent 由数据库 access token 的 `caller` 派生），`role` 为角色名。可选键 `token_id`（数据库 token id）与 `source`（MCP token 配置的 `source` / 管理面固定 `admin-api`）只在有值时出现；stdio 与管理面静态 token 不写 `token_id`。未认证失败会留下 `id` 为空串的记录 |
| `event_type` | 闭集：`tools_list_served`、`capability_search`、`capability_describe`、`tool_call`、`template_run`、`policy_decision`、`admin_change` |
| `prev_hash` / `hash` | 哈希链；`hash = sha256(canonical(envelope 去 hash) + prev_hash)`，SHA-256 64 位小写 hex。首条 `prev_hash` 为空串 |
| `surface` | 事件来自哪个面：`admin` / `mcp`（旧行为空串）。**由 `actor.type` 派生**（`admin` → `admin`，其余 → `mcp`），不是调用点传参；**不参与哈希**，因此加列/改口径都不会让历史链失效 |

`session_id` 只存在于信封与数据库列，不进入业务载荷；当前实现固定为 `default`（旧格式事件该值为空串）。

## 2. payload 的 14 个键

所有 `event_type` 共用同一骨架，缺省值一律 `null` / `[]` / `{}`：

| 键 | 类型 | 语义 |
|---|---|---|
| `schema_version` | int，固定 `1` | **载荷结构版本**，只随 14 键骨架变化 |
| `object_type` | string | 事件作用的对象类别（见 §3） |
| `object_id` | string | 对象标识：能力 id / 组名 / 工具名 / 模板 id；`catalog` 类事件的 `object_id` 为 `catalog`（检索）或 `tools`（下发）；策略拒绝无法判定对象时为 `request` |
| `action` | string | 发生的动作（见 §3）。字符串，无代码级闭集校验，但 `admin_change` 要求非空 |
| `outcome` | string | `ok` / `error` / `denied` / `summary` |
| `error_code` | string \| null | 失败或拒绝时的错误码（如 `MCP_CAPABILITY_NOT_FOUND`） |
| `error_message` | string \| null | 人类可读错误信息 |
| `from_status` | string \| null | 生命周期迁移前状态；无状态对象为 `null` |
| `to_status` | string \| null | 迁移后状态 |
| `changes` | array | 字段级真实前后值 `[{field, before, after}]`；无变化为 `[]` |
| `approval` | object \| null | 外部审批引用（见 §5） |
| `reason` | string \| null | 退回等动作的说明 |
| `correlation_id` | string \| null | 关联事件 id：`policy_decision` 指向同一次失败先写入的 `tool_call.event_id` |
| `data` | object | 事件专属明细（入参、脚本、token、哈希、版本等） |

### 2.1 `schema_version` 与 `data.version`

- `schema_version`：**载荷结构**版本，固定在 payload 顶层，当前恒为 `1`。它与能力/模板自身版本无关，也不随业务版本变化。
- `data.version`：**业务版本**，即能力/模板的 `version`（注册表版本号）。每次定义修订或发布时 +1。
- 命名按用途区分：结构版本叫 `schema_version`，业务版本叫 `data.version`；不要用前者判断业务版本。

### 2.2 `admin_change` 的必填约束

`admin_change` 写入时强制 `object_type`、`object_id`、`action`、`outcome` 四者非空，缺失直接拒绝写入（不会落一条半成品事件）。

### 2.3 `changes` 的形态

- 值一律是**真实前后值**：新增字段 `before=null`，删除字段 `after=null`，不做字符串化。
- 能力定义修订（`create` / `update_definition`）：按 `definition` **递归展开对象为点路径**生成，例如 `template.description`、`template.constraints.auto_limit`；数组与标量是叶子，给完整 `before`/`after`。新增字段 `before=null`，删除字段 `after=null`。递归深度上限 6 段，超过上限的子树整体作为一行（`field` 为当前点路径）。`create` 相当于与 `{}` 比较，同样逐字段展开，不输出整份 `template` 对象。
- 组变更：固定字段 `priority`、`min_budget`、`roles`。
- 状态迁移：`[{field:"status", before:"draft", after:"review"}]`。
- 可见性：`discoverable` / `callable` 两项。
- 下架：`status`、`successor_id`、`discoverable`、`callable`。

### 2.4 `data` 的键随 action 变化

| action | `data` 键 |
|---|---|
| `create` | `definition_hash_before`(null)、`definition_hash_after`、`version` |
| `submit` | `definition_hash_before`、`definition_hash_after`、`version`、`submitted_by` |
| `publish` | `definition_hash_before`、`definition_hash_after`、`version` |
| `update_definition` | `definition_hash_before`、`definition_hash_after`、`version` |
| `visibility` | `definition_hash_before`、`definition_hash_after`、`version` |
| `deprecate` | `definition_hash_before`、`definition_hash_after`、`version` |
| `reject` | `definition_hash_before`、`definition_hash_after`、`version` |
| `group_create` / `group_update` / `group_delete` | `group{name,priority,min_budget,roles}` |
| `create` / `update` / `delete`（`object_type="upstream"`） | `transport`、`group`、`category`、`read_only`、`enabled`；由内置包安装时另含 `catalog_id`、`catalog_version` |
| `refresh`（`object_type="upstream"`） | `transport`、`group`、`category`、`read_only`、`tool_count`、`status`；失败时事件顶层另有 `error_code`/`error_message` |
| `update_tool_groups`（`object_type="upstream"`） | `basic_total` |
| `token_create` | `name`、`role`、`groups`、`builtin_tools`、`source`、`caller`、`expires_at`(null\|string)、`token_prefix`；**绝不含明文 token 或 hash** |
| `token_revoke` | `name`、`role`、`groups`、`source`、`caller`、`token_prefix`、`revoked_at` |
| `token_reissue` | `name`、`role`、`groups`、`builtin_tools`、`source`、`caller`、`token_prefix`（新前缀）；**绝不含明文 token 或 hash** |
| `token_delete` | `name`、`role`、`groups`、`builtin_tools`、`source`、`caller`、`token_prefix`、`status`、`created_at`、`expires_at`(null\|string)、`revoked_at`(null\|string)；**绝不含明文 token 或 hash** |
| `invoke`（tool_call / template_run） | `arguments`、`tokens`、`cache`、`elapsed_ms`、`version`、`row_count`、`truncated`、`summary`、`executed_script`、`sql`、`params`/`bound_params`、`sql_hash`、`sql_bytes`、`params_hash`、错误上下文等，按工具实际返回 |
| `describe` | `schema_tokens` |
| `search` / `list_tools` | 检索词与下发工具列表、常驻预算等 |
| `deny` | `detail{…}`（拒绝上下文，键随原因变化） |

定义哈希三处同源：`definition_hash = sha256(canonical_json(definition))`；`data.definition_hash_before/after` 即变更前后的该值。不涉及定义改写的事件两者相等。

## 3. `object_type` 与 `action`

`object_type` 闭集：`capability`、`group`、`tool`、`template`、`catalog`、`request`、`token`、`upstream`。

| object_type | 典型 action |
|---|---|
| `capability` | `create`、`submit`、`reject`、`publish`、`update_definition`、`visibility`、`deprecate`、`describe` |
| `group` | `group_create`、`group_update`、`group_delete` |
| `token` | `token_create`、`token_revoke`、`token_delete` |
| `tool` | `invoke`（内置工具调用，如 `search_capabilities`） |
| `template` | `invoke`（模板执行） |
| `catalog` | `search`、`list_tools` |
| `request` | `deny`（策略拒绝，`object_id` 为被拒工具/模板/能力 id，均不可判定时用 `request`） |
| `upstream` | `create`、`update`、`delete`、`refresh`、`update_tool_groups`（`object_id` 为上游 id） |

`upstream` 的 `changes` 只含真实变化的字段（env/headers 只输出键与脱敏值，`changed_keys`
指出真正变了的键）；`update_tool_groups` 的每条 `changes` 是
`{field:"tool_group", tool:<原始工具名>, before:<原组或 null>, after:<"basic" 或 null>}`。
上游调用本身不走 `admin_change`：它是 `object_type="tool"`、`action="call"` 的 `tool_call`，
`object_id` 为暴露名 `<server_id>__<tool>`。

`action` 与 `object_type` 的组合决定 `changes`/`data` 的形状，消费方应按 `action` 分支。

## 4. 关联：`correlation_id`

执行前被拒绝的调用会写两条事件，顺序固定：

1. 先写 `tool_call`（或 `template_run`）记录该次失败，`correlation_id=null`；
2. 再写 `policy_decision`，其 `correlation_id` 等于上一条的 `event_id`。

`policy_decision` 的 `object_type` 为 `request`、`action=deny`、`outcome=denied`，`object_id` 依次尝试 `detail.object_id` → `detail.tool` → `detail.capability_id` → `detail.template_id` → `detail.filter`，都没有则 `request`。无前置调用的拒绝（如未认证请求）`correlation_id` 为 `null`。

## 5. `approval` 语义

```
approval: { id, source, verified, submitted_by, approved_by, approved_at } | null
```

- `publish` 与已发布定义的 `update_definition` 携带 `approval`；草稿阶段的 `update_definition`、`create`/`submit`/`reject`/`visibility`/`deprecate` 为 `null`。
- `source` 恒为 `"external"`：本系统没有接入外部审批系统，只留痕。
- `verified` 恒为 `false`：单号真伪不由本系统校验，如实标注。
- `id` 来自发布或已发布定义修订请求体的 `approval_id`，格式不受约束。
- `submitted_by` 取提审时的鉴权派生身份；`approved_by` 取发布或定义修订请求的鉴权派生身份（表单不能自述批准人）。两者如实记录，当前不限制二者相同。

## 6. 审计查询

`GET /admin/audit-events` 与 MCP `admin_audit_queries` 共用同一套过滤与分页：

```
响应: { events[], total, limit, offset, order }
参数: tenant, event_type, event_id, object_type, object_id,
      actor_id, actor_role, token_id, ts_after, ts_before, surface,
      limit, offset, order
```

- `order`∈{`asc`,`desc`}，**默认 `desc`（最新优先）**；响应回显生效值。
- `limit` 默认 50、上限 200（越界夹紧）；`offset` 最小 0。
- `object_type` / `object_id` 由 `json_extract(payload, …)` 匹配，因此**只命中新格式事件**（旧行没有这些键）。
- `token_id` 由 `json_extract(actor_json, '$.token_id')` 匹配，同样只命中带 token 来源的新事件；旧事件与管理面静态 token 事件不被命中。
- `surface` 按列匹配（`admin` / `mcp`，也接受多值）。旧行为的空串只有显式传 `surface=` 空值时才命中——它是历史遗留，不是第三个面。
- `event_type` 不做闭集校验，未知类型返回空集。
- 非法过滤键或非法 `order` 返回 `MCP_FILTER_UNSUPPORTED` / `MCP_BAD_REQUEST`，并写一条 `policy_decision`。

### 6.1 统计口径：平台操作不计入业务统计

管理台每次打开看板都会产生 `tool_call` / `admin_change`，若算进排行，"活跃度"就变成自证。
因此统计类接口**一律排除 `surface="admin"`**：

| 接口 | 口径 |
|---|---|
| `GET /admin/tools/stats`（工作台/工具排行/失败榜） | 只统计 `surface="mcp"` 的 `tool_call` 与 `template_run`；内置工具的台账行仍在（`calls=0`） |
| `GET /admin/tools/{id}/calls`（调用明细） | 同上，排除平台面 |
| `GET /admin/audit-events`（审计查询） | **不排除**：管理面事件照常返回，用 `surface` 参数区分 |

被排除的事件在审计里仍可查——"不计入统计"不等于"不可见"，这是可复核的前提。
`surface_not` 是内部过滤键（统计用它排除平台面），未列入对外支持的过滤键。

## 7. 管理面接口

`/health` 与 `/admin/login` 免鉴权；其余 `/admin/*` 接受管理台会话 cookie（`POST /admin/login` 下发，
cookie 优先）或 `Authorization: Bearer <JUMCP_ADMIN_TOKENS 中的 token>`，两者都没有/无效返回 401 `MCP_UNAUTHORIZED`；
cookie 身份的非 GET 请求还要求同源 `Origin`/`Referer`，否则 403 `MCP_FORBIDDEN`（契约见 `docs/mcp-router.md` §9.0–9.1）。

### 7.1 `GET /admin/me`

```
{ id, role, type }
```

即当前 token 派生主体，与审计信封 `actor` 同义，用于前端确认当前操作主体，并把主体写进审批记录与审计。

### 7.2 `GET /admin/capabilities/{id}`

`id, kind, group, risk, status, discoverable, callable, version, definition_hash, successor_id, last_changed_at, submitted_by, submitted_at, definition, definition_updated_at`。

- `last_changed_at`：**任意**变更（含可见性）的最后时间。
- `submitted_by` / `submitted_at`：最近一次提交审核的主体与时间；未提交过为 `null`。
- `definition_updated_at`：最新版本行的创建时间（定义本身的更新时间），与 `last_changed_at` 分开。
- 无 `updated` 字段；定义更新时间见 `definition_updated_at`，任意变更见 `last_changed_at`。

`GET /admin/capabilities?status=&group=&q=&limit=&offset=` 返回 `{items,total,limit,offset}`，条目字段同上去掉 `definition`/`definition_updated_at`。`status` 可逗号分隔多个值（`draft,review`），非法值返回 400；`q` 匹配 id 与描述（小写包含）。

### 7.3 `GET /admin/capabilities/{id}/versions`

```
{ items:[{ version, definition, definition_hash, status, created_at,
           submitted_by, submitted_at, approved_by, approved_at, approval_id }], total }
```

按 `version` 升序。旧版本行没有审批元数据，这些键**仍然输出，值为 `null`**（不是缺键）；`definition` 与 `definition_hash` 始终可读。

### 7.4 `GET /admin/review-queue`

```
{ items:[{ id, kind, group, risk, status, version, definition_hash,
           submitted_by, submitted_at, last_changed_at }], total }
```

只含 `draft` / `review`。响应无 `limit`/`offset`（队列是全量）。

### 7.5 `GET /admin/tools/stats?window_days=&limit=`

```
{ window:{days,from,to}, truncated, tools:[{
  id, kind, group, lifecycle_status, risk, discoverable, callable,
  version, last_changed_at, source, health,
  calls, success, failure, success_rate, cache_hits, hit_rate, retries,
  avg_response_tokens, p95_response_tokens, max_response_tokens,
  last_call_at, last_outcome, recent_tokens }] }
```

- `lifecycle_status`：注册表生命周期（`draft`/`review`/`published`/`deprecated`）；内置工具恒 `published`，未注册 id 为 `unknown`。`lifecycle_status` 是唯一状态字段（没有 `status`）。
- 调用里出现过但注册表没有的 id 也会成行：`kind=unknown`、`group=""`、`risk`/`discoverable`/`callable` 为 `null`、`source=managed`。
- `risk` / `discoverable` / `callable` / `version` / `last_changed_at` 来自注册表；内置工具为 `read`/`true`/`true`/`null`/`null`。
- `window_days` 默认 7、上限 30；`limit` 默认 200、上限 200。
- `truncated`：窗口内某类审计扫描命中上限（50,000 条）时为 `true`，表示聚合不是全量。
- `p95_response_tokens`：对窗口内 token 样本复制后升序排序，按最近秩法取第 `ceil(0.95 × n)` 个（同值多时取该秩的值），样本为空时为 `null`。`recent_tokens` 仍按时间顺序保留最近 20 个非空样本，两者排序不同。

### 7.6 `GET /admin/tools/{id}/calls?window_days=&limit=&offset=`

```
{ id, total, limit, offset, calls:[{
  event_id, ts, event_type, object_type, object_id,
  actor:{id,role,source?,token_id?}, outcome, tokens, cache, elapsed_ms, version,
  error_code, error_message, arguments, executed_script, bound_params }] }
```

- `event_type` 区分 `tool_call` 与 `template_run`（同一能力可能两类都有记录）。
- 错误信息以 `error_code` / `error_message` 呈现（没有 `mcp_code`）。
- 不含 `session_id`（会话只保留在审计信封与库列中）。
- `actor.source` / `actor.token_id` 为可选键：MCP token 调用会带出配置的 `source` 与 token id；管理面/stdio/旧事件没有这两个键（省略，不写 null）。
- `window_days` 默认 30、上限 90；时间倒序，同秒按写入顺序倒排。

### 7.7 `GET /admin/tokens`

```
{ items: [AccessTokenItem], total }
AccessTokenItem = {
  id, name, token_prefix, tenant, role, groups: string[],
  builtin_tools: string[], source, caller,
  status: "active" | "revoked" | "expired",
  created_at, created_by, expires_at: string|null,
  last_used_at: string|null, revoked_at: string|null
}
```

- 永远不返回明文 token 或 hash。
- `status` 在读取时计算：`revoked_at` 非空 → `revoked`；否则 `expires_at <= now` → `expired`；否则 `active`。
- 已撤销/已过期记录仍出现在列表里（撤销不删行）；硬删除后的记录不再出现。

### 7.8 `POST /admin/tokens`

```
body: { name, role, groups: string[], builtin_tools: string[],
        source, caller, expires_at?: string|null }
201: { token: string, item: AccessTokenItem }   // token 只在这里返回一次
```

校验（任一项失败返回 400 `MCP_VALIDATION_ERROR`）：

- `name` 1..64 字符；`caller` 符合 `^[a-z0-9._-]{2,32}$`；`source` 1..64 且 `^[a-z0-9._-]+$`；
- `role` 必须是 `policy.roles` 已定义角色；
- `groups` 去重后非空，且每个组都来自 `effective_policy`（配置组 + 管理组，**允许包含 `admin`**）；
- `builtin_tools` 去重后每项必须是合法 allowlist 条目：内置 id、上游暴露名 `<server_id>__<tool>`、`*` 或 `<server_id>__*`（空数组合法，表示没有任何工具；`**`、`a__b__*`、`*__x`、`__*`、空串一律 400）；`admin_*` 还要求 `groups` 含 `admin`，否则 400；
- `expires_at` 可空；非 RFC3339 或早于当前时间一律拒绝。

写 `admin_change`：`object_type="token"`、`object_id=id`、`action="token_create"`、`source="admin-api"`，`data` 只放上述配置与 `token_prefix`。审计未启用时拒绝写入（`MCP_AUDIT_ERROR`）。

实现细节：新签发/重签发的明文形如 `jumcp_<64 hex>`，明文只在创建/重签发响应里出现一次；库里只存 `sha256`（按 hash 查）；id 形如 `tok_<16 hex>`。

### 7.9 `POST /admin/tokens/{id}/revoke`

```
200: AccessTokenItem          // 直接返回条目（与前端 `revokeAccessToken` 对齐）
```

- 只置 `enabled=0` + `revoked_at`，不删除行；已撤销再撤销返回 409 `MCP_TOKEN_ALREADY_REVOKED`，且不重复写审计。
- 写 `admin_change`：`action="token_revoke"`（同样不写明文/hash）。
- 不存在返回 404 `MCP_TOKEN_NOT_FOUND`。

### 7.9.1 `DELETE /admin/tokens/{id}`

```
200: { "deleted": true, "id": string }
```

- 只有 `revoked` 或 `expired` 的令牌可硬删除；`active` 返回 409 `MCP_STATE_TRANSITION_INVALID`，`error.data` 含 `token_id`、`status:"active"`、`hint:"先撤销令牌再删除"`。撤销语义不变：撤销仍只置 `revoked_at`，不删行。
- 删除是硬删除记录本身；删除后该组的 `token_count` 与令牌总数自然下降。
- 不存在（含重复删除）返回 404 `MCP_TOKEN_NOT_FOUND`。
- 写 `admin_change`：`object_type="token"`、`object_id=id`、`action="token_delete"`、`outcome="ok"`、`source` 为 admin-api，`data` 见 §2.4；审计保留，**绝不写明文 token 或 hash**。审计未启用时拒绝删除（`MCP_AUDIT_ERROR`）。

### 7.9.2 `POST /admin/tokens/{id}/reissue`

```
200: { "token": "jumcp_<64 hex>", "item": AccessTokenItem }   // 与创建同形，明文只出现一次
```

- 同一记录原地换钥匙：`id`、`name`、`role`、`groups`、`builtin_tools`、`source`、`caller`、`expires_at` 不变，`token` / hash / 前缀更新、`last_used_at` 清空；**旧 token 立即失效**。
- 明文只在本响应出现一次（库里只存 `sha256` 与前缀），管理台据此复制 MCP 接入配置。
- 只有 `active` 可重签；`revoked` / `expired` 返回 409 `MCP_STATE_TRANSITION_INVALID`（`error.data` 含 `token_id`、`status`、`hint`），不改行、不写审计；不存在返回 404 `MCP_TOKEN_NOT_FOUND`。
- 写 `admin_change`：`action="token_reissue"`，`data` 只放配置与新 `token_prefix`，**绝不写明文或 hash**。

### 7.10 `GET /admin/tokens/options`

```
{ roles: [{ code, name, default_groups: string[], default_budget }],
  groups: [{ code, name, source: "policy"|"managed", priority, min_budget, token_count }],
  builtin_tools: [{ name, label, group: "builtin"|"admin" }],
  tool_wildcards: [{ value: "*", label }],
  upstreams: [{ id, name, group, enabled, tool_count, wildcard }],
  upstream_tools: [{ name, upstream_id, tool, group, label }] }
```

- 角色与组都返回 `code` + `name`：`code` 是权限标识（token.role、token.groups、审计 actor.role 一律用它），`name` 只是中文展示名，**不参与**任何权限判定；
- `roles` 按 code 稳定排序，`name` 非空且 1..64 字符；旧的「name=code」语义已删除；
- `groups` 是配置组 + 管理组合并（`Service::effective_policy`）后的结果，`token_count` 是该组被多少个**未撤销** token 引用；
- `builtin_tools` 的顺序与 `BUILTIN_CAPABILITY_IDS` 一致，`label` 为中文（检索能力/描述能力/组索引/登记模板草案/审核队列/审计查询/改可见性）；
- `tool_wildcards` 是 allowlist 的两档通配（`*`），`upstreams[].wildcard` 是 `<id>__*` 形态，前端据此提供"整台上游"授权；
- `upstreams` 列出全部已注册上游（未 refresh 的也在，`tool_count=0`），`upstream_tools` 的 `name` 是暴露名、`tool` 是上游原始名、`group` 是生效组（`basic` 覆盖优先），`label` 形如 `<上游名> / <原始工具名>`；
- 三个上游字段只读 registry 里的缓存视图，**不触网**：授予面不因管理面打开而探测上游；未配置上游管理器时三个字段都是空数组。

### 7.11 角色与组的 code / name 双轨

**角色**：`config/policy.toml` 的 `[roles.<code>]` 是权限标识，`name` 是中文展示名（`数据分析` / `执行器` / `管理员`）。

```
GET /admin/roles
200: { items: [{ code, name, default_groups: string[], default_budget }] }
```

按 code 稳定排序；`name` 必填非空、1..64 字符。token 的 `role`、审计 `actor.role`、组的 `roles` 都存 code。

**组**：配置形状 `{ code, name, priority, min_budget }`。`code` 是 scope / `capability.group` / `token.groups` / `role.default_groups` / 审计的唯一标识；`name` 是中文展示名。只写 `name` 的旧配置把 name 当 code，展示名回退 code；`name` 从不改变可见范围。

```
GET /admin/groups?q=&source=&role=
200: { items: [{ code, name, source: "builtin"|"policy"|"managed", priority, min_budget,
                roles: [code], grantable, capability_count, token_count }], total }
```

- `basic` 是代码内置组，固定出现在列表首位（`source:"builtin"`、`name:"基础工具"`、
  `priority:0`、`min_budget:800`、`roles:[]`、`grantable:false`、`token_count:0`）；
  `capability_count` 是常驻内置工具数（3）加当前逐工具加入 `basic` 的上游工具数；
- `q` 对 code/name 做不区分大小写的包含匹配；`source` 只接受 `policy`|`managed`|`builtin`
  （其他值 400 `MCP_VALIDATION_ERROR`）；`role` 是角色 code，精确匹配条目的 `roles`；
- 三层过滤同时生效，`total` 是过滤后的条数；
- 配置组的 `roles` 由 `policy.roles[*].default_groups` 反查（角色 code）；管理组用其 `roles` 列表；
- `grantable` 表示该组能否出现在令牌 `groups` / 角色 `default_groups` 里：只有 `policy` 与
  `managed` 组为 `true`，`basic` 为 `false`（`GET /admin/tokens/options` 的 `groups` 也不含它）。

```
POST /admin/groups            { code, name, priority, min_budget, roles: [code] }
PUT  /admin/groups/{code}     { name, priority, min_budget, roles: [code] }   // body 不接受 code
DELETE /admin/groups/{code}
```

- `code` 建成后不可改：路径即 code，PUT body 里出现 `code` 一律 400；
- `name` 只做展示：trim 后 1..64 字符；code 仍要求 `^[a-z0-9._-]+$`、≤64 字符、保留名 `basic`/`builtin`/`admin` 拒绝（409 `MCP_GROUP_RESERVED`）；
- `config/policy.toml` 的 `[[groups]]` 不得定义 `basic` 或 `builtin`（启动校验失败）；`admin` 仍允许作为配置组存在；
- 配置组只读：POST 重名 / PUT / DELETE 返回 409 `MCP_GROUP_CONFLICT`；
- 组被能力或 token 引用时删除返回 409 `MCP_GROUP_IN_USE`，`error.data` 同时给 `capability_count` 与 `token_count`；
- 管理组表用 `groups.name` 列存 code，展示名存 `groups.display_name`（该列为空串时 API 回退 code），编辑只改展示列。

审计：`object_type=group`、`object_id=code`；`changes` 记录 `name` / `priority` / `min_budget` / `roles` 的真实变化（`name` 是展示名），`data.group` 带 `{ code, name, priority, min_budget, roles }`。

## 8. 发布与可见性是两步

1. `POST /admin/capabilities/{id}/publish`：`review → published`，必须带外部审批单号 `approval_id`；批准人取当前 token 派生身份并记入 `approval.approved_by`。
2. `POST /admin/capabilities/{id}/visibility`：设置 `discoverable` / `callable`。

**发布 ≠ 对调用方开放**：只有两个开关都打开、且调用方属于该能力所在组，才既出现在 `tools/list`/`search` 又能 `tools/call`。管理台「审批」分栏把两步合并为一次操作，但仍是两次独立请求；第二步失败不会回滚已发布状态，界面会明确提示「已发布但可见性保存失败」。

可见性判定只看组名与调用方角色；组的 `min_budget` / `priority` 不改变可见范围。

## 9. 旧行边界（旧格式事件）

审计表是 append-only（`BEFORE UPDATE` / `BEFORE DELETE` trigger 直接 `RAISE(ABORT)`），因此：

- **不回填**：旧行不会被改写成新格式，也不会补 `object_type` / `object_id` / `action`；
- **不做字段回退**：读取端不尝试从 `capability_id` / `diff` / `approval_id` / `before_hash` / `after_hash` 还原语义；
- **只显示原始载荷**：管理台对旧格式事件标注「旧格式事件，仅原始载荷」，只折叠展示 payload 原文；
- **不计入统计**：工具台账与调用明细只聚合带 `object_id` 的新格式事件，旧调用的次数、成功率、token、缓存等全部不参与；`object_id` / `object_type` 过滤同样匹配不到旧行；
- 两类记录在同一个 `total` 里计数（审计查询不区分新旧）。

## 10. 数据库与 append-only

- SQLite 单表 `audit_events`，主键 `event_id`；索引 `(tenant, ts)`、`(event_type, ts)`。
- `capability_versions` 主键 `(capability_id, version)`；`BEFORE UPDATE OF definition_json, definition_hash, capability_id, version` 禁止定义改写，但允许审批元数据（`status`/`submitted_*`/`approved_*`/`approval_id`）在版本行上盖章；`BEFORE DELETE` 全表禁止。
- 哈希链校验结果通过 `/health` 的 `audit_chain`（`valid`/`tampered`/`error`）暴露。
- registry 库新增 `access_tokens`（`CREATE TABLE IF NOT EXISTS` + `ensure_column` 补列，旧库不受影响）：
  `id, name, token_hash(UNIQUE), token_prefix, tenant, role, groups_json, builtin_tools_json, source, caller, enabled, created_at, created_by, expires_at, revoked_at, last_used_at`。

## 11. MCP 接入令牌与 scope

### 11.1 鉴权边界

- `/mcp` 接受 `Authorization: Bearer <平台生成的 access token>`；没有该头时回落到 `?access_token=<token>`（仅此一个参数名，仅 MCP 面；两者同时给以头为准）。token 必须命中 `access_tokens` 且状态为 `active`（未撤销、未过期）。缺 token / 未知 / 撤销 / 过期一律 `MCP_UNAUTHORIZED`（HTTP 200 + JSON-RPC 错误，与既有入口一致），不存在静态 token 兜底。
- 成功鉴权后刷新 `last_used_at`；刷新失败只记日志，不让已认证请求失败。
- 管理面 `/admin/*` 仍只认 `JUMCP_ADMIN_TOKENS`（静态，必需），数据库 token 不能访问管理面。
- HTTP 启动只要求管理面 token（`JUMCP_ADMIN_TOKENS`），不要求 `JUMCP_AGENT_TOKENS`。

### 11.2 Actor 与来源

- `Actor.actor_id = token.caller`、`role = token.role`、`token_id = token.id`、`source = token.source`；
  审计 `ActorRef` 相应带出可选 `token_id` / `source`。
- 管理面事件 `source="admin-api"`；stdio 不写 `source`/`token_id`。
- `X-MCP-Tool-Groups` 与 `initialize._meta.mcp.groups` 只改变组集合，不改变 `token_id`/`source`。

### 11.3 scope 规则

- token 的 `groups` 同时是**默认组与硬上限**：未声明时使用它；header/`_meta` 只能在其中收窄。越界仍按租户模式判定（strict → `MCP_GROUP_FORBIDDEN`，intersect → 裁剪并标记 degraded），**永远不会扩展到角色默认组**。
- token 的 `builtin_tools` 是**工具名 allowlist**（内置 id、上游暴露名 `<server_id>__<tool>`、`<server_id>__*` 整台上游、`*` 全部工具）：`tools/list` 只输出命中项，`tools/call` 再判一次（列表过滤不放行调用）。`admin_*` 还需 scope.groups 含 `admin`（双重判定）；两者都不是请求头可扩大的。
- managed 能力仍按组过滤（`published ∧ discoverable/callable ∧ group ∈ scope`）；header/`_meta` 同样不能扩大 token 组。
- stdio/管理面 actor 的 `allowed_groups`/`allowed_builtin_tools` 均为 `None`，行为保持不变。

### 11.4 引导导入（兼容旧 dev/e2e）

启动时若 `access_tokens` 为空且 `JUMCP_AGENT_TOKENS` 非空，把每个静态 token 一次性导入为 active 记录：
`name="环境引导令牌"`、`source="bootstrap"`、`caller=agent_id`、`role` 同配置、`groups=该角色 default_groups`、
`builtin_tools=["*"]`（全部工具，含之后安装的上游；可见性仍由组与上游开关约束）、`created_by="env"`；日志输出「已导入 N 个引导令牌」。
导入的是同一明文的 sha256，原静态 token 仍可用；表非空时绝不重导。
