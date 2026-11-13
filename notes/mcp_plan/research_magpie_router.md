# magpie 的 MCP 路由：结论、证据、可借鉴与不适用

调研对象：`github.com/yetone/magpie`（Go，2026-11-13 时 35,404 stars，HEAD `390dd40`）。
证据全部来自该 commit 的源码，行号为该 commit 下的行号；引用格式 `路径:起始-结束`。

## 1. 一页结论

magpie 的主体是**本地模型网关**：把各 agent 自己配置文件里的 model/baseURL/key 改写成指向本地 gateway，并在配额耗尽时换账号。MCP 是它的第二等能力（hub 的注释自称 #2679 才加），但**确实是真路由**，不是一个只写配置的工具。它同时提供三种形态：

| 形态 | 端点 | 语义 |
|---|---|---|
| 聚合路由（hub） | `/mcp/magpie/hub`、`/mcp/magpie/hub/<agent>` | 把 Library 里勾选给该 agent 的多个上游 MCP server 的 tools 合成一份，命名为 `<server>__<tool>`，`tools/call` 按前缀回派 |
| 单上游透传 | `/mcp/<server>` | 字节级 Streamable HTTP 反向代理：POST/GET 事件流/DELETE 会话全透传，只把 `Authorization` 换成 magpie 自己登录得到的 token |
| 本机命令按会话拉起 | `/mcp/run/<server>` | 仅本机、仅 loopback Host、禁 Origin；每个客户端会话起一个 stdio 子进程，转发上游通知（含 `list_changed`） |

能力边界（必须写清，避免照抄时误判）：

- 只做 tools，**不做 resources / prompts**（`internal/gateway/hub_mcp.go:28-30`）。
- hub 自己只收 POST，**不发 GET 流、不接 DELETE**，因此**无法向客户端推送 `notifications/tools/list_changed`**（`internal/gateway/search_mcp.go:107-127`）。上游的 `list_changed` 只用来让本地缓存失效（`internal/library/mcp_hub.go:559-565, 835-863`）。
- 它不消费 MCP 作为 client 去"用"工具；它是 server + proxy，工具最终交给各 agent 的模型。
- 它不改自己的库来"注册工具"：上游 server 是用户在 Library 里手工/市场添加的条目，工具列表是运行时 `tools/list` 现拉的。

值得借鉴（按价值排序）：命名规则与"旧名回退"、失败隔离与超时预算、三层可见性过滤（key / agent / project）、审计不落参数、动态写入的 applied ledger。不适用：单文件 JSON + 明文 OAuth 落盘、全局单 backend 共享会话、改写客户端配置文件本身。

## 2. 证据表

| 机制 | magpie 位置 | 结论 / 对本仓库的含义 |
|---|---|---|
| 上游数据模型 | `internal/library/mcp.go:35-96` | 字段：`name`、`transport`(stdio/http/sse)、`command/args/env`、`url/headers`、`agents`、`disabledTools`、`oauth`、`accounts/account`、`startupTimeout`、`local`、`plugin`、`hidden/paused`、`bridge`、`hub`、`note`。没有独立的"健康状态"字段，健康是即时探测结果 |
| 传输合法性校验 | `internal/library/mcp.go:137-200` | stdio 必须有 command；http/sse 的 URL 必须 `http(s)://`；未知 transport 直接拒绝。校验在写入边界一次完成 |
| 持久化 | `internal/library/library.go:22-96, 202-220, 346-371` | 全部在 `library.json` 单文件；`change()` = 载入→改→存→`sync()` 写客户端→再存。`Applied` 映射记录"magpie 往每个 agent 写了什么"，撤销只删自己写的（`internal/library/sync.go:211-300`） |
| 密钥存储 | `internal/mcpauth/mcpauth.go:142, 170-190` | OAuth 登录结果明文 JSON（`mcp-signins.json`，权限 0600）；env 值用 `${NAME}` 引用，运行时从环境展开（`internal/library/cmdvars.go:1-20`、`mcp_check.go:550-560`）。**不可照搬**：我们多租户服务端不能明文落上游凭证 |
| 聚合入口与路由 | `internal/gateway/gateway.go:1129-1130, 1141-1142`；`hub_mcp.go:33, 51, 295-303` | `/mcp/{name}`→透传，`/mcp/run/{name}`→按会话跑命令，`/mcp/magpie/hub[/{agent}]`→聚合 |
| 工具命名 | `internal/gateway/hub_mcp.go:312-331`；`tools_mcp.go:61, 92-100` | `<server>__<tool>`；两侧非 `[A-Za-z0-9_-]` 的字符替换为 `-`；整名截到 64 字符；同名冲突加 `_2/_3`。`__` 作为保留分隔符 |
| 调用解析（含旧名回退） | `internal/gateway/hub_mcp.go:690-721` | 先在当前聚合表精确匹配；找不到再用"配置里的 server 名 + `__`"反推上游，直接问；两个 server 归一化后同名则拒绝（不猜） |
| 聚合列表与失败隔离 | `internal/gateway/hub_mcp.go:333-350`；`internal/library/mcp_hub.go:265-305` | 每个上游一个 goroutine 并行 `tools/list`；**任一出错就整体跳过该上游**（工具静默消失，不阻塞其他）；整体列表预算 40s |
| 超时与预算常量 | `internal/gateway/hub_mcp.go:282-291, 587-592, 899`；`internal/library/mcp_hub.go:61`；`internal/library/mcp_run.go:50` | `tools/list` 40s、`tools/call` 50s（低于客户端 60s）、`initialize` 填充说明 5s、body 4MiB、空闲 10min 收 backend、`/mcp/run` 会话空闲 15min、prune 每 5s |
| 调用结果与错误形态 | `internal/gateway/hub_mcp.go:739-773` | 上游超时/不可达 → `isError` 文本而非 JSON-RPC error（只有未知工具才是 -32602）；每次都先按调用方 key 扣预算再执行 |
| 健康检查 | `internal/library/mcp_check.go:37-55, 145-210, 550-640` | 显式 `CheckServers`（管理面点一次查一次），状态 `ok/auth/error` + `why`（notfound/start/exited/timeout/http/refused/unreachable/protocol/novar/wsldown）；**无后台轮询、无自动重连循环**，下次调用自然重连 |
| 工具列表缓存 | `internal/library/mcp_hub.go:617-667` | 每个上游缓存 `tools/list`（翻页到 `nextCursor` 空，最多 10 页），直到上游发 `list_changed`、条目被编辑或开关关闭 |
| 连接池/生命周期 | `internal/library/mcp_hub.go:400-465, 472-600` | 按 server 名一个 `backend`；`hubKey` 包含配置 + 登录态，变了就建新 backend 并停旧的；空闲 10min 关连接；开关关闭/进程退出全停 |
| 推送能力 | `internal/gateway/search_mcp.go:107-127`；`internal/library/mcp_run.go:308-318` | hub 不推送；只有 `/mcp/run` 把上游通知（`tools/list_changed` 等）转给客户端。`internal/claudebridge/mcp.go:105-130` 是另一种：合成一个 stdio MCP server 主动发 `list_changed`（为 Claude 订阅桥） |
| 权限：key 级 | `internal/access/mcp.go:12-90`；`internal/gateway/mcp_key.go:65-130, 231-260` | key 上挂 `<server>` 或 `<server>/<tool>` 列表；列表阶段就裁剪，调用阶段再判一次；透传路径无法解析时按 JSON 报文重写 `tools/list`。`-` 表示全禁；空列表表示全放行 |
| 权限：agent / project 级 | `internal/gateway/hub_mcp.go:249-267`；`internal/library/mcp_hub.go:193-230`；`internal/gateway/hub_mcp.go:39` | `Server.Agents` 决定给哪些 agent；project 用 `/mcp/magpie/hub/<agent>~<key>` 追加/关闭某个 server；外来调用者（LAN/隧道/网页）见 `HubVisitor`，本机特权 server 一律不给（`internal/gateway/hub_mcp.go:176-190`） |
| 审计 | `internal/gateway/mcp_calls.go:16-25, 129+` | 每次工具调用一行请求日志：server、tool、耗时、错误、所用 provider/model；**参数从不落盘** |
| 工具按需暴露 | `internal/gateway/hub_ondemand.go:35-41, 50, 63-72` | 上游工具 >10 个且客户端自身没有 tool search 时，改为只给 `find_tools` + `call_tool` 两个工具；`find_tools` 的描述里内联 server 清单与工具摘要（≤1200 字符） |

包级结构与取舍：

| 包 | 职责 | 借鉴 / 不照搬 |
|---|---|---|
| `internal/gateway` | HTTP 路由、鉴权、MCP 服务端实现（`hub_mcp.go`、`mcp.go`、`mcp_key.go`、`mcp_calls.go`、`hub_ondemand.go`、`search_mcp.go` 的 `serveMCP`） | 借鉴：一个 `serveMCP` 统一出来做"守卫→解析→分发→日志"；不照搬：路由层同时直连配置（`library.json` 反复读） |
| `internal/library` | MCP server 目录 + 写入客户端配置 + 连接池 + 健康检查（`mcp*.go`、`sync.go`） | 借鉴：`backend` 池 + `hubKey` 版本化 + `Applied` ledger；不照搬：单 JSON 文件、把"写别人配置文件"当同步机制 |
| `internal/mcpauth` | 远端 server 的 OAuth 登录/续期/失效 | 借鉴：401 → 续期一次 → 仍失败则转为"上游不可用"而不是把 401 透给客户端；不照搬：明文 token 文件 |
| `internal/access` | 网关 key 的 MCP 白名单（`<server>` / `<server>/<tool>` / `-`） | 借鉴：白名单在"列表"与"调用"两处各判一次 |
| `internal/claudebridge` | 合成 stdio MCP server（Claude 订阅桥），唯一主动发 `list_changed` 的地方 | 借鉴：需要推送时的最小实现形态（自有工具集 + 变化时通知）；不照搬：只实现客户端用到的少数方法 |
| `internal/agentplug` | 新 agent 的 JS 模块（`agent`/`connect`/`library.mcp` 声明） | 借鉴：manifest 声明"这个 agent 的 MCP 文件在哪、什么格式"；对我们无对应物（我们不写客户端文件） |

## 3. 动态插入的完整流程（照着这条链路设计我们的写路径）

1. 写入：`SaveServer` → `change()`（`internal/library/library.go:346-371`）。单次加锁内完成"载入 JSON → 改 → 落盘 → 逐 agent 写配置 → 再落盘"。校验先于持久化（`internal/library/mcp.go:137-200`）。
2. 传播到 agent：`syncMCP`（`internal/library/sync.go:211-300`）按 agent 的格式适配器写文件，只删 `Applied` 里记过的条目；被编辑/删除的条目同时从所有 agent 配置里撤掉。**agent 不重启**：它下一次连的仍是同一个 HTTP 端点，`tools/list` 现取。
3. 传播到运行中的 hub：`backendOf` 发现 `hubKey`（配置+登录态）变了 → 建新 backend、异步停旧（`internal/library/mcp_hub.go:400-465`）；另有 5s 一次的 `hubPrune` 兜底，处理"别的 magpie 实例/CLI 改了库"（`internal/gateway/hub_mcp.go:896-915`）。
4. 传播到已建立的 stdio 会话：`runGiven` 对比 spec，变更的会话直接断开，下一次调用按新命令拉起（`internal/library/mcp_run.go:146-171`）。
5. 客户端侧：hub 不发 `list_changed`。旧工具名靠"前缀反推"继续可调（`internal/gateway/hub_mcp.go:690-721`），失败时返回带 server 名的 `isError` 文本。

结论：它的热加载是"下一次请求可见 + 旧名兜底"，不是推送式热更新。这与本仓库 `research_dynamic_tool_editing.md` 已定的"不依赖 `list_changed`"一致。

## 4. 本仓库（jumcp-server）router 目标模型

### 4.1 上游传输

- 只允许 `http`（MCP Streamable HTTP，POST + JSON/SSE 响应）。理由：我们是常驻多租户服务，stdio 上游等于把"以服务进程身份执行任意命令"暴露给管理通道；magpie 对 stdio 的约束只是"不给网页、不给 HereOnly 的 server"，带 key 的远端调用者仍可通过 hub 触发本机命令（`internal/gateway/hub_mcp.go:176-190` 的 `Program` 只挡 `Held` 的 key），不足以作为我们的边界。
- `stdio` 作为**部署级开关**（默认关）+ 可执行文件白名单 + 独立低权限账户 + 固定 command（不接受请求侧参数）。开启后仍禁止 agent 通过维护通道注册。
- `sse`（旧版 HTTP+SSE）不实现：上游生态已迁移，且多一套连接状态只增加取消/重连分支。

### 4.2 持久化

- 新表放现有 SQLite（`jumcp_core::registry` 同一库，沿用 `CREATE TABLE IF NOT EXISTS` 自迁移风格，`registry.rs:310-333`）：
  - `upstream_mcp(id, name UNIQUE, transport, url, header_refs JSON, env_refs JSON, group_code, read_only, timeout_ms, enabled, definition_hash, created_at, updated_at)`
  - `upstream_mcp_tools(server_id, tool_name, schema JSON, schema_hash, last_seen_at)` — 仅缓存，可重建
- **密钥只存引用**：headers/env 的值来自环境变量名或既有凭证表，与 `DatasourceRef.dsn_env`（`jumcp-core/src/config.rs:57-70`）同一模式；表中不出现明文值。
- 内置工具仍为代码常量（`RESIDENT_CAPABILITY_IDS` / `ADMIN_CAPABILITY_IDS`），维护通道写入路径继续被 `ensure_mutable`（`registry.rs:63-79`）拦截；上游工具用独立表，不写 `capabilities`，避免"上游可覆盖内置定义"。

### 4.3 命名与冲突

- `<server>__<tool>`，两侧 `[^A-Za-z0-9_-]` → `-`，总长 ≤64。与 magpie 一致，好处是 agent 侧可归属、审计可按前缀解析。
- 与内置 id 的冲突用**注册期拒绝**而非加后缀：server 名不得是保留字（`admin`、`analysis`、`basic`、`builtin`、`jumcp` 及内置 id 集合），归一化后与内置 id 相同的上游工具拒绝接入。magpie 的 `_2` 静默改名会让"同一工具在审计里对不上原文"，我们不采纳。
- `tools/call` 解析顺序：当前投影精确匹配 → 按配置中的 server 名反推前缀 → 未命中返回 `-32602`。反推保留，用来吃住客户端缓存了旧 `tools/list` 的情况。

### 4.4 投影与热加载

- `tools/list` 每次请求现拼（本仓库本来就不缓存，`discovery.rs:1-10` 已写明"下一个请求即可见"）：内置常驻 + 注册表可见行 + 上游投影，逐上游独立超时（建议 8s，远小于客户端的 60s）并行拉取，失败的跳过并在投影里记 `upstream_unavailable`。
- 管理面写 → 单事务 + 审计 → 递增 `upstream_mcp.updated_at` → 路由器下一次请求重建该上游连接（连接池按 `(id, definition_hash)` 做 key）。
- 不发 `list_changed`；不实现 GET 流。

### 4.5 健康与失败隔离

- 惰性连接，空闲回收；**不做后台轮询**，只在管理面提供"测试连接"（等价于 magpie 的 `CheckServers`），结果落审计。
- 上游不可达：该上游工具从 `tools/list` 消失，其他上游不受影响；调用返回 `isError` 文本并写明上游名与原因（超时/连接失败/鉴权失败）。
- 单次调用超时（建议 30s，可配置）必须传播取消到上游请求；客户端断开时立即取消。

### 4.6 权限

- 三级，全部在服务端判定，客户端 Header/`_meta` 只能收窄不能扩大：
  1. 组：`upstream_mcp.group_code` 必须 ∈ `scope.groups`（复用 `EffectiveScope`，`entry.rs`）。
  2. token allowlist：现有 `allowed_builtin_tools` 语义扩展为"允许的工具名集合"，上游工具名同样受限。
  3. 只读声明：`read_only=false` 的上游默认不投影；启用必须走管理面人工审批。
- 上游注册/启用/改地址都是管理面写操作，agent 维护通道只能产出 draft（与 `admin_register_template` 的"只产 draft"一致）。

### 4.7 审计

- 每次上游调用一条事件：`upstream`、`tool`、`duration_ms`、`outcome`、`definition_hash`；**不落参数**（沿用 magpie `mcp_calls.go:16-25` 的取舍，也符合本仓库既有载荷规范）。
- 上游管理写操作走既有 audit hash chain（`jumcp_core::audit`）；`tools/list` 投影记录被省略的上游与原因。

### 4.8 内置工具三组拆分

- 现状：`builtin` 组（`discovery.rs:20`）同时承担两个语义——"组标签"和"不受 scope 收窄的常驻集合"。两者必须解耦，否则拆成 `admin/analysis/basic` 后，角色 `default_groups` 一变就会丢掉 `search_capabilities`。
- 做法：`project_builtin(def, group)` 已接受组参数（`discovery.rs:60-80`），只需把 id→组映射写在 `builtin_capabilities`；常驻性继续由 `RESIDENT_TOOL_NAMES` 判定。`list_groups`/预算按新组展示，可见性语义零变化。
- **不把 `analysis`/`basic` 变成授权组**：`scope.groups` 可由请求头收窄，用它当"权限"会形成"收窄即提权"的反向通道。按 token 的 allowlist 才是授权位（`discovery.rs:37-46` 的双重判定保持不变）。

### 4.9 依赖

- 服务端目前没有 HTTP 客户端（`jumcp-server/Cargo.toml` 只有 axum 服务端 + tokio；axum 的 hyper 不能直接拿来做 client）。router 必须新增一个：建议 `reqwest`（`rustls-tls` + `stream`），它替代不了任何现有依赖，也不与 axum 重叠；若拒绝新增，则用 `hyper` + `hyper-util` 手写，代码量与错误面更大。按 AGENTS.md §5 在落地时记录取舍。

## 5. 关键设计选择与取舍

**选择一：只做聚合，不做逐上游透传端点。** magpie 两种都做，是因为它要给每个 agent 写各自的配置、且要兼容有 OAuth 会话的上游。我们的入口只有一个、scope/预算/审计链都在这一层；再加 `/mcp/<name>` 会让每个入口重复一遍鉴权与审计，并且让"谁调了哪个工具"分散到两套日志。代价：上游自身的 `resources`/`prompts`、上游侧会话态无法透传——接受，我们只提供只读数据工具。

**选择二：前缀命名 + 注册期拒绝冲突（不采纳静默 `_2`）。** 前缀让审计与归属无歧义，也避免上游工具名与内置 id 相撞。代价是 64 字符截断会改变极长工具名，且 server 改名等于换工具名。缓解：server 名不可变（改名 = 新 server + 旧 server `deprecated` 映射）；截断后仍冲突则拒绝注册，要求改名。

**选择三：默认禁止 stdio。** stdio 上游以服务进程身份执行任意命令，等于把命令执行面挂在管理通道上；magpie 的隔离只在单机桌面成立（带 key 的远端仍能触发命令），在多租户服务端直接不成立。代价：部分只提供 stdio 的上游（多为本地文件系统类）无法接入——接受，它们本就不属于只读数据源边界。

**选择四：不推送 `list_changed`，靠"下一次请求 + 旧名回退"。** 我们无 GET 流、且客户端支持度不一（已有结论 `research_dynamic_tool_editing.md`）。代价是客户端缓存的工具清单会短暂陈旧；回退解析把"陈旧但可调"变成可能，而"陈旧且已下线"会得到明确的 `isError`，不会静默错调。

**选择五：上游工具复用现有发现面，不新增第二套元工具。** 把上游工具作为 `source=upstream` 的投影项进 `search_capabilities`/`describe_capability`/`list_groups`，`tools/list` 仍受常驻预算 + `dropped_groups` 回显约束。magpie 需要 `find_tools/call_tool` 是因为它没有目录检索层；我们有，再引入一对会形成两条口径（正是 `catalog.rs:1-10` 明令禁止的）。

## 6. 风险清单

| 风险 | 触发条件 | 处置 |
|---|---|---|
| 工具数撑爆常驻预算 | 上游工具多且被平铺 | 逐上游工具上限；超限只留 `search/describe` 可发现；`dropped_groups` 必须回显（现有语义） |
| 命名碰撞/截断 | 上游工具名含 Unicode 或 >64 字符 | 注册期规范化并检测；冲突拒绝，不静默改名 |
| 上游工具写数据 | 上游 `readOnlyHint` 撒谎 | 不信任 annotations；以注册时人工声明的 `read_only` 为准，非只读默认不投影 |
| SSRF / 内网穿透 | 管理面填了任意 URL | 仅允许 http(s)、禁重定向跨主机、可选内网地址黑名单；地址变更单独审计 |
| 有状态上游共享连接 | 上游依赖 `Mcp-Session-Id` 保存状态 | 明确不支持：接入时声明 stateless；需要会话态的拒绝接入（magpie 的全局单 backend 就是该缺陷，只能靠 `_meta` 打补丁） |
| 维护通道自注册上游 | agent 拿到 admin 组后写入上游 | 维护通道只能产 draft，发布必须人工审批；上游表写路径不复用 `capabilities` 写权限 |
| 取消传播缺失 | 客户端断开后上游仍执行，占连接/配额 | 调用超时 + 断连取消必须都在 router 内实现，不能只靠 axum 的 drop |
| 明文凭证 | 照抄 magpie 的 JSON 落盘 | 只存 env/凭证引用名，值在连接时解析，审计与日志永不输出 |

## 7. 最小可行实现顺序

1. 表 + 管理面 CRUD + 审计（仅 `http` 传输，`read_only` 与 `group_code` 必填，发布需审批）。
2. 聚合投影：`tools/list` 合并内置 + 上游（并行、独立超时、失败跳过）、命名规则、scope 过滤。
3. 调用链：前缀解析 + 旧名回退、超时/取消、`isError` 整形、审计事件、预算计量。
4. 内置工具三组拆分（组标签与常驻性解耦，可见性语义不变）。
5. 发现面/审批闭环：上游工具进 `search_capabilities`，`definition_hash` 变更触发重新审批。
6. `stdio`（部署开关 + 白名单 + 独立账户），默认关闭，最后做。

前 3 步完成即具备"动态插入上游 MCP server"的端到端能力；4、5 是治理收口；6 是可选扩展。

## 8. 被否决的方案

- **逐上游透传端点 `/mcp/<name>`**：与聚合入口重复鉴权/审计，且把"工具归属"拆成两套事实来源。
- **静默加后缀解决重名**：审计无法回指上游原始工具名，agent 侧也无法预测名字。
- **依赖 `notifications/tools/list_changed` 做正确性保证**：客户端支持不一，且我们不提供 GET 流。
- **用 `library.json` 式单文件 + 明文 OAuth 存储**：多租户服务端不可接受；我们已有 SQLite + 凭证引用模式。
- **全局共享一个上游 stdio 进程/会话**：有状态上游会在调用方之间串数据（magpie 用 `_meta` 会话键打补丁，是已知补丁而非设计）。
- **运行时按调用者生成工具定义**（与 `research_dynamic_tool_editing.md` 的否决一致）：审计不可复现，缓存键失效。
