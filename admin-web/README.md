# jumcp 管理台

Fluent 2 风格的管理面。信息架构以「能力」为主键：列表页只做入口，所有治理与运行期信息收进统一的能力档案 `/capabilities/:id`。

能力档案有两条进入方式：直接访问 `/capabilities/:id`，或在任意列表行点「详情」，在 content 区（视图栈）内打开，URL 不变；视图内的「返回」回到来源列表，切侧栏即清空视图。视图栈只承载 content 区的操作视图，路由仍由侧栏决定。

## 页面

| 路由 | 页面 | 职责 |
|---|---|---|
| `/` | 工作台 | KPI（待审批、未提交草稿两个独立计数）、调用趋势、健康/生命周期分布、工具调用排行、失败榜、数据源、治理动态（主谓宾一句话摘要）；支持「每 30 秒自动刷新」并显示「数据更新于 …」 |
| `/review` | 审核队列 | 分段「待审批」/「未提交草稿」，不暴露英文枚举；显示提审人/提审时间/等待时长；草稿「提交审核」为二次确认弹层（能力 ID/版本/草稿→待审/不可自行撤回）；批准填写外部审批单号 + 发布后可见性两步 |
| `/tools` | 工具台账 | 按「受管能力 / 内置工具」分段；列序按首屏可见性：工具 ID（第二行类型/版本）→ 调用/失败 → 成功率 → 健康 → 生命周期 → 风险 → 可见性 → 组 → 分类 → 最近调用 → 操作；组显示中文名（内置工具显示「内置」，不露出 builtin code），组过滤是页内状态（初始兼容 `?group=<code>`，切换过滤不再写 URL），搜索匹配工具 ID / 组 code / 组名 / 分类中文名；分类列与筛选只显示中文名（基础 / 分析 / 审计 / 自定义；旧数据缺 `category` 显示破折号），可按分类过滤；行内「详情」在 content 区打开能力档案；「新建草案」为弹层，打开聚焦第一个输入，组为必选下拉 |
| `/upstreams` | MCP 接入 | 维护 router 的 MCP 上游：名称/ID、传输（HTTP/stdio）、目标（URL 或 command+args）、授权组、分类、只读、探测状态（正常/降级/错误/未知 + 工具数，title 含最近探测时间）；行内「测试连接」重新探测，支持编辑、启用/停用、删除（二次确认，说明会从 router 移除工具）；新建/编辑弹层支持 env/headers 每行 `KEY=值`（敏感值用 `env:NAME` 引用，GET 脱敏为 `***` 原样回传即保持原值） |
| `/capabilities/:id` | 能力档案 | 概览 / 定义 / 版本 / 审批 / 调用 / 审计 六个分栏；内置工具降级为只读概览 + 调用；概览头部的组显示中文名（内置工具显示「内置」）；草稿「提交审核」同样走二次确认弹层；直接路由仍兼容 `?tab=approval`，视图内切换分栏不写 URL |
| `/groups` | 组维护 | 组 code 为契约值、name 为中文展示名；搜索（code/name）、来源、角色过滤（后端 `q/source/role`）；角色多选发送 code；能力数 / 令牌数；操作列（「查看 / 编辑 / 删除」）紧凑排列，行内「查看」在 content 区打开按该组过滤的工具台账，不改 URL；策略组只读语义只在来源徽标与卡头说明（行内不重复），策略组不可编辑；删除被引用时展示两个计数 |
| `/tokens` | 访问令牌 | 管理员生成 MCP 接入 token：角色、组、内置工具 allowlist、来源、调用者、有效期；状态过滤（全部/有效/已撤销/已过期，客户端）；明文只展示一次；卡片「MCP 接入」一键复制 HTTP MCP JSON 配置（服务名 `jumcp`、URL 同源 `/mcp`、`Authorization: Bearer <token>`），已有 active 令牌需确认重新签发后才能复制；active 只能「撤销」，已撤销/已过期可「删除记录」（先撤销才能删），两者「MCP 接入」按钮禁用；「查看调用」在 content 区打开审计查询并预填该令牌的 token_id（不改 `/audit?token_id`）；管理类工具为橙色徽标并有图例；状态行旁有说明；卡片不重复页面标题 |
| `/audit` | 审计查询 | 默认最新优先；表格列为 时间 → 摘要 → 主体 → 动作 → 对象 → 操作，结果/审批单由摘要与详情弹层承载；时间范围预设/自定义、主体 ID、对象类型、对象 ID、事件 ID；从令牌 / 能力档案 / 工具台账进入时在 content 视图内预填筛选（token_id / object_id / event_id），直接打开 `/audit` 路由仍可用且兼容这些查询参数；筛选与「重置」只改页内状态，不写 URL；结构化摘要 + 变更表 + 审批块 + 执行 SQL/绑定值；主体展示来源与 token ID（表格截断到 12 位，完整值在 title/详情） |
| `/exceptions` | 例外与可见性 | 能力可见性只读清单，用文本「是/否」表达可发现/可调用（不是可点开关），行内「详情」在 content 区打开能力档案 |
| `/health` | 健康检查 | 运行状态（卡头「数据更新于 …」+「刷新」）、审计链校验结论、数据源 |

## 契约要点

- 审计 payload 只认结构化字段：`object_type / object_id / action / outcome / changes / approval / error_code / error_message / correlation_id / data`。缺少结构化字段的旧事件显示「旧格式事件，仅原始载荷」，不按旧字段（`capability_id`、`diff`、`approval_id`）回退。
- `GET /admin/me` 提供当前登录主体；批准人由后端按 token 派生，前端不发送 `approved_by`。
- 审批流：草稿「提交审核」在审核队列与能力档案内都是二次确认弹层（能力 ID / 版本 / 草稿→待审 / 提交后不可自行撤回）；批准走 `POST publish {approval_id}` → `POST visibility {discoverable?, callable?}` 两步，第二步失败时明确提示「已发布但未开放」。
- 审计查询不再有 `session_id`；调用记录不再有会话与 MCP 码，错误统一为 `error_code + error_message`。
- 时间统一显示 `YYYY-MM-DD HH:mm:ss`（本地时区，到秒），不显示毫秒与时区尾注；完整 RFC3339 保留在 `title` 与详情弹层。
- 访问令牌（`/tokens`）：`GET /admin/tokens` 只返回 hash 对应的前缀与元数据，明文 token 仅在 `POST /admin/tokens` 与 `POST /admin/tokens/{id}/reissue` 的响应里出现一次，关闭弹窗后无法再次查看，前端不写 `localStorage`。前缀为 `jumcp_`。调用方用 `POST /mcp` + `Authorization: Bearer <token>` 接入。卡片「MCP 接入」复制 HTTP MCP JSON 配置：本页已知明文（刚创建或刚重签发）直接复制；否则弹「重新签发」确认——重签发原地换钥匙，名称/角色/组/工具授权/到期时间不变，`token`、哈希、前缀与 `last_used_at` 重置，**旧 token 立即失效**。复制内容形状：

  ```json
  {
    "mcpServers": {
      "jumcp": {
        "type": "http",
        "url": "https://<管理台同源>/mcp",
        "headers": { "Authorization": "Bearer jumcp_<64 hex>" }
      }
    }
  }
  ```

  `url` 取页面同源 `/mcp`（生产由网关同源转发，开发期走 Vite `/mcp` 代理）；`revoked` / `expired` 卡片的「MCP 接入」按钮禁用（不能接入，也不能重签发）。令牌卡片另有「查看调用」在 content 视图内打开审计查询并预填 token_id（不改 URL；直接 `/audit?token_id=<id>` 仍可用），审计查询与调用明细的主体均带来源 / 令牌 ID（表格截断到 12 位，完整值在 `title`/详情）。列表顶部按状态（全部 / 有效 / 已撤销 / 已过期）在客户端过滤并显示各自计数；active 只能「撤销」，已撤销 / 已过期可「删除记录」，后端只允许删除非 active 记录（先撤销再删）。管理类工具的徽标为橙色，页面有图例；状态行旁有撤销/过期说明；卡片不再重复页面标题。
- 令牌授权 = 角色 + 组 + 内置工具 allowlist。`builtin_tools` 控制 search/describe/list/db_query_readonly/admin_* 等内置能力；`admin_*` 必须同时勾选 `admin` 组并显式选中该工具。组的 `token_count` 参与删除保护：仍被能力或令牌引用时 `DELETE /admin/groups/{code}` 返回 409，`error.data` 含 `capability_count` 与 `token_count`，页面两个计数都展示。
- 组与角色都是 `code + name` 双字段：`code` 是唯一契约值（能力、令牌、审计 `actor.role`、组 `group` 字段都传 code），界面显示中文 `name` 并用 `title` 保留 code；缺映射时回退 code，不隐藏原值。引用数据统一来自 `GET /admin/groups` 与 `GET /admin/roles`，共享 `groups` / `roles` query key，不逐页重复请求。
- `GET /admin/groups?q=&source=policy|managed&role=<code>`：`q` 同时匹配 code 与 name；页面按后端过滤结果展示，`total` 与筛选一致。组写操作：`POST /admin/groups` 带 `code`，`PUT /admin/groups/{code}` 改名/预算/角色（code 不可改）；策略组只读语义只在来源徽标与卡头说明，行内不重复，策略组不可编辑。
- 审计 `actor` 新增可选 `source` / `token_id`（MCP token 事件携带）；旧事件缺少字段时展示「—」，不按旧字段推断。审计主体角色、组对象与组字段展示中文名（title 保留 code）；token ID 在表格中截断到 12 位，完整值在 `title` 与详情弹层。
- 工具台账与调用明细只聚合审计 v2 事件（带 `object_id`）；改造前的旧格式调用未回填、不计入聚合，页面上有固定口径说明，`calls=0` 不代表从未调用。审计查询中旧格式事件只折叠展示原始载荷，不猜旧字段。

## 启动

推荐用仓库根目录的一键启动脚本（会同时拉起后端并注入管理面凭证）：

```bash
pwsh scripts/dev.ps1      # Windows / PowerShell 7+（后端 + 本前端）
scripts/dev.sh            # Linux / macOS / CI
```

手动起前端时必须带上管理面 token，否则所有请求 401：

```bash
npm install
JUMCP_DEV_ADMIN_TOKEN=tok_alice_000000000 npm run dev   # 监听 0.0.0.0:5173，本机访问 http://127.0.0.1:5173
npm run build      # tsc --noEmit + vite build，产物在 dist/
npm run typecheck
npm run lint
```

## 后端地址与鉴权配置

- `VITE_API_BASE`：后端 admin API 根地址，缺省空串（走同源相对路径）。例：`VITE_API_BASE=http://127.0.0.1:8000`
- 前端不读取也不发送任何管理面凭证：没有 `VITE_ADMIN_*` 变量，`Authorization` / `X-Admin-Actor` 均不由浏览器发出，操作者身份由后端按 token 派生。
- `JUMCP_DEV_API_TARGET`：开发期 Vite 代理的后端地址，默认 `http://127.0.0.1:8000`。与 `JUMCP_DEV_ADMIN_TOKEN` 一样只存在于 Node 进程环境，不是 `VITE_*` 变量、不会进浏览器产物；后端换端口时用它覆盖，例如 `JUMCP_DEV_API_TARGET=http://127.0.0.1:8100 JUMCP_DEV_ADMIN_TOKEN=... npm run dev`。
- 本地开发：Vite 代理把 `/admin`、`/health` 转发到 `JUMCP_DEV_API_TARGET`（默认 `http://127.0.0.1:8000`）并注入 Bearer，token 只存在于 Node 进程环境 —— `JUMCP_DEV_ADMIN_TOKEN=... npm run dev`（后端 token 由 `JUMCP_ADMIN_TOKENS="alice:tok1,bob:tok2"` 配置）。**一键启动脚本已经代你设好这个变量**：`pwsh scripts/dev.ps1`（或 `scripts/dev.sh`）会同时拉起后端与前端并注入 `JUMCP_DEV_ADMIN_TOKEN`，直接开 http://127.0.0.1:5173 即可；手动起则必须自己 export，否则请求全部 401。
- 生产部署：静态产物与后端同源部署，由网关/BFF 在 `/admin` 路径注入 `Authorization`（或改用会话 Cookie），前端不做任何凭证处理；缺凭证时后端返回 401。

## 边界（不可放宽）

- 不提供「以某租户/某 Agent 身份执行工具」的入口，也不提供工具调试器（排障走 MCP Inspector + 审计回放）。
- 审计视图只读：无编辑控件，不得导出后回写状态；UI 不直连数据库，变更须经 admin API 并产生 `admin_change` 审计。
- 审批人、批准时间、提审人均来自后端；前端不提供自由文本审批人输入。
