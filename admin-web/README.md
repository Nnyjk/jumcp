# jumcp 管理台

Fluent 2 风格的管理面。信息架构以「能力」为主键：列表页只做入口，所有治理与运行期信息收进统一的能力档案 `/capabilities/:id`。

能力档案有两条进入方式：直接访问 `/capabilities/:id`，或在任意列表行点「详情」，在 content 区（视图栈）内打开，URL 不变；视图内的「返回」回到来源列表，切侧栏即清空视图。视图栈只承载 content 区的操作视图，路由仍由侧栏决定。

## 页面

页面源码按功能目录拆分：`pages/login/`、`pages/dashboard/`、`pages/audit/`、`pages/tools/`、`pages/groups/`、`pages/capability-dossier/`、`pages/upstreams/`、`pages/tokens/`、`pages/admins/`；`pages/ExceptionPage.tsx` 仍是单文件页面。`pages/audit/` 内部按视图拆成 `ReviewQueuePanel`（审核队列）与 `AuditEventsPanel`（审计事件）。

| 路由 | 页面 | 职责 |
|---|---|---|
| `/login` | 登录 | 用户名 + 口令（`POST /admin/login`）换会话 cookie；已登录访问会直接进管理台，未登录访问其他页面会被带 `?from=<原路径>` 重定向到这里 |
| `/` | 工作台 | KPI（总调用、成功率、失败数、活跃工具、待审批、未提交草稿、审计链）、调用趋势、健康/生命周期分布、工具调用排行、失败榜、治理动态（主谓宾一句话摘要）；支持「每 30 秒自动刷新」并显示「数据更新于 …」 |
| `/tools` | 工具台账 | 按「受管能力 / 内置工具」分段；列序按首屏可见性：工具 ID（第二行类型/版本）→ 调用/失败 → 成功率 → 健康 → 生命周期 → 风险 → 可见性 → 组 → 分类 → 最近调用 → 操作；组显示中文名（归属 `basic` 的内置工具显示「基础」，不露出 `basic`/`builtin` code），组过滤是页内状态（初始兼容 `?group=<code>`，切换过滤不再写 URL），搜索匹配工具 ID / 组 code / 组名 / 分类中文名；分类列与筛选只显示中文名（基础 / 分析 / 审计 / 自定义；旧数据缺 `category` 显示破折号），可按分类过滤；行内「详情」在 content 区打开能力档案；「新建草案」为弹层，打开聚焦第一个输入，组与执行目标（上游 + 工具名 + 可选库，提交为 `definition.target`）必选 |
| `/upstreams` | MCP 接入 | 分「接入清单」「安装目录」两个页签（`role=tablist`，方向键 / Home / End 切换）；旧深链 `/mcp-install` 重定向到本页。「接入清单」维护 router 的 MCP 上游：名称/ID、传输（HTTP/stdio）、目标（URL 或 command+args）、授权组、分类、探测状态（正常/降级/错误/未知 + 工具数，title 含最近探测时间）；行内「测试连接」重新探测，支持编辑、启用/停用、删除（二次确认，说明会从 router 移除工具）；新建/编辑弹层支持 env/headers 每行 `KEY=值`（敏感值用 `env:NAME` 引用，GET 脱敏为 `***` 原样回传即保持原值），授权组下拉只列 `grantable !== false` 的组（不含 `basic`）；行内「基础工具」弹层读取已缓存工具清单（不触网），可「刷新工具」重新探测，逐工具加入/移出 `basic`（eligible 才可开启，管理类给中文原因），只提交实际变更，孤儿与已失效覆盖可清理（提交 `null`）。「安装目录」：顶部按名称 / id 过滤（客户端匹配，显示「显示 x / 共 y」）；每个内置包一张紧凑卡（名称 / id、版本、两行摘要、最多 3 个标签 + 余数、已安装数量），「详情」弹层展示摘要、版本、传输（一律 http）、默认上游 id、标签，以及安装字段表（key / label / kind / 必填 / 密文）、工具清单和已安装实例表（上游 id、名称、授权组、启用、安装版本（落后当前包版本给徽标）、创建/更新时间、编辑配置 / 卸载）；安装弹层按包 `fields` 生成表单，不填写上游 id——按 `default_upstream_id` 自动推导（重复安装依次 `default` → `default-2` → `default-3` …，结果满足 id 正则且不含 `__`），只读提示「将安装为 upstream_id」；名称默认包名，重复安装为「包名 (2)」…，提交前校验包内唯一（忽略大小写），冲突提示「名称已被该包的其它安装使用，请换一个。」且不发请求；后端返回 `MCP_UPSTREAM_CONFLICT` 时提示上游 id 已被占用并刷新目录后重试。授权组只列可授予项，`POST /admin/mcp/catalog/{id}/install`；`password` 字段提交前用 `/admin/transport-key` 的密钥做 AES-256-GCM 传输加密（取不到密钥则提示密码未提交且不发请求）；实例行「编辑配置」弹层只提交安装字段（非密字段按当前值全量提交、空串即清空；密文字段输入框永远为空，`secrets_set` 为真时占位「已设置，留空保持不变」，留空即不带该 key、保持原值，保存后不可回读），走 `PATCH /admin/mcp/catalog/{id}/install/{upstream_id}/config`；「卸载」二次确认后 `DELETE /admin/mcp/catalog/{id}/install/{upstream_id}`；启停、改名、授权组仍走「接入清单」的上游编辑 |
| `/capabilities/:id` | 能力档案 | 概览 / 定义 / 版本 / 审批 / 调用 / 审计 六个分栏；内置工具降级为只读概览 + 调用；概览头部的组显示中文名（归属 `basic` 的内置工具显示「基础」）；草稿「提交审核」同样走二次确认弹层；直接路由仍兼容 `?tab=approval`，视图内切换分栏不写 URL |
| `/groups` | 组维护 | 组 code 为契约值、name 为中文展示名；搜索（code/name）、来源（policy/managed/builtin）、角色过滤（后端 `q/source/role`）；角色多选发送 code；能力数 / 令牌数；操作列（「查看 / 编辑 / 删除」）紧凑排列，行内「查看」在 content 区打开按该组过滤的工具台账，不改 URL；`basic` 是后端合成的只读内置组（`grantable=false`），行内显示「不可编辑」且不提供编辑/删除；策略组只读语义只在来源徽标与卡头说明，策略组不可编辑；受管组可编辑/删除；删除被引用时展示两个计数 |
| `/tokens` | 访问令牌 | 管理员生成 MCP 接入 token：角色、组、内置工具 allowlist、来源、调用者、有效期；组的勾选清单排除 `grantable=false` 的组（`basic` 不是可授权组）；`builtin_tools` 是精确白名单，除内置工具外还需在「上游工具名」文本框按 `<server_id>__<tool>` 显式填写，上游 `basic` 工具同样需要列入；状态过滤（全部/有效/已撤销/已过期，客户端）；明文只展示一次；卡片「MCP 接入」一键复制 HTTP MCP JSON 配置（服务名 `jumcp`、URL 同源 `/mcp`、`Authorization: Bearer <token>`），已有 active 令牌需确认重新签发后才能复制；active 只能「撤销」，已撤销/已过期可「删除记录」（先撤销才能删），两者「MCP 接入」按钮禁用；「查看调用」在 content 区打开审计事件并预填该令牌的 token_id（不改 `/audit?token_id`）；管理类工具为橙色徽标并有图例；状态行旁有说明；卡片不重复页面标题 |
| `/audit` | 审计 | 两个页签，状态与 `?view=review|events` 同步（缺省 events）：<br>「审核队列」（`?view=review`）分段「待审批」/「未提交草稿」，不暴露英文枚举；显示提审人/提审时间/等待时长；草稿「提交审核」为二次确认弹层（能力 ID/版本/草稿→待审/不可自行撤回）；批准填写外部审批单号 + 发布后可见性两步。旧深链 `/review` 重定向到 `/audit?view=review`。<br>「审计事件」默认最新优先；表格列为 时间 → 摘要 → 主体 → 动作 → 对象 → 操作，结果/审批单由摘要与详情弹层承载；时间范围预设/自定义、主体 ID、对象类型、对象 ID、事件 ID（后端固定只返回平台事件，没有 surface 条件）；从令牌 / 能力档案 / 工具台账进入时在 content 视图内预填筛选（token_id / object_id / event_id），直接打开 `/audit` 路由仍可用且兼容这些查询参数；筛选与「重置」只改页内状态，不写 URL；结构化摘要 + 变更表 + 审批块 + 执行 SQL/绑定值；主体展示来源与 token ID（表格截断到 12 位，完整值在 title/详情） |
| `/exceptions` | 例外与可见性 | 能力可见性只读清单，用文本「是/否」表达可发现/可调用（不是可点开关），行内「详情」在 content 区打开能力档案 |
| `/admins` | 账户管理 | 管理账户清单（用户名 / 状态 / 最后登录 / 创建时间，不分页）；「新建账户」弹层（用户名 + 密码 + 确认密码）；「编辑」改用户名、重置密码、启用/停用；「删除」二次确认（当前登录账户不可删）。错误按 `MCP_ADMIN_USER_EXISTS` / `MCP_ADMIN_LAST_ACTIVE` / `MCP_ADMIN_SELF_DELETE` / `MCP_VALIDATION_ERROR` 翻成中文，不直接展示后端 message |

## 契约要点

- 审计 payload 只认结构化字段：`object_type / object_id / action / outcome / changes / approval / error_code / error_message / correlation_id / data`。缺少结构化字段的旧事件显示「旧格式事件，仅原始载荷」，不按旧字段（`capability_id`、`diff`、`approval_id`）回退。
- `GET /admin/session` 提供当前登录主体（`{"actor":{"id","user_id","role","type"}}`，`id` 是用户名（侧栏显示该值），`user_id` 是账号主键（32 位 hex，改自己账号时按它定位））；`GET /admin/me` 提供同一形状的身份，供批准发布取操作者；批准人由后端按会话派生，管理台不发送 `approved_by`。
- 账户管理：`GET /admin/admins`、`POST /admin/admins {username,password}`、`PATCH /admin/admins/{id}`（任意子集 `username`/`password`/`state`）、`DELETE /admin/admins/{id}`；错误码 `MCP_ADMIN_USER_EXISTS`、`MCP_ADMIN_LAST_ACTIVE`、`MCP_ADMIN_SELF_DELETE`、`MCP_VALIDATION_ERROR`。
- 审批流：草稿「提交审核」在审核队列与能力档案内都是二次确认弹层（能力 ID / 版本 / 草稿→待审 / 提交后不可自行撤回）；批准走 `POST publish {approval_id}` → `POST visibility {discoverable?, callable?}` 两步，第二步失败时明确提示「已发布但未开放」。
- 审计事件不返回 `session_id`；调用记录不含会话与 MCP 码，错误统一为 `error_code + error_message`。
- 时间统一显示 `YYYY-MM-DD HH:mm:ss`（本地时区，到秒），不显示毫秒与时区尾注；完整 RFC3339 保留在 `title` 与详情弹层。
- 访问令牌（`/tokens`）：`GET /admin/tokens` 只返回 `token_prefix` 与元数据（不含明文与哈希），明文 token 仅在 `POST /admin/tokens` 与 `POST /admin/tokens/{id}/reissue` 的响应里出现一次，关闭弹窗后无法再次查看，浏览器不写 `localStorage`。前缀为 `jumcp_`。调用方用 `POST /mcp` + `Authorization: Bearer <token>` 接入。卡片「MCP 接入」复制 HTTP MCP JSON 配置：本页已知明文（刚创建或刚重签发）直接复制；否则弹「重新签发」确认——重签发原地换钥匙，名称/角色/组/工具授权/到期时间不变，`token`、哈希、前缀与 `last_used_at` 重置，**旧 token 立即失效**。复制内容形状：

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

  `url` 取页面同源 `/mcp`（生产由管理面同源提供，开发期走 Vite `/mcp` 代理）；`revoked` / `expired` 卡片的「MCP 接入」按钮禁用（不能接入，也不能重签发）。令牌卡片另有「查看调用」在 content 视图内打开审计事件并预填 token_id（不改 URL；直接 `/audit?token_id=<id>` 仍可用），审计事件与调用明细的主体均带来源 / 令牌 ID（表格截断到 12 位，完整值在 `title`/详情）。列表顶部按状态（全部 / 有效 / 已撤销 / 已过期）在客户端过滤并显示各自计数；active 只能「撤销」，已撤销 / 已过期可「删除记录」，后端只允许删除非 active 记录（先撤销再删）。管理类工具的徽标为橙色，页面有图例；状态行旁有撤销/过期说明；卡片不重复页面标题。
- 令牌授权 = 角色 + 组 + 内置工具 allowlist。令牌的「组」只列 `grantable !== false` 的组，`basic` 是内置通道、不是可授权组；`builtin_tools` 是精确白名单，控制 search/describe/list/admin_* 等内置能力，除内置工具外还需在「上游工具名」文本框按 `<server_id>__<tool>` 显式填写，上游 `basic` 工具同样不自动授予；`admin_*` 必须同时勾选 `admin` 组并显式选中该工具。内置 `basic` 只保留三个元工具（`search_capabilities` / `describe_capability` / `list_groups`）；ClickHouse 查询由可安装包 `clickhouse-basic` 提供（在「MCP 接入」的「安装目录」页签安装后成为普通 http 上游，工具 `<upstream_id>__<tool>`），安装字段为 `url` / `username` / `password` / `database` / `allowed_schemas`，`password` 在浏览器先用 `GET /admin/transport-key` 下发的 `JUMCP_TRANSPORT_KEY` 做 AES-256-GCM 传输加密（`enc:v1:` 线格式），后端解密后再用 `JUMCP_SECRET_KEY` 加密落库，目录、安装与 `PATCH .../config` 响应只回 `secrets_set`、不回显明文或密文，通用 `GET` 上游时 env 值统一脱敏为 `***`；上游只读工具可逐项加入 `basic` 组。组的 `token_count` 参与删除保护：仍被能力或令牌引用时 `DELETE /admin/groups/{code}` 返回 409，`error.data` 含 `capability_count` 与 `token_count`，页面两个计数都展示。
- 组与角色都是 `code + name` 双字段：`code` 是唯一契约值（能力、令牌、审计 `actor.role`、组 `group` 字段都传 code），界面显示中文 `name` 并用 `title` 保留 code；缺映射时回退 code，不隐藏原值。引用数据统一来自 `GET /admin/groups` 与 `GET /admin/roles`，共享 `groups` / `roles` query key，不逐页重复请求。
- `GET /admin/groups?q=&source=policy|managed|builtin&role=<code>`：`q` 同时匹配 code 与 name；页面按后端过滤结果展示，`total` 与筛选一致。`source=builtin` 目前返回后端合成的 `basic` 条目：`{code:"basic",name:"基础工具",priority:0,min_budget:800,roles:[],grantable:false,token_count:0,capability_count:<n>}`，它不可编辑、不可删除，行内显示「不可编辑」且不出现编辑/删除按钮。组写操作：`POST /admin/groups` 带 `code`，`PUT /admin/groups/{code}` 改名/预算/角色（code 不可改）；策略组只读语义只在来源徽标与卡头说明，行内不重复，策略组不可编辑；受管组可维护。
- 审计 `actor` 含可选 `source` / `token_id`（MCP token 事件携带）；旧事件缺少字段时展示「—」，不按旧字段推断。审计主体角色、组对象与组字段展示中文名（title 保留 code）；token ID 在表格中截断到 12 位，完整值在 `title` 与详情弹层。
- 工具台账与调用明细只聚合审计 v2 事件（带 `object_id`）；旧格式调用不计入聚合，页面上有固定口径说明，`calls=0` 不代表从未调用。审计事件中旧格式事件只折叠展示原始载荷，不猜旧字段。平台自身的管理操作（登录、审核、发布等）不计入调用统计，口径文案由 `src/components/ScopeNote.tsx` 统一渲染。

## 启动

推荐用仓库根目录的一键启动脚本（会同时拉起管理面后端与 Vite dev server）：

```bash
pwsh scripts/dev.ps1      # Windows / PowerShell 7+（后端 + 管理台）
scripts/dev.sh            # Linux / macOS / CI
```

管理台有自己的端口与登录页：打开后先看到「jumcp 管理台」登录页，输入用户名与密码（`POST /admin/login`）换取会话（HttpOnly cookie），未登录时任何页面都会被重定向回登录页并带上 `?from=<原路径>`。账户由「账户管理」页维护：用户名 3–32 位 `[a-z0-9._-]`，密码至少 12 位；停用的账户不能再登录，最后一个启用中的账户不能被停用或删除，当前登录账户不能删除自己。管理面只认会话 cookie。

```bash
npm install
npm run dev        # 监听 0.0.0.0:5173，本机访问 http://127.0.0.1:5173
npm run build      # tsc --noEmit + vite build，产物在 dist/
npm run typecheck
npm run lint
```

## 后端地址与鉴权配置

- `VITE_API_BASE`：管理面 admin API 根地址，缺省空串（走同源相对路径）。例：`VITE_API_BASE=http://127.0.0.1:8080`
- 身份来自会话 cookie：`POST /admin/login` 下发 `Set-Cookie: jumcp_session=...; HttpOnly; SameSite=Strict; Path=/`，管理台所有请求带 `credentials: 'include'`。浏览器不读取、不存储、也不发送 `Authorization` / `X-Admin-Actor`；`VITE_ADMIN_TOKEN` / `VITE_ADMIN_ACTOR` 被 `vite.config.ts` 硬性拒绝（出现即构建失败），令牌不写 `localStorage`/`sessionStorage`、也不放进 URL。
- 会话探测与登出：`GET /admin/session` 返回当前主体，`POST /admin/logout` 吊销会话（幂等）；任意接口返回 401 时管理台清登录态并跳 `/login?from=<当前路径>`，登录成功后回跳。
- `JUMCP_DEV_API_TARGET` / `JUMCP_DEV_ADMIN_TARGET`：开发期 Vite 代理的两个后端地址。后端拆成两个端口——MCP 面（`/mcp`、`/health`）默认 `http://127.0.0.1:8000`，管理面（`/admin`）默认 `http://127.0.0.1:8080`。两者都只存在于 Node 进程环境，不是 `VITE_*` 变量、不会进浏览器产物；后端换端口时用它们覆盖，例如 `JUMCP_DEV_ADMIN_TARGET=http://127.0.0.1:8180 npm run dev`。
- 本地开发：Vite 代理把 `/admin` 转发到 `JUMCP_DEV_ADMIN_TARGET`（默认 8080）、把 `/mcp`、`/health` 转发到 `JUMCP_DEV_API_TARGET`（默认 8000），代理不注入任何头部——登录后会话 cookie 随同源请求自动带上。一键启动脚本会分别导出这两个变量（`JUMCP_DEV_ADMIN_TARGET`=管理面端口、`JUMCP_DEV_API_TARGET`=MCP 面端口），直接开 http://127.0.0.1:5173 登录即可；单独起管理台时两个后端都需已在对应端口上监听，登录会以「无法连接后端。」提示失败。
- 生产部署：管理面跑在独立端口（示例 52913，MCP 面 52912），静态产物由该端口的服务同源提供；cookie 是 `HttpOnly` 且 `SameSite=Strict`，`Path=/`，HTTP 部署没有 `Secure` 标志（见 `notes/admin_web/login_session.md`）。

## 边界（不可放宽）

- 不提供「以某租户/某 Agent 身份执行工具」的入口，也不提供工具调试器（排障走 MCP Inspector + 审计回放）。
- 审计视图只读：无编辑控件，不得导出后回写状态；UI 不直连数据库，变更须经 admin API 并产生 `admin_change` 审计。
- 审批人、批准时间、提审人均来自后端；管理台不提供自由文本审批人输入。
