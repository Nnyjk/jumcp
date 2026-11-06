# Harness Agent 平台 MCP 服务实施计划

> **状态**：预览稿；待 §11.1 三项阻塞决策后定稿。写路径另受 §11.2（D4–D12）约束，阶段五开工前须闭环。
> **首个落地载体**：`hjzl-mcp-server` 仓库。技术栈已定稿（Rust + React，见 `docs/plan/implementation-plan.md` §3.2）；本计划只锁定契约与不变量。
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
- **阶段一–四不注册 `risk ∈ {write, sensitive}` 能力**；阶段五按 §4.4.6 开放 `write`（默认关，`sensitive` 另行开关），不新增 `prepare_call` / `apply_call` 之类工具。

---

## 2. 设计原则

1. **Header 只做可见性裁剪，不做授权。** 授权必须在 `tools/call` 执行前由确定性策略层独立判定。
2. **唯一强只读保证在数据层。** 只读账号 + 只读事务是保证；AST 校验与 EXPLAIN 预检是纵深防御，不是保证。
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
  risk:          read | write | sensitive | admin
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

> `risk ∈ {write, sensitive}` 的能力在调用面前增加确认阶段：同一入口的 `phase=prepare` → `phase=apply`（§4.4.6）。确认不构成第二条调用面，`prepare` 不执行任何副作用。

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
L2 连接/角色级只读     ← 强（DSN 锁参数或 role GUC；非运行期 SET）
L3 资源上限           ← 强（statement_timeout、work_mem、应用层行数截断、结果 token 上限）
L4 AST 单语句 + 黑名单 ← 强（语法级拦截，见附录 B）
L5 EXPLAIN 预检       ← 弱（尽力而为，发布期为主，运行期抽样）
L6 全量审计           ← 事后可追溯
```

**只读设置的施加位置**：只读必须落在连接/角色级（DSN 参数或 role GUC），**禁止**每次执行前做运行期 `SET`：

- 运行期 `SET default_transaction_read_only = on` 可被 `COMMIT; SET ... = off; DROP ...` 链式绕过。
- 连接池（PgBouncer 等）下运行期 `SET` 会污染共享 socket，泄漏到其他服务的写请求。
- 托管 PG 有不可覆盖的会话锁参数（Cloud SQL `cloudsql_session_read_only=locked`、AlloyDB `alloydb_session_read_only=locked`）；自建库用 `ALTER ROLE ... SET default_transaction_read_only = on` + 仅 SELECT 授权。

**只读部署不暴露写工具**：写能力在只读部署中**不注册**，而非注册后靠策略拒绝（目录裁剪即 §4.1 的投影）。

**能力边界必须先声明**：`max_scan_rows`、`required_index_hit` 在 PostgreSQL 上**无法在运行时强制**（PG 无行扫描上限设置；EXPLAIN 计划在绑定参数后可能变化）。这两项标注为「发布期预检 + 数据源支持时强制」，字段名改为 `preflight.max_scan_rows` / `preflight.require_index`，避免被读作运行时保证。附录 A 给出各数据源的强制能力矩阵。生态对照：Postgres MCP Pro 同样以数据库级只读为主保证、SQL 解析只用于阻断 `COMMIT`/`ROLLBACK` 逃逸（附录 C）。

#### 4.3.2 第一层：通用只读入口（常驻 1 个 tool）

```
db_query_readonly(
  sql: string, params?: object,
  max_rows?: int = 100,          # 上限 1000；超限夹紧并回显 effective_max_rows
  max_tokens?: int,
  cell_char_limit?: int = 200,   # 单格返回字符上限，上限 4000
  cell_char_offset?: int = 0,    # 长值续读起点，配合结果中的 next_offset
  allowed_schemas?: string[]     # 缺省取连接声明，不可在入参里扩大
)
```

- L4：仅单条 `SELECT` / `WITH`，AST 校验（非正则）；黑名单见附录 B。
- L1/L2：绑定只读账号 + 连接级只读（DSN 锁参数 / role GUC）+ 只读副本。
- L3：`statement_timeout`、行数上限（应用层截断并标注 `truncated`）、结果 token 上限（§5）。
- **限制语义分两类**：行数与单格字符超限一律**夹紧 + 回显**（`effective_max_rows`、`next_offset`），不因超限直接失败；token 预算超限仍按 §5 显式失败或摘要模式——前者是载荷整形，后者是预算约束，不可混用。
- **策略以整个请求为单位校验**：请求内出现任何一条写语句即整体拒绝，不逐语句放行。
- `sql` 只能访问 `allowed_schemas` 内的对象，越界返回 `MCP_SCHEMA_OUT_OF_SCOPE`，不做隐式降级；存在按库差异化授权时禁止跨库查询。
- 结果强制返回 `row_count / truncated / effective_max_rows / next_offset / elapsed_ms / sql_hash` 元信息。
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
  allowed_schemas: [public]         # 模板可引用的 schema，越界即失败
  auto_limit: 1000
  max_result_tokens: 1500
  statement_timeout_ms: 15000
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
| `sql`（`:name` 命名占位符） | `statement`（`$1` 位置占位符） | 按 `params` 声明顺序编号；导入时反向映射 |
| `params[].type` | `parameters[].type` | 子集直接对应（string / int / float / bool / array）；`enum` / `date` / `timestamp` / `column_ref` 导出时降级为 `string` 并保留约束描述 |
| `group` | `toolsets` 中的成员关系 | 双向一一映射 |
| `description` | `description` | 原样 |
| `risk` / `version` / `status` / `discoverable` / `callable` / `constraints` / `owner` | 无 | 治理字段仅存于本计划；导出丢弃，导入补默认值（`status=draft`） |

**参数类型系统**

| 字段 | 适用类型 | 说明 |
|---|---|---|
| `type` | `string\|int\|float\|bool\|date\|timestamp\|enum\|array<T>\|column_ref` | `column_ref` 仅允许引用 `enum` 白名单内的标识符，由执行器做标识符引用（quote），**不使用绑定**；`array<T>` 本轮未启用（注册期拒绝），启用需实现 `= ANY($n)` 展开与 `T[]` 绑定 |
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
- **EXPLAIN 预检**：发布/更新时全量做（绑定参数用样本值）；运行期按 `(template_id, version, param_shape_hash)` 缓存，TTL 24h，仅对缓存未命中做抽样预检（采样率可配，默认 1%）。预检超时按 `fail-closed` 处理已发布模板。

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
- 全部调用写审计（`tool_call`，含 `capability_id` / `version` / `params_hash` / `outcome`）。
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
⑤ 评估   EXPLAIN(发布期) / 成本预估 / 只读复检 / PII 列检测 / 参数枚举合理性
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
event_type ∈ { session_init, tools_list_served, capability_search,
               capability_describe, tool_call, template_run,
               policy_decision, admin_change }
```

| 类 | 保留 | 访问 |
|---|---|---|
| `admin_change` / `policy_decision` | 7 年 | 安全组只读，运维无写权限 |
| `tool_call` / `template_run` | 90 天 | 运维只读 |
| `capability_search` 等 | 30 天 | 运维只读 |

- Append-only：应用账号仅 `INSERT`，无 `UPDATE/DELETE`；哈希链校验作业每日运行。
- `admin_change` 必带 `before_hash / after_hash / diff / approval_id`。

#### 4.4.4 Admin 治理面

```
admin_register_template(config)           # → draft
admin_publish_template_version(template_id, config_or_diff)  # 生成候选 version 入审核；diff 仅用于构造新版本，不回写已发布记录
admin_promote_query_to_template(fingerprint, meta)
admin_set_visibility(template_id, groups, discoverable)
admin_audit_queries(filter)
admin_review_queue()
```

**description 即代码**：description 是 prompt injection 高危载体，任何变更重新审核，禁止静默生效；建议由结构化字段（指标名/维度/过滤条件）模板化生成。

**编辑即新版本**：`published` 记录的定义字段（description / 参数 schema / SQL）不可原地 UPDATE，只允许插入新 `version`；旧版本保留可查，不再对新建会话生效。

- `definition_hash`（定义全文哈希）与 `approval_id`、生效时间同时写入 `admin_change`；三者缺一视为非法变更。
- 仅改 `discoverable` / `callable` / 预算与超时不触发重新审核，但必须记审计。
- 哈希变化的定义不得沿用旧审批（对应 OWASP MCP03 的 rug pull 模式：审批后改定义等于未审批）。

#### 4.4.5 管理台形态

| 决策 | 内容 |
|---|---|
| 形态 | 薄前端，只做「审核队列 + 例外处理 + 审计查询」视图；不提供工具调试器 |
| 数据面 | UI 调用 admin API，与 agent 侧 admin group 共用同一套策略判定与审计写入；禁止 UI 直连库改状态 |
| 调试 | 联调与回归用官方 MCP Inspector，不自建第二个调用通道 |
| 权限 | 提交与批准必须不同主体（四眼）；审计视图只读，且不可导出后回写状态 |
| 禁止项 | 不提供「以某租户/某 Agent 身份执行工具」的能力；排障依赖 Inspector + 审计回放 |
| 能力准入 | 风险无法可靠分类的能力不提供（集群管理、ACL/授权管理、License 写入类），登记时无法给出 `risk` 与 `annotations` 的一律拒绝 |

配置来源唯一：管理台保存的组可见性与预算，必须能还原为入口层 Header / `_meta` 的等价约束，不允许存在只活在 UI 里的配置。

#### 4.4.6 写路径契约（prepare / apply）

发布期审批管"这个能力能不能存在"，调用期确认管"这一次要不要放行"，两者不可互相替代。字段级细则、校验分层 L7–L12、开关与配额见 `docs/plan/write-path.md`。

**生效范围**：阶段一–四不注册写能力；阶段五开放 `risk=write`，`sensitive` 另行开关，`admin`（TRUNCATE/DDL/GRANT）不开放。写能力默认关：全局/租户/组/能力开关关闭时能力不可见（`MCP_CAPABILITY_NOT_FOUND`）；可见后数据源/角色/会话开关关闭时返回 `MCP_WRITE_DISABLED`。两者都是拒绝，不降级为只读执行。

**调用形态**：不新增工具。prepare/apply 是同一入口的 `phase`，写能力的第二个出口一律不存在（§2 原则 9）。

```
db_run_template(template_id, version?, params, phase?, confirm_token?, idempotency_key?)
invoke_capability(capability_id, version?, params, phase?, confirm_token?, idempotency_key?)
```

| 阶段 | 入参 | 副作用 | 返回 |
|---|---|---|---|
| prepare | 省略 `phase` 或 `phase="prepare"` | 无；只做校验、`EXPLAIN` 预估、落意图、签发令牌 | `summary` + `confirm_token` + `expires_at` + `status` |
| apply | `phase="apply"` + `confirm_token` + 相同 `params` | 执行一条 DML | `affected_rows` / `returning` / `replayed` / `elapsed_ms` |

`summary` 必含：能力 id/version、目标数据源与 schema、操作类型与目标表、写入列、谓词形状（占位符形式）、`affected_rows` 预估（标注 `preview: estimate` 或 `count`）、`params_hash`、审批要求。**不回传参数值**；参数值仅管理台审批视图可见（加密留存）。

**prepare 请求 / 返回**（`db_run_template` 形态；`invoke_capability` 同构）：

```json
{"template_id":"order.mark_paid","version":7,
 "params":{"order_id":"O-1","status":"paid","expected_row_version":3}}
→ {"phase":"prepare","intent_id":"wi_01J9","status":"prepared",
   "confirm_token":"wt_9f2c","expires_at":"2026-11-06T10:20:00Z",
   "summary":{"capability_id":"order.mark_paid","version":7,"datasource":"core-pg",
              "operation":"update","tables":["public.orders"],
              "columns":["status","paid_at","version"],"predicate":"id = $1 AND version = $3",
              "params_hash":"sha256:2b0f","affected_rows_estimate":1,
              "preview":"estimate","approval":{"required":true,"approvers_needed":1}}}
```

**令牌**：`wt_` 前缀的不透明串，服务端只存 SHA-256；单次使用，TTL 300s（60–900 可配），绑定 `tenant + actor + session + capability + version + params_hash` 六元组。apply 以原子 `UPDATE ... WHERE used_at IS NULL AND expires_at > now` 消费，影响行数非 1 即拒绝。重放、过期、跨会话、参数变化统一 `MCP_CONFIRM_TOKEN_INVALID`，`data.reason` 区分。

**apply 顺序**：校验参数哈希与令牌绑定（租户/人/会话/能力/版本）→ 消费令牌 → 重新判定可见性/`callable`/角色授权/开关/审批状态 → 幂等键检查 → 单语句事务执行 → 行数后检查（超限回滚）→ `COMMIT` → 写 `write_apply` 审计 → 返回。提交前任何失败都不产生写入；提交阶段断连返回 `MCP_WRITE_OUTCOME_UNKNOWN`，**禁止重试与重放**。

**语句与行数**：一条语句、一个事务；仅 INSERT/UPDATE/DELETE；UPDATE/DELETE 必须有非退化 WHERE；值只能来自绑定参数，或已声明的自增列/服务端时间列（`write-path.md` L10）；列为能力声明白名单。`affected_rows > max_affected_rows`（默认 100）时整体回滚并返回 `MCP_WRITE_LIMIT_EXCEEDED`，不截断。

**幂等与并发**：`idempotency_key` 按 `(tenant, actor, capability)` 落库，同键同参返回首次结果（`replayed:true`），同键异参 `MCP_IDEMPOTENCY_CONFLICT`；窗口 24h。声明 `optimistic_lock` 的能力在 `affected_rows=0` 时返回 `MCP_ROW_VERSION_MISMATCH`，不自动重试。并发写事务按数据源限流，超限拒绝不排队。

**错误码**（新增，写侧专用）：`MCP_WRITE_DISABLED` / `MCP_WRITE_REJECTED` / `MCP_WRITE_LIMIT_EXCEEDED` / `MCP_WRITE_QUOTA_EXCEEDED` / `MCP_WRITE_APPROVAL_REQUIRED` / `MCP_WRITE_OUTCOME_UNKNOWN` / `MCP_IDEMPOTENCY_CONFLICT` / `MCP_ROW_VERSION_MISMATCH`；复用 `MCP_MULTI_STATEMENT`、`MCP_CONFIRM_TOKEN_INVALID`、`MCP_VERSION_CONFLICT`、`MCP_SCHEMA_OUT_OF_SCOPE`、`MCP_EXEC_TIMEOUT`。

**开关与生效顺序**：

| 级别 | 键 | 默认 | 说明 |
|---|---|---|---|
| 全局 | `HJZL_MCP_WRITE_ENABLED` | `false` | 写能力不进入可见集；`draft` 仍可入库审核 |
| 租户 | `policy.write_enabled` | `false` | 只做收敛 |
| 组 | `[[groups]] write_enabled` | `false` | 组内写能力不可发现/调用 |
| 数据源 | `[datasources.write] enabled` | `false` | 写 DSN 不加载 |
| 能力 | `write.enabled` | `false` | 不进入可见集 |
| 角色/会话 | `policy.write_roles` / `_meta.mcp.write` | 空 / 继承 | 会话只能关，不能开 |

顺序：全局 → 租户 → 组 → 数据源 → 能力 → 角色 → 会话 → 审批 → 配额；全部逻辑与，读取失败按关闭处理。

**审批与配额**：

| 项 | 取值 |
|---|---|
| 谁能写 / 谁能批 | `write_roles` / `approver_roles`；提交人与批准人不同 |
| 批准有效期 / 覆盖次数 | 3600s / 默认 1 次（`sensitive` 固定 1） |
| DBA 二次签核 | 可配，默认关 |
| 时间窗 | 可配，窗外拒绝 |
| 自助低风险 | 仅 staging ∧ 单表主键定位 ∧ 影响 1 行 |
| 笔数配额 | 租户 60/min、会话 10/min、能力 20/min、数据源 30/min |
| 行数配额 | 50000 行/租户/日；并发写事务 2/数据源 |
| 拒绝策略 | 拒绝优先于排队，返回 `retry_after_ms` |

**审计**：prepare 落 `write_intent`（能力/版本/参数哈希/列名/影响行数预估/令牌 id/申请人），提交成功落 `write_apply`（`intent_id`、批准人、`affected_rows`、耗时、`statement_hash`），失败落 `write_failed`（错误码、阶段、是否回滚、`retryable`）。三者进入同一哈希链；审计中不出现参数明文。审计写失败则拒绝写（fail-closed）。

**失败与重试**：

| 场景 | 已写 | 处理 |
|---|---|---|
| 前置拒绝（开关/授权/形状/配额） | 否 | 修正后重新 prepare |
| 令牌/审批失效 | 否 | 重新 prepare 或等审批 |
| 事务内失败（约束/死锁/超时） | 否（已回滚） | 仅死锁与超时可带幂等键重试 |
| 行数超限 / 乐观锁未命中 | 否（回滚） | 修正参数或重读后重新 prepare |
| 提交未知（断连/提交超时） | **未知** | 禁止重试，按幂等键查询或人工核对 |
| 外部系统写 5xx/超时 | 视目标 | 仅 `idempotent=true` + 幂等键重试 1 次 |

不提供跨系统两阶段提交；需要业务一致性时由目标服务本地事务写 outbox，本服务只负责送达与留痕。

**授权与撤销**：写角色 allowlist；四眼在 apply 时复查；撤销权限后下一次 apply 立即失败；回滚/清理类能力保留，避免锁死。写权限被撤销后不得执行新的写操作。

**注解**：写能力 `readOnlyHint=false`；INSERT 为 `destructiveHint=false`，UPDATE/DELETE/upsert 为 `true`；声明幂等键或天然幂等时 `idempotentHint=true`。入口工具在写开关关闭的会话中仍按只读注解投影，开启后按最保守语义投影。

**管理台**：写意图队列与详情（摘要、参数解密视图、批准/拒绝/转 DBA 签核、执行结果、`intent_id` 审计追溯）；UI 不直连库，批准人取自 token。上线后替换「未接通」文案。

> 本契约不改变 JSON-RPC 结构，不新增 MCP 方法；`prepare` 不构成第二条调用面。指标口径见 §9，字段级细则见 `docs/plan/write-path.md`。

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
> 会话累计 result 超限时：后续调用强制「摘要模式」（只回聚合值与 `row_count`，明细走分页引用），并在结果元信息标注 `summary: true`。

**写路径预留**：阶段一–四不注册 `risk = write|sensitive` 能力；阶段五不新增常驻工具（prepare/apply 是 `db_run_template` / `invoke_capability` 的入参），常驻总量不变，启用时只需复核新增入参与描述带来的 token 增量并附实测。

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
Server → 授权（租约/角色）→（write/sensitive：prepare → 人工确认 → apply 复查）→ 执行 → 审计
```

### 6.2 自由查询 → 固化

```
Agent → db_query_readonly(sql) → L1..L4 校验 → 执行 → 审计(sql_hash, 参数形状)
（离线）归一化 → 指纹 → 聚类 → 高频模式
（定时）admin → admin_promote_query_to_template → draft → 评估 → 审核 → published
       → admin_set_visibility(discoverable=true, groups=[...]) → 灰度
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
| EXPLAIN 预检超时 | fail-closed（published 模板）；draft 直接失败 | — |
| `invoke_capability` 收到 `template` kind | 400 `MCP_USE_DB_RUN_TEMPLATE`（禁止双调用路径） | — |
| endpoint 执行超时 / 5xx | 统一 envelope 返回 `ok:false` + `retryable`；仅 `idempotent=true` 时重试 1 次 | 不计入 DB 分母 |
| `invoke_capability` 目标不可见 | `MCP_CAPABILITY_NOT_FOUND`（与不存在同构） | — |
| 客户端不支持动态工具集 | 按 §4.2.0 降级矩阵 | 正常计 |
| 执行超时 / 取消 / 连接中断（结果未知） | 标记 `outcome: unknown`，**禁止重试与重放**，隔离该连接并写审计；调用方需自行重新查询确认 | 计入（反映执行不稳定性） |
| SQL 引用未声明 schema / 跨库越界 | `MCP_SCHEMA_OUT_OF_SCOPE`，不隐式降级到默认 schema | 计入 |
| 确认令牌失效（过期/重放/参数变化） | `MCP_CONFIRM_TOKEN_INVALID`，要求重新 prepare | — |
| 写开关未开 / 目标不在清单 | 不可见 → `MCP_CAPABILITY_NOT_FOUND`；可见但运行期开关关闭 → `MCP_WRITE_DISABLED` + `reason`；两种情况都不进入执行链 | — |
| 写语句未通过 L8–L10 | `MCP_WRITE_REJECTED` + `reason`（形状/WHERE/列/函数/字面量） | — |
| 影响行数超限 | 事务内回滚，`MCP_WRITE_LIMIT_EXCEEDED`，不截断 | — |
| 写提交结果未知 | `MCP_WRITE_OUTCOME_UNKNOWN`，**禁止重试与重放**，按幂等键查询 | — |
| 同幂等键异参 | `MCP_IDEMPOTENCY_CONFLICT`，不执行 | — |

### 6.4 错误通道（协议限制）

MCP `tools/list` / `tools/call` 无独立的"越权"结果通道，越界与预算错误只能走 JSON-RPC error：

```
{"jsonrpc":"2.0","id":1,"error":{"code":-32602,"message":"...",
 "data":{"mcp_code":"MCP_GROUP_FORBIDDEN","offending_groups":["db.admin.template"],
         "hint":"请求角色不可见该组；请使用角色默认组或联系平台管理员"}}}
```

**限制**：模型无法据此自行恢复，只能由客户端提示或人工介入。这是协议面约束，**不得**用"返回空目录"来规避（那会退化为静默降级）。

---

## 7. 安全与权限模型

| 风险 | 对策 |
|---|---|
| Header 伪造提权 | 服务端鉴权后按 §4.1.3 判定；Header 仅"请求子集" |
| 非 HTTP 通道伪造 | `_meta` 仅接受客户端握手期一次性值；禁止从 tool 参数读取 |
| 能力探测 | 检索前硬过滤；不可见 = not found（§4.2.3）；`invoke_capability` 对不可见 id 返回 not-found |
| Admin 被 prompt injection 操控 | 双人/审批流；admin 工具不直连生产；description 变更重审 |
| 模板 description 注入 | 结构化生成 + 变更重审 + 审核队列可见 diff |
| 自由 SQL 越权/写穿 | L1 只读账号为唯一强保证（§4.3.1）；L2–L4 纵深 |
| 只读账号配置错误 | 启动自检：以只读账号执行 `CREATE TEMP TABLE`/`INSERT` 必须失败，否则拒绝启动（fail-fast） |
| 写账号权限过大 | 写 DSN 独立于只读 DSN；启动自检要求 DML 可用、DDL/超管不可用，失败即拒绝启动（§4.4.6、`docs/plan/write-path.md` L7） |
| 误写与越权写 | 单语句 + WHERE 必填 + 列白名单 + 影响行数上限；超限回滚；五级开关默认关 |
| 写操作无法追责 | `write_intent` / `write_apply` / `write_failed` 三类事件入同一哈希链；参数只存哈希与列名 |
| 令牌重放/参数替换 | 单次令牌 + 六元绑定（租户/人/会话/能力/版本/参数哈希）+ 吊销 |
| 敏感数据外泄 | 列黑名单 + 行数/token 上限 + 全量审计 |
| 敏感组审批打断 | 授权租约 `(tenant, role, groups, expires_at, requested_by, approval_id)`，会话内复用，不逐请求审批 |
| 模板膨胀 | 单组模板数告警；低频自动 `deprecated`；组数 ≤20 |
| 检索质量恶化 | Top-5 recall 监控 + golden 集回归门禁 |
| `invoke_capability` 成为 SSRF 跳板 | 仅允许注册期声明的 base URL；禁止参数指定绝对 URL；禁止跨 host 重定向；出站白名单 + 响应字节上限 |
| 审计被篡改 | append-only + 哈希链 + 每日校验作业 |

---

## 8. 实施路线图

| 阶段 | 周期 | 核心交付 | 门禁 |
|---|---|---|---|
| 〇 决策与基线 | 第 0–2 周 | D1–D3 冻结、技术栈与骨架、客户端能力矩阵、golden 集与基线、数据源清单、运行环境与非功能目标 | G0：无"待实测"空白，基线可复跑，staging 可端到端调用 |
| 一 只读面 + 裁剪 + 审计 | 第 1–5 周 | 入口与越界判定、组模型、`db_query_readonly`、启动自检、审计链、审批最小版、配置与密钥、可观测与部署 | G1：常驻 ≤2,000，越界零静默降级，staging 验收通过 |
| 二 渐进发现 | 第 6–9 周 | 元工具三件套、可见集检索、预算判定、静态客户端降级 | G2：峰值 ≤8,000，Top-5 recall ≥85% |
| 三 注册表与分发 | 第 10–14 周 | capability 存储、模板校验门禁、`db_run_template`、`invoke_capability`、授权租约 | G3：模板占 DB 调用 ≥40%，静态客户端可调 |
| 四 治理闭环 | 第 15–19 周 | Admin API 与管理台、审批流、聚类固化、评估自动化、灰度淘汰 | G4：admin 审计 100%，模板占比 ≥60% |
| 五 写路径 | 第 20–27 周 | 写账号与开关、L8–L10 校验、prepare/apply 令牌、单语句写执行器、幂等与乐观锁、审批与配额、管理台写意图页；G5 之后扩展多执行器与多数据源 | G5：写开关默认全关且越权零放行、超限回滚、四眼与令牌契约全绿、staging 端到端可用 |

阶段〇与阶段一后半重叠：骨架、CI 与数据源联调可在决策冻结前启动。工作包拆分、每阶段完成定义、风险回退见 `docs/plan/implementation-plan.md`；指标口径见 §9。

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
| 安全 | admin 变更审计覆盖 | 100% | 每次 `admin_change` 必须有对应事件与 `approval_id` |
| 安全 | 注册→可发现最小间隔 | ≥1 次独立审批 | 审计链校验 |
| 质量 | `search_capabilities` Top-5 recall | ≥85% | golden 集；判定 = 返回前 5 含 golden id；每 2 周回归 |
| 质量 | 工具选择准确率 | 不低于阶段〇基线 | 同模型、同 prompt、同任务集 A/B；差异 >5% 视为回归 |
| 质量 | fallback 率 | 阶段四 ≤15% | 分母同上 |
| 写 | 写成功率 | ≥99%（剔除前置拒绝与重放） | `write_apply{ok} / (ok + failed)`，按能力 7 日滚动 |
| 写 | 拒绝原因分布 | 单一原因 ≤40%（参数错误除外） | `write_failed` 按 `error_code` + `reason` 分组 / 全部 `write_intent` |
| 写 | affected_rows 分布 | p99 不超过能力上限；7 日内 p95 上升 >20% 告警 | 审计 `write_apply.affected_rows` 分桶；超 10× 均值单独告警 |
| 写 | prepare→apply 比率 | 0.5–0.95 | 已消费令牌 / 新建意图；<0.3 视为模型误用或审批阻塞 |
| 写 | prepare→apply 延迟 | 审批 p95 ≤4h，执行 p95 ≤500ms | 拆分 `approval_wait_ms` 与 `exec_wait_ms` |
| 写 | 误写率 | <0.1% | 24h 内标记误写/补偿的意图数 ÷ apply 成功数，均需关联 `intent_id` |
| 写 | 四眼违规 / 审计一致性 | 0 / 0 | 同人批准计数；有 apply 无 intent 的记录数 |

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
| R7 | 只读账号配置错误 | 写穿生产 | 启动自检 fail-fast + 每日权限漂移巡检 |
| R8 | 模板变体爆炸 | 审核负担与预检成本 | 三类结构变体白名单 + 变体数上限（≤5/模板） |
| R9 | 双通道配置漂移 | 权限判定不一致 | 通道优先级固定 + 生效值写入审计 `session_init` |
| R10 | EXPLAIN 预检误判 | 放行高成本查询 | 预检仅作提示，L1–L3 为硬约束；高成本标记进审核队列 |
| R11 | 审计存储增长 | 成本 | 分类保留 + 冷热分层；`capability_search` 采样保留 |
| R12 | 写路径误伤在线业务 | 锁等待、长事务、批量更新拖垮业务库 | 单语句 + `lock_timeout` + 影响行数上限；按数据源开关逐级关停；读路径不受影响 |
| R13 | 写账号权限漂移 | 账号被授予 DDL/superuser，写面变全权 | 启动自检 W2/W3/W6 + 每日权限巡检；不通过即拒绝启动 |
| R14 | 审批与令牌被绕过 | 未批准写入、令牌重放、参数替换 | 四眼在 apply 复查 + 六元绑定 + 单次消费 + 吊销；异常计数触发全局关写 |

---

## 11. 待决策事项

### 11.1 阻塞（未定则不得进入阶段一）

| # | 决策 | 建议 | 影响 |
|---|---|---|---|
| D1 | groups 越界语义：`strict`（整体失败）还是 `intersect`（交集） | `strict` 为默认；`intersect` 仅租户级可开且必须 `degraded:true` | 决定 G2 是否成立 |
| D2 | 客户端能力基线：是否要求 Agent 客户端升级到支持动态工具集 | 以阶段〇矩阵结果决定；对不支持的客户端单独设基线，不做统一承诺 | 决定 §4.2 形态与 G1 口径 |
| D3 | 状态模型：线性三态还是双布尔 | 双布尔（§4.4.1），系统直调场景必须有 `discoverable=false, callable=true` | 决定注册表数据模型，越晚改成本越高 |

### 11.2 写路径待决策（D4–D12，未定则不得进入阶段五）

| # | 决策 | 建议默认 | 不决策的后果 |
|---|---|---|---|
| D4 | 写权限授予方式：角色 allowlist 由谁维护、是否需申请表 | 按组授权，落 `policy.toml` + admin API，不按人开白名单 | 只能按人开口子，转岗/离职后权限残留，审计无法回答"谁被授权" |
| D5 | 是否要求生产写挂工单/变更单号 | 生产写必填 `change_ticket`，staging 不需要 | 生产写无法与变更管理对账，事后只能靠时间线推测 |
| D6 | 写审计保留期 | `write_*` 与 `admin_change` 同为 7 年；参数密文 7 天 | 合规审计缺证，或存储与密钥管理成本无人认领 |
| D7 | 写账号来源与轮换（谁创建、周期、是否独立实例） | 平台 DBA 创建，一数据源一账号，90 天轮换；高价值库独立实例 | 写账号被复用为通用账号，轮换无 owner |
| D8 | 允许写的目标系统清单 | 首批仅内部订单库 staging + 一个只写 API，清单入配置 | 任意注册的写能力都可能指向未评估系统 |
| D9 | PII 列写保护：黑名单由谁定、能否能力级豁免 | 数据源级硬黑名单不可豁免，能力级只能加严 | 敏感列被写能力覆盖，只读侧列黑名单失效 |
| D10 | 生产写是否首批开放 | 不开放，阶段五只到 staging 与内测租户 | 审批组织未就绪即上线，误写直接暴露业务 |
| D11 | 审批视图参数留存：加密方式、密钥归属、留存期 | AES-GCM，密钥在 KMS/环境变量，留存 7 天 | 不留存则审批人盲批；长期明文留存则审计面变 PII 仓库 |
| D12 | 二次签核：是否需要独立 DBA 角色与 SLA | 需要 `dba_roles`，SLA 4h，默认关闭 | 高风险写只有单人批准，四眼退化 |

### 11.3 本稿已预置默认值（可在评审中推翻，不阻塞）

| 事项 | 本稿取值 |
|---|---|
| 分组命名 | `域.风险级[.子域]`，组数 ≤20 |
| 审批形态 | 策略自动 + 高风险人工（sensitive/admin 强制人工） |
| 向量检索 | 复用现有向量库，接口抽象为 `Retriever` |
| token 计量 | 服务端按目标模型 tokenizer 冻结版本；客户端实测仅作校准 |
| 模板归属 | 按数据域归属（`owner` 为团队），生命周期由治理平面统管 |
| 预算溢出 | 默认 `fail`，`truncate` 需显式开启 |
| 结果缓存 | 默认 60s，按模板可配 0–300s |

---

## 12. 附录

### 附录 A：数据源能力矩阵（只读强制能力）

| 能力 | PostgreSQL | MySQL | ClickHouse |
|---|---|---|---|
| 只读账号/事务 | ✅ 强 | ✅ 强 | ✅ 强 |
| 语句超时 | `statement_timeout` ✅ | `max_execution_time` ✅ | `max_execution_time` ✅ |
| 行扫描上限 | ❌ 不支持（仅应用层截断） | ❌ 不支持（`LIMIT` 兜底） | ✅ `max_rows_to_read` |
| 读取字节上限 | ❌ | ❌ | ✅ `max_bytes_to_read` |
| 强制索引 | ❌（`enable_seqscan` 仅提示） | ❌（hint 不保证） | ❌（`force_index_by_date` 有限） |
| 危险函数 | `pg_read_file`/`lo_export`/`dblink`/`set_config`/`nextval` | `LOAD_FILE`/`INTO OUTFILE`/`SLEEP`/`:=` | `url()`/`file()`/`remote()` |
| 可写 CTE / 锁 | `WITH ... DELETE RETURNING` ✅存在；`FOR UPDATE` | `FOR UPDATE` | 不支持事务写入（部分表引擎） |

> 结论：**行扫描与索引命中不能作为运行时约束**，只能作为发布期预检与告警；强约束只有 L1–L3。

### 附录 B：AST 黑名单（最小集）

单语句（拒绝多语句与 `;` 拼接）、仅 `SELECT`/`WITH`；拒绝下列结构：

- 可写 CTE（`WITH ... AS (INSERT/UPDATE/DELETE/MERGE ... RETURNING ...)`）
- 行锁：`FOR UPDATE` / `FOR SHARE` / `LOCK IN SHARE MODE`
- 序列与配置：`nextval` / `setval` / `set_config`
- 文件与外部：`COPY`、`pg_read_file`、`pg_ls_dir`、`lo_import`、`lo_export`、`dblink*`、`LOAD_FILE`、`INTO OUTFILE`
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
| 权限与调用安全 | DBX（t8y2/dbx）MCP | 最具体作用域继承 + 硬上限、配置只收窄、prepare/apply 单次确认令牌、outcome unknown 禁止重放、`max_rows` 夹紧 + 单格续读 | https://github.com/t8y2/dbx |
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
- 写路径的方言扩面（MySQL / ClickHouse）在 G5 之后单独评估：无事务或 `affected_rows` 语义不一致的引擎不得进入写面，直到 §4.4.6 与 `docs/plan/write-path.md` 的能力矩阵补齐。
- 写路径新增执行器类型（如 gRPC）必须复用同一 prepare/apply 与同一审计事件，禁止为其增加专用确认参数。
