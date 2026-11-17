# Harness Agent 平台 MCP 服务实施计划

> **历史设计记录，不作为当前契约。** 当前契约见 `docs/`：架构与 router 模型见 `docs/mcp-router.md`，审计/令牌字段见 `docs/audit-openness.md`，目录职责见 `docs/repository-structure.md`。
>
> **状态**：预览稿；待 §11.1 三项阻塞决策后定稿。服务对全部接入数据源只读；agent 可修改的只有本项目自有数据（§4.4.6）。
> **首个落地载体**：`jumcp` 仓库。技术栈已定稿（Rust + React，见同目录 `implementation_plan.md` §3.2）；本计划只锁定契约与不变量。
> **适用对象**：平台架构师、MCP 服务开发者、Agent 平台运维与安全负责人。

---

## 1. 背景与目标

### 1.1 背景

平台需为多类型 Agent（执行型、分析型、管理型）提供统一 MCP 服务，覆盖多源数据访问（数据库、内部 API、知识库），并支持大量**定制化固定查询路径**。核心矛盾：

- 单个 MCP tool 定义约 **100–270 tokens**（生态实测：平均 123 tok/tool、中位 103，重型工具 800–1,200；按 server 汇总时 GitHub MCP 的 26 个工具约 3,546 tok/轮，被测 server 中位 547 tok/轮）。
- `tools/list` 结果按轮进入上下文，注入的 schema 与工具结果同样常驻 transcript，工具数增长直接侵蚀可用窗口——Anthropic 的官方示例中，一次性预载全部工具定义并把中间结果放回上下文约 15 万 tokens，改为按需加载 + 结果不进上下文后约 2 千。
- 长上下文不等于可用上下文：上下文越长，中段信息利用率越低。

> 方案来源与取舍见 `notes/mcp_plan/research_*.md`；本计划的预算数字以实测校准，不引用二手百分比。

### 1.2 目标

| 编号 | 目标 | 量化指标 | 边界条件 |
|---|---|---|---|
| G1 | 约束上下文注入 | 初始常驻 ≤ 2,000 tokens；单会话 tool 相关峰值 ≤ 8,000（告警 9,000） | **仅适用于支持动态工具集的客户端**，其余按 §4.2.0 各自基线 |
| G2 | 按 Agent 类型精确裁剪 | 越界 group 请求 100% 显式失败；静默降级 0 次 | `strict` 模式下定义；`intersect` 模式需租户级显式开启并计入审计 |
| G3 | 定制查询规模化 | 常驻工具数不随模板数增长；常驻索引 ≤20 组 | 索引规模受组数上限约束 |
| G4 | 固化闭环 | 系统侧：采集→候选草案 ≤ 1 个工作日；人工侧：草案→published 按审批 SLA（建议 ≤5 工作日） | 拆为两段，不合并承诺 |
| G5 | 治理可控 | 动态变更审计覆盖率 100%；`discoverable` / `callable` 独立授权 | 双布尔模型 |

### 1.3 非目标

- 不重构 Harness Agent 平台自身编排逻辑。
- 不实现跨 MCP Server 的工具联邦路由。
- 不解决模型侧工具选择准确率的训练优化（本计划只做评测与回归门禁）。
- 不提供数据源权限清单导出；数据源账号权限由租户运维配置，本服务只消费。
- **不提供改变接入数据源的能力**：所有 capability 对数据源只读（只读账号是唯一强保证，§4.3.1）；`write` / `admin` 类风险不存在，也不存在 `prepare_call` / `apply_call` 之类工具。agent 可修改的只有本项目自有数据（§4.4.6）。

---

## 2. 设计原则

1. **Header 只做可见性裁剪，不做授权。** 授权必须在 `tools/call` 执行前由确定性策略层独立判定。
2. **唯一强只读保证在数据层。** 只读账号是保证；请求级 `readonly=1`、AST 校验与注册期复检是纵深防御，不是保证。
3. **边界、时机、粒度、安全分属不同平面**（见 §3）。
4. **注册 ≠ 可发现 ≠ 可调用。** 三者正交，见 §4.4.1。
5. **通用入口 + 注册表分发优先于工具膨胀。**
6. **能力探测面必须关闭。** 会话不可见的能力，在检索与错误信息中一律表现为"不存在"。
7. **模板只做结构选择，不做值插值。** 任何参数值不得以字符串拼接进入 SQL 文本。
8. **一切变更可审计、可回滚、可版本钉住。**
9. **每个 kind 有且仅有一个调用面。** 同一能力不得存在两条调用路径（否则版本、约束、审计必然分叉），见 §3.3。
10. **权限只做收敛。** 部署配置、环境变量、请求声明只能收紧租户策略，永不放宽；只读状态、生产保护、数据源账号权限、允许清单是硬上限，任何更具体的设置都不可越过。

---

## 3. 总体架构

### 3.1 四平面

| 平面 | 职责 | 关键机制 |
|---|---|---|
| **A. Header 裁剪平面** | 收窄**可发现上限** | `X-MCP-Tool-Groups`、`X-MCP-Tool-Budget`、`X-MCP-Tool-Groups-Mode` |
| **B. 渐进发现平面** | 控制定义**何时进入上下文** | `search_capabilities` 元工具 + 按需 schema 注入 |
| **C. 分发执行平面** | 少量通用 tool 覆盖大量能力 | `db_query_readonly` + `db_run_template` + `invoke_capability` |
| **D. 治理与固化平面** | 动态管理 + 日志闭环 + 审批 | Admin group；`status`/`discoverable`/`callable` 分离；日志聚类 → 候选 → 审核 |

### 3.2 请求主链路

```
Agent 请求
  → [鉴权] 身份 / 租户 / 角色
  → [A] 解析 groups → 越界判定(strict→403) → 预算判定(默认超限→失败)
  → [B] 返回常驻目录（元工具 + 通用入口 + 组索引，不含完整 schema）
  → 模型调用 search_capabilities → 按会话可见集过滤 → 返回候选
  → 模型发起 tools/call
  → [授权] 确定性策略（角色 + capability + 参数约束 + 授权租约）
  → [C] 通用入口 / 注册表分发执行（数据层只读 + 超时 + 行/token 上限）
  → [D] 追加式审计（哈希链），admin 事件独立保留策略
```

### 3.3 统一能力抽象（多源前提）

DB 只是首个实现。所有可发现能力统一建模，避免阶段三把数据库语义焊死：

```yaml
capability:
  id:            order.daily_summary          # 稳定标识
  kind:          template | tool | endpoint   # 执行器种类
  group:         db.read.analytics            # 归属组（决定可见性）
  risk:          read | sensitive           # 无 write/admin：工具不得改变接入数据源
  status:        draft | review | published | deprecated
  discoverable:  bool                         # 可被检索
  callable:      bool                         # 可被调用（仍需角色授权）
  params_schema: {...}                        # §4.3.3
  annotations:   {readOnlyHint, destructiveHint, idempotentHint, openWorldHint}  # MCP 标准注解，必填
  executor:      {type, connection_ref, ...}  # §4.3.1
  constraints:   {...}                        # 行数/超时/列黑名单
  owner:         team
  version:       n
  successor_id:  optional                     # deprecated 后的迁移目标
```

规则：**发现 = `status=published ∧ discoverable ∧ group ∈ 会话可见集`；调用 = 发现条件 ∧ `callable` ∧ 角色授权 ∧ 参数校验通过。**

`annotations` 必须按真实语义显式声明，不得依赖协议缺省值（缺省为 `readOnlyHint=false`、`destructiveHint=true`、`openWorldHint=true`）：客户端据此决定是否免人工确认自动执行，与审计中的 `risk` 不一致时以更保守者为准。

**调用面矩阵**（每个 kind 只有一个调用出口）：

| kind | 发现 | 调用面 | 静态客户端（无动态注入）可用性 |
|---|---|---|---|
| `tool` | `search_capabilities` | 客户端 `tools/call`（服务端按需注入 schema） | 仅能在会话初始化时整组注入（§4.2.0 降级矩阵），**不能按需追加** |
| `template` | `search_capabilities` | 常驻 `db_run_template`（§4.3.3） | ✅ 始终可用（不依赖 schema 注入） |
| `endpoint` | `search_capabilities` | 常驻 `invoke_capability`（§4.3.8） | ✅ 始终可用 |

> `sensitive` 只影响可见性与审批强度，不引入第二调用面；不存在写能力，也没有调用期确认令牌。

> `kind=tool` 的调用的确依赖客户端能力；`template` / `endpoint` 走常驻分发器，因此 `api.internal.read`、`kb.search` 这类能力在静态客户端上**仍然可调用**——前提是它们被建模为 `endpoint` 而非 `tool`。建模为 `tool` 的能力在静态客户端上等于不可用，注册审核时须显式确认客户端形态。

---

## 4. 详细设计

### 4.1 平面 A：Header 裁剪平面

#### 4.1.1 入参与信任级别

| 参数 | 语义 | 信任级别 | 缺失时 | 越界/非法时 |
|---|---|---|---|---|
| `X-MCP-Tool-Groups` | 请求可见组子集 | **不可信** | 使用角色默认组 | `strict`：403 整体失败；`intersect`：取交集 + 审计 WARN |
| `X-MCP-Tool-Groups-Mode` | `strict` / `intersect` | **不可信，请求头不得修改**；仅租户级配置生效 | 租户配置，缺省 `strict` | 忽略并 WARN |
| `X-MCP-Tool-Budget` | 期望常驻定义 token 上限 | 不可信 | 角色默认预算 | 取 `min(请求, 角色默认, 租户上限)`；低于最小可行预算（800）→ 400 |
| `X-MCP-Features` | 功能开关（`progressive-discovery` 等） | 不可信 | 服务端默认 | 无法识别的 feature → 忽略 + WARN（向前兼容，不失败） |
| `X-MCP-Readonly` | 只读会话标记 | 不可信 | 取角色默认 | **仅影响目录裁剪，不改变执行权限**；只读真正的保证在 §4.3.1 L1/L2 |

语义细则：

- `X-MCP-Tool-Groups` 缺失 → 角色默认组；**显式空值 → 空集**（仅返回元工具），二者必须区分。
- 组名格式 `域.风险级[.子域]`，未注册的组名视为越界（防止客户端拼写错误被静默吞掉）。

#### 4.1.2 分组模型

```
db.read.core          # 核心表只读
db.read.analytics     # 分析表只读
db.read.sensitive     # 敏感表只读（需授权租约）
db.admin.schema       # 元数据管理（仅 admin）
db.admin.template     # 模板治理（仅 admin）
api.internal.read     # 内部 API 只读
kb.search             # 知识库检索
```

**组数量上限 ≤ 20**（硬约束：常驻索引规模由此界定，见 §4.2.1）。超出需合并子域或改用二级检索。

每个组配置含 `priority`（整数，预算溢出时 `truncate` 策略按此降序装载，见 §4.1.4）；`priority` 只存在于组配置，不挂在单个 capability 上。

#### 4.1.3 服务端解析流程（严格按序）

1. **鉴权**：解析身份、租户、角色；失败即 401/403。
2. **填充默认值**：缺失的 groups 用角色默认；`_meta`/profile 通道按 §4.1.6 合并（优先级：Header > `_meta` > profile）。
3. **越界判定（先判后裁）**：
   - `strict`（默认）：请求组 ⊄ 角色 allowlist → 整体失败 `403 MCP_GROUP_FORBIDDEN`，`data.offending_groups` 列出越界项；**不裁剪、不执行**。
   - `intersect`：记录审计 WARN + 取交集继续；仅允许租户级开关开启，且必须出现在会话目录元信息的 `degraded: true` 中。
4. **预算判定**：`effective_budget = min(header, role_default, tenant_cap)`；按 §4.1.4 处理溢出。
5. **返回目录**。

> 顺序不可调整：先取交集会使第 3 步条件恒假；先截断再判定则会掩盖越界配置错误。

#### 4.1.4 预算溢出策略

| 策略 | 触发 | 行为 |
|---|---|---|
| `fail`（默认） | 目录超出 effective_budget | 返回 `400 MCP_BUDGET_EXCEEDED`，`data.required/available/dropped_candidates`；不返回半套目录 |
| `truncate`（显式开启） | 同上 | 按 `group.priority`（整数，注册表字段）降序贪心装载；返回 `truncated: true` + `dropped_groups[]` + `budget_spent` |

两种策略都必须写入会话元信息；`truncate` 不得作为默认，避免与 G2「禁止静默降级」冲突。

#### 4.1.5 分角色策略

| Agent 类型 | 初始注入 | 预算 |
|---|---|---|
| 执行型 | 元工具 + `db_query_readonly` + 1 个高频入口（`db_run_template` 或 `invoke_capability`，按角色主域二选一） | 1,600 |
| 分析型 | 元工具 + `db_query_readonly` + `db_run_template` + `invoke_capability` + 分析组索引 | 2,000 |
| Admin | 治理面 tool 目录（独立核算，不进本预算模型） | 独立 |

> 算术校验：分析型 400(元工具) + 750(3 入口×250) + 300(索引) + 400(描述) = 1,850 ≤ 2,000。

#### 4.1.6 stdio / 非 HTTP 通道

stdio 客户端无 HTTP Header，**Header 不是唯一入口**：

| 通道 | 载体 | 优先级 |
|---|---|---|
| HTTP Header | 见 §4.1.1 | 1 |
| `initialize` 的 `_meta` | `_meta.mcp.{toolGroups, toolBudget, features, readonly}`（键名在阶段〇冻结） | 2 |
| 服务端 profile | 按客户端 API key / 进程 profile 绑定 | 3 |
| 默认 | 角色默认 | 4 |

**禁止**从 tool 参数读取这些值（模型可伪造）。

**HTTP `/mcp` 的 per-request 身份**：`Authorization: Bearer <token>` 对照 `JUMCP_AGENT_TOKENS="agent_id:role:token,..."` 派生 `Actor{actor_id=agent_id, role}`，`role` 必须在 `policy.roles` 中（启动期校验，非法条目即拒绝启动；HTTP 模式下该变量缺失同样拒绝启动）。缺 token 或 token 未知一律 `MCP_UNAUTHORIZED`，不存在默认角色兜底；`X-MCP-Tool-Groups` 与 `_meta` 只在该身份之上收窄可见性；预算记账与模板版本钉住同样按该身份分账。

**stdio**：无 HTTP 头，全部请求共用进程默认身份（`analyst` 角色，无该角色时取字典序第一个），审计 `actor.id` 为空。这是传输限制而非身份降级；`admin` 组工具在 stdio 下同样只取决于该默认身份的组。

---

### 4.2 平面 B：渐进发现平面

#### 4.2.0 客户端能力矩阵（前置条件，非风险脚注）

渐进发现的收益完全取决于客户端能否在会话中途获得新的工具定义（`notifications/tools/list_changed` 支持度差异极大）。**这张表填满之前，G1 的 3K/8K 不可作为承诺。**

| 客户端 | list_changed | 会话内重拉 tools/list | 单轮工具上限 | 适用策略 | token 口径 |
|---|---|---|---|---|---|
| 远程 MCP 客户端 A | 待实测 | 待实测 | 待实测 | 按矩阵结果 | 待定 |
| 本地 stdio 客户端 B | 待实测 | 待实测 | 待实测 | 会话级快照 | 待定 |
| HTTP 单次快照客户端 C | 待实测 | 待实测 | 待实测 | 全量注入 + 预算裁剪 | 待定 |

**实测方法**（阶段〇交付，可作为回归用例固化）：`initialize` → `tools/list` → 服务端注册一个新 capability → 观察 60s 内是否收到 `list_changed` → 若不支持，重建会话后再 `tools/list` 观察是否生效。结果写入本表并冻结为版本化配置。

**降级矩阵**：

| 客户端能力 | 服务端策略 |
|---|---|
| 支持 list_changed | 完整渐进发现，目标 8K 峰值 |
| 不支持但可重建会话 | 会话级目录快照；`search_capabilities` 命中后提示客户端重建会话并携带该组的 `X-MCP-Tool-Groups` |
| 完全静态 | 以 group 为单位全量注入该组 tool，预算裁剪走 §4.1.4；G1 指标替换为「该客户端基线」 |

#### 4.2.1 两阶段注入与索引最小化

| 阶段 | 内容 | 成本 |
|---|---|---|
| 索引态（常驻） | 元工具定义 + **组名与工具计数** | ≤ 300 |
| 完整态（按需） | 单个 capability 的 name/description/inputSchema | 100–270 / 个 |

**常驻索引不含"一句话描述"**：描述本身是注入面，且随规模增长；描述只出现在 `search_capabilities` 结果中（且必须来自已审核注册表）。

#### 4.2.2 统一检索入口

不得为工具与模板各设一套语义重叠的检索面——那会成为新的膨胀点。统一为：

```
search_capabilities(query: string, kind?: "tool"|"template"|"endpoint", group?: string, limit?: int = 5)
  → [{ id, kind, group, one_line, param_summary, token_cost }]   # 不含完整 schema

describe_capability(id: string)
  → 完整 schema（用于注入下一轮）

list_groups()
  → [{ group, capability_count }]   # 不含定义
```

检索实现：关键词 + 向量混合。分页/过滤语义统一，避免出现第三套。

> 等价机制：Anthropic Tool Search Tool 用 `defer_loading` 在客户端侧达成同一目标；服务端实现不依赖客户端支持（附录 C）。

`describe_capability` 返回的 schema 只用于**让模型知道怎么调用**，不构成调用通道：`endpoint` 类能力由常驻 `invoke_capability` 执行，`template` 类由 `db_run_template` 执行（§3.3 调用面矩阵）。三件套不随模板数量增长。

#### 4.2.3 检索面安全

- **检索前硬过滤**：候选集 = 会话可见 capability 集合（`group ∈ 可见集 ∧ discoverable ∧ published`）。过滤发生在检索内部，不依赖检索后置过滤。
- **不可见 = not found**：不可见能力一律不进候选、不出现在"无权访问"提示里，防止存在性泄露。
- **查询本身入库审计**（`capability_search` 事件，含 query 与返回 id 列表），这是 §9 检索质量指标的唯一数据源。
- 检索结果按 `id` 稳定排序，避免同一 query 抖导致模型行为漂移。

#### 4.2.4 缓存

- 会话内已注入 schema 缓存，不重复注入。
- `search_capabilities` 结果按 `(query, visible_set_hash, kind, group, limit)` 做 5 分钟 TTL 缓存。

---

### 4.3 平面 C：分发执行平面

#### 4.3.1 分层保证模型

```
L1 数据源账号只读     ← 唯一强保证（SELECT 权限、无 DDL/DML、非 superuser）
L2 请求/连接级只读     ← 强（每条查询附 ClickHouse `readonly=1`；账户只读仍是 L1）
L3 资源上限           ← 强（max_execution_time、max_result_rows、应用层行数/单格截断、结果 token 上限）
L4 AST 单语句 + 黑名单 ← 强（语法级拦截，见附录 B）
L5 预检               ← 弱（ClickHouse 当前仅注册期 sqlguard 复检；不依赖 EXPLAIN）
L6 全量审计           ← 事后可追溯
```

**只读设置的施加位置**：ClickHouse 没有事务级只读开关，`readonly` 是查询级设置。agent 对**每条**查询都把 `readonly=1` 放进请求 URL 参数（与 SQL 内 `SETTINGS` 不同，不受语句内设置先后影响），并同时设置 `max_execution_time` 与结果行上限：

- 数据源账号只读（L1）是唯一强保证；`readonly=1` 只是纵深防御，不能替代权限收敛。
- 不使用跨请求的 `session_id` 维持只读：那会引入会话语义与泄漏面；每条查询独立施加。
- ClickHouse `readonly=1` 下修改设置会被拒绝，因此防爆设置必须与 `readonly=1` 在同一个请求里下发。

> 当前联调环境使用 `default` 超级用户：`readonly=1` 与 L4 只是客户端兜底，不能替代账号权限；生产必须换成仅 SELECT 授权的只读账号，并收紧 `allowed_schemas` 白名单。

**只读是产品不变量，不是部署开关**：所有数据源连接只以只读账号建立；能力定义不存在"写入数据源"的语义，因此无需按部署裁剪写工具。

> L1–L6 是唯一执行面：不存在写语句路径，也没有第二套校验分层。唯一可写的是本项目自有数据（注册表），且不经数据源连接。

**能力边界必须先声明**：`max_scan_rows`、`required_index_hit` 在 ClickHouse 上不能作为运行时硬约束（`max_rows_to_read` 是块级限制，索引命中由表引擎与查询计划决定）。这两项标注为「发布期预检 + 数据源支持时强制」，字段名改为 `preflight.max_scan_rows` / `preflight.require_index`，避免被读作运行时保证。附录 A 给出当前数据源的强制能力矩阵。

#### 4.3.2 第一层：通用只读入口（常驻 1 个 tool）

```
db_query_readonly(
  sql: string, params?: array,         # 单条只读 ClickHouse SQL；params 是标量位置数组（按 `?` 顺序绑定）
  database?: string,                   # 目标 ClickHouse database；省略用连接默认库，给出时必须落在白名单内
  max_rows?: int = 100,          # 上限 1000；超限夹紧并回显 effective_max_rows
  max_tokens?: int,
  cell_char_limit?: int = 200,   # 单格返回字符上限，上限 4000
  cell_char_offset?: int = 0,    # 长值续读起点，配合结果中的 next_offset
  allowed_schemas?: string[]     # 缺省取连接声明，不可在入参里扩大；ClickHouse 语义为 database 白名单
)
```

- L4：单条只读语句 + 结构校验（非正则）。仅单条 `SELECT` / `WITH`，其余结构（DDL、DML、多语句）一律拒绝；ClickHouse 方言由 `jumcp_sqlguard` 解析，并按词法层拦截外部访问/执行函数（见附录 B）。黑名单见附录 B。
- L1/L2：绑定只读账号 + 请求级只读（agent 对每条查询附 `readonly=1`）。ClickHouse 账户权限仍是唯一强保证；`readonly=1` 是纵深防御。部署/巡检独立验证账号没有写权限；服务启动只验证连接与 agent 握手，不执行写探测。
- L3：`max_execution_time`、`max_result_rows` + `result_overflow_mode=break`、行数上限（应用层截断并标注 `truncated`）、结果 token 上限（§5）。
- 查询级 `SETTINGS` 子句一律拒绝（`MCP_SQL_REJECTED`，`reason=settings_clause`）：只读与资源上限由 agent 在请求层统一施加，调用方不得通过 `SETTINGS readonly=0` / `max_result_rows=0` 覆盖。
- **限制语义分两类**：行数与单格字符超限一律**夹紧 + 回显**（`effective_max_rows`、`next_offset`），不因超限直接失败；token 预算超限仍按 §5 显式失败或摘要模式——前者是载荷整形，后者是预算约束，不可混用。
- **策略以整个请求为单位校验**：请求内出现任何一条写语句即整体拒绝，不逐语句放行。
- `sql` 只能访问 `allowed_schemas` 内的对象，越界返回 `MCP_SCHEMA_OUT_OF_SCOPE`，不做隐式降级；显式 `database.table` 前缀逐个校验，省略 `database` 时实际执行库也必须落在白名单内。
- ClickHouse 数据源（`engine = "clickhouse"`）由独立子进程 `jumcp-connector-clickhouse` 执行（不是 MCP server）：host 负责 L4、database 收窄、行数/单格整形、token 预算、`sql_hash` 与审计；agent 只做连接、`?` 位置绑定、执行与取数。`params` 只支持标量（bool/number/string/null）。
- 结果强制返回 `columns`（`[{name, type}]`）/ `rows`（二维数组）/ `row_count` / `truncated` / `effective_max_rows` / `next_offsets` / `elapsed_ms` / `tokens` / `sql_hash` 元信息。
- 全部调用写审计（**模板固化的唯一数据源**）。

#### 4.3.3 第二层：模板注册表

```
db_run_template(template_id: string, version?: int, params: object)
```

模板定义：

```yaml
template_id: order.daily_summary
version: 1
group: db.read.analytics
risk: read
description: "按日汇总订单量与成交额，支持按渠道过滤"
tags: [order, daily, revenue]
sql: |
  SELECT date_trunc('day', created_at) AS day,
         channel,
         count(*) AS order_cnt,
         sum(amount) AS gmv
  FROM orders
  WHERE created_at >= :start_date
    AND created_at <  :end_date
    AND (:channel IS NULL OR channel = :channel)
  GROUP BY 1, 2
  ORDER BY 1 DESC
params:
  - name: start_date
    type: timestamp
    required: true
    timezone: UTC
  - name: end_date
    type: timestamp
    required: true
    timezone: UTC
  - name: channel
    type: enum
    nullable: true
    enum: [web, app, mini]
constraints:
  allowed_schemas: [env_air]        # 模板可引用的 database，越界即失败
  auto_limit: 1000
  max_result_tokens: 1500
  statement_timeout_ms: 15000      # ClickHouse 映射为 max_execution_time
  forbidden_columns: [user_phone, id_card]
preflight:
  max_scan_rows: 10000000          # 仅数据源支持时强制，否则发布期告警
  require_index: true              # 仅发布期预检
status: published
discoverable: true
callable: false                    # 授权由角色策略判定，见 §4.4.1
owner: data-platform
```

**注册表结构对齐**：`sources` / `tools` / `toolsets` 三层与 MCP Toolbox 的 `tools.yaml` 同构，便于迁移既有声明文件。**执行器只读本计划的 schema**，`tools.yaml` 兼容仅存在于导入/导出边界，禁止运行时双读两套定义。

| 本计划字段 | `tools.yaml` 字段 | 转换规则 |
|---|---|---|
| `template_id`（`order.daily_summary`） | `name`（`search_flights_by_number`） | 点号命名空间 ↔ 下划线名，导入/导出时按 `group` 前缀还原 |
| `executor` / 数据源属性 | `source` | 一一对应；只读账号等连接属性留在部署配置，不进定义 |
| `sql`（`:name` 命名占位符） | `statement`（位置占位符） | ClickHouse 按出现顺序改写为 `?`；导入时反向映射 |
| `params[].type` | `parameters[].type` | 子集直接对应（string / int / float / bool / array）；`enum` / `date` / `timestamp` / `column_ref` 导出时降级为 `string` 并保留约束描述 |
| `group` | `toolsets` 中的成员关系 | 双向一一映射 |
| `description` | `description` | 原样 |
| `risk` / `version` / `status` / `discoverable` / `callable` / `constraints` / `owner` | 无 | 治理字段仅存于本计划；导出丢弃，导入补默认值（`status=draft`） |

**参数类型系统**

| 字段 | 适用类型 | 说明 |
|---|---|---|
| `type` | `string\|int\|float\|bool\|date\|timestamp\|enum\|array<T>\|column_ref` | `column_ref` 仅允许引用 `enum` 白名单内的标识符，由执行器做标识符引用（quote），**不使用绑定**；`array<T>` 本轮未启用（注册期拒绝），启用需实现 `?` 数组绑定与展开 |
| `required` / `nullable` | 全部 | 区分「未提供」与「显式 null」 |
| `max_len` / `min` / `max` / `pattern` | string / 数值 | 服务端校验，失败即 400 |
| `enum` | enum / column_ref | 白名单 |
| `timezone` | date/timestamp | 缺省 UTC；跨时区必须显式传 |
| `empty_string_as_null` | string | 缺省 `true`，避免 `''` 与 NULL 语义混淆 |
| `array_max_items` | array | 防 IN 列表爆炸 |

数组参数统一展开为绑定占位符（`= ANY(:ids)`），不得拼接。

#### 4.3.4 模板 SQL 书写规范

**唯一允许的三类结构变体**：

1. **可选过滤**：`(:p IS NULL OR col = :p)` —— 推荐，单条 SQL，无变体。
2. **白名单标识符**：`{{col:enum}}` 仅在 `column_ref` 参数上替换，替换完成后做标识符引用与语法复检；替换后不得再出现任何 `{{ }}`。
3. **显式变体矩阵**：`variants: [{when: {channel: not_null}, sql_id: with_channel}, ...]`，每个变体是独立、预审核、独立预检的 SQL 文本。

**明令禁止**：任意 Jinja 表达式/循环/过滤器/函数调用；任何 `{{ param }}` 的值插值；条件块内出现用户可控字符串；`sql` 字段在运行期做字符串替换（除第 2 类的标识符替换）。

**校验门禁**：注册与更新时，模板必须通过「渲染后仍为单条 `SELECT`/`WITH` 且通过附录 B 黑名单」的静态检查，否则拒绝入库。

#### 4.3.5 版本钉住与预检

- `db_run_template` 首次解析建立 `(session_id, template_id) → version` 绑定；会话内后续调用必须使用同一版本，显式传不同 `version` 则拒绝（`MCP_VERSION_CONFLICT`）。
- `published` 模板更新产生新 `version`，不改变已有绑定；`deprecated` 返回 `404 + successor_id`。
- **发布前复检**：注册与更新时对 base/variants/column_ref 每个白名单值做 sqlguard（与运行期同一 ClickHouse 方言）复检；发布不依赖 EXPLAIN。运行期不再重复解析 SQL，只做占位符与参数形状校验。

#### 4.3.6 结果缓存

`(template_id, version, normalized_params_hash)` 短 TTL（默认 60s，可按模板配置 0–300s）结果缓存，仅缓存 `read` 类。收益：降 token、降 DB 压力、降延迟。缓存命中必须在结果元信息标注 `cache: hit`，并计入指标分母（避免占比失真）。

#### 4.3.7 结构漂移处理

- 维护数据源 schema 版本快照，变更检测为异步任务（非常规执行路径）。
- 模板执行返回 `undefined_table/undefined_column` → 自动标记 `suspect`、通知 owner、计数；连续 N 次失败或人工确认后自动转 `deprecated` 并给出 `successor_id`。
- 不在每次执行时校验表结构存在性（代价不可接受）。

#### 4.3.8 第三层：通用能力调用入口（非 DB 执行器）

`api.internal.read`、`kb.search` 这类能力若建模为 `endpoint`，需要与 DB 同构的调用面；否则 `describe_capability` 只能给出"看得见调不到"的 schema。

```
invoke_capability(id: string, params: object, version?: int)
  → { ok, data, meta: { capability_id, version, items, truncated, elapsed_ms, cache } }
```

处理链：`id` 解析（不可见或不存在 → `MCP_CAPABILITY_NOT_FOUND`，两者同构，防探测）→ `callable` + 角色授权/租约 → 参数按 §4.3.3 校验 → 按 `executor.type` 分发。

| `executor.type` | 约束 |
|---|---|
| `http` | 仅允许注册期声明的 base URL + 路径模板；**禁止**参数指定绝对 URL；禁止跨 host 重定向；连接/读取超时；响应字节与 token 上限（≤1,500 tokens，超出截断） |
| `kb` | `top_k` 上限（默认 10）、单条文档长度上限、结果 token 上限同上 |
| `template` | **拒绝**，返回 `MCP_USE_DB_RUN_TEMPLATE`（禁止双调用路径） |

统一要求：

- `idempotent: bool` 写入 capability 定义，决定失败是否可重试；未声明幂等的一律不重试。
- `meta.truncated` 必填；结果不写缓存（除 `read` 且 `idempotent=true`，TTL ≤60s）。
- 全部调用写审计（`tool_call`）：对象与被调动作在顶层 `object_type` / `object_id` / `action=invoke` / `outcome`，业务版本与入参在 `data`。每条 `tool_call` / `template_run` 在 `data` 记录调用方原始入参 `arguments`、实际执行的脚本全文 `executed_script`（SQL 类工具另有 `sql` / `params`（或 `bound_params`）/ `sql_hash` / `sql_bytes`），模板另有 `data.version` / `data.params_hash`。
- 版本语义与 §4.3.5 一致：会话内钉住首次解析版本。

> 该入口是"通用分发"而非"通用后门"：可调用集合恒等于当前会话可见且已授权的 capability 集合，参数校验与审计与专用入口完全一致。

---

### 4.4 平面 D：治理与固化平面

#### 4.4.1 状态模型（双布尔）

`status` 是生命周期，`discoverable` / `callable` 是能力位，**正交**：

| status | discoverable | callable | 用途 |
|---|---|---|---|
| draft / review | false | false | 注册、审核中 |
| published | false | true | 系统内部直调（报表服务、定时任务）、灰度白名单 |
| published | true | false | 已发布但该角色未授权（发现即执行期拒绝） |
| published | true | true | 正式开放 |
| deprecated | false | false | 下架，保留 `successor_id` |

转换规则：

- `draft → review → published`：由审批流控制（§4.4.4）。
- `discoverable` 的开启**额外**要求「组可见性配置」；组可见性只决定谁能看见，不改变 `status`。
- `callable` 的开启**额外**要求角色策略显式授权（或存在有效授权租约，§7）。
- 动态注册只更新能力目录，**不自动授予任何非 admin agent 的调用权限**。

#### 4.4.2 查询固化闭环

```
① 采集   db_query_readonly 审计（sql_hash + 参数形状 + 主体 + 频次）
      ↓
② 归一化 字面量 → 占位符，生成结构化指纹（AST 级，非正则）
      ↓
③ 聚类   指纹频次 + 语义相似度；7 日内 ≥ N 次入候选
      ↓
④ 草案   admin agent 生成模板配置草案（status=draft）
      ↓
⑤ 评估   只读复检 / 成本提示 / PII 列检测 / 参数枚举合理性
      ↓
⑥ 审核   审核队列：通过 / 打回 / 修改（记 approval_id）
      ↓
⑦ 入库   draft → review → published（不自动 discoverable）
      ↓
⑧ 灰度   按组开放 discoverable，观察命中率/错误率/延迟
      ↓
⑨ 反馈   命中率与 fallback 率统计 → 低频 deprecated
```

时间口径：①→④ 系统 SLA ≤ 1 个工作日；④→⑦ 受审批 SLA 约束（建议 ≤ 5 个工作日）。**不承诺端到端 7 天。**

#### 4.4.3 审计（单追加表，不物理分库）

物理分离会切断「admin 变更 ↔ 权限判定 ↔ 执行」的因果链。改为**单追加日志表 + 哈希链 + 分类字段 + 分保留策略**：

```
envelope: event_id, ts, tenant, actor{type,id,role}, session_id,
          event_type, payload, prev_hash, hash
event_type ∈ { tools_list_served, capability_search, capability_describe,
               tool_call, template_run, policy_decision, admin_change }
```

`payload` 统一按“主体/动作/对象/前后态/审批/结果/关联”：

```
object_type ∈ {capability, group, tool, template, catalog, request}
object_id, action, outcome{ok,error,denied,summary},
error_code, error_message, from_status, to_status,
changes[{field,before,after}], approval{id,source,verified,submitted_by,approved_by,approved_at}|null,
reason, schema_version, correlation_id, data
```

- `admin_change` 必带 `object_type / object_id / action / outcome`。
- 所有 capability 类 `admin_change` 都写 `data.definition_hash_before / definition_hash_after`（新建无前值为 `null`；发布/提交/可见性等不变更定义的场景二者相等）；group 类只写 `data.group`，不伪造定义哈希。
- `session_id` 只在不可变 envelope / DB 列中保留，不进入新业务载荷与 UI。

| 类 | 保留 | 访问 |
|---|---|---|
| `admin_change` / `policy_decision` | 7 年 | 安全组只读，运维无写权限 |
| `tool_call` / `template_run` | 90 天 | 运维只读 |
| `capability_search` 等 | 30 天 | 运维只读 |

- Append-only：应用账号仅 `INSERT`，无 `UPDATE/DELETE`；哈希链校验作业每日运行。
- `admin_change` 必带 `object_type / object_id / action / outcome`；没有实际变化不写事件。

#### 4.4.4 Admin 治理面

治理面分两条通道，身份与授权各自独立：

**MCP 面**（`group=admin` 的 builtin 工具，agent 可用，仅草案 / 只读 / 可见性）

```
admin_register_template(config)                              # → draft，不发布
admin_review_queue()                                         # 只读
admin_audit_queries(filter, limit ≤ 200, offset)             # 只读
admin_set_visibility(capability_id, discoverable, callable)  # 记 admin_change
```

**HTTP 管理面**（`Authorization: Bearer <JUMCP_ADMIN_TOKENS>`，人工操作）

```
POST /admin/capabilities                 # 登记草案（body: {definition}）；builtin id → 409 MCP_BUILTIN_IMMUTABLE
POST /admin/capabilities/{id}/submit      # draft → review
POST /admin/capabilities/{id}/publish     # review → published；须 approval_id，批准人取 token 派生身份
POST /admin/capabilities/{id}/reject      # review → draft；须 reason；记 admin_change
POST /admin/capabilities/{id}/visibility  # discoverable / callable；无变化不写审计，不生成 approval_id
POST /admin/capabilities/{id}/deprecate   # 下架（body: {successor_id}）；builtin id → 409
GET  /admin/me | /admin/review-queue | /admin/capabilities/{id}/versions
GET  /admin/audit-events | /admin/tools/stats | /admin/tools/{id}/calls
```

- 可见性变更不触发重新审核，也不接受/生成 `approval_id`；只有 `publish` 与已发布定义变更要求显式 `approval_id`（外部票号），批准人一律取 token 派生身份并记入 `approval.approved_by`。
- **MCP 面不含发布与审批工具**：`review → published` 只能由 HTTP 管理面完成；批准人由 token 派生，请求体无法自述，agent 无法自我批准。
- 定义变更走 `PUT /admin/capabilities/{id}/definition`：draft 直接修订；published 须新 `approval_id`，编辑即新版本。
- 查询固化（`fingerprint` → 草案）不设独立工具：维护 agent 用 `admin_audit_queries` 取调用历史，再用 `admin_register_template` 落草案。

**description 即代码**：description 是 prompt injection 高危载体，任何变更重新审核，禁止静默生效；建议由结构化字段（指标名/维度/过滤条件）模板化生成。

**编辑即新版本**：`published` 记录的定义字段（description / 参数 schema / SQL）不可原地 UPDATE，只允许插入新 `version`；旧版本保留可查，不再对新建会话生效。

- `admin_change` 的载荷固定带 `object_type` / `object_id` / `action` / `outcome`；定义哈希写在 `data.definition_hash_before` / `data.definition_hash_after`，审批单与生效时间写在 `approval{id,source,verified,submitted_by,approved_by,approved_at}`。
- 仅改 `discoverable` / `callable` / 预算与超时不触发重新审核，但必须记审计。
- 哈希变化的定义不得沿用旧审批（对应 OWASP MCP03 的 rug pull 模式：审批后改定义等于未审批）。

#### 4.4.5 管理台形态

| 决策 | 内容 |
|---|---|
| 形态 | 薄前端，只做「审核队列 + 工具台账 + 例外处理 + 审计查询」视图；不提供工具调试器 |
| 数据面 | UI 调用 admin API，与 agent 侧 admin group 共用同一套策略判定与审计写入；禁止 UI 直连库改状态 |
| 调试 | 联调与回归用官方 MCP Inspector，不自建第二个调用通道 |
| 权限 | 发布必须填写外部审批单号，提审人与批准人如实记录；审计视图只读，且不可导出后回写状态 |
| 禁止项 | 不提供「以某租户/某 Agent 身份执行工具」的能力；排障依赖 Inspector + 审计回放 |
| 能力准入 | 风险无法可靠分类的能力不提供（集群管理、ACL/授权管理、License 写入类），登记时无法给出 `risk` 与 `annotations` 的一律拒绝 |
| 工具台账 | `GET /admin/tools/stats` 给出注册表全部能力与全部 builtin 的调用统计（`lifecycle_status`/`risk`/`discoverable`/`callable`/`version`/`last_changed_at`），`GET /admin/tools/{id}/calls` 给出单工具调用明细（每条含 `event_id`、`event_type`、`object_type/object_id`、`actor{id,role}`、`outcome`、`tokens`、`cache`、`elapsed_ms`、`version`、`error_code/error_message`、`arguments`、`executed_script`、`bound_params`；只读新载荷，不回退旧字段）；指标全部来自审计聚合，不新增表。人工维护范围＝新建草案（`POST /admin/capabilities`）、提交、发布、可见性、下架（`POST /admin/capabilities/{id}/deprecate`），builtin 只读 |
| 台账口径 | 健康：calls=0 → `unknown`，成功率 ≥95% → `ok`、≥80% → `degraded`、否则 `failing`；成功 = `ok` / `summary`，失败 = `error` / `unknown`；命中率只对 `template` 有意义；重试 = 同一会话内失败后的下一次调用；响应上下文给 avg / p95 / max 与最近 20 次样本。统计窗口 1–30 天，调用明细窗口默认 30 天（1–90） |

配置来源唯一：管理台保存的组可见性与预算，必须能还原为入口层 Header / `_meta` 的等价约束，不允许存在只活在 UI 里的配置。

#### 4.4.6 可修改面：仅限本项目自有数据

**不变量**：任何 MCP tool 都不得改变接入数据源。所有数据源连接只以只读账号 + 只读会话建立（§4.3.1），不存在写能力，也不预留写开关。agent 可修改的只有本项目自有数据，且只能经 admin 通道、只落审计链。

| 数据 | 位置 | 可改方式 | 规则 |
|---|---|---|---|
| capability 定义（模板 SQL / 参数 / 约束 / 描述） | `capability_versions` | 只插入新版本 | 定义哈希变化即新 `version`，须新 `approval_id`；旧版本保留可查 |
| 生命周期 `status` / `discoverable` / `callable` | `capabilities` | 原地更新 | 状态迁移经审批；可见性变更记 `admin_change`；不改定义则不触发重新审核 |
| 调用历史 / 审计 | `audit_events` | 只追加 | append-only trigger + 哈希链；无更新/删除接口 |
| 组、角色、预算、数据源引用 | `config/policy.toml` + 进程环境 | **agent 不可改** | 部署配置，只做收敛；改动走发布流程 |
| 数据源连接与凭证 | 进程环境变量 | **agent 不可改** | 配置只存环境变量名，凭证由部署注入 |

**内置工具边界**：代码注册的常驻工具（检索三件套、`db_query_readonly`、`db_run_template`）标记为 `source=builtin`；维护通道对 builtin 一律拒绝（`MCP_BUILTIN_IMMUTABLE`），且 builtin 不出现在维护队列与调用历史聚类的候选里。

**维护闭环**：维护通道共四个工具（登记草案、审核队列、审计查询、可见性），**不含发布与审批工具**。维护 agent 用 `admin_audit_queries` 读调用历史、用 `admin_register_template` 生成草案；草案只能落在 `draft`，`review → published` 只能经 HTTP 管理面，且必须填写外部审批单号（§4.4.4）。工具定义文本（尤其 `description`）是 prompt injection 载体，任何变更都必须重新审核。维护通道的可写范围严格等于"capability 定义（按版本）/ `discoverable` / `callable`"：`policy.toml`（组、角色、预算、DSN 引用）对其只读，`audit_events` 只能追加。

**明确不做**：不写业务库、不写任何外部系统、不接受 agent 提交的 DSN、不允许 agent 改组/角色/预算。

---

## 5. 上下文预算模型

### 5.1 支持动态工具集的客户端

| 项目 | 预算（tokens） | 说明 |
|---|---|---|
| 元工具（`search_capabilities` / `describe_capability` / `list_groups`） | 400 | 固定 |
| 常驻通用入口（`db_query_readonly` + `db_run_template` + `invoke_capability`） | 600 | 每个 150–250 |
| 组索引（≤20 组，组名 + 计数） | 300 | 硬上限由 §4.1.2 约束 |
| 其他常驻描述 | 400 | 会话/错误语义说明 |
| **初始常驻合计上限** | **2,000** | 硬顶 3,000，超线即视为设计错误 |
| 会话累计注入 schema | **≤12 个**（≈3,000） | 注入后常驻于 transcript，按累计计 |
| 单次 tool result | ≤1,500 | 超出强制聚合/分页 + `truncated` |
| **会话累计 tool result** | **≤2,500** | 结果与 schema 一样常驻 transcript，**必须按会话计**；单轮上限无法约束峰值 |
| **单会话峰值上界** | **2,000 + 3,000 + 2,500 = 7,500** | 目标 ≤8,000；告警阈值 9,000（含 tokenizer 差异缓冲，按 provider 计量校准） |

> schema 按会话累计（12 个）而非按轮计，且与常驻、结果预算联合约束——按轮计不可行：20 个 schema 即达 5,400，单轮上限会先被打破。
> 会话累计注入超限时：返回 `MCP_CONTEXT_BUDGET_EXHAUSTED` 并提示改用模板（`db_run_template` / `invoke_capability` 不新增 schema）。
> 会话累计 result 在计入本次结果后超过 2,500 时，**本次**即只回聚合值与 `row_count`（`summary: true`），明细走分页引用；后续调用保持摘要模式。

**会话口径**：会话即 token 派生的 agent 身份（`<tenant>:<agent_id>`）；schema / 结果累计与模板版本钉住均按该身份分账，不同 agent 互不影响（`X-MCP-Tool-Groups` / `_meta` 只决定可见性，不改变分账）。stdio 无 per-request 身份，统一记在 `default`。

**自维护不改变常驻预算**：维护通道只增删注册表条目，不新增常驻工具；草案与变更走 admin 组，按会话投影，不进入普通 agent 的目录。

**pruning 假设**：上表**不假设客户端会丢弃历史 tool result**。若某客户端确认支持按阈值 pruning，其预算可按 §4.2.0 矩阵单列一行，不得默认沿用。

### 5.2 不支持动态工具集的客户端

不套用上表。按 §4.2.0 矩阵，以「该客户端 + 该组」的实测常驻值为基线，单独设定上限并纳入回归门禁。

平铺阈值：该组可见能力 ≤40 个时直接平铺（省掉一次检索往返，常驻值按实测计入基线）；>40 个时只注入分组索引，按 `search_capabilities` 分页取用。阈值可配，改动需附实测对比。

### 5.3 计量口径

- 服务端按**目标模型 tokenizer 冻结版本**估算，CI 中断言常驻预算与每个 schema 的 token 成本。
- schema 序列化格式（字段顺序、是否含 `$schema`、description 换行）纳入版本化，变更即触发预算回归。
- 客户端回传实测值作为校准数据（不参与实时判定）。

---

## 6. 关键流程与时序

### 6.1 目录获取

```
Agent → Server: tools/list + (Header | _meta)
Server: 鉴权 → 默认值填充 → 越界判定(strict→403) → 预算判定(fail/truncate)
        → 返回：元工具 + 通用入口 + 组索引（不含 schema）
Agent → search_capabilities("订单日汇总") → 候选（已按可见集过滤）
Agent → describe_capability(id) → 完整 schema（或依赖客户端 list_changed）
按 kind 分流：kind=tool → tools/call ｜ kind=template → db_run_template ｜ kind=endpoint → invoke_capability
Server → 授权（租约/角色）→ 执行 → 审计
```

### 6.2 自由查询 → 固化

```
Agent → db_query_readonly(sql) → L1..L4 校验 → 执行 → 审计(sql_hash, 参数形状)
（离线）归一化 → 指纹 → 聚类 → 高频模式
（定时）admin → admin_audit_queries 取高频模式 → admin_register_template → draft → 评估 → HTTP 管理面人工审核 → published
       → admin_set_visibility(capability_id, discoverable=true) → 灰度（组授权由 policy.toml 决定）
（持续）命中率/fallback 率 → 低频 deprecated
```

### 6.3 失败与降级路径

| 场景 | 行为 | 是否计入模板占比分母 |
|---|---|---|
| 越界 group（strict） | 403 `MCP_GROUP_FORBIDDEN` + `offending_groups` | — |
| 预算溢出 | 默认 400 `MCP_BUDGET_EXCEEDED`；`truncate` 模式返回清单 | — |
| 检索无命中 | 返回空 + 当前可见组列表；允许 fallback 到 `db_query_readonly`，结果标注 `fallback: true` | **剔除** |
| 注册表不可用 | 模板调用失败（5xx），**禁止回退自由 SQL**（否则治理被绕过） | 剔除 |
| 模板 deprecated | 404 + `successor_id` | 剔除 |
| 版本冲突 | `MCP_VERSION_CONFLICT`，要求重建会话 | — |
| 参数校验失败 | 400 + 字段级错误 | 计入（反映模板可用性） |
| 注册期 sqlguard 复检失败 | 拒绝入库（draft）；已发布版本不受影响 | — |
| `invoke_capability` 收到 `template` kind | 400 `MCP_USE_DB_RUN_TEMPLATE`（禁止双调用路径） | — |
| endpoint 执行超时 / 5xx | 统一 envelope 返回 `ok:false` + `retryable`；仅 `idempotent=true` 时重试 1 次 | 不计入 DB 分母 |
| `invoke_capability` 目标不可见 | `MCP_CAPABILITY_NOT_FOUND`（与不存在同构） | — |
| 客户端不支持动态工具集 | 按 §4.2.0 降级矩阵 | 正常计 |
| 执行超时 / 取消 / 连接中断（结果未知） | 结果标记为未知（不产生成功返回），**禁止重试与重放**，隔离该连接并写审计；调用方需自行重新查询确认 | 计入（反映执行不稳定性） |
| SQL 引用未声明 schema / 跨库越界 | `MCP_SCHEMA_OUT_OF_SCOPE`，不隐式降级到默认 schema | 计入 |
| 同幂等键异参（若启用幂等） | `MCP_IDEMPOTENCY_CONFLICT`，不执行 | — |

### 6.4 错误通道

`tools/call` 的失败统一放在 `result.content.error`，并以 `result.isError=true` 标记，协议层不抛 JSON-RPC error：

```
{"jsonrpc":"2.0","id":1,"result":{"content":{"error":{
  "code":"MCP_GROUP_FORBIDDEN","message":"...",
  "data":{"offending_groups":["db.admin.template"],
          "hint":"请求角色不可见该组；请使用角色默认组或联系平台管理员"}}},
 "isError":true}}
```

- `code` 是稳定的 MCP 错误码（如 `MCP_CAPABILITY_NOT_FOUND` / `MCP_SQL_REJECTED` / `MCP_UNAUTHORIZED`），`data` 携带结构化上下文。
- 参数形状错误、越权、预算拒绝都走这条通道；调用方据 `isError` 判断成败，不再解析 `data.mcp_code`。
- 管理面 HTTP 接口用 `{"error":{"code","message","data"}}` + 相应状态码（400/401/404/409），与 MCP 面同一套错误码。

**限制**：`content.error` 不是 JSON-RPC error，忽略 `isError` 的客户端会把它当成功返回；模型侧也只能据 `code` / `message` 自行恢复或交人工。任何情况下**不得**用"返回空目录"来规避（那会退化为静默降级）。

---

## 7. 安全与权限模型

| 风险 | 对策 |
|---|---|
| Header 伪造提权 | 服务端鉴权后按 §4.1.3 判定；Header 仅"请求子集" |
| 非 HTTP 通道伪造 | `_meta` 仅接受客户端握手期一次性值；禁止从 tool 参数读取 |
| 能力探测 | 检索前硬过滤；不可见 = not found（§4.2.3）；`invoke_capability` 对不可见 id 返回 not-found |
| Admin 被 prompt injection 操控 | 双人/审批流；admin 工具不直连生产；description 变更重审 |
| 模板 description 注入 | 结构化生成 + 变更重审 + 审核队列可见字段级前后值 |
| 自由 SQL 越权/写穿 | L1 只读账号为唯一强保证（§4.3.1）；L2–L4 纵深 |
| 只读账号配置错误 | 部署/巡检验证：只读账号执行 `CREATE TEMP TABLE` / `INSERT` 必须失败；服务启动只验证连接与 agent 握手，不代为验证账号权限 |
| 敏感数据外泄 | 列黑名单 + 行数/token 上限 + 全量审计 |
| 敏感组审批打断 | 授权租约 `(tenant, role, groups, expires_at, requested_by, approval_id)`，会话内复用，不逐请求审批 |
| 模板膨胀 | 单组模板数告警；低频自动 `deprecated`；组数 ≤20 |
| 检索质量恶化 | Top-5 recall 监控 + golden 集回归门禁 |
| `invoke_capability` 成为 SSRF 跳板 | 仅允许注册期声明的 base URL；禁止参数指定绝对 URL；禁止跨 host 重定向；出站白名单 + 响应字节上限 |
| 审计被篡改 | append-only + 哈希链 + 每日校验作业 |
| 审计链断链 | `/health` 暴露 `audit_chain: valid/degraded/broken`；`degraded` 继续写并告警，`broken` 停止注册与可见性变更并告警，读路径不受影响；已提交数据不回滚 |

---

## 8. 实施路线图

| 阶段 | 周期 | 核心交付 | 门禁 |
|---|---|---|---|
| 〇 决策与基线 | 第 0–2 周 | D1–D3 冻结、技术栈与骨架、客户端能力矩阵、golden 集与基线、数据源清单、运行环境与非功能目标 | G0：无"待实测"空白，基线可复跑，staging 可端到端调用 |
| 一 只读面 + 裁剪 + 审计 | 第 1–5 周 | 入口与越界判定、组模型、`db_query_readonly`、启动配置/握手检查、审计链、审批最小版、配置与密钥、可观测与部署 | G1：常驻 ≤2,000，越界零静默降级，staging 验收通过 |
| 二 渐进发现 | 第 6–9 周 | 元工具三件套、可见集检索、预算判定、静态客户端降级 | G2：峰值 ≤8,000，Top-5 recall ≥85% |
| 三 注册表与分发 | 第 10–14 周 | capability 存储、模板校验门禁、`db_run_template`、`invoke_capability`、授权租约 | G3：模板占 DB 调用 ≥40%，静态客户端可调 |
| 四 治理闭环 | 第 15–19 周 | Admin API 与管理台、审批流、聚类固化、评估自动化、灰度淘汰 | G4：admin 审计 100%，模板占比 ≥60% |
| 五 自维护通道 | 第 20–24 周 | 注册表→发现面单一事实源、builtin 边界、维护 MCP 工具组（统计/草案/修改/下架）、调用历史聚合、定时触发与幂等 | G5：维护 agent 产出草案→人工批准→新会话可检索可调用；builtin 零变更；全程审计可回放 |

阶段〇与阶段一后半重叠：骨架、CI 与数据源联调可在决策冻结前启动。工作包拆分、每阶段完成定义、风险回退见同目录 `implementation_plan.md`；指标口径见 §9。

## 9. 验收指标与测量方法

| 类别 | 指标 | 目标 | 测量方法 |
|---|---|---|---|
| 上下文 | 初始常驻 tokens | ≤2,000 | CI 用冻结 tokenizer 计算，每次 schema 变更跑 |
| 上下文 | 单会话峰值 tokens | ≤8,000（告警 9,000） | 会话结束回放 transcript；仅统计支持动态工具集的客户端 |
| 上下文 | 会话累计 tool result tokens | ≤2,500 | 回放 transcript 累计；超限调用须为 `summary:true` |
| 裁剪 | 越界拒绝率 | 100% | 契约测试：越界矩阵逐条断言 403 |
| 裁剪 | 静默降级次数 | 0 | 审计中 `degraded:true` 且无 WARN 记录的计数 |
| 模板 | 常驻工具数随模板增长 | 恒定 | 注册 N 个模板后重测常驻定义 |
| 模板 | 模板调用占 DB 类调用比 | 阶段三 ≥40%，阶段四 ≥60% | 分母 = `db_query_readonly` + `db_run_template` 调用次数（7 日滚动窗口）；**剔除 `fallback:true` 的会话** |
| 固化 | 采集→草案 | ≤1 工作日 | 流水线任务时间戳差 |
| 固化 | 草案→published | 审批 SLA ≤5 工作日 | 审批系统时间戳，超出计流程缺陷 |
| 安全 | admin 变更审计覆盖 | 100% | 每次治理变更都有 `admin_change`，且四件套（`object_type` / `object_id` / `action` / `outcome`）齐全；仅 `publish` 必带 `approval` |
| 安全 | 注册→可发现最小间隔 | ≥1 次独立审批 | 审计链校验 |
| 质量 | `search_capabilities` Top-5 recall | ≥85% | golden 集；判定 = 返回前 5 含 golden id；每 2 周回归 |
| 质量 | 工具选择准确率 | 不低于阶段〇基线 | 同模型、同 prompt、同任务集 A/B；差异 >5% 视为回归 |
| 质量 | fallback 率 | 阶段四 ≤15% | 分母同上 |
| 维护 | 草案采纳率 | 观察指标，不设硬门禁 | 人工批准的 draft / 维护 agent 产出的 draft，按 7 日滚动 |
| 维护 | 维护通道审计覆盖 | 100% | 每次 create/update/deprecate 都有 `admin_change`，变更以 `changes[{field,before,after}]` 记录 |

---

## 10. 风险与对策

| 编号 | 风险 | 影响 | 对策 |
|---|---|---|---|
| R1 | 客户端不支持动态工具集 | 渐进发现失效 | 已前置为 §4.2.0 能力矩阵 + 降级矩阵；G1 不作为跨客户端承诺 |
| R2 | 分组过粗/过多 | 裁剪无效或索引膨胀 | 二维分组 + 组数 ≤20 + 预算 fail 兜底 |
| R3 | 模板检索质量差 | 退回自由 SQL，固化失效 | 混合检索 + 同义词维护 + golden 回归 + Top-N 反馈 |
| R4 | Admin 成为攻击面 | 高危 | 审批流 + 最小权限 + 审计链 + 禁止直连生产 |
| R5 | 多源适配复用度低 | 工期延长 | §3.3 统一 capability 抽象，DB 为首个 `executor` 实现 |
| R6 | token 估算偏差 | 预算失准 | 冻结 tokenizer + 阶段〇校准 + 9,000 告警阈值 |
| R7 | 只读账号配置错误 | 写穿生产 | 部署期只读探测 + 每日权限漂移巡检；服务启动只做连接/握手检查 |
| R8 | 模板变体爆炸 | 审核负担与预检成本 | 三类结构变体白名单 + 变体数上限（≤5/模板） |
| R9 | 双通道配置漂移 | 权限判定不一致 | 通道优先级固定；两个通道的身份各自从本通道凭证派生（HTTP 用 `JUMCP_ADMIN_TOKENS`，`/mcp` 用 `JUMCP_AGENT_TOKENS`），派生的主体写入每条审计事件的 `actor` |
| R10 | 高成本查询绕过成本提示 | 资源压力 | 成本提示不进执行门槛；L1/L3 为硬约束，高成本标记进审核队列 |
| R11 | 审计存储增长 | 成本 | 分类保留 + 冷热分层；`capability_search` 采样保留 |
| R12 | 维护 agent 产出噪声草案 | 审核队列积压、审核质量下降 | 聚合门槛（频次/失败率）+ 同指纹去重 + 每日草案配额；低价值草案自动过期 |
| R13 | 自维护工具定义被投毒 | 模型被 description 引导越权 | 定义哈希钉住 + 变更重审 + builtin 不可改 + 发布必填外部审批单号 |
| R14 | 发现面与注册表不一致 | 新建工具不可检索，或已下架仍可见 | 单一事实源投影 + 启动重建 + 变更后刷新 + 契约测试断言 |

---

## 11. 待决策事项

### 11.1 阻塞（未定则不得进入阶段一）

| # | 决策 | 建议 | 影响 |
|---|---|---|---|
| D1 | groups 越界语义：`strict`（整体失败）还是 `intersect`（交集） | `strict` 为默认；`intersect` 仅租户级可开且必须 `degraded:true` | 决定 G2 是否成立 |
| D2 | 客户端能力基线：是否要求 Agent 客户端升级到支持动态工具集 | 以阶段〇矩阵结果决定；对不支持的客户端单独设基线，不做统一承诺 | 决定 §4.2 形态与 G1 口径 |
| D3 | 状态模型：线性三态还是双布尔 | 双布尔（§4.4.1），系统直调场景必须有 `discoverable=false, callable=true` | 决定注册表数据模型，越晚改成本越高 |

### 11.2 自维护通道待决策（不阻塞阶段一–四）

| # | 决策 | 建议默认 | 不决策的后果 |
|---|---|---|---|
| D4 | 维护 agent 的身份与权限边界 | 独立 role + `admin` 工具组，只能产出 `draft` | 维护通道与普通调用面混在一起，审计无法区分"谁改的工具" |
| D5 | 调用历史聚合的门槛与口径 | 频次 + 失败率 + 7 日未用三条并列，阈值配置化 | 维护 agent 只能凭模型直觉提草案，噪声草案淹没审核队列 |
| D6 | 定时触发归属 | 平台侧 cron 拉起维护 agent；本服务只提供幂等接口 | 服务内建调度会引入第二个时钟与第二套失败语义 |
| D7 | 草案幂等键 | `(group, sql_fingerprint, params_shape)` 去重，重复不产生新草案 | 定时任务反复造同一草案，审核队列不可用 |
| D8 | 自维护工具的定义文本审核强度 | 全人工审核 + 外部审批单号；后续可对低风险情形开策略自动批准 | 自动批准等于把 prompt injection 面直接开放给模型 |
| D9 | 维护范围是否包含跨组移动/批量下架 | 首批只做单条 create/update/deprecate | 批量变更无法逐条审批，回滚粒度丢失 |

### 11.3 本稿已预置默认值（可在评审中推翻，不阻塞）

| 事项 | 本稿取值 |
|---|---|
| 分组命名 | `域.风险级[.子域]`，组数 ≤20 |
| 审批形态 | 策略自动 + 高风险人工（`sensitive` 强制人工） |
| 向量检索 | 复用现有向量库，接口抽象为 `Retriever` |
| token 计量 | 服务端按目标模型 tokenizer 冻结版本；客户端实测仅作校准 |
| 模板归属 | 按数据域归属（`owner` 为团队），生命周期由治理平面统管 |
| 预算溢出 | 默认 `fail`，`truncate` 需显式开启 |
| 结果缓存 | 默认 60s，按模板可配 0–300s |

---

## 12. 附录

### 附录 A：数据源能力矩阵（只读强制能力）

当前唯一接入的数据源是 ClickHouse（connector agent）。强约束只有 L1（只读账号）与 L3（资源/载荷上限）；请求级 `readonly=1`、L4 与注册期复检属于纵深防御。

| 能力 | ClickHouse（connector agent） |
|---|---|
| 只读账号/事务 | ✅ 强（L1：仅 SELECT 授权；ClickHouse 无事务级只读，`readonly` 为查询级设置） |
| 查询级只读 | ✅ `readonly=1`（agent 对每条查询通过请求 URL 参数施加） |
| 语句超时 | ✅ `max_execution_time`（块级软限制）+ agent 客户端硬超时 |
| 行数上限 | ⚠️ `max_result_rows` + `result_overflow_mode=break` 是块级限制；精确上限由 host 应用层截断并回显 `truncated` |
| 读取字节上限 | ⚠️ `max_result_bytes` 同为块级限制；当前未启用，按结果 token 预算兜底 |
| 强制索引 | ❌ 无运行时强制；表引擎与查询计划决定 |
| 危险函数 | ✅ L4 按函数名拦截 ClickHouse 外部访问/执行函数（`url`/`file`/`s3`/`remote`/`jdbc`/`mysql`/`postgresql`/`sqlite`/`mongodb`/`redis`/`odbc`/`hdfs`/`azureBlobStorage`/`gcs`/`iceberg`/`deltaLake`/`hudi`/`executable`/`format`/`input`/`clusterAllReplicas`/`distributed`/`zookeeper`/`arrowFlight`）；`numbers`/`system.*`/`values`/`null` 不受影响 |
| 可写 CTE / 锁 | ClickHouse 无事务写入与行锁；DDL/DML/多语句由 L4 拒绝 |

> 结论：**行扫描与索引命中不能作为运行时约束**，只能作为发布期提示；强约束只有 L1 与 L3。
>
> ClickHouse 没有 PG 的 `default_transaction_read_only` 一类会话锁；只读保证来自只读账号 + 每条查询的 `readonly=1` + L4 守卫，属于纵深防御而非账号级保证。

### 附录 B：AST 黑名单（最小集）

单语句（拒绝多语句与 `;` 拼接）、仅 `SELECT`/`WITH`；拒绝下列结构：

- 可写 CTE（`WITH ... AS (INSERT/UPDATE/DELETE/MERGE ... RETURNING ...)`）
- 行锁：`FOR UPDATE` / `FOR SHARE` / `LOCK IN SHARE MODE`
- 序列与配置：`nextval` / `setval` / `set_config`
- 文件与外部：`COPY`、`pg_read_file`、`pg_ls_dir`、`lo_import`、`lo_export`、`dblink*`、`LOAD_FILE`、`INTO OUTFILE`
- ClickHouse 外部访问/执行函数（大小写不敏感，词法层按「函数名 + `(`」判定）：`url`、`file`、`s3`、`s3Cluster`、`remote`、`remoteSecure`、`jdbc`、`mysql`、`postgresql`、`sqlite`、`mongodb`、`redis`、`odbc`、`hdfs`、`azureBlobStorage`、`gcs`、`iceberg`、`deltaLake`、`hudi`、`executable`、`format`、`input`、`clusterAllReplicas`、`distributed`、`zookeeper`、`arrowFlight`
- 阻塞与副作用：`pg_sleep*`、`pg_advisory_lock*`、`LISTEN` / `NOTIFY`、`pg_terminate_backend`
- 执行控制：`EXPLAIN ANALYZE`（实际执行）、`CALL`、`DO`
- 临时对象与 DDL：`CREATE TEMP TABLE`、`GRANT` / `REVOKE`
- 版本注释绕过：MySQL `/*! ... */`、Unicode 同形字符与控制字符

> 黑名单是纵深防御，不是唯一保证；L1 只读账号必须独立生效。

### 附录 C：参考方案（链接已核验）

| 功能点 | 方案 | 可借鉴点 | 链接 |
|---|---|---|---|
| 工具定义成本 | Anthropic：Code execution with MCP | 定义按需加载、中间结果不进上下文（15 万 → 2 千 tokens） | https://www.anthropic.com/research/code-execution-with-mcp |
| | Anthropic：Writing effective tools | 前缀命名空间化、`response_format: concise/detailed`、结果只回高信号内容 | https://www.anthropic.com/research/writing-tools-for-agents |
| | MCP 讨论 #2812 | 生态基线：123 tok/tool（中位 103），重型工具 800–1,200 | https://github.com/modelcontextprotocol/modelcontextprotocol/discussions/2812 |
| 渐进发现 | RAG-MCP | 工具选择当检索做，只注入命中项；prompt token −50%+，选择准确率提升 3 倍以上 | https://arxiv.org/abs/2505.03275 |
| | Toolshed（Advanced RAG-Tool Fusion） | 工具域扩至数百至数千时检索准确率不显著下降，无需微调 | https://arxiv.org/abs/2410.14594 |
| | Anthropic：Tool Search Tool / Agent Skills | `defer_loading` 与两级渐进披露是客户端侧等价机制 | https://www.anthropic.com/research/advanced-tool-use |
| 注册表分发 | Cloudflare：Code Mode | 单工具 + 服务端分发，整套 API 约 1,000 tokens | https://blog.cloudflare.com/code-mode-mcp |
| | MCP 官方 Registry / `server.json` | 注册项元数据格式（名称 + 位置 + capabilities） | https://modelcontextprotocol.io/registry/about |
| | IBM ContextForge | 虚拟服务器 = 按调用方投影目录；RBAC 判定作用于可见资源 | https://ibm.github.io/mcp-context-forge |
| 只读 SQL 面 | Postgres MCP Pro | `default_transaction_read_only` + 每查询只读事务 + 解析阻断 `COMMIT`/`ROLLBACK` 逃逸 | https://github.com/crystaldba/postgres-mcp |
| | Bytebase DBHub | 工具级 `readonly` 白名单（SELECT/SHOW/DESCRIBE/EXPLAIN）+ `max_rows` | https://github.com/bytebase/dbhub |
| | SQLGlot | 零依赖多方言 SQL 解析，作为 AST 白名单与结构变体校验的实现基础 | https://sqlglot.com/ |
| 管理台 | IBM ContextForge | Admin UI 与 REST API 同源，虚拟服务器即目录投影 | https://ibm.github.io/mcp-context-forge |
| | MCP Inspector（官方） | 开发期调试 UI，resources / prompts / tools / notifications 分栏 | https://modelcontextprotocol.io/docs/draft/tools/debugging |
| | OWASP MCP Top 10 | MCP02 scope creep：管理面即提权面 | https://owasp.org/projects/mcp-top-10 |
| DB 工具形态 | Google MCP Toolbox | `tools.yaml`：sources / tools / toolsets 三层声明 + 类型化参数 + `authRequired` | https://mcp-toolbox.dev/documentation/configuration/tools/ |
| | Google MCP Toolbox：只读三层 | 引擎级会话锁（DSN `*_session_read_only=locked`）+ 写工具裁剪 + `readOnlyHint` 注解；并给出 prompt/正则/运行期 SET 三种"软锁"失效原因 | https://mcp-toolbox.dev/documentation/configuration/security/read-only/ |
| | Google MCP Toolbox：groups | 工具组用于目录裁剪与只读模式下的工具抑制 | https://mcp-toolbox.dev/documentation/configuration/groups/ |
| 权限与调用安全 | DBX（t8y2/dbx）MCP | 最具体作用域继承 + 硬上限、配置只收窄、`max_rows` 夹紧 + 单格续读、outcome unknown 禁止重放 | https://github.com/t8y2/dbx |
| 协议注解 | MCP 规范：Tools | `annotations`（`readOnlyHint` / `destructiveHint` / `idempotentHint` / `openWorldHint`）及其缺省值 | https://modelcontextprotocol.io/specification/2025-06-18/server/tools |
| 动态编辑 | Dynamic FastMCP | 运行期按请求上下文生成工具定义（本计划不采用其按调用者变形） | https://github.com/ragieai/dynamic-fastmcp |
| | OWASP MCP03 Tool Poisoning | 定义哈希钉住、变更重审、不可变版本化注册表 | https://owasp.org/www-project-mcp-top-10/2025/MCP03-2025%E2%80%93Tool-Poisoning |
| 上下文预算 | Lost in the Middle（TACL 2024） | 信息位于中段时利用率显著下降（U 形） | https://arxiv.org/abs/2307.03172 |
| | Chroma：Context Rot（2025-07，18 模型） | 输入变长时输出质量非均匀退化 | https://research.trychroma.com/context-rot |

> 各方案的采纳与放弃理由见 `notes/mcp_plan/research_*.md`；预算与门禁数值以阶段〇实测校准，不引用二手百分比。

---

## 13. 未决与后续

- 阶段〇矩阵与基线结果回填 §4.2.0 / §5.2 后，本稿转为定稿。
- 首个 `executor` 实现（数据库）落地后，评估 API / 知识库执行器能否复用 §3.3 抽象；不能复用则回写契约。
- 契约测试清单（越界矩阵、黑名单用例、预算边界、版本冲突、降级矩阵）作为仓库结构的一部分在阶段一建立。
- 新增 `kind` 时必须同时定义唯一调用面（§2.9）与静态客户端可用性（§3.3 调用面矩阵）；禁止出现"只可发现、无处调用"的建模。
- 若 API / KB 执行器无法复用 §4.3.8 分发链（例如需要流式或多步交互），必须回写 §3.3 契约，不得在 `invoke_capability` 内做特例分支。
- 自维护通道的草案生成口径、调用历史聚合维度在阶段五定稿后回写 §4.4.2；新增维度不得引入第二条调用面。
- 维护通道的 MCP 工具与 HTTP admin API 必须共用同一服务层与同一审计事件，禁止各写一套状态机。
