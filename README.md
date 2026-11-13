# jumcp

**jumcp**（jump + mcp）是面向 agent 的 MCP **聚合 router**：一个 `/mcp` 端点同时暴露内置工具与动态接入的上游 MCP server，统一做入口裁剪（§4.1）、渐进发现（§4.2）、只读分发（§4.3）与治理审计（§4.4）。架构见 `docs/plan/jumcp-preview.md`。

> 品牌说明：产品对外名称统一为 **jumcp**；crate/包名、`JUMCP_*` 环境变量与数据路径一律使用 `jumcp` 前缀。令牌前缀为 `jumcp_`（新签发与重签发），明文只在创建/重签发响应中出现一次。

仓库地址：<https://github.com/Nnyjk/jumcp>　克隆：`git clone https://github.com/Nnyjk/jumcp.git`

## 能力

- **三组内置工具**：`basic`（`search_capabilities`、`describe_capability`、`list_groups`、`db_query_readonly`、`db_run_template`）、`analysis`（`admin_review_queue`、`admin_register_template`、`admin_set_visibility`）、`admin`（`admin_audit_queries`）。分类只是 `tools/list` / 搜索 / 统计上的展示与检索标签，授权仍看组与 token allowlist。
- **动态上游 MCP server**：管理台或 `POST /admin/upstreams` 接入，工具以 `<server_id>__<tool>` 暴露在同一 `/mcp`；写成功后下一次 `tools/list` / `tools/call` 即生效，单个上游不可达只影响它自己。
- **双传输**：stdio（默认，本地 agent 直接拉起）与 streamable-http（`POST /mcp`）。
- **组 + token allowlist 双重约束**：`groups` 是 token 的硬上限（header / `_meta` 只能收窄），`builtin_tools` 是精确名字白名单（内置 id 与上游暴露名同表约束）；`admin_*` 另需组 `admin` 在 scope 内。管理台访问令牌卡片提供「MCP 接入」一键复制 HTTP MCP JSON 配置：服务名 `jumcp`、URL 为页面同源 `/mcp`、`headers.Authorization = Bearer <token>`；明文只在创建/重签发的响应里出现一次，已有 active 令牌需先重新签发（`POST /admin/tokens/{id}/reissue`，旧 token 立即失效）才能复制配置。
- **治理审计**：写操作与调用统一落哈希链审计（`admin_change` / `policy_decision`），管理台可按令牌 / 对象 / 事件检索并查看详情。
- **管理台**：工作台看板、审核队列、工具台账与能力档案、组维护、访问令牌、MCP 接入、审计查询、例外与可见性、健康检查。
- **只读边界**：数据源工具对**接入数据源只读**（只读账号 + 查询级 `readonly`，不存在写数据源的路径）；上游 MCP 工具默认 `read_only=true`，只有部署显式开启 `JUMCP_MCP_ALLOW_WRITE_UPSTREAM=1` 才允许写上游（默认关闭）。agent 可修改的只有**本项目自有数据**（能力注册表，且只产草案、人工审批后发布，见 §4.4.6）。

## 快速开始

一条命令拉起「后端（streamable-http）+ 管理台（Vite dev）」，打开 http://127.0.0.1:5173 即可试用（两端都监听 `0.0.0.0`，容器/WSL/局域网可用宿主 IP 访问）。

```
pwsh scripts/dev.ps1            # Windows / PowerShell 7+
scripts/dev.sh                  # Linux / macOS / CI
```

| 入口 | 地址 |
|---|---|
| 管理台 | `http://127.0.0.1:5173` |
| MCP endpoint | `POST http://127.0.0.1:8000/mcp`，`Authorization: Bearer <平台 token>` |
| 健康检查 | `http://127.0.0.1:8000/health` |

最小接入示例（token 在管理台「访问令牌」生成，或见下方 `POST /admin/tokens`）：

```bash
curl -X POST http://127.0.0.1:8000/mcp \
  -H "Authorization: Bearer <平台 token>" -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
```

stdio 接入用 `cargo run -p jumcp-server`（默认 stdio）；HTTP 传输用 `JUMCP_MCP_TRANSPORT=streamable-http cargo run -p jumcp-server`。

- 后端默认监听 `0.0.0.0:8000`、管理台默认监听 `0.0.0.0:5173`（`JUMCP_MCP_HOST` / `-BackendPort` / `--backend-port` 可改；`-WebPort` / `--web-port` 被占用时自动 +1 顺延）；本机访问仍用 `http://127.0.0.1:8000` / `http://127.0.0.1:5173`，容器 / WSL / 局域网用宿主 IP。管理台的 `server.host` 固定为 `0.0.0.0`（`admin-web/vite.config.ts`），手动 `npm run dev` 也是全网卡监听。
- 每次启动都先 `cargo build --bin jumcp-server --bin jumcp-connector-clickhouse`（增量构建，不会运行过期二进制）；启动前若后端端口**已有监听者**（多半是上次遗留的旧后端），脚本立即失败并打印端口/排查命令（`netstat -ano | findstr :<port>`、`lsof -i :<port>`），**不会自动杀进程**，也不会把旧进程的 `/health` 当成“就绪”。请先停掉旧后端或换端口。
- **管理面凭证只存在于进程环境**：后端 `JUMCP_ADMIN_TOKENS="alice:tok_alice_000000000,bob:tok_bob_00000000000"`，前端由 Vite 代理注入 `JUMCP_DEV_ADMIN_TOKEN` —— 不设这个变量，管理台请求会全部 401。
- 前端代理目标用 `JUMCP_DEV_API_TARGET` 指定（默认 `http://127.0.0.1:8000`），启动器会按后端实际端口自动设置；手动起前端时后端换端口就要带上它。
- `-ClickHouseDsn <dsn>` / `--clickhouse-dsn <dsn>` 可选：给了就生成 `.dev-data/policy.toml`（追加只读数据源 `dev-clickhouse`，`engine = "clickhouse"`，DSN 走 `JUMCP_DEV_CLICKHOUSE_DSN`）；DSN 形如 `http://<user>:<password>@<host>:<port>[/database]`，凭证只经环境变量传递。`allowed_schemas` 对 ClickHouse 是 **database 白名单**，脚本生成的是空数组（不限制）；不给 DSN 则用 `config/policy.toml` 原样，`db_query_readonly` 返回 `MCP_STORE_ERROR`（属预期）。
- 脚本会同时构建 `jumcp-server` 与 `jumcp-connector-clickhouse`；host 默认在可执行文件同目录查找 agent，也可用 `JUMCP_CLICKHOUSE_AGENT_BIN` 指定绝对路径。
- `-ResetData` / `--reset-data` 启动前清空 `.dev-data/`（默认保留，便于连续试用）；`-NoWeb` / `--no-web` 只起后端。
- Ctrl+C 同时停掉前后端；日志在 `.dev-data/logs/`（`backend.log` / `web.log`），数据在 `.dev-data/{registry,audit}.sqlite3`。

### ClickHouse 试用

```
pwsh scripts/dev.ps1 -ClickHouseDsn 'http://<user>:<password>@<host>:<port>' -NoWeb
curl -X POST http://127.0.0.1:8000/mcp -H "Authorization: Bearer tok_agent_analyst_0001" -H "Content-Type: application/json" -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"db_query_readonly","arguments":{"sql":"SELECT version()","max_rows":10}}}'
```

`params` 是位置数组，按 SQL 中 `?` 的顺序绑定，例如 `{"sql":"SELECT ? + 1","params":[41]}`。`database` 可选：省略时用 DSN 路径中的库（没有路径则是服务端默认库），给出时必须落在 `allowed_schemas` 白名单内；`allowed_schemas` 为空表示不限制。DSN 里的 `<user>`/`<password>` 只经环境变量传入，不写进配置文件、不提交进仓库。

`db_query_readonly` 不接受查询级 `SETTINGS` 子句（如 `SETTINGS readonly=0`）：只读与行数上限由 agent 统一施加，L4 会以 `MCP_SQL_REJECTED`（`reason=settings_clause`）拒绝，避免调用方覆盖。

执行面是独立子进程 `jumcp-connector-clickhouse`（不是 MCP server）：host 做 L4 校验（含 ClickHouse 外部访问函数黑名单）、database 收窄、行数/单格整形、token 预算与审计，agent 只做连接/绑定/执行/取数。每条查询都由 agent 附上 ClickHouse `readonly=1`、`max_execution_time`、`max_result_rows` + `result_overflow_mode=break`。

> **当前联调环境的 `default` 是超级用户**：`readonly=1`、L4 函数黑名单与 `SETTINGS` 拦截只是客户端兜底，不能替代账号权限。黑名单也无法覆盖已存在的 `jdbc`/`mysql` 等外部表引擎、`dictGet` 配置字典或 `system.query_log` 等目录读取。生产必须使用 L1 只读账号（仅 SELECT 授权），并用 `allowed_schemas` 收紧 database 白名单。

## 构建与运行

```bash
cd backend
cargo build --workspace
cargo run -p jumcp-server                                    # stdio（默认）
JUMCP_MCP_TRANSPORT=streamable-http cargo run -p jumcp-server  # HTTP（POST /mcp）
```

配置走 `JUMCP_MCP_*`（`JUMCP_MCP_CONFIG_FILE`、`JUMCP_MCP_AUDIT_PATH`、`JUMCP_MCP_REGISTRY_PATH`、
`JUMCP_MCP_TEMPLATES_FILE`、`JUMCP_MCP_GROUPS_MODE`、`JUMCP_MCP_BUDGET_OVERFLOW=fail|truncate`、`JUMCP_MCP_HOST`（默认 `0.0.0.0`）、`JUMCP_ADMIN_TOKENS="id:token,id:token"`（管理面必需）、`JUMCP_AGENT_TOKENS="agent_id:role:token,..."`（**仅用于启动引导**：`access_tokens` 表为空时一次性导入为数据库 token，之后可移除））。

MCP 接入令牌由管理平台生成并持久化在 registry SQLite 的 `access_tokens` 表；`/mcp` 只认 `Authorization: Bearer <平台 token>`，缺/错/撤销/过期一律 `MCP_UNAUTHORIZED`。创建示例：

```bash
curl -X POST http://127.0.0.1:8000/admin/tokens \
  -H "Authorization: Bearer tok_alice_000000000" -H "Content-Type: application/json" \
  -d '{"name":"分析只读","role":"analyst","groups":["db.read.core"],
       "builtin_tools":["search_capabilities","describe_capability","list_groups","db_run_template"],
       "source":"agent","caller":"agent-analyst","expires_at":null}'
# 201 { "token": "jumcp_…（只出现一次）", "item": { … } }
```

组与内置工具的双重约束：`groups` 是 token 的硬上限（header/`_meta` 只能收窄），`builtin_tools` 是内置工具的精确 allowlist；`admin_*` 还需 `groups` 含 `admin`。撤销用 `POST /admin/tokens/{id}/revoke`；只有已撤销或已过期的记录可用 `DELETE /admin/tokens/{id}` 硬删除（active 必须先撤销，删除写审计）。明文只在创建/重签发响应里出现一次，库里只有 hash：需要重新取回 MCP 接入配置时用 `POST /admin/tokens/{id}/reissue` 原地换钥匙（`id` 与全部授权不变，旧 token 立即失效，撤销/过期令牌 409）。完整契约见 `docs/audit-openness.md` §7.7–§7.11 与 §11。

组与角色都是 **code + name 双轨**：`code` 是权限标识（token 的 `role` / `groups`、`capability.group`、审计 `actor.role`、scope 一律用 code），`name` 只是中文展示名，**不参与权限判定**。`GET /admin/roles` 返回 `{code,name,default_groups,default_budget}`；`GET /admin/groups?q=&source=&role=` 支持按 code/展示名包含、来源（policy|managed）与角色 code 过滤；`POST /admin/groups` 收 `{code,name,priority,min_budget,roles}`，`PUT /admin/groups/{code}` 改 `name/priority/min_budget/roles`（code 不可改）。配置里组写 `[[groups]] code=… name=…`，只写 `name` 的旧配置按 name=code 兼容。

**上游 MCP router**：同一个 `/mcp` 同时暴露内置工具与所有启用上游 MCP server 的工具，上游工具名为 `<server_id>__<tool>`（无 `/mcp/<upstream>` 透传端点）。内置工具按职责分为三个分类：`basic`（基础查询/计算：`search_capabilities`、`describe_capability`、`list_groups`、`db_query_readonly`、`db_run_template`）、`analysis`（工具列表分析与变更提交审计：`admin_review_queue`、`admin_register_template`、`admin_set_visibility`）、`admin`（仅审计操作：`admin_audit_queries`）；分类只用于展示与检索过滤，授权仍看组和 token allowlist。上游配置在管理面 `GET/POST /admin/upstreams`、`PUT/DELETE /admin/upstreams/{id}`、`POST /admin/upstreams/{id}/refresh` 维护，写成功下一次 `tools/list` / `tools/call` 即生效（无需重启）；上游工具按注册时声明的组随 token 的 `groups` 收窄，且与内置工具共用 token 的 `builtin_tools` 名字白名单（要调用上游工具必须把 `<server_id>__<tool>` 列进去）；单个上游不可达只影响它自己。上游声明 `read_only=false` 需 `JUMCP_MCP_ALLOW_WRITE_UPSTREAM=1`；stdio 上游默认关闭，需 `JUMCP_MCP_UPSTREAM_STDIO=1`。字段、命名冲突、脱敏与审计契约见 `docs/mcp-router.md`。

策略与 `[[datasources]]` 同文件；数据源只写
**环境变量名**（`dsn_env`），DSN 由部署注入。非法配置、DSN 缺失或 agent 握手失败时启动即退出（非零码）；L1 只读账号由部署与巡检独立验证，启动不执行写探测（当前联调账号为超级用户）。

## 测试

```bash
cd backend
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace              # 契约测试不需要数据库
```

真实 ClickHouse 用例默认 `#[ignore]`，需要一个可用的 ClickHouse DSN（读取 `JUMCP_TEST_CLICKHOUSE_DSN`）；联调可用现有账号，**生产验证必须使用只读账号**：

```bash
export JUMCP_TEST_CLICKHOUSE_DSN='http://<user>:<password>@<host>:<port>'
cargo build -p jumcp-connector-clickhouse   # ignored 用例会拉起该二进制
cargo test -p jumcp-server -- --ignored
```

## 一键测试

一条命令跑完「后端 fmt/clippy/测试 + 前端 typecheck/build/lint + E2E 冒烟」并输出 PASS/FAIL 汇总表。

```
pwsh scripts/test-all.ps1      # Windows / PowerShell 7+
scripts/test-all.sh            # Linux / macOS / CI
```

参数（两脚本等价）：`-SkipE2E`/`--skip-e2e` 只跑静态检查与测试；`-ClickHouseDsn <dsn>`/`--clickhouse-dsn <dsn>` 设 `JUMCP_TEST_CLICKHOUSE_DSN` 并额外跑 `#[ignore]` 真库用例；`-NoClean`/`--no-clean` 保留临时目录与服务日志。

## 目录

- `backend/crates/jumcp-core/`：`config`、`entry`（三通道 + EffectiveScope）、`audit`（哈希链）、`registry`（双布尔 + 版本审批元数据）、`budget`（§5 上限）
- `backend/crates/jumcp-sqlguard/`：只读 SQL 校验（L4）与结果整形
- `backend/crates/jumcp-server/`：JSON-RPC MCP 服务、`ConnectorExecutor`（拉起 `jumcp-connector-clickhouse` 子进程、L4/database 收窄/整形/预算/审计）、stdio / HTTP 双入口
- `backend/crates/jumcp-connector-clickhouse/`：ClickHouse connector agent（stdin/stdout 按行 JSON-RPC；只做连接/绑定/执行/取数）
- `admin-web/`：React + Vite 管理台（jumcp 治理面）；`npm run typecheck` / `build` / `lint`
- `config/`：`policy.toml`（组、角色、数据源、预算与只读策略）
- `scripts/`：`dev.ps1` / `dev.sh`（一键启动）、`test-all.ps1` / `test-all.sh`（一键测试）
- `docs/`：当前契约与路由文档（见下方文档索引）

## 管理台（admin-web）

Fluent 2 风格管理台，首页是**工作台 / 数据看板**：KPI（总调用、成功率、失败数、活跃工具、待审批、未提交草稿、审计链、数据源）、调用趋势（手写 SVG 折线/面积图）、健康分布（SVG 环形图）、生命周期/来源堆叠条、工具调用排行与失败榜、数据源、治理动态；支持「每 30 秒自动刷新」并显示「数据更新于 …」；图表不引第三方库，全部有加载/错误/空态。

其余页面：**审核队列**（「待审批」与「未提交草稿」两段，批准填写审批单号 + 发布后可见性两步）、**工具台账**（受管能力 / 内置工具分段的可筛选列表）、**能力档案**、**组维护**（策略组只读在来源徽标/卡头表达，不可编辑）、**访问令牌**（创建时明文只展示一次，Bearer 接入 `/mcp`；按角色/组/工具名收窄，已撤销/已过期可删除记录，组引用闭环；卡片「MCP 接入」一键复制 HTTP MCP JSON 配置——服务名 `jumcp`、URL 同源 `/mcp`、`Authorization: Bearer <token>`；明文不可回读，已有 active 令牌需确认重新签发（旧 token 立即失效）后再复制，revoked/expired 按钮禁用）、**MCP 接入**（维护 router 上游：测试连接 / 启停 / 删除，工具按 `<server_id>__<tool>` 暴露）、**审计查询**、**例外与可见性**（能力可见性只读清单）、**健康检查**（运行状态卡头「数据更新于 …」+「刷新」）。

**能力档案 `/capabilities/:id`**：可从工具台账、审核队列、例外与可见性点「详情」，在 content 视图栈内打开（URL 不变），也可直接访问 `/capabilities/:id`；「返回」回到来源列表，切侧栏即清空视图。它是单个能力的唯一工作面，分六个栏——概览（生效范围：组→角色；状态与可见性；治理时间线与窗口指标）、定义（结构化字段视图）、版本（版本行与定义哈希）、审批（提交 / 批准发布 + 第二步可见性 / 退回）、调用（最近 1 / 7 / 30 天的调用记录）、审计（该对象的事件流）。内置工具没有受管定义，只有「概览」与「调用」两栏；审核队列进入时直接落在「审批」栏。草稿「提交审核」（审核队列与档案内一致）走二次确认弹层，展示能力 ID / 版本 / 草稿→待审 / 提交后不可自行撤回。

**工具台账**：按受管能力 / 内置工具分段，列顺序按首屏可见性排：工具 ID（第二行类型/版本）→ 调用/失败 → 成功率 → 健康 → 生命周期 → 风险 → 可见性 → 组 → 分类 → 最近调用 → 操作；组显示中文名（内置工具显示「内置」），分类列只显示中文名（基础 / 分析 / 审计 / 自定义），筛选同样是中文分类；点「详情」在 content 视图内打开能力档案，URL 不变（不再用弹层承载详情）。**新建草案**是独立弹层（打开即聚焦第一个输入，组为必选），创建后进入草稿，需到「审批」栏提交审核。

**审计查询**：默认**最新优先**（`desc`，可切「最旧优先」）；从令牌、能力档案或工具台账进入时在 content 视图内打开并预填筛选（token_id / object_id / event_id），URL 不变，直接访问 `/audit` 路由仍可用并兼容这些查询参数，筛选与「重置」只改页内状态；表格列为 时间 → 摘要 → 主体 → 动作 → 对象 → 操作，结果与审批单不再单列，由摘要与详情弹层承载（主体里的令牌 ID 表格中截断到 12 位，完整值在 `title`/详情）。时间统一显示 `YYYY-MM-DD HH:mm:ss`（本地时区，到秒），完整 RFC3339 保留在 `title` 与详情。详情里对 `tool_call`、`template_run` 展示入参、执行脚本与载荷结构版本。改造前写入的旧事件没有 `object_type` / `object_id` / `action`，界面标注「旧格式事件，仅原始载荷」，不按旧字段推断语义，也不计入工具统计。

所有写操作经 admin API 落审计（`admin_change` / `policy_decision`），前端不直连数据库、不持有管理面凭证。审计事件结构、管理面字段与统计口径见 `docs/audit-openness.md`。本地试用请直接用上面的「快速开始」；手动起法是 `cd admin-web && npm install && npm run dev`，但**必须**带上 `JUMCP_DEV_ADMIN_TOKEN=<管理面 token>`（Vite 代理据此注入 `Authorization`，缺失则请求全部 401），后端 token 由 `JUMCP_ADMIN_TOKENS="id:token,id:token"` 配置。`npm run build` 做类型检查与构建。

## 文档索引

- `docs/mcp-router.md`：router 模型——内置工具三组分类、上游配置与工具暴露规则。
- `docs/audit-openness.md`：审计事件契约、管理面接口字段与统计口径（§7.7–§7.11、§11 令牌契约）。
- `docs/plan/jumcp-preview.md`：架构与设计预览（§4.1–§4.4）。
- `admin-web/README.md`：管理台页面职责、契约要点与本地启动。
