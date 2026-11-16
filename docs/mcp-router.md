# jumcp MCP Router：内置工具 + 动态上游 MCP server 聚合

**jumcp**（jump + mcp）是**聚合 router**：一个 `/mcp` 同时暴露内置工具与所有启用上游 MCP server 的工具。
不存在 `/mcp/<upstream>` 透传端点——鉴权、scope、预算与审计只有这一层。

## 1. 内置工具的分类与 `basic` 组

分类按**职责**划分，是 `tools/list` / 搜索 / `/admin/tools/stats` 上的展示与检索标签：

| category | 职责 | 工具 | 可见性判定 |
|---|---|---|---|
| `basic` | 基础元工具：走查工具目录/取 schema | `search_capabilities`、`describe_capability`、`list_groups` | 常驻；只要 token 的 `builtin_tools` allowlist 命中即可见 |
| `analysis` | 工具列表分析、变更提交审计 | `admin_review_queue`、`admin_register_template`、`admin_set_visibility` | 组 `admin` 在 scope 内 **且** allowlist 命中（双重判定） |
| `admin` | 仅审计操作 | `admin_audit_queries` | 同上双重判定 |

未知 id 落 `custom`（新增内置工具忘记归类时不会冒充 `admin`）。

注解与行为一致：`admin_register_template` / `admin_set_visibility` 实际写库，声明
`readOnlyHint=false`（`destructiveHint=false`，不触碰数据源）；`admin_review_queue` /
`admin_audit_queries` 与其余内置工具保持只读注解。上游工具自带 `annotations` 时原样透传，
未声明时按上游 `read_only` 保守生成（`read_only=false` → `destructiveHint=true`、`readOnlyHint=false`）。

### `basic` 是代码内置组，不是授权组

`basic` 同时是一个**代码内置组**（`discovery::BASIC_GROUP`）：常驻三件内置元工具的 `group`
就是 `basic`，上游可以把**个别**只读工具逐工具加入 `basic`（见 §2.1）。它不是配置组
（`config/policy.toml` 不得定义）、不是管理组、也不是可授权组：

- `GET /admin/groups` 固定返回一条合成条目：`{code:"basic", name:"基础工具", source:"builtin",
  priority:0, min_budget:800, roles:[], grantable:false, capability_count: 3 + 上游 basic 覆盖数,
  token_count:0}`；`source` 过滤接受 `policy|managed|builtin`。
- 创建/改名/删除组一律 409 `MCP_GROUP_RESERVED`（`basic`/`builtin`/`admin` 都在
  `RESERVED_GROUP_NAMES`）；`basic` 不能出现在令牌的 `groups` 里（`token_options.groups`
  不含它，创建令牌传它会得到"未知组"）。配置文件中出现 `basic` 或 `builtin` 组 → 启动校验失败。
- `builtin` 只作为**拒绝创建的保留名**存在，不再作为任何工具的 `group` 值。

分类**不是授权组**，不改变权限判定：`analysis`/`admin` 这两类不是 `scope.groups` 的取值，
`X-MCP-Tool-Groups` 收窄也不会改变常驻工具的可见性。常驻性由代码常量
`RESIDENT_TOOL_NAMES` 判定（`basic` 的三件全是常驻工具）；`analysis` 与 `admin` 里的
`admin_*` 一律要求组 `admin` 在 scope 内 **且** token allowlist 命中（`discovery::admin_enabled`
与 allowlist 双重判定），分类标签既不放宽也不收紧这条规则。

`basic` 组成员则相反：**不要求 `scope.groups` 含 `basic`（或含上游组）**，只要 allowlist
命中、上游 `enabled`、`read_only`/写开关允许，就对所有 scope 可见/可调；`X-MCP-Tool-Groups`
收窄不能隐藏它们。`basic` 只承载只读工具，且拒绝 `category=admin` 的上游与 `admin_` 前缀的
工具，因此不会成为绕过维护通道（组 `admin`）的旁路。

token 的 `builtin_tools` 是**工具名 allowlist**，同时约束内置 id 与上游暴露名
（`<server_id>__<tool>`）：白名单是精确匹配，未列出的名字在 `tools/list`、发现面与
`tools/call` 三处都表现为"不存在"（调用写 `policy_decision`）。创建令牌时上游名必须符合
`<server_id>__<tool>` 形状，但不要求上游此刻已注册。

`tools/list` 的每条内置工具带 `category`；`search_capabilities` 支持可选 `category` 过滤；
`describe_capability` 与 `/admin/tools/stats` 同样输出 `category`（托管能力归 `analysis`）。
组索引（`list_groups` 与 `tools/list` 的 `groups`）只统计上游等非内置来源：没有上游工具
加入 `basic` 时索引里没有 `basic`，有覆盖时按生效组计数（常驻内置工具不计入）。

## 2. 上游配置模型

配置存 registry SQLite 的 `mcp_upstreams` 表（`CREATE TABLE IF NOT EXISTS` 自迁移），
**不写入 `capabilities`**，因此上游无法影子覆盖内置工具定义。

| 字段 | 说明 |
|---|---|
| `id` | `^[a-z][a-z0-9_-]{1,31}$` 且**不含连续 `__`**（保证暴露名唯一可反解）；创建后不可改；不得为 `admin`/`analysis`/`basic`/`builtin`/`jumcp` 或内置工具 id |
| `name` | 展示名，trim 后 1..64 字符 |
| `transport` | `http`（Streamable HTTP）或 `stdio`（部署开关） |
| `url` | `http` 必填，只允许 `http`/`https`、禁止 userinfo；`stdio` 必须为空 |
| `command` / `args` | `stdio` 必填 command；`http` 的 command 必须为空；args 上限 64 |
| `env` / `headers` | 值可为字面量或 `env:NAME` 引用；各上限 64 项，key/value 非空 |
| `group` | 必须存在于 effective policy（配置组或管理组）；决定上游工具的授权范围。**不得为 `basic`**：`POST`/`PUT` 返回 `400 MCP_UPSTREAM_INVALID`（`data.field="group"`），个别工具要走 §2.1 |
| `category` | `basic` / `analysis` / `admin` / `custom` |
| `read_only` | 默认 `true`；`false` 仅在 `JUMCP_MCP_ALLOW_WRITE_UPSTREAM=1` 时允许注册 |
| `enabled` | 默认 `true`；`false` 时不投影也不转发 |
| `timeout_ms` | 1..120000，默认 10000；作用于建连、`tools/list` 与 `tools/call` |

`env` / `headers` 的值允许两种形态：字面量，或 `env:NAME`（建连时从进程环境解析，
变量缺失即建连失败，不静默空串）。两种传输都在建连时解析 `env` 引用：`http` 不使用 `env`，
但写错的引用同样会失败（避免切换到 `stdio` 后才暴露配置错误）。

### 2.1 逐工具加入 `basic`

配置存在 registry 的 `mcp_upstream_tool_groups(upstream_id, tool_name, group_code, updated_at,
PRIMARY KEY(upstream_id, tool_name))`：**只存显式覆盖**，没有行 = 继承 `upstreams.group`。
`group_code` 目前只允许 `basic`；上游删除时同一事务级联删除；每台上游最多 64 个覆盖。

- 生效组：有覆盖且工具**仍可加入**（`eligible`）→ `basic`；否则继承上游组。管理视图用
  `overridden`（有覆盖行）与 `basic`（覆盖生效）两个独立字段区分"覆盖存在但已失效"，避免
  上游配置漂移后静默丢失用户意图。`eligible` = 上游 `read_only=true` **且**上游 `category≠admin` **且**工具名
  不以 `admin_` 开头（后两条保证 `basic` 不会变成维护通道旁路）。
- `basic` 成员对所有 scope 可见/可调，不要求 `scope.groups` 含 `basic` 或上游组；但仍要求
  token allowlist 命中、上游 `enabled`、`read_only`/写开关允许。
- 覆盖是**合并**语义：`{"tools": {"<raw_tool_name>": true|false|null}}`，`true` 写入、
  `false`/`null` 删除（回到继承）。幂等：无真实变化不写库、不写审计。
- 校验是整请求粒度（不做部分写入）：未知工具 `409 MCP_UPSTREAM_TOOL_UNKNOWN`
  （`data.unknown`）、写工具 `409 MCP_UPSTREAM_TOOL_NOT_READ_ONLY`、admin 类别或 `admin_`
  前缀 `409 MCP_UPSTREAM_TOOL_NOT_ELIGIBLE`（`data.reason=upstream_category_admin|admin_prefix`）、
  单次超过 64 键 `400 MCP_VALIDATION_ERROR`、上游不存在 `404 MCP_UPSTREAM_NOT_FOUND`。
- 上游改名/刷新失败导致覆盖行对不上当前缓存时：覆盖**保留**并在视图里标 `orphaned=true`
  （`basic` 仍为 true），由管理面显式清除；不静默删用户配置，也不让孤行参与投影。
- 工具缓存仍是进程内 30s TTL、无持久化：`GET .../tools` 只读缓存（与 `GET /admin/upstreams`
  同一语义），需要最新清单时显式 `POST .../refresh`。

## 3. 命名与冲突

上游工具对外名为 `<server_id>__<tool>`：两侧只保留 `[A-Za-z0-9_-]`，其他字符转 `-`。
**不做截断**：整名超过 64 字符的工具会让整台上游被拒绝（`status=error` + 明确
`status_message`），因为截断后的名字无法再回指上游原始工具。`server_id` 不允许连续 `__`，
所以暴露名里第一次出现的 `__` 一定是分隔符，前缀反解无歧义。

冲突在注册/刷新时**拒绝**，不静默加后缀：

- `server_id` 为保留字或内置工具 id → 注册 409 `MCP_UPSTREAM_CONFLICT`；
- 同一上游内两条工具规范化后同名 → 整台上游拒绝（`status=error`，`status_message` 说明原因）；
- 与注册表里已有的 `capability_id`（managed 能力，含 draft）撞名 → 整台上游拒绝；
- 与另一上游已缓存的暴露名撞名 → 后进入缓存写锁的那台上游整台拒绝（检查与插入在同一把锁内，
  并发首拉不会各自通过校验后静默去重）；
- 与内置 id 撞名（`__` 分隔符使常规工具不可能命中，仍然显式拦截）。

拒绝是整台上游粒度：不会出现"只接入一半工具"的静默降级。

## 4. 传输

**HTTP Streamable MCP**：`POST` 上游 URL，`Accept: application/json, text/event-stream`；
响应既可以是单个 JSON 报文，也可以是 SSE 事件流（逐 `data:` 事件解析，取与请求 id 匹配的
JSON-RPC 响应）。`initialize` 响应头里的 `Mcp-Session-Id` 会被记住并附加到后续请求。
握手顺序固定：`initialize` → `notifications/initialized` → `tools/list`（按 `nextCursor`
翻页，上限 20 页）→ `tools/call`。

只允许 http/https；禁止 URL userinfo；重定向只在同 host 内跟随（最多 3 跳），跨 host 直接停止。

**stdio**：默认关闭。仅当 `JUMCP_MCP_UPSTREAM_STDIO=1` 时允许注册与连接；命令用
`tokio::process::Command` 直接 spawn（不经过 shell），显式 `.env_remove` 掉
`JUMCP_ADMIN_TOKENS` 与 `JUMCP_AGENT_TOKENS`，stdin/stdout 走行分隔 JSON-RPC，stderr 丢弃
（上游可能把 env 里的凭据打到 stderr，而 `status_message` 会出现在 GET 响应里）。
进程随连接池回收（`kill_on_drop`）。

旧版 HTTP+SSE 传输不实现。

## 5. 可见性、预算与调用解析

- 上游工具默认只对 `group ∈ scope.groups` 的请求可见/可调；组不在 scope 内时与"不存在"同构。
  例外是逐工具加入 `basic` 的成员：它们对所有 scope 可见/可调（见 §2.1），组门在**工具级**
  生效而不是整台上游级——上游组不在 scope 时，只有 `basic` 覆盖且 `eligible` 的工具出现。
- `tools/call` 的前缀回退同样受工具级组门约束：上游组不在 scope 且该 suffix 没有 `basic`
  覆盖时，回退不会命中（`-32602`），不能借旧缓存绕过组门。
- 上游工具**始终**出现在 `tools/list`（不受平铺阈值影响），并计入常驻预算；上游工具全部
  视为常驻项，`truncate` 溢出时保留它们、只裁 managed 平铺项。
- 授权是"组 ∧ 工具名 allowlist"两道：`group` 决定该上游对谁可用，token 的
  `builtin_tools` 决定该身份能看见/调用哪些**名字**（内置 id 或上游暴露名）。
  `tools/call` 的 allowlist 判定先于上游解析（只查 SQLite 前缀），被拒绝的名字不会产生
  任何上游连接或 `tools/list` 拉取；不在任何上游前缀上的名字仍返回 `-32602`。
- `read_only=false` 的上游在投影与调用两处都要求 `JUMCP_MCP_ALLOW_WRITE_UPSTREAM=1`：
  数据库里残留的写上游在开关关闭时既不可见也不可调，不只是写入期校验。
- `search_capabilities` / `describe_capability` / `list_groups` / `tools/list` 都包含上游工具；
  `list_groups` 按上游注册的组计数。
- `tools/call` 名字解析：当前投影精确匹配 → 按 `<server_id>__` 前缀回退（吃住客户端缓存的
  旧 `tools/list`）→ 未命中 `-32602`。
- 上游自身的 JSON-RPC error、连接失败、超时都整形为 MCP `isError` 结果，错误码
  `MCP_UPSTREAM_ERROR` / `MCP_UPSTREAM_UNREACHABLE` / `MCP_EXEC_TIMEOUT`，**不会**变成本服务
  的协议级错误。

## 6. 失败隔离与状态

单个上游不可达只影响它自己：其他上游与内置工具照常工作。工具缓存 TTL 30 秒；管理面写操作
（create/update/delete/refresh）立即失效对应上游的连接与缓存。不做后台轮询。

`POST .../refresh` 同时丢弃连接与工具缓存（强制重新 `initialize` + `tools/list`），不是只看
TTL 的"假探测"；失败时保留旧工具、只把状态降为 `degraded`。失败的上游在 TTL 内不重复探测
（避免每次 `tools/list` 都等一次超时）。

`status` 取缓存（不触发网络）：

| status | 含义 |
|---|---|
| `ok` | 最近一次拉取成功（有或没有工具） |
| `degraded` | 有旧工具，但最近一次拉取/调用失败 |
| `error` | 没有任何可用工具，且最近一次尝试失败 |
| `unknown` | 尚未探测（含重启后未连接） |

`POST /admin/upstreams/{id}/refresh` 会立即建连并拉取；连接失败仍返回 `200`，由
`status=error` 与 `status_message` 表达，UI 直接展示。

## 7. 审计与脱敏

- 上游调用写 `tool_call`：`object_type="tool"`、`object_id=<暴露名>`、`action="call"`；
  `data` 含 `upstream_id`、`upstream_tool`、`exposed_tool`、`duration_ms`、`outcome`、
  `definition_hash` 与 `arguments`。`arguments` 按 key 递归脱敏（`authorization` /
  `token` / `password` / `secret` / `api_key` / `apikey` / `cookie` → `***`）。
- 管理面写操作写 `admin_change`：`object_type="upstream"`，`action` ∈
  `create|update|delete|refresh|update_tool_groups`；`changes` 只含真实变化的字段（env/headers 只输出键与
  脱敏值，`changed_keys` 指出真正变了的键）；`data` 只含 `transport`/`group`/`category`/
  `read_only`/`enabled`（refresh 另含 `tool_count`/`status`；`update_tool_groups` 另含
  `basic_total`）。
- 逐工具加入 `basic` 的 `changes` 每条是 `{field:"tool_group", tool:<原始工具名>,
  before:<原组或 null>, after:<"basic" 或 null>}`（`after=null` 表示移出 `basic` 回到继承）；
  无真实变化不写审计。
- `definition_hash` 是对完整配置（含 env/headers 值）的 sha256，用于连接池失效与审计回溯；
  只输出哈希。
- 审计、日志、GET 响应都不出现上游密钥明文。

## 8. 管理面 API

所有路由挂 `/admin`，Bearer + `require_token`，写操作要求审计已启用（fail-closed）。

| 方法与路径 | 结果 |
|---|---|
| `GET /admin/upstreams` | `200 {items:[UpstreamItem], total:int}` |
| `GET /admin/upstreams/{id}` | `200 UpstreamItem`；不存在 `404 MCP_UPSTREAM_NOT_FOUND` |
| `POST /admin/upstreams` | `200 UpstreamItem`；id 重复 `409 MCP_UPSTREAM_CONFLICT` |
| `PUT /admin/upstreams/{id}` | `200 UpstreamItem`；`id` 不可改 |
| `DELETE /admin/upstreams/{id}` | `200 {deleted:true}` |
| `POST /admin/upstreams/{id}/refresh` | `200 UpstreamItem`（失败也是 200 + `status=error`） |
| `GET /admin/upstreams/{id}/tools` | `200 UpstreamTools`；只读缓存、不触网 |
| `PUT /admin/upstreams/{id}/tools/basic` | `200 {changed:[…], basic_total, tools:[UpstreamToolItem]}` |

`GET .../tools` 响应：`{status, status_message, last_checked_at, total, basic_total,
orphaned_total, tools:[UpstreamToolItem]}`；`UpstreamToolItem` = `name`（原始工具名）、
`exposed_name`（`<server_id>__<tool>`）、`group`（生效组）、`group_source`
（`basic|upstream`）、`basic`（覆盖且当前 `eligible` 的生效状态）、`overridden`（**存在**
basic 覆盖行，不论当前是否 `eligible`）、`read_only`、`eligible`（是否**可以**加入 `basic`）、
`orphaned`（有覆盖行但当前缓存无此工具）、`description`。

`overridden=true, basic=false` 表示上游配置漂移导致旧覆盖失效（上游改成写、`category=admin`
或工具名变成 `admin_` 前缀）：管理面据此展示该行并用 `null` 清理，服务端不会静默删除用户配置。

`PUT .../tools/basic` 的 body 是 `{tools:{"<原始工具名>": true|false|null}}`（`true` 加入、
`false`/`null` 移出），合并语义、幂等；`changed` 每条 `{name, before, after}`（组名或
`null`）。校验整请求粒度：`409 MCP_UPSTREAM_TOOL_UNKNOWN`（`data.unknown`）、
`409 MCP_UPSTREAM_TOOL_NOT_READ_ONLY`、`409 MCP_UPSTREAM_TOOL_NOT_ELIGIBLE`
（`data.reason`）、超过 64 键 `400 MCP_VALIDATION_ERROR`。

`UpstreamInput` = `UpstreamItem` 去掉 `status`/`status_message`/`tool_count`/`last_checked_at`/
`updated_at`。`PUT` 省略字段表示保留现有值。

`UpstreamItem` 字段（snake_case）：`id`、`name`、`transport`、`url`（http 才有值，否则
`null`）、`command`（stdio 才有值，否则 `null`）、`args`、`env`、`headers`、`group`、
`category`、`read_only`、`enabled`、`timeout_ms`、`status`、`status_message`、`tool_count`、
`last_checked_at`、`updated_at`。

### 密钥脱敏与 PUT 保留语义

- `GET` 对**敏感 header** 返回值 `***`：`Authorization`、`Cookie`、`Set-Cookie`、
  `Proxy-Authorization`，以及 key 含 `token`/`secret`/`password` 的项。
- `env` 按同一规则：key 含 `token`/`secret`/`password`（以及其他密钥类关键词）的项输出 `***`。
- `env:NAME` 是引用而非密钥，原样返回；其他普通 header/env 原样返回。
- `PUT` 收到敏感字段的 `***` 时**保留旧值**，绝不把占位符写库；新建或新增键给 `***`
  而无旧值可保留 → `400 MCP_UPSTREAM_INVALID`。

### 请求校验

| 情形 | 响应 |
|---|---|
| `id` 非法/保留/与内置 id 冲突 | `400 MCP_UPSTREAM_INVALID` / `409 MCP_UPSTREAM_CONFLICT` |
| `name` 空或超长、`category` 越界、`timeout_ms` 越界、args/env/headers 超限 | `400 MCP_UPSTREAM_INVALID` |
| `group` 不在 effective policy | `400 MCP_UPSTREAM_INVALID` |
| `http` 缺 url / 非 http(s) / 带 userinfo；`stdio` 缺 command 或带 url | `400 MCP_UPSTREAM_INVALID` |
| `transport=stdio` 但 `JUMCP_MCP_UPSTREAM_STDIO!=1` | `400 MCP_UPSTREAM_STDIO_DISABLED` |
| 切换 transport 时另一侧字段（http 的 `command`、stdio 的 `url`） | 归一化为空后落库，不报错 |
| `read_only=false` 但 `JUMCP_MCP_ALLOW_WRITE_UPSTREAM!=1` | `400 MCP_UPSTREAM_WRITE_FORBIDDEN` |
| 上游不存在 | `404 MCP_UPSTREAM_NOT_FOUND` |

## 9. 部署开关

| 变量 | 作用 |
|---|---|
| `JUMCP_MCP_UPSTREAM_STDIO=1` | 允许注册/连接 stdio 上游（默认关闭） |
| `JUMCP_MCP_ALLOW_WRITE_UPSTREAM=1` | 允许注册 `read_only=false` 的上游（默认关闭） |

两个开关都在进程启动时读取；改动需要重启。上游的增删改**不需要**重启：管理面写成功后，
下一次 `tools/list` / `tools/call` 即反映变化。

## 10. 明确不支持

- 上游的 `resources` / `prompts` / `sampling` / `logging` / `completion`；
- 逐上游透传端点与字节级反向代理；
- `notifications/tools/list_changed` 推送（`/mcp` 只有 POST，没有 GET 流）；
- 旧版 HTTP+SSE 传输；
- 依赖 `Mcp-Session-Id` 保存跨调用状态的**有状态**上游（接入前须确认其会话不承载业务状态）。
