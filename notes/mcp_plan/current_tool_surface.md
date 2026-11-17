# 当前工具面盘点（改造成 MCP router / 三组切分前的基线）

> **当前事实（覆盖下文历史盘点）**：ClickHouse 内置只读查询 `db_query_readonly` 与模板执行 `db_run_template` 已移除，`basic` 组不再有数据源查询/执行入口。现在常驻内置工具只有三个元工具：`search_capabilities`、`describe_capability`、`list_groups`；`basic` 作为代码内置组与契约继续存在（不可配置、不可授权），上游 MCP 只读工具可按工具逐项加入 `basic`（`<server_id>__<tool>`）。ClickHouse 等数据源查询改由上游 MCP server 提供工具接入。下文是改造前的工具面基线，仅作历史参考。

结论先行：内置工具共 **9 个**（常驻 5 + 维护 4），全部硬编码在 `mcp.rs`，**没有 group 字段**、**没有上游转发**、**没有动态注册**。
组语义只有两档：`builtin`（常驻、恒可见）与 `admin`（组内可见），`basic/analysis/admin` 三分不存在；
`basic/analysis` 目前没有对应实现，也不是现有组 code。

## 1. tools/list 的工具清单

内置定义 9 条全部由 `mcp::tool_definitions()`（`backend/crates/jumcp-server/src/mcp.rs:35-76`）与
`mcp::admin_tool_definitions()`（`mcp.rs:108-141`）产出，`tool()` 只写 `name/description/inputSchema/annotations`
四个键（`mcp.rs:78-80`）——**内置条目在 JSON 里没有 `group` 键**。

| 工具 | 来源 | 定义位置 | 按组过滤 | 过滤谓词 | annotations | inputSchema 关键参数 |
|---|---|---|---|---|---|---|
| `search_capabilities` | 内置/常驻 | `mcp.rs:37-41` | 否 | 仅 allowlist（`builtin_allowed`，`discovery.rs:37-42`） | `READONLY_ANNOTATIONS` | `query`(必填)、`kind`、`group`、`limit`（默认 5，上限 50，`mcp.rs:520-528`） |
| `describe_capability` | 内置/常驻 | `mcp.rs:42-46` | 否 | 同上 | 同上 | `capability_id`(必填) |
| `list_groups` | 内置/常驻 | `mcp.rs:47-51` | 否 | 同上 | 同上 | 无 |
| `db_run_template` | 内置/常驻 | `mcp.rs:52-60` | 否 | 同上 | 同上 | `template_id`(必填)、`params`(object)、`version`(int) |
| `db_query_readonly` | 内置/常驻 | `mcp.rs:61-75` | 否 | 同上 | 同上 | `sql`(必填)、`params`(位置数组)、`max_rows`(默认 100/上限 1000)、`max_tokens`、`cell_char_limit`(默认 200/上限 4000)、`cell_char_offset`、`allowed_schemas`、`database` |
| `admin_register_template` | 内置/维护 | `mcp.rs:110-116` | 是 | `admin_enabled(scope)` ∧ allowlist（`mcp.rs:335-343`） | 同上（**写工具也用只读注解**） | `definition`(object，必填) |
| `admin_review_queue` | 内置/维护 | `mcp.rs:117-121` | 是 | 同上 | 同上 | 无 |
| `admin_audit_queries` | 内置/维护 | `mcp.rs:122-131` | 是 | 同上 | 同上 | `filter`(object)、`order`(asc/desc)、`limit`(≤200)、`offset` |
| `admin_set_visibility` | 内置/维护 | `mcp.rs:132-140` | 是 | 同上 | 同上（**写工具也用只读注解**） | `capability_id`(必填)、`discoverable`、`callable` |

- 常量：`READONLY_ANNOTATIONS`（`mcp.rs:23-25`）= `readOnlyHint:true, destructiveHint:false, idempotentHint:true, openWorldHint:false`，9 个工具共用；`admin_register_template`/`admin_set_visibility` 实际写库，注解与行为不一致。
- 目录拼接：`catalog_view`（`mcp.rs:312-353`）＝ 常驻定义（全部经 allowlist 过滤）＋（admin 组时）维护定义＋（平铺时）可见 `kind=tool` 能力；平铺条目由 `flattened_tool`（`mcp.rs:146-153`）产出，**带 `group` 键**、schema 与 `describe_capability` 同源。
- `tools/list` 结果不是标准 MCP 形状：除 `tools` 外还有 `groups`、`token_estimate`、`truncated`、`flattened`、`flattened_count`（`mcp.rs:432-490`），truncate 分支另有 `dropped_groups`、`budget_spent`。
- 常驻预算：`resident_tokens` 逐工具 `estimate_tokens` 求和 + 组索引 token（`mcp.rs:386-388`），上限 2000（`jumcp-core/src/budget.rs:12`）；`group_counts` 不计 builtin（`catalog.rs:124-140`）。

## 2. tools/call 分发路径

统一入口 `handle_tools_call`（`mcp.rs:494-646`）。分发前门禁：内置 id ∧ 不在 allowlist → `MCP_CAPABILITY_NOT_FOUND` + `policy_decision`（`mcp.rs:505-514`）。

| 工具 | match 分支 | handler | 服务层调用 | 审计 event_type |
|---|---|---|---|---|
| `search_capabilities` | `mcp.rs:517` | 内联 | `visible_capabilities` + `catalog::search` | `capability_search` |
| `describe_capability` | `mcp.rs:553` | 内联 | `visible_capabilities` + `check_schema`/`record_schema` | `capability_describe`（预算不足走 `policy_decision`） |
| `list_groups` | `mcp.rs:615` | 内联 | `visible_capabilities` + `catalog::group_counts` | 无 |
| `db_query_readonly` | `mcp.rs:623` | `tool_db_query_readonly`（`mcp.rs:1092-1252`） | `QueryParams::new` → `executor.validate` → `check_result` → `executor.execute` → `record_result` | `tool_call`（+ 早失败 `policy_decision` 关联） |
| `db_run_template` | `mcp.rs:624` | `tool_db_run_template`（`mcp.rs:1254-1591`） | `TemplateRegistry::current/pin/at_version`、`compiled.render`、`cache_lookup/store`、`executor.execute` | `template_run` |
| `admin_register_template` | `mcp.rs:626-643` | `admin_register_template`（`mcp.rs:753-838`） | `TemplateRegistry::register` → store 落 draft | `admin_change`(create) + 收尾 `tool_call` |
| `admin_review_queue` | 同上 | `mcp.rs:839-859` | `store.review_queue()` + `admin::queue_item` | `tool_call` |
| `admin_audit_queries` | 同上 | `mcp.rs:860-912` | `AuditLog::query_page` | `tool_call` |
| `admin_set_visibility` | 同上 | `mcp.rs:913-976` | `store.set_visibility` | `admin_change`(visibility) + `tool_call` |

错误面（客户端可分支的依据是 `result.content.error.code`，见 `err_tool` `mcp.rs:1633-1635`）：

| 情形 | 返回 |
|---|---|
| 未知工具名 | JSON-RPC error `-32602` `"Unknown tool"`（`mcp.rs:644`）——**不在 `reason` 里带 mcp_code** |
| 内置工具被 token allowlist 拒绝 | `isError:true`, `MCP_CAPABILITY_NOT_FOUND` + `policy_decision`（`mcp.rs:505-514`、`reject` `mcp.rs:355-366`） |
| 非 admin 调 `admin_*` | `MCP_CAPABILITY_NOT_FOUND`（与"不存在"同构）+ `policy_decision`（`detail.required_group="admin"`、`surface="mcp/admin"`）（`mcp.rs:652-673`） |
| 审计未启用时调用写工具 | `MCP_AUDIT_ERROR`（fail-closed，`mcp.rs:690-706`） |
| 模板不可见/未发布/组不在 scope | `MCP_CAPABILITY_NOT_FOUND`（`tool_db_run_template` 用 `callable_capabilities` 判定，`mcp.rs:1310-1322`） |
| 方法名不存在 | `-32601` `"Method not found"`（`mcp.rs:228`） |

- `admin_*` 是**双重判定**：allowlist（token 配置）× `scope.groups` 含 `admin`；管理台 `POST /admin/tokens` 在创建时也校验 `builtin_tools` 含 `admin_*` 必须 `groups` 含 `admin`（`jumcp-core/src/access.rs` `NewAccessToken::validate`，`access.rs:171-199`）。
- 用能力 id 直接调 `tools/call` 不存在：任何非内置名都落到 `-32602`；错误码 `MCP_USE_DB_RUN_TEMPLATE`（`jumcp-core/src/errors.rs:16`）与 `MCP_NOT_IMPLEMENTED`（`errors.rs:17`）**全仓无发码点**。

## 3. 组与角色模型

`EffectiveScope`（`jumcp-core/src/entry.rs:101-110`）：`groups: Vec<String>`、`budget: i64`、`degraded: bool`、`source: String`、`mode: String`、`allowed_builtin_tools: Option<Vec<String>>`。

通道与优先级（`mcp.rs:250-278`）：HTTP `X-MCP-Tool-Groups`（1）> `initialize._meta.mcp.groups` 会话 scope（2）> 角色默认（4）。
- Header：`service::header_groups` 只认 `x-mcp-tool-groups`（大小写不敏感，`service.rs:43-48`）；`X-MCP-Tool-Budget` **未解析**（`GroupRequest.header_budget` 恒 None）。
- `_meta`：`mcp.rs:280-310`，只在声明 `mcp.groups` 或 `mcp.toolBudget` 时生效，按 `token:<token_id>` / `actor_id` 分桶（`mcp.rs:1601-1607`）。
- `profile` 通道存在于纯函数（`entry.rs:145-170`），本仓库所有调用点都传 `GroupRequest::default()`（`mcp.rs:256-278`、`service.rs:353-365`），**无生产调用点**。
- 解析顺序不可换：填默认 → 越界判定 → 预算取 min（`entry.rs:172-240`）；`strict` 越界整体报 `MCP_GROUP_FORBIDDEN`，`intersect` 裁剪并置 `degraded`；预算 < 800 报 `MCP_BUDGET_EXCEEDED`。
- token 派生的组是硬上限兼默认组（`Entry::Actor::access_token`，`entry.rs:75-93`）；`resolve_scope` 用 `allowed_groups` 覆盖角色默认（`entry.rs:184-189`）。
- 有效策略 = 配置 policy ∪ 管理面新增组（`service.rs:303-332`），管理组按 `roles` 追加进角色 `default_groups`。

组 code 现状（`config/policy.toml:19-71`；`.dev-data/policy.toml` 同集合，仅展示名不同）：

| code | 用途 | 角色默认组 |
|---|---|---|
| `db.read.core` | 核心数据只读（能力分组最高频） | analyst、executor |
| `db.read.analytics` | 分析数据只读 | analyst |
| `db.read.sensitive` | 敏感数据只读 | 无 |
| `db.admin.schema` | 库表管理 | 无 |
| `admin` | **维护通道开关**：`admin_*` 可见/可调的唯一依据 | admin（`config/policy.toml:26-31`） |
| `api.internal.read` | 内部接口只读 | analyst |
| `kb.search` | 知识库检索 | analyst |

- 保留组名 `builtin`、`admin` 不可由管理面创建/改/删（`config.rs:14-18`），组数上限 20（`config.rs:9`）；`builtin` 只是发现面投影用的保留组（`discovery.rs:20-26`），不在 policy 里注册。
- 内置工具当前**没有 group 字段**；`builtin`/`admin` 只在发现面投影时附加（`discovery.rs:49-99`），`/admin/tools/stats` 的台账也用同一份常量（`admin.rs:1119-1150`）。
- 测试与脚本实际使用的组 code：`db.read.core`、`db.read.analytics`、`db.admin.schema`（`tests/contracts.rs:30-64`、`tests/discovery.rs`、`tests/discovery_projection.rs:40-70`）、`admin`（`tests/identity_admin.rs`、`tests/tokens.rs:442-480`）；`dev.ps1:37` 用 `agent-analyst:analyst` / `agent-admin:admin` 两个角色。

## 4. 注册表 / 能力模型

`CapabilityStore`（`jumcp-core/src/registry.rs:299-560+`）：

- `capabilities`：`capability_id PK, kind, group_name, risk, status, discoverable, callable, version, definition_json, definition_hash, successor_id, updated_at, submitted_by, submitted_at`（`registry.rs:310-316`）。
- `capability_versions`：append-only（触发器 `trg_ver_no_update`/`trg_ver_no_delete`，`registry.rs:317-326`），列含 `approval_id/created_at/submitted_by/submitted_at/approved_by/approved_at/status`。
- `groups`：`name PK, display_name, priority, min_budget, roles_json, updated_at`（`registry.rs:327-331`）；旧库补列清单 `registry.rs:333-350`。
- 闭集常量：`STATUSES = draft|review|published|deprecated`（`registry.rs:19`）、`KINDS = tool|template|endpoint`（`registry.rs:20`）、`RISKS = read|sensitive`（`registry.rs:21`）。**新增 `kind=upstream` 要动 `KINDS`，并且前端 `CapabilityKind`/`ToolKind`（`admin-web/src/api/types.ts:159,235`）与 `TOOL_KIND_LABEL`（`labels.ts:160`）必须同步，否则 UI 落到 unknown。**
- 内置 id 三份常量：`RESIDENT_CAPABILITY_IDS`（5）、`ADMIN_CAPABILITY_IDS`（4）、`BUILTIN_CAPABILITY_IDS`（并集，`registry.rs:25-57`）；`is_builtin_id`（`registry.rs:59-61`）是写保护唯一集合 `ensure_mutable`（`registry.rs:69-78`）。**内置工具不写 SQLite**：注册表只读代码定义，写入即 `MCP_BUILTIN_IMMUTABLE`（`registry.rs:70-76`、`tests/discovery_projection.rs:438`）。
- `source`：`builtin|managed`（`catalog.rs:12-14`），是投影期推导值（`discovery.rs:49-75`、`discovery.rs:105-127`），不落库。
- 可见/可调用两条谓词：`status='published' AND discoverable=1` / `... AND callable=1`，二者共用 `rows_where` 的 SQL 拼装（`registry.rs:63-67`、`494-541`）；内存目录另一套等价判定 `catalog::is_visible/is_callable`（`catalog.rs:77-85`）。
- 版本/定义哈希：`register` 落 `version=1` + `definition_hash`（`registry.rs:551-590`）；改定义走 `revise_draft_definition`（`registry.rs:827`）、发布走 `promote_to_published`（`registry.rs:743`）、审批戳在版本行（`registry.rs:660-741`）。
- 版本钉住与结果缓存只在内存（`TemplateRegistry.pins/cache`，`template.rs:1198-1215`）；模板生命周期复用能力表 `kind=template`（`template.rs:9`、`1228-1237`）。
- **只有模板能被生产写入**：`store.register` 的唯一生产调用点是 `TemplateRegistry::register`（`template.rs:1228-1237`），HTTP `POST /admin/capabilities` 也走它（`admin.rs:1590-1605`）。`kind=tool|endpoint` 只在测试的内存 `Catalog` 或直接 `store.register` 中出现，**没有管理面入口**。
- 新增 `kind=upstream` 需同时处理：`KINDS` 闭集、`project_record` 的 schema 分支（`discovery.rs:105-127`，非 template 走 `declared_schema`，键名 `params_schema|inputSchema`）、`tools/call` 分发（现在只有内置 match，无按 id 转发）、`db_run_template` 的 `kind != "template"` 判定（`template.rs:1334`、`1349`）、统计 `kind` 透传（`admin.rs:1272`、`ToolKind`）。

## 5. 管理面与审计

路由全表在 `admin::routes()`（`admin.rs:2623-2652`，26 条）与 `router()`（`admin.rs:66-98`）；`/health` 免鉴权，其余挂 Bearer 中间件（`require_token`，`admin.rs:202-224`）。

| 与工具/能力相关 | 处理函数 | 请求/响应要点 |
|---|---|---|
| `GET /admin/tools/stats` | `admin.rs:1309-1428` | 查询 `window_days`(默认 7/上限 30)、`limit`(默认 200/上限 200)；响应 `window/truncated/tools[]`，每行字段见 `tool_stats_item`（`admin.rs:1251-1300`） |
| `GET /admin/tools/{id}/calls` | `admin.rs:1433-1520+` | `limit/offset/window_days`(默认 30/上限 90)；`calls[]` 含 `arguments`、`executed_script`、`actor.source/token_id` |
| `GET /admin/capabilities` | `admin.rs:645-720` | 过滤 `status`(逗号列表)/`group`/`q`/`limit`/`offset`；条目 `capability_item`（`admin.rs:368-382`） |
| `POST /admin/capabilities` | `admin.rs:1550-1640` | body `{definition: object}`；只产 draft，内置 id 返回 409 `MCP_BUILTIN_IMMUTABLE` |
| `GET /admin/capabilities/{id}`、`/versions`、`PUT /definition` | `admin.rs:389-460`、`463-630` | 详情返回摘要 + `definition` 原文 |
| `POST /{id}/submit`、`/publish`、`/reject`、`/visibility`、`/deprecate` | `admin.rs:749`、`821`、`871`、`928`、`1661` | publish 必须 `approval_id`（缺 → `MCP_APPROVAL_REQUIRED`），批准人取 token 派生 id（`admin.rs:837-856`）；visibility body `{discoverable?, callable?}`（`admin.rs:923-926`） |
| `GET /admin/review-queue` | `admin.rs:358-367` | `{items,total}`，条目形状 `queue_item`（`admin.rs:343-356`），与 MCP `admin_review_queue` 共用 |
| `GET /admin/audit-events` | `admin.rs:259-333` | 白名单过滤键 `FILTER_KEYS`（`admin.rs:39-50`），未知键 400 `MCP_FILTER_UNSUPPORTED`；`limit` 夹紧 1..200 |
| `GET/POST /admin/tokens`、`/tokens/options`、`/revoke`、`DELETE` | `admin.rs:1764-1965`、`1966`、`1902`、`1927` | token 记录含 `role/groups/builtin_tools/source/caller/expires_at`（`access.rs:32-43`、`245-255`） |

- 审计写操作 payload：统一骨架 `jumcp_core::audit::payload`（`audit.rs:34-57`；字段 `object_type/object_id/action/outcome/error_code/error_message/from_status/to_status/changes/approval/reason/schema_version/correlation_id/data`）；`admin_change` 另加 `data.definition_hash_before/after`、`version`（`mcp.rs:800-830`）。
- 审计表 append-only（触发器，`audit.rs:104-113`），`session_id` 恒为 `"default"`（`service.rs:76`、`audit_event` `service.rs:224-250`）——**不是按 agent 分账**。
- `ToolStatsRow` 契约字段：`id/kind/group/lifecycle_status/risk/discoverable/callable/version/last_changed_at/source/health/calls/success/failure/success_rate/cache_hits/hit_rate/retries/avg|p95|max_response_tokens/last_call_at/last_outcome/recent_tokens`（`admin.rs:1272-1300`；前端 `types.ts:251-275`）。
- 台账 universe = 全部 registry 行 + 全部 builtin（含 `admin_*`）+ 只在审计里出现的 id（`admin.rs:1119-1170`）；builtin 的 `group` 直接取 `builtin`/`admin` 常量，`lifecycle_status="published"`。
- 前端依赖：`ToolsPage` 按 `source` 分页签（`'managed'` 默认）、健康/风险/组/搜索过滤、列 `id/calls/success_rate/health/lifecycle/risk/visibility/group/last_call/actions`（`ToolsPage.tsx:55-213`）；`AuditQueryPage` 过滤 `tenant/event_type/actor_role/actor_id/token_id/object_type/object_id/event_id/order/ts_*`（`AuditQueryPage.tsx:109-128`）。
- 新增 upstream 的最小改动面：`KINDS` + `TOOL_KIND_LABEL`/`CapabilityKind`（UI 分类）、`routes()` 与 `routes_are_documented` 的长度断言、`tool_universe` 的 `kind` 透传（已按 registry 行原样输出，无需改）、`POST /admin/capabilities` 目前把 body 当模板定义（`admin.rs:1560-1605`），要新增非模板创建路径或泛化 body（`kind` 字段）。

## 6. 传输与运行时

- 入口二选一：`settings.transport ∈ {stdio, streamable-http}`（`config.rs:521,574`）；`main.rs:121-125` 分派。
  - stdio：逐行 JSON-RPC，`stdin/stdout`，无 per-request 身份（`main.rs:128-164`），身份=`Service::process_actor`（`service.rs:122-125`），scope 来自 `default_scope`（`service.rs:414-423`）。
  - streamable-http：`POST /mcp`（`mcp.rs:1664-1667`），身份只认 `Authorization: Bearer` → `access_tokens` 表（`post_mcp`，`mcp.rs:1697-1773`）；`/admin/*` 与 `/mcp` 合并在同一 listener（`main.rs:199-208`）。HTTP 模式强制要求 `JUMCP_ADMIN_TOKENS` 与审计路径，否则拒绝启动（`main.rs:172-184`）。
- JSON-RPC 方法集合（`mcp.rs:207-230`）：`initialize`、`tools/list`、`tools/call`、`ping`；`notifications/*` 一律返回 null；其余 `-32601`。**没有** `resources/*`、`prompts/*`、`logging/setLevel`、`completion/*`，也没有 `Mcp-Session-Id` 头/SSE 流（每条请求独立 HTTP POST，响应即 JSON）。
- `initialize` 回执硬编码 `protocolVersion:"2025-03-26"`、`serverInfo.name="jumcp"`、`version:"0.1.0"`（`mcp.rs:214-217`），不从 `CARGO_PKG_VERSION` 取。
- 子进程 connector（可复用的 stdio 上游骨架，`jumcp-server/src/connector.rs`）：
  - 协议：行分隔 JSON-RPC 2.0，`initialize`/`query`/`shutdown`，版本常量 `PROTOCOL_VERSION=1`（`jumcp-connector-protocol/src/lib.rs:10-14`）。
  - 启动：`ConnectorAgent::spawn`（`connector.rs:104-157`），`stdin/stdout/stderr` 全 piped、`kill_on_drop(true)`、剔除 `JUMCP_ADMIN_TOKENS`/`JUMCP_AGENT_TOKENS` 环境变量；stderr 异步收集 32 行尾部并脱敏（`connector.rs:134-152`）。
  - 握手：`initialize` deadline 10s，协议版本不符即杀进程（`connector.rs:315-361`）。
  - 请求：每请求自增 id、写一行读一行，`timeout(deadline)`；超时/EOF 标 `fatal` 并杀进程，下一次调用重新拉起，**不重试**（`connector.rs:159-215`、`363-430`）。
  - 查询 deadline = `statement_timeout_ms + 5s`，上限 310s（`connector.rs:438-444`）。
  - 二进制解析：`JUMCP_CLICKHOUSE_AGENT_BIN` → 可执行同级/上级目录同名文件 → `PATH`（`connector.rs:492-518`）。
  - 串行化：`tokio::sync::Mutex<Option<ConnectorAgent>>`，同一时刻只允许一个在途请求（`connector.rs:363-372`）。

## 7. 测试覆盖（现有契约，勿删）

| 文件 | 关键用例 | 与本次改造的关系 |
|---|---|---|
| `tests/identity_admin.rs` | `tools_list_depends_on_derived_identity`（398）、`header_groups_can_only_narrow_the_derived_identity`（455）、`non_admin_cannot_call_admin_tools`（501）、`admin_register_template_creates_draft_then_human_publish_exposes_it`（529）、`admin_set_visibility_rejects_builtin_and_audits_success`（646）、`admin_read_tools_serve_queue_and_audit_pages`（702）、`meta_scope_applies_when_header_absent`（762）、`admin_tool_calls_are_counted_in_stats`（1050）、`admin_register_template_without_audit_rejects_and_persists_nothing`（982） | 用 `RESIDENT_TOOL_NAMES.len()` / `+ ADMIN_TOOL_NAMES.len()` 精确断言清单长度——**新增任何内置工具或上游工具都会破**，必须同步扩展（不是删） |
| `tests/tokens.rs` | `admin_tools_require_group_and_allowlist`（442）、`options_lists_roles_groups_and_builtin_labels`（600）、`meta_groups_can_only_narrow_token_scope`（687）、`call_details_and_audit_filter_carry_token_origin`（735） | allowlist×组双重判定、`/tokens/options` 的内置工具标签需随新工具扩展 |
| `tests/discovery.rs` | `flattens_when_visible_tools_within_threshold`（119）、`collapses_when_over_threshold`（147）、`threshold_zero_disables_flattening`（167）、`flatten_over_budget_*`（175/192）、`flattened_definition_matches_describe`（209）、`meta_groups_channel_applies_when_header_absent`（235） | 平铺/收缩与常驻预算口径；断言 `RESIDENT_TOOL_NAMES.len()+N` |
| `tests/discovery_projection.rs` | `published_template_becomes_visible_without_restart`（222）、`hidden_but_callable_template_stays_callable`（331）、`builtin_ids_reject_every_maintenance_write`（438）、`tools_list_keeps_one_copy_of_each_builtin`（466）、`endpoint_capability_projection_never_panics_on_missing_schema`（503） | 唯一可见集与内置去重；`endpoint` 投影已存在但无生产写入路径 |
| `tests/admin.rs` | `review_queue_lists_only_draft_and_review`（301）、`submit_*`、`publish_*`、`visibility_updates_flags_and_writes_admin_change`（598）、`audit_events_*`（672/731/740）、`routes_are_documented`（756）、`stats_retries_are_counted_per_session_not_globally`（1747）、`tool_calls_window_defaults_to_thirty_days_and_clamps`（1830）、`groups_crud_takes_effect_in_effective_policy`（2209）、`groups_reject_reserved_policy_and_missing`（2456） | 新增 admin 路由必须同时改 `routes()` 与该用例的 `table.len()` 断言 |
| `tests/contracts.rs` | `strict_rejects_group_outside_allowlist`（149）、`resident_budget_*`（295/310）、`describe_schema_*`（370/391）、`happy_path_audits_tool_call_with_metrics`（408） | 组越界/预算/审计契约，工具面变动后仍需保持 |
| `tests/templates.rs` | 19 个用例（`named_placeholders_*` … `render_output_shape_is_stable`） | 模板链不受工具面改造影响，但 `db_run_template` 签名是上游改造的兼容边界 |
| `jumcp-connector-clickhouse/tests/protocol.rs` | `agent_protocol_initialize_query_and_shutdown`（41）、`agent_protocol_rejects_non_scalar_params`（118） | 若复用 connector 协议做 stdio 上游，这是现成的协议骨架参考 |

- 已知不一致（未运行验证）：`admin::routes()` 现有 **26** 条（`admin.rs:2623-2652`），`tests/admin.rs:795` 断言 `table.len() == 25`。本次盘点时 `cargo test` 无法链接（`target/debug/jumcp-server.exe` 被运行中的服务占用，报 `os error 5`），因此该断言是否为在途改动未确认。
- 前端无自动化测试：`admin-web/package.json` 只有 build/dev 脚本，UI 断言靠 `.screenshots/` 人工核对。

## 8. 明确空白（按文件证据）

| 空白 | 证据 |
|---|---|
| 动态/外部上游工具 | 全仓无 `kind=upstream`、无按能力 id 转发的外部调用；`tools/call` 只有 9 个硬编码分支（`mcp.rs:517-644`），非内置名直接 `-32602` |
| 工具命名空间/前缀 | 无 `server`/`namespace`/`prefix` 概念；能力 id 是扁平字符串（`registry.rs:310`），`search` 只做子串匹配（`catalog.rs:87-119`） |
| 上游健康检查 | `health()` 只报 groups 数与数据源（`service.rs:367-380`）；`/admin/tools/stats` 的 `health` 由审计成功率推导（`admin.rs:1233-1243`），不探活任何上游；无 ping/超时/重连状态字段 |
| 工具热加载 | 无 reload/watch；内置定义是 `Vec<Value>` 常量函数（`mcp.rs:35,108`），进程重启才变；只有 SQLite 行的发布是"下一请求生效"（无缓存，`discovery.rs:11-13`） |
| 上游调用审计 | `tool_call`/`template_run` 只覆盖内置两入口（`mcp.rs`、`audit.rs:216-226`）；无 `upstream_call` 事件类型，前端 `AuditEventType` 闭集（`types.ts:8-15`）也没有 |
| 非模板能力的写入路径 | 生产只有 `TemplateRegistry::register`（`template.rs:1228-1237`）；`kind=tool|endpoint` 无管理面入口，`POST /admin/capabilities` 只接模板定义（`admin.rs:1555-1605`） |
| 上游凭据/连接管理 | 数据源只有 `DatasourceRef{name,engine,readonly,allowed_schemas,statement_timeout_ms,dsn_env}`（`config.rs:56-70`），且启动期强制"恰好一个 readonly"（`config.rs:596-620`）；无上游注册表/密钥表 |
| 工具级权限（非组） | 授权单位是组（`catalog.rs:77-85`）与 token allowlist（`discovery.rs:37-42`）；无单工具标签/ACL |
| 协议面 | 无 `resources`/`prompts`/`sampling`/`logging`/`completion`，无 SSE、无 `Mcp-Session-Id`（`mcp.rs:207-230`）；`initialize` 版本硬编码（`mcp.rs:214-217`） |
| 写工具的注解语义 | 4 个 admin 工具共用 `READONLY_ANNOTATIONS`（`mcp.rs:23-25,108-141`），与 `destructiveHint=false` 的事实不符 |
| 风控闭集与 UI 不一致 | 后端 `RISKS=read|sensitive`（`registry.rs:21`），前端 `CapabilityRisk` 含 `write|admin` 且风险筛选列出四项（`types.ts:161`、`ToolsPage.tsx:280-290`） |
| 审计会话粒度 | `session_id` 恒 `"default"`（`service.rs:76`），统计的 retries 按 `(session_id,id)` 分组（`admin.rs:1226-1235`）实际等价于按 id |

## 复核用命令

```bash
grep -n "tool(\|\"admin_" backend/crates/jumcp-server/src/mcp.rs
grep -n "RESIDENT_CAPABILITY_IDS\|ADMIN_CAPABILITY_IDS\|BUILTIN_CAPABILITY_IDS\|KINDS\|RISKS" backend/crates/jumcp-core/src/registry.rs
grep -n "code = \|default_groups" config/policy.toml
grep -rn "x-mcp-tool-groups" --include=*.rs backend/crates
grep -n "\.route(" backend/crates/jumcp-server/src/admin.rs
grep -n "fn tool_universe\|fn tool_stats_item\|fn health_of" backend/crates/jumcp-server/src/admin.rs
grep -n "fn resolve_scope\|pub struct EffectiveScope" backend/crates/jumcp-core/src/entry.rs
grep -c '("' backend/crates/jumcp-server/src/admin.rs   # routes() 条目数
```
