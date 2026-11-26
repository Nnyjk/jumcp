# jumcp MCP Router：内置工具 + 动态上游 MCP server 聚合

**jumcp**（jump + mcp）是**聚合 router**：一个 `/mcp` 同时暴露内置工具与所有启用上游 MCP server 的工具。
不存在 `/mcp/<upstream>` 透传端点——鉴权、scope、预算与审计只有这一层。

## 1. 内置工具的分类与 `basic` / `admin` 组

分类按**职责**划分，是 `tools/list` / 搜索 / `/admin/tools/stats` 上的展示与检索标签：

| category | 职责 | 工具 | 所属授权组 |
|---|---|---|---|
| `basic` | 基础查询与计算：走查工具目录 / 取 schema / 按已发布模板取数 | `search_capabilities`、`describe_capability`、`list_groups`、`db_run_template` | `basic`（`scope.groups` 含 `basic` 即对 `tools/list` / `search_capabilities` / `tools/call` 可见/可调） |
| `analysis` | 工具列表分析、变更提交审计、定制化候选排名 | `admin_review_queue`、`admin_tool_candidates`、`admin_register_template`、`admin_set_visibility`、`admin_publish_capability` | `admin`（`scope.groups` 含 `admin` 即对 `tools/list` / `search_capabilities` / `tools/call` 可见/可调） |
| `admin` | 仅审计操作 | `admin_audit_queries` | `admin`（同上） |
未知 id 落 `custom`（新增内置工具忘记归类时不会冒充 `admin`）。

注解与行为一致：`admin_register_template` / `admin_set_visibility` / `admin_publish_capability`
实际写库，声明 `readOnlyHint=false`（`destructiveHint=false`，不触碰数据源）；`admin_review_queue` /
`admin_tool_candidates` / `admin_audit_queries` 与其余内置工具保持只读注解。上游工具自带 `annotations` 时原样透传，
未声明时按来源保守生成：内置包安装的上游（`catalog_id` 非空）给只读注解，手工接入的外部 MCP
给协议缺省值（`destructiveHint=true`、`readOnlyHint=false`）。上游本身没有"只读声明"字段：
外部 MCP 的执行语义不可审计，本服务不替它担保。

维护通道的分工：`admin_register_template` 只产草案（`status=draft`，不改已发布版本）、
`admin_set_visibility` 只改 `discoverable`/`callable`、`admin_publish_capability` 可发布——
**agent 自审**：审批留痕 `source="agent"`、`verified=true`（批准人身份来自已鉴权的 agent token，
不是自述），单号缺省由服务端生成 `agent:<actor_id>:<rfc3339>`。发布只推进状态、不改定义：
改 SQL/参数仍需重新登记草案，因此不存在"改完定义立即生效"的旁路。人发布仍走 HTTP 管理面
`POST /admin/capabilities/{id}/publish`，必带外部单号，留痕 `source="external"`、`verified=false`。

### `basic` 与 `admin` 是普通可授权组

`basic` 是代码内置的可授权组（`discovery::BASIC_GROUP`，`source:"builtin"`、`grantable:true`），承载上述 4 个常驻元工具；`admin` 组承载 6 个 `admin_*` 工具：

- `GET /admin/groups` 固定返回 `basic` 条目：`{code:"basic", name:"基础工具", source:"builtin", priority:0, min_budget:800, roles:[], grantable:true, capability_count: 4 + 暴露给 basic 的上游工具数, token_count:<n>}`；`source` 过滤接受 `policy|managed|builtin`。
- `TenantPolicy::validate` 仍拒绝在配置文件（`config/policy.toml`）中定义 `basic` 或 `builtin`（启动校验失败），创建/改名/删除保留组仍返回 `409 MCP_GROUP_RESERVED`（`basic`/`builtin`/`admin` 都在 `RESERVED_GROUP_NAMES`）。但 `policy.group_codes()` 恒含 `basic`、`policy.group("basic")` 返回内置定义，因此 `basic` 可以被令牌与角色正常引用（`GET /admin/tokens/options` 的 `groups` 含 `basic`）。
- `builtin` 只作为**拒绝创建的保留名**存在，不作为任何工具的 `group` 值。

`basic` 不再是隐式公共通道，而是普通可授权组：令牌不勾选 `basic` 就看不到常驻元工具，不勾选 `admin` 就看不到 `admin_*`；`X-MCP-Tool-Groups` / `_meta` 收窄掉 `basic` 或 `admin` 时同样会隐藏对应内置工具。`basic` 拒绝 `category=admin` 的上游与 `admin_` 前缀的工具，因此不会成为绕过维护通道（`admin` 组）的旁路。

分类（`basic` / `analysis` / `admin`）**不是授权组**，不改变权限判定：`admin_*` 统一归 `admin` 组，分类标签既不放宽也不收紧组门规则。

`JUMCP_AGENT_TOKENS` 启动引导导入的身份只取角色 `default_groups`，不再自动补 `basic`（需要 `basic` 时请在角色的 `default_groups` 中显式配置）。

`tools/list` 的每条内置工具带 `category`；`search_capabilities` 支持可选 `category` 过滤；`describe_capability` 与 `/admin/tools/stats` 同样输出 `category`（注册的能力与 `admin_*` 工具归 `analysis`，`admin_audit_queries` 归 `admin`，其余内置工具按 §1 的分类表）。组索引（`list_groups` 与 `tools/list` 的 `groups`）统计包含组内暴露的上游工具（按组暴露矩阵计数，常驻内置工具不计入）。
### `db_run_template`：模板执行臂

内置工具里**只有**这一条会碰数据库，也是"从审计提取高频查询 → 注册成模板 → agent 调用"
闭环的执行端。agent 不能自由拼 SQL，只能调注册期已过 sqlguard 的固定查询路径。

路由器自己不持有数据库连接：模板在定义里用 `target` 声明由**哪个已接入的上游 MCP 工具**代劳，
执行时只是"渲染 → 经该上游工具调用"。

**方言是隐含契约**：注册期编译与渲染期门禁都按**上游的 SQL 方言**走，当前固定 ClickHouse
（`main.rs` 用 `TemplateRegistry::with_dialect(store, Dialect::ClickHouse)` 构造；
`TemplateRegistry::new()` 的 PostgreSQL 默认只服务单元测试）。`render_inline` 只认 `?` 占位符，
`validate_rendered` 也按同一方言跑。因此模板 SQL 与 `target` 指向的上游必须与 ClickHouse 词法
兼容——这是当前契约，不是缺陷：接入方言不兼容的上游时，模板会在注册期或渲染期被门禁拒绝，
而不是悄悄发出错误语法。注册期不做方言探测，`target` 也没有方言字段。

```toml
[[templates]]
template_id = "order.daily_summary"
group = "db.read.analytics"
risk = "read"
sql = "SELECT day, count(*) AS orders FROM public.orders WHERE day = :day"

[target]                  # 必填：没有它模板注册就被拒（data.field="target"）
upstream = "ch"           # 已接入的上游 id（不含 __、不是保留 id）
tool = "query"            # 上游工具名（不得以 admin_ 开头）
database = "analytics"    # 可选：透传给上游的默认库
```

`inputSchema`：`{"template_id": string(必填), "params": object, "version": integer}`。
`params` 按模板定义里的具名参数取值（缺失/类型/枚举/column_ref 白名单在渲染期判定）。

调用顺序是契约（`backend/crates/jumcp-server/src/mcp/dispatch/template.rs`）：

1. **调用面判定**：`published ∧ callable ∧ group ∈ scope`（`Service::callable_capabilities`
   + `catalog::find`）。可见性（`discoverable`）不决定可调用性，组不在 scope 内与"不存在"同构，
   一律 `MCP_CAPABILITY_NOT_FOUND`。
2. **版本钉住**：`TemplateRegistry::pin(session_key(actor), template_id, version, current)`，
   键是 `<tenant>:<actor_id>`。同一会话显式换版本 → `MCP_VERSION_CONFLICT`
   （`data.pinned`/`data.requested`）。钉住只在进程内存里，重启或换 agent 身份即失效。
3. **渲染**：`CompiledTemplate::render_inline(&params)` → `RenderedSql{sql, params_hash}`。
   命名占位符在注册期已改写为 `?`，渲染期按 `bind_order` 依次替换成**转义后的字面量**
   （字符串 `\`→`\\`、`'`→`\'`；数字原样；`NULL`；`??` 是字面 `?`）。渲染完再用
   `preflight::sql::check` 前检一遍（`MCP_SQL_BLOCKED`），防止拼接产生注入。
4. **结果缓存**：键 `(template_id, version, params_hash)`，TTL 取模板的 `result_cache_ttl_s`
   （≤0 不缓存，上限 300 秒）。命中即不触达上游、不再计预算。
5. **执行**：`UpstreamManager::resolve(scope, "{target.upstream}__{target.tool}")`，解析不到
   → `MCP_UPSTREAM_NOT_FOUND`（`data.upstream`/`data.tool`）；参数是
   `{"sql": <渲染后 SQL>, "database"?: <target.database>}`。上游的失败码原样透传
   （`MCP_UPSTREAM_UNREACHABLE` / `MCP_EXEC_TIMEOUT` / …），审计 `outcome` 用上游给的
   `error`/`unknown`；成功时从 `structuredContent`（缺则解析首个 `content.text` 的 JSON）取
   `{columns, rows, row_count, elapsed_ms}`，`elapsed_ms` 缺失才回落到本地计时。
6. **结果预算**：结果 token 数超过模板 `constraints.max_result_tokens` → `MCP_BUDGET_EXCEEDED`
   （`data.scope="max_result_tokens"`）；否则记入会话预算，预算溢出按服务端策略
   `OverflowPolicy::Fail`（回错误）或 `Truncate`（回摘要）。

返回：`structuredContent` 为 `{content, cache: "hit"|"miss", version}`；`content` 里带上
`template_id`/`version`/`cache`/`variant`、上游的 `columns`/`rows`/`row_count`/`elapsed_ms`
与预算 `summary`。预算摘要模式下只回 `{row_count, summary:true, template_id, version}`。

审计事件 `template_run` 的关键 `data`：`executed_script`（真正发出去的 SQL）、`bound_params`
（调用方给的原始参数）、`upstream_id`、`upstream_tool`、`tokens`、`cache`、`elapsed_ms`、
`version`、`row_count`、`truncated`。

**组门与 `db_run_template`**：`db_run_template` 归属 `basic` 组；令牌的 `groups` 不含 `basic`（或请求收窄掉 `basic`）时，调用它会拿到 `MCP_CAPABILITY_NOT_FOUND`（`data.tool="db_run_template"`，拒绝判定记 `policy_decision.detail.surface="mcp/builtin-groups"`），该工具也不会出现在 `tools/list` 里。要放行就通过 `PATCH /admin/tokens/{id}` 为令牌勾选 `basic` 组。`tools/list` 与 `tools/call` 用同一处判定，所以"列表里没有"就"调不了"，不会出现半可见。

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
| `category` | `basic` / `analysis` / `admin` / `custom` |
| `enabled` | 默认 `true`；`false` 时不投影也不转发 |
| `timeout_ms` | 1..120000，默认 10000；作用于建连、`tools/list` 与 `tools/call` |
| `catalog_id` / `catalog_version` | 只读坐标，由内置包安装时写入（§3）；不在 `PUT` 的可写字段里，手工创建为 `null` |

`env` / `headers` 的值允许两种形态：字面量，或 `env:NAME`（建连时从进程环境解析，
变量缺失即建连失败，不静默空串）。两种传输都在建连时解析 `env` 引用：`http` 不使用 `env`，
但写错的引用同样会失败（避免切换到 `stdio` 后才暴露配置错误）。

### 2.1 组暴露矩阵（两层暴露）

控制面采用组内两层暴露矩阵，上游本身不绑定组：

1. 第一层 `mcp_group_upstreams(group_code, upstream_id)`：组对整台上游授予；授予后该上游的全部工具对该组可见（除非被第二层排除）。
2. 第二层 `mcp_group_tool_rules(group_code, upstream_id, tool_name, state)`，`state ∈ include|exclude`：对单工具的显式规则，**优先于第一层**；无规则行表示继承第一层（即三态：`inherit` / `include` / `exclude`）。

- 判定按组独立求值，互不影响；工具对一次请求可见 ⇔ `scope.groups` 中**至少一个**组判定可见（没有隐式公共通道）。
- `admin` 类别上游（`category=admin`）与 `admin_` 前缀工具只能进 `admin` 组：授予整台 `admin` 类别上游给非 `admin` 组，或对 `admin` 类别上游 / `admin_` 前缀工具写 `include` 到非 `admin` 组，一律返回 `409 MCP_UPSTREAM_TOOL_NOT_ELIGIBLE`（`data.reason` 为 `upstream_category_admin` 或 `admin_prefix`）。
- 单组单上游规则上限 `GROUP_TOOL_RULE_MAX = 128`；单次 `PUT /admin/groups/{code}/exposure` 提交变更上限 `512`（`GROUP_TOOL_RULE_MAX * 4`）。
- 暴露矩阵变更采用**合并**语义，整请求先校验后落库（不做部分写入）：未知工具 `409 MCP_UPSTREAM_TOOL_UNKNOWN`（`data.unknown`）、不合资格 `409 MCP_UPSTREAM_TOOL_NOT_ELIGIBLE`、非法规则状态或超限 `400 MCP_VALIDATION_ERROR`、上游不存在 `404 MCP_UPSTREAM_NOT_FOUND`。
- 上游改名/刷新导致规则行对不上当前缓存时：规则保留并在 `GET /admin/groups/{code}/exposure` 中标 `orphaned=true`，由管理面提交 `null` 显式清理（`orphaned` 工具只允许提交 `null`）；服务端不静默删除配置，也不让孤行参与投影。
- 工具缓存仍是进程内 30s TTL、无持久化：`GET /admin/upstreams/{id}/tools` 与 `GET /admin/groups/{code}/exposure` 只读缓存（不触网），需要最新清单时显式 `POST /admin/upstreams/{id}/refresh`。

### 2.2 存量库迁移（`CapabilityStore::open`）

旧模型到组暴露矩阵的迁移在 `CapabilityStore::open` 内、任何读写之前自动执行，幂等且只按旧结构是否存在判断（不额外记标记）：

- `mcp_upstream_tool_groups` → 迁入 `mcp_group_tool_rules(state='include')` 后 `DROP TABLE mcp_upstream_tool_groups`；
- `mcp_upstreams.group_code` → 迁入 `mcp_group_upstreams` 后 `DROP INDEX IF EXISTS idx_upstream_group` 与 `ALTER TABLE mcp_upstreams DROP COLUMN group_code`；
- `access_tokens.builtin_tools_json` → `ALTER TABLE access_tokens DROP COLUMN builtin_tools_json`。

**不补授权**：旧令牌的工具白名单没有等价物，升级后不会自动回填 `basic`；管理员需为需要常驻元工具的令牌显式勾选 `basic` 组。
## 3. 内置 MCP 包

「内置包」把「一个已知的 MCP server + 一份配置模板」收敛成一次安装。目录来自**代码内静态表**
（`jumcp-core::builtin`，由各插件 crate 声明），没有远程拉取，也不接受任意 URL 安装。
安装产物是一台普通 HTTP 上游，URL 指向本进程的内嵌端点
`POST /internal/mcp/{package_id}/{upstream_id}`；`/admin/upstreams` 可见、可编辑（包管理字段除外）、
可单独删除，运行期只有 HTTP 上游这一条路径。插件标准与端点协议见 `docs/builtin-mcp.md`。

| 目录条目字段 | 说明 |
|---|---|
| `id` / `name` / `summary` / `version` | 包标识、展示名、摘要、版本 |
| `tags` | 检索标签 |
| `transport` | 恒为 `http`（内置包只经内嵌 HTTP 端点接入） |
| `default_upstream_id` | 未传 `upstream_id` 时的上游 id |
| `fields` | 安装表单字段：`key`、`label`、`kind`（只有 `text` 与 `password` 两种）、`required`、`secret`、`default`、`placeholder`、`help` |
| `tools` | 包声明的工具清单 `{name,summary}`，用于安装前展示；实际工具以 `tools/list` 为准 |
| `installations` | `mcp_upstreams` 里 `catalog_id` 等于包 id 的行，见下 |

字段到上游 env 的映射由包定义固定（字段声明里的 `env`）。`secret=true` 的字段**必须密文提交**：
管理台用 `GET /admin/transport-key` 下发的 `JUMCP_TRANSPORT_KEY` 在浏览器里把它加密成
`enc:v1:<hex>`（AES-256-GCM，与落库同一线格式），服务端用传输密钥解密后再用
`JUMCP_SECRET_KEY` 重新加密落库。明文提交直接拒绝（`MCP_TRANSPORT_CIPHERTEXT_REQUIRED`），
不存在「解不开就当明文用」。目录、安装响应与上游 `GET` 都只回显
非密字段，密字段只给 `secrets_set: {key: bool}`——明文与密文都不出现。`config` 的值一律按字符串提交；
`kind` 只决定表单控件（`password` 是给 `secret` 字段用的输入框），不做取值校验。

`installations` 每条：`upstream_id`、`name`、`enabled`、`transport`、`url`、
`config`（仅非密字段的当前值）、`secrets_set`、`catalog_version`、`created_at`、`updated_at`。
目录不触网，因此不含探测状态。

当前目录只有 `clickhouse-basic`：字段 `url`（必填，ClickHouse HTTP 端点，如
`http://host:8123`；不接受带 userinfo 的 URL，缺端口时 http 记 8123、https 记 8443）、
`username`（必填，默认 `default`）、`password`（密）、`database`（可选）、
`allowed_schemas`（可选，逗号分隔的 database 白名单；白名单非空时，未指定库的查询以列表**首个库**
为会话库，多库场景请在 SQL 里写库前缀；库名按实际大小写填写，比较不区分大小写）；
工具 `query` / `list_databases` / `list_tables`。

### 3.1 安装

`POST /admin/mcp/catalog/{id}/install`：

```json
{
  "upstream_id": "clickhouse",
  "name": "ClickHouse 基础查询",
  "enabled": true,
  "config": {
    "url": "http://127.0.0.1:8123",
    "username": "default",
    "password": "enc:v1:<hex(12B nonce ‖ ciphertext+tag)>"
  }
}
```

- `password` 这类 `secret=true` 字段的值必须是 `JUMCP_TRANSPORT_KEY` 下的 `enc:v1:` 密文；
  `enc:v1` 里没有明文密码，日志/审计/错误响应也不会带上它。空串表示未提供（不写该 env）。
- `upstream_id` 缺省取目录的 `default_upstream_id`，`name` 缺省取包的展示名，`enabled` 缺省 `true`。
  安装时不绑定授权组，安装后在组暴露矩阵（`PUT /admin/groups/{code}/exposure`）中授权。
- 生成的上下游固定为：`transport="http"`、`url={端点}/internal/mcp/{package_id}/{upstream_id}`、
  `command`/`args` 为空；`env` 由 `config` 按字段映射生成（值会 `trim`；未填且字段有 `default` 时用
  默认值；仍为空的可选字段不写入 `env`；secret 值加密后写入）；`category="custom"`、
  `timeout_ms=30000`；`catalog_id`/`catalog_version` 记录安装来源；id/name 校验与 `validate()` 与手工创建一致。
- 成功 `200 {"upstream": UpstreamItem}`（敏感 env 已脱敏），审计沿用 upstream `create`，
  `data` 带 `catalog_id`。

### 3.2 配置编辑

`PATCH /admin/mcp/catalog/{id}/install/{upstream_id}/config`，body `{"config": {fieldKey: string}}`：

- 只更新出现的 key；未出现的 key 保持原值。secret 字段传空串 = 保持原值（浏览器回填的空输入框
  不能顺手清掉密码）；非空值必须是 `enc:v1:` 传输密文，明文会被 `400
  MCP_TRANSPORT_CIPHERTEXT_REQUIRED` 拒绝。非密字段传空串表示清空，清空必填字段会被拒。
- 合并后重新校验必填（secret 以"已有非空或本次提供"为准）；校验失败不写库。未提交的 secret
  不解密，因此换过 `JUMCP_SECRET_KEY` 之后改别的字段仍然成功，旧密文原地保留。
- 成功 `200 {"installation": InstallationItem}`（同目录 `installations` 的元素），写 `admin_change`
  审计（secret 一律 `***`）。密钥未显式改写时保留磁盘上的旧密文，不做无意义的重加密。

### 3.3 卸载

`DELETE /admin/mcp/catalog/{id}/install/{upstream_id}`：只有 `catalog_id` 等于该包 id 的行会被
删除（连同工具组覆盖与连接缓存，写 upstream `delete` 审计）；手工创建或由别的包安装的上游
一律 `404 MCP_UPSTREAM_NOT_FOUND`。卸载不动历史审计；包 id 不存在时先报 `404 MCP_CATALOG_NOT_FOUND`。

### 3.4 错误码

错误按下表自上而下判定（部署能力先于请求内容）：

| 情形 | 响应 |
|---|---|
| 包 id 不存在（安装/卸载/改配置） | `404 MCP_CATALOG_NOT_FOUND` |
| 进程没有内置端点（stdio 模式）或没有 `JUMCP_SECRET_KEY` | `503 MCP_BUILTIN_UNAVAILABLE`（message/hint 说明需要 HTTP 模式 + 密钥） |
| 进程没有 `JUMCP_TRANSPORT_KEY`（非 HTTP 构造下才会出现） | `503 MCP_TRANSPORT_KEY_MISSING` |
| 缺必填字段（`data.field` 指向字段 key，message 用字段 `label`）、`config` 出现包未声明的键（message「未知配置字段：k1,k2」，`data.field` 为首个未知键，`data.unknown` 列出全部、按字典序） | `400 MCP_VALIDATION_ERROR` |
| `secret=true` 字段的非空值不是 `enc:v1:` 密文 | `400 MCP_TRANSPORT_CIPHERTEXT_REQUIRED`（message「secret 字段必须密文传输：&lt;key&gt;」） |
| `secret` 密文用传输密钥解不开（密钥已轮换/被篡改） | `400 MCP_TRANSPORT_DECRYPT_FAILED`（message「secret 字段密文无法解密：&lt;key&gt;」） |
| `secret` 密文解出来是空串 | `400 MCP_VALIDATION_ERROR`（message「secret 字段不能为空：&lt;key&gt;」） |
| `secret` 密文超过 4096 字符 | `400 MCP_VALIDATION_ERROR`（`data.max_len`） |
| `upstream_id` 已存在或为保留 id | `409 MCP_UPSTREAM_CONFLICT` |
| 卸载/改配置时该上游不是这个包安装的 | `404 MCP_UPSTREAM_NOT_FOUND` |

校验失败不落行：失败请求不会留下半配置的上游。

### 3.5 传输密钥与密文提交

`GET /admin/transport-key`（需管理鉴权，响应 `Cache-Control: no-store`）：

```json
{ "algorithm": "AES-256-GCM", "key": "<64 位小写 hex>" }
```

- 密钥就是进程的 `JUMCP_TRANSPORT_KEY`。管理台在浏览器里用 WebCrypto 的 AES-256-GCM 加密
  `secret=true` 的字段，线格式与落库**完全一致**：`enc:v1:` + `hex(12 字节随机 nonce ‖ ciphertext ‖ tag)`，
  AAD 为空，hex 小写。服务端解出明文后用 `JUMCP_SECRET_KEY` 重新加密再落库——两把密钥用途分离，
  传输密钥泄漏不等于库里的密文可解，反之亦然。
- 为什么经鉴权端点下发而不是编译进管理台 bundle：静态资源不需要登录就能下载，密钥进构建产物等于公开。
- 轮换：换掉 `JUMCP_TRANSPORT_KEY` 后，未重新提交的旧传输密文不可解，保存会得到
  `MCP_TRANSPORT_DECRYPT_FAILED`，重新填一次密码即可；落库密文由 `JUMCP_SECRET_KEY` 管理，
  与传输密钥轮换无关。
- **边界**：这把共享密钥随页面下发，只让「请求体里的密码是密文」，避免密码在请求日志、抓包、浏览器
  表单回放里直接可读；它**不替代 TLS**，能读到页面与响应的中间人同样能拿到密钥。

### 3.6 与手工上游的关系

安装产物没有特殊运行期路径，但有**包管理字段**：`transport`、`url`、`command`、`args`、`env`、
`headers` 由包维护，`PUT /admin/upstreams/{id}` 改它们会 `409 MCP_UPSTREAM_PACKAGE_MANAGED`
（`data.fields` 列出被改的字段）。`name`、`enabled`、`timeout_ms` 仍可改，
启停按钮照常可用；要改配置请走 §3.2 的 PATCH。`PUT` 保留 `catalog_id`/`catalog_version`
（UPDATE 语句不含这两列），`DELETE /admin/upstreams/{id}` 也能删。

## 4. 命名与冲突

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

## 5. 传输

**HTTP Streamable MCP**：`POST` 上游 URL，`Accept: application/json, text/event-stream`；
响应既可以是单个 JSON 报文，也可以是 SSE 事件流（逐 `data:` 事件解析，取与请求 id 匹配的
JSON-RPC 响应）。`initialize` 响应头里的 `Mcp-Session-Id` 会被记住并附加到后续请求。
握手顺序固定：`initialize` → `notifications/initialized` → `tools/list`（按 `nextCursor`
翻页，上限 20 页）→ `tools/call`。

只允许 http/https；禁止 URL userinfo；重定向只在同 host 内跟随（最多 3 跳），跨 host 直接停止。

**stdio**（仅手工上游；内置包不走这条路）：默认关闭。仅当 `JUMCP_MCP_UPSTREAM_STDIO=1` 时允许
注册与连接；命令用
`tokio::process::Command` 直接 spawn（不经过 shell；`command` 按 §10 的查找顺序解析
——`JUMCP_MCP_BUNDLED_BIN_DIR` → 服务同目录 → `PATH`，随包分发的
连接器不必写绝对路径），显式 `.env_remove` 掉
`JUMCP_AGENT_TOKENS`，stdin/stdout 走行分隔 JSON-RPC，stderr 丢弃
（上游可能把 env 里的凭据打到 stderr，而 `status_message` 会出现在 GET 响应里）。
进程随连接池回收（`kill_on_drop`）。

旧版 HTTP+SSE 传输不实现。

### 5.1 `/mcp` 鉴权

凭证取自 `Authorization: Bearer <平台 token>`；**没有该头**时回落到查询参数 `access_token`
（`POST /mcp?access_token=<token>`）。只认 `access_token` 这一个参数名，空值等于没带。

- 回落仅限 MCP 面：`/admin/*` 与 `/internal/mcp/*` 一律只认头，URL 上的 token 不被接受。
- 头与 query 同时给以头为准（不会因为 query 里带了无效 token 而拒绝带合法头的请求）。
- 两种来源走同一条校验（`access_tokens` 查表 + 常量时间比较），未知/撤销/过期一律
  `MCP_UNAUTHORIZED`（HTTP 200 + JSON-RPC 错误），不因"没头"放行 `initialize`；
  审计里的 `actor.token_id` / `source` / 组同样由该 token 派生。
- 为什么存在这条通道：不支持自定义请求头的 MCP 客户端（例如只在 `tools/call` 上带
  `Authorization` 的 mcp-go 系客户端）会在 `initialize` 阶段被拒。**不支持自定义头的
  MCP 客户端可用 `?access_token=`（仅 /mcp；token 会出现在 URL 中，故仅限内网、且应可轮换）**。


## 6. 可见性、预算与调用解析

- `tools/list` 与 `search_capabilities` 的可见集合 = 注册表可见能力 ∪ 组内暴露的上游工具。
- 上游工具的可见性由组暴露矩阵决定（第一层整台授予 + 第二层逐工具 `inherit` / `include` / `exclude`），
  `scope.groups` 中**至少一个**组判定可见时才出现在 `tools/list` 并可被调用；没有组放行时与"不存在"同构。
  `UpstreamManager::projection` 逐工具按矩阵过滤，取消整台组命中即全放行的短路。
- `tools/call` 的上游门禁在 `resolve`（会触网）之前完成，拒绝写 `policy_decision`
  （`detail.surface="mcp/upstream-groups"`），不会产生任何上游连接或 `tools/list` 拉取；
  前缀回退同样受工具级组门约束，未命中返回 `-32602`，不能借旧缓存绕过组门。
  内置工具门禁的审计 `detail.surface="mcp/builtin-groups"`。
- 上游工具**始终**出现在 `tools/list`（不受平铺阈值影响），并计入常驻预算；上游工具全部
  视为常驻项，`truncate` 溢出时保留它们、只裁 managed 平铺项。
- 上游没有"只读声明"：`enabled` 与组暴露矩阵是唯一的准入条件；SQL/文件/接口前置
  拦截仍按入参形状生效，但不承诺识别上游内部语义（见 `docs/preflight.md` 的边界说明）。
- `search_capabilities` / `describe_capability` / `list_groups` / `tools/list` 都包含组内暴露的上游工具；
  `list_groups` 按组暴露矩阵统计各组计数。
- `tools/call` 名字解析：当前投影精确匹配 → 按 `<server_id>__` 前缀回退（吃住客户端缓存的
  旧 `tools/list`）→ 未命中 `-32602`。
- 上游自身的 JSON-RPC error、连接失败、超时都整形为 MCP `isError` 结果，错误码
  `MCP_UPSTREAM_ERROR` / `MCP_UPSTREAM_UNREACHABLE` / `MCP_EXEC_TIMEOUT`，**不会**变成本服务
  的协议级错误。

## 7. 失败隔离与状态

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

## 8. 审计与脱敏

- 上游调用写 `tool_call`：`object_type="tool"`、`object_id=<暴露名>`、`action="call"`；
  `data` 含 `upstream_id`、`upstream_tool`、`exposed_tool`、`duration_ms`、`outcome`、
  `definition_hash` 与 `arguments`。`arguments` 按 key 递归脱敏（`authorization` /
  `token` / `password` / `secret` / `api_key` / `apikey` / `cookie` → `***`）。
- 模板执行写 `template_run`（MCP 面唯一会碰数据源的内置工具，也是 `/admin/tools/stats`
  与 `/admin/tools/{id}/calls` 里模板行的唯一数据来源）：`object_type="template"`、
  `object_id=<template_id>`、`action="invoke"`、`outcome="ok"|"summary"|"unknown"|"error"`，
  `data` 字段固定为：
  | 键 | 含义 |
  |---|---|
  | `arguments` | agent 的原始入参（`template_id`/`params`/`version`，不脱敏） |
  | `executed_script` | 渲染后的 SQL（命名占位符已改写为 `?`） |
  | `bound_params` | 渲染后的位置参数数组 |
  | `tokens` | 结果 token 数（i64） |
  | `cache` | `"hit"` / `"miss"` |
  | `elapsed_ms` | i64；缓存命中固定 0 |
  | `version` | 实际执行的模板版本（i64） |
  | `row_count` / `truncated` | 成功路径才有 |

  失败（含执行前被拒：模板不存在/未发布/组不在 scope/版本冲突/渲染失败）写同一条
  `template_run`，`outcome="error"`（执行结果未知时 `"unknown"`），`payload` 顶层带
  `error_code`/`error_message`。模板调用**不再**同时写 `tool_call`，两类事件不会对同一次
  失败各计一遍。
- 管理面写操作写 `admin_change`：
  - 上游操作：`object_type="upstream"`，`action` ∈ `create|update|delete|refresh`；`changes` 只含真实变化的字段（env/headers 只输出键与脱敏值，`changed_keys` 指出真正变了的键）；`data` 只含 `transport`/`category`/`enabled`（refresh 另含 `tool_count`/`status`；由内置包安装的上游另含 `catalog_id`/`catalog_version`）。
  - 组暴露矩阵：`object_type="group"`，`action` ∈ `group_upstreams|group_tool_rules`（逐工具规则变更的 `changes` 字段为 `field:"tool_rule"`）；无真实变化不写审计。
- `definition_hash` 是对完整配置（含 env/headers 值）的 sha256，用于连接池失效与审计回溯；
  只输出哈希。
- `/admin/audit-events` 的 `surface` 过滤为 `admin|mcp|all`，**缺省 `all`**：MCP 面的
  `tool_call` 与 agent 发起的 `admin_change` 因此对平台可见；鉴权噪音用 `event_type` /
  `actor_id` 收窄，不由面隐藏。
- 审计、日志、GET 响应都不出现上游密钥明文。

## 9. 管理面 API

### 9.0 两个监听面（端口分面）

进程在 HTTP 模式下起**两个**监听，各自只挂自己的路由表（不是同一路由表两倍监听）：
一个面上不存在另一个面的端点，跨面访问一律 `404 MCP_NOT_FOUND`（**不是** 403/405，
避免客户端误以为"换个方法/补个凭证就能用"）。

| 面 | 端口变量（默认） | 挂载 | 给谁 |
|---|---|---|---|
| MCP 面 | `JUMCP_MCP_PORT`（8000） | `/mcp`、`/internal/mcp/{package}/{upstream}`、`/health` | agent / 内置包回环 |
| 管理面 | `JUMCP_ADMIN_PORT`（8080） | `/admin/*`、`/admin/login`、`/admin/logout`、`/admin/session`、`/health`、管理台静态站 + SPA 回退 | 浏览器 |

- 两面端口**相同即拒绝启动**（`JUMCP_ADMIN_PORT 与 JUMCP_MCP_PORT 相同（…）：管理面与 MCP 面必须使用不同端口`）；
  监听失败/端口占用会点名具体变量与地址。
- `/health` 两面都留：healthcheck 不该因为选错端口而失败。
- 内置 MCP 包的回环地址 `BuiltinEndpoint::local(host, JUMCP_MCP_PORT)` 永远绑 **MCP 面**端口
  （绑错端口时症状是 404 而不是鉴权失败，很难查）。
- 管理面不托管 `/mcp`，MCP 面不托管任何页面：根路径在 MCP 面是 JSON 404。

### 9.1 登录与会话（管理面只认账号密码 + cookie）

管理面没有共享令牌：身份来自管理员账号登录换来的 HttpOnly cookie。

| 方法与路径 | 鉴权 | 结果 |
|---|---|---|
| `POST /admin/login` | 不需要 | body `{"username":"<账号>","password":"<口令>"}` → `204` + `Set-Cookie: jumcp_session=<opaque>; HttpOnly; SameSite=Strict; Path=/; Max-Age=<ttl>`（`Secure` 仅在 `JUMCP_ADMIN_COOKIE_SECURE=1` 时加）；失败一律 `401 MCP_UNAUTHORIZED`，message 固定 `用户名或密码错误`（账号不存在 / 口令错 / 已停用不区分）。带 `Origin`/`Referer` 且跨站 → `403 MCP_FORBIDDEN`（不带的客户端照常放行）；body 非 JSON 或超 8KB → `400 MCP_BAD_REQUEST`，`data={"field":"body"}`，**不计入限流** |
| `POST /admin/logout` | 可选 cookie | `204`（幂等）+ 立即吊销会话、下发 `Max-Age=0` 的同名 cookie |
| `GET /admin/session` | cookie | `200 {"actor":{"id":"<username>","user_id":"<32 位 hex 账号主键>","role":"admin","type":"admin"}}`；未登录 `401` |
| `GET /admin/me` | cookie | `200 {"id":"<username>","user_id":"<32 位 hex>","role":"admin","type":"admin"}`（平铺，与 session 的 `actor` 字段集合一致）；未登录 `401` |
| `GET/POST /admin/admins`、`PATCH/DELETE /admin/admins/{id}` | cookie | 管理员账号管理，契约见 `docs/audit-openness.md` §7.1.1 |

- 账号存在 `registry.sqlite3` 的 `admin_users(id, username, password_hash, state, created_at, updated_at, last_login_at)`：
  口令是 Argon2id 哈希（`$argon2id$` 前缀），登录时用 password-hash 的常量时间比较。
  空库首次启动由 `JUMCP_ADMIN_USERNAME` / `JUMCP_ADMIN_PASSWORD`（≥12 位）播种；缺变量或口令不合规即拒绝启动。
- 会话存在 `admin_sessions(id, admin_user_id, created_at, expires_at, last_seen_at)`：
  `id` 是 32 字节 CSPRNG 的 hex（不可预测、与服务端记录常量时间比较）；
  TTL 由 `JUMCP_ADMIN_SESSION_TTL_SECS`（默认 43200 = 12 小时）控制，登录时顺手清理过期行。
  改口令或停用账号会吊销该账号的全部会话。
- `/admin/*` 鉴权：**只认 cookie**。没有 cookie 或会话无效（含账号已停用/已删除）一律 `401 MCP_UNAUTHORIZED`，
  不回落到任何 Bearer。脚本也走 `POST /admin/login` 拿 cookie。
- CSRF：方法非 `GET`/`HEAD` 时要求 `Origin`（或 `Referer`）与 `Host` 同源，
  否则 `403 MCP_FORBIDDEN`（缺头同样拒绝，fail-closed）。`SameSite=Strict` 保留作为第二道防线。
- 登录限流：按 `username` 计，60 秒内连续失败 5 次后 `429 MCP_RATE_LIMITED`。
  计数在**进程内存**里，因此前提是单副本部署（与 SQLite 单写者约束一致），滚动重启会清零。
  写入前按当前时刻清除过期项，且总量封顶 1024 条（超限淘汰窗口起点最旧的一条）——
  否则随机用户名轰炸就是一条可控的内存增长路径。
- cookie 未启用 `Secure` 时启动日志会 warn 一次（"请仅在内网使用或启用 TLS"）：TLS 终止后
  仍需显式置 `JUMCP_ADMIN_COOKIE_SECURE=1`，否则 cookie 会以明文经 HTTP 传输。

所有路由挂 `/admin`，写操作要求审计已启用（fail-closed）。

| 方法与路径 | 结果 |
|---|---|
| `GET /admin/upstreams` | `200 {items:[UpstreamItem], total:int}` |
| `GET /admin/upstreams/{id}` | `200 UpstreamItem`；不存在 `404 MCP_UPSTREAM_NOT_FOUND` |
| `POST /admin/upstreams` | `200 UpstreamItem`；id 重复 `409 MCP_UPSTREAM_CONFLICT` |
| `PUT /admin/upstreams/{id}` | `200 UpstreamItem`；`id` 不可改；包安装行的包管理字段被改时 `409 MCP_UPSTREAM_PACKAGE_MANAGED` |
| `DELETE /admin/upstreams/{id}` | `200 {deleted:true}` |
| `POST /admin/upstreams/{id}/refresh` | `200 UpstreamItem`（失败也是 200 + `status=error`） |
| `GET /admin/upstreams/{id}/tools` | `200 UpstreamTools`；只读缓存、不触网 |
| `GET /admin/groups/{code}/exposure` | `200 GroupExposure`；只读缓存、不触网 |
| `PUT /admin/groups/{code}/exposure` | `200 {exposure:GroupExposure, changes:{upstreams:int, tools:int}}` |
| `PATCH /admin/tokens/{id}` | `200 TokenItem`；改 `name`/`groups`/`expires_at`，不换密钥、不改 `role` |
| `GET /admin/tokens/options` | `200 {roles, groups}`（`groups` 含 `basic`） |
| `GET /admin/mcp/catalog` | `200 {items:[CatalogItem]}`；目录 + 主机状态 + `installations`，见 §3 |
| `POST /admin/mcp/catalog/{id}/install` | `200 {upstream:UpstreamItem}`；错误码见 §3.4 |
| `PATCH /admin/mcp/catalog/{id}/install/{upstream_id}/config` | `200 {installation:InstallationItem}`；见 §3.2 |
| `DELETE /admin/mcp/catalog/{id}/install/{upstream_id}` | `200 {deleted:true}`；非本包安装 `404 MCP_UPSTREAM_NOT_FOUND` |
| `GET /admin/transport-key` | `200 {algorithm:"AES-256-GCM", key:"<64 位 hex>"}`，`Cache-Control: no-store`；未配置 `503 MCP_TRANSPORT_KEY_MISSING`，见 §3.5 |

`GET /admin/upstreams/{id}/tools` 响应：`{groups:[string], tools:[{name, exposed_name, description, admin_only}], total, status, status_message, last_checked_at}`；其中 `groups` 为第一层授予整台上游的组 code 列表，只读缓存不触网。

`GET /admin/groups/{code}/exposure` 响应：`{group, upstreams:[{id, name, category, admin_only, enabled, granted, tool_count}], tools:[{upstream_id, tool, exposed, state, visible, admin_only, orphaned}]}`（只读缓存、不触网；停用的上游同样列出，否则其残留规则没有清理入口）。

`PUT /admin/groups/{code}/exposure` 的 body 是 `{upstreams:{id: bool}, tools:{upstream_id: {tool: "include"|"exclude"|null}}}`，合并语义、幂等，整请求先校验后落库（不做部分写入）；`orphaned` 工具只允许提交 `null`。错误码：`409 MCP_UPSTREAM_TOOL_UNKNOWN`（`data.unknown`）、`409 MCP_UPSTREAM_TOOL_NOT_ELIGIBLE`（`data.reason` 为 `upstream_category_admin` 或 `admin_prefix`）、`400 MCP_VALIDATION_ERROR`（非法规则状态或单次变更超过 512 / 单组规则超过 `GROUP_TOOL_RULE_MAX=128`）、`404 MCP_UPSTREAM_NOT_FOUND`。
`UpstreamInput` = `UpstreamItem` 去掉 `status`/`status_message`/`tool_count`/`last_checked_at`/
`updated_at`。`PUT` 省略字段表示保留现有值。

`UpstreamItem` 字段（snake_case）：`id`、`name`、`transport`、`url`（http 才有值，否则
`null`）、`command`（stdio 才有值，否则 `null`）、`args`、`env`、`headers`、
`category`、`enabled`、`timeout_ms`、`status`、`status_message`、`tool_count`、
`last_checked_at`、`updated_at`、`catalog_id`、`catalog_version`（后两者由内置包安装写入，手工创建为 `null`）。

### 密钥脱敏与 PUT 保留语义

- `GET` 对**敏感 header** 返回值 `***`：`Authorization`、`Cookie`、`Set-Cookie`、
  `Proxy-Authorization`，以及 key 含 `token`/`secret`/`password` 的项。
- `env` 按同一规则（小写 key 含下列任一即脱敏）：`authorization`、`cookie`、`token`、`secret`、
  `password`、`api-key`、`apikey`、`dsn`——内置包写入的 `JUMCP_CLICKHOUSE_PASSWORD` 因此显示 `***`。
- `env:NAME` 是引用而非密钥，原样返回；其他普通 header/env 原样返回。
- `PUT` 收到敏感字段的 `***` 时**保留旧值**，绝不把占位符写库；新建或新增键给 `***`
  而无旧值可保留 → `400 MCP_UPSTREAM_INVALID`。

### 请求校验

| 情形 | 响应 |
|---|---|
| `id` 非法/保留/与内置 id 冲突 | `400 MCP_UPSTREAM_INVALID` / `409 MCP_UPSTREAM_CONFLICT` |
| `name` 空或超长、`category` 越界、`timeout_ms` 越界、args/env/headers 超限 | `400 MCP_UPSTREAM_INVALID` |
| `http` 缺 url / 非 http(s) / 带 userinfo；`stdio` 缺 command 或带 url | `400 MCP_UPSTREAM_INVALID` |
| `transport=stdio` 但 `JUMCP_MCP_UPSTREAM_STDIO!=1` | `400 MCP_UPSTREAM_STDIO_DISABLED` |
| 切换 transport 时另一侧字段（http 的 `command`、stdio 的 `url`） | 归一化为空后落库，不报错 |
| 上游不存在 | `404 MCP_UPSTREAM_NOT_FOUND` |

## 10. 部署开关

| 变量 | 作用 |
|---|---|
| `JUMCP_MCP_UPSTREAM_STDIO=1` | 允许注册/连接 stdio 上游（默认关闭） |
| `JUMCP_SECRET_KEY=<64 位 hex>` | 内置包密码字段的加密密钥；HTTP 模式必需，缺失/非法即拒绝启动（fail-closed） |
| `JUMCP_TRANSPORT_KEY=<64 位 hex>` | 传输密钥，管理台加密 `secret` 字段请求体用；HTTP 模式必需，缺失/非法即拒绝启动（fail-closed） |
| `JUMCP_MCP_BUNDLED_BIN_DIR=<目录>` | 手工 stdio 上游命令解析的第一个候选，用于随包分发或非常规部署；每次解析时读取，未设置则只看服务同目录与 `PATH` |

前两个开关都在进程启动时读取，改动需要重启；`JUMCP_MCP_BUNDLED_BIN_DIR` 每次解析时读取。
上游的增删改**不需要**重启：管理面写成功后，
下一次 `tools/list` / `tools/call` 即反映变化。

## 11. 明确不支持

- 上游的 `resources` / `prompts` / `sampling` / `logging` / `completion`；
- 逐上游透传端点与字节级反向代理；
- `notifications/tools/list_changed` 推送（`/mcp` 只有 POST，没有 GET 流）；
- 旧版 HTTP+SSE 传输；
- 依赖 `Mcp-Session-Id` 保存跨调用状态的**有状态**上游（接入前须确认其会话不承载业务状态）。
