# jumcp

[![CI](https://github.com/Nnyjk/jumcp/actions/workflows/ci.yml/badge.svg?branch=dev)](https://github.com/Nnyjk/jumcp/actions/workflows/ci.yml)

**jumcp**（jump + mcp）是面向 agent 的 MCP **聚合 router**：一个 `/mcp` 端点同时暴露内置工具与动态接入的上游 MCP server，统一做入口裁剪、渐进发现、只读分发与治理审计。仓库自带内置 MCP 包 `clickhouse-basic`（`backend/crates/jumcp-mcp-clickhouse/`，库，无独立二进制）：管理台「MCP 接入」页或 `POST /admin/mcp/catalog/{id}/install` 安装后，产物是一台**普通 HTTP 上游**，指向本进程内嵌端点 `POST /internal/mcp/{package_id}/{upstream_id}`（进程内随机 Bearer token；密码类字段用 `JUMCP_SECRET_KEY` 加密落库），工具以 `<upstream_id>__<tool>` 暴露在同一 `/mcp`。架构与 router 模型见 `docs/mcp-router.md`，插件标准见 `docs/builtin-mcp.md`。

> 品牌说明：产品对外名称统一为 **jumcp**；crate/包名与 `JUMCP_*` 环境变量统一使用 `jumcp` 前缀；本地数据路径保持 `.dev-data/` 的运行约定（不随品牌改名）；令牌前缀为 `jumcp_`（新签发与重签发），明文只在创建/重签发响应中出现一次。

仓库地址：<https://github.com/Nnyjk/jumcp>　克隆：`git clone https://github.com/Nnyjk/jumcp.git`

## 能力

- **内置工具分类 + `basic` 内置组**：`basic`（`search_capabilities`、`describe_capability`、`list_groups` 三个元工具）、`analysis`（`admin_review_queue`、`admin_register_template`、`admin_set_visibility`）、`admin`（`admin_audit_queries`）是 `tools/list` / 搜索 / 统计上的**展示分类**；其中 `basic` 同时是后端合成的**不可编辑内置组**——3 个常驻元工具的 `group` 就是 `basic`，上游只读工具可逐工具加入（见下文「上游 MCP router」）。`analysis` / `admin` 仍只是分类，`admin_*` 仍需组 `admin` 在 scope 内。
- **动态上游 MCP server**：管理台或 `POST /admin/upstreams` 接入，工具以 `<server_id>__<tool>` 暴露在同一 `/mcp`；写成功后下一次 `tools/list` / `tools/call` 即生效，单个上游不可达只影响它自己。
- **内置 MCP 包**：`GET /admin/mcp/catalog` 列出代码内置的静态目录（当前只有 `clickhouse-basic`，含字段定义、工具清单与已安装实例），`POST /admin/mcp/catalog/{id}/install` 按 `config` 生成一台指向内嵌端点的 HTTP 上游（`catalog_id` / `catalog_version` 记录来源），`PATCH /admin/mcp/catalog/{id}/install/{upstream_id}/config` 改配置（secret 传空串保持），`DELETE /admin/mcp/catalog/{id}/install/{upstream_id}` 只卸载该包安装的上游。安装检查顺序固定为 HTTP 模式/密钥 → 包与组 → 字段校验。管理台在「MCP 接入」页。
- **双传输**：stdio（默认，本地 agent 直接拉起）与 streamable-http（`POST /mcp`）。
- **组 + token allowlist 双重约束**：`groups` 是 token 的硬上限（header / `_meta` 只能收窄），`builtin_tools` 是工具名 allowlist（内置 id、上游暴露名 `<server_id>__<tool>`、`<server_id>__*` 整台上游、`*` 全部工具）；`admin_*` 另需组 `admin` 在 scope 内；`basic` 成员不要求 token `groups` 含 `basic` 或上游组，但仍受 `builtin_tools` allowlist 与上游 `enabled`/`read_only` 约束——`basic` 不是授权组，不能写进令牌 `groups`。管理台访问令牌卡片提供「MCP 接入」一键复制 HTTP MCP JSON 配置：服务名 `jumcp`、URL 为页面同源 `/mcp`、`headers.Authorization = Bearer <token>`；明文只在创建/重签发的响应里出现一次，已有 active 令牌需先重新签发（`POST /admin/tokens/{id}/reissue`，旧 token 立即失效）才能复制配置。
- **治理审计**：写操作与调用统一落哈希链审计（`admin_change` / `policy_decision`），管理台可按令牌 / 对象 / 事件检索并查看详情。
- **管理台**：工作台看板、审核队列、工具台账与能力档案、组维护（内置 `basic` 只读不可编辑）、访问令牌、MCP 接入（单页两页签：「接入清单」维护 router 上游，含「基础工具」逐工具开关/刷新/失效覆盖清理；「安装目录」列出内置包并安装/编辑配置/卸载，产物是 HTTP 上游）、审计查询、例外与可见性、健康检查。
- **只读边界**：数据源工具对**接入数据源只读**（只读账号 + 查询级 `readonly`，不存在写数据源的路径）；上游 MCP 工具默认 `read_only=true`，只有部署显式开启 `JUMCP_MCP_ALLOW_WRITE_UPSTREAM=1` 才允许写上游（默认关闭）。agent 可修改的只有**本项目自有数据**（能力注册表，且只产草案、人工审批后发布）。

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

stdio 接入用 `cargo run -p jumcp-server`（默认 stdio）；HTTP 传输用 `JUMCP_MCP_TRANSPORT=streamable-http cargo run -p jumcp-server`。HTTP 模式必须同时注入两把 64 位 hex 密钥（生成方式 `openssl rand -hex 32`），否则启动即失败：`JUMCP_SECRET_KEY` 负责落库加密，`JUMCP_TRANSPORT_KEY` 负责管理台提交密码时的请求体加密。

- 后端默认起两个监听：MCP 面 `0.0.0.0:8000`、管理面 `0.0.0.0:8080`（`JUMCP_MCP_HOST` / `-BackendPort` / `-AdminPort` / `--backend-port` / `--admin-port` 可改，两个端口必须不同）；管理台（Vite）默认监听 `0.0.0.0:5173`（`-WebPort` / `--web-port` 被占用时自动 +1 顺延）。本机访问用 `http://127.0.0.1:8000`（MCP 面）/ `http://127.0.0.1:8080`（管理面）/ `http://127.0.0.1:5173`（管理台 UI），容器 / WSL / 局域网用宿主 IP。管理台的 `server.host` 固定为 `0.0.0.0`（`admin-web/vite.config.ts`），手动 `npm run dev` 也是全网卡监听。
- 每次启动都先 `cargo build --bin jumcp-server --bin jumcp-connector-clickhouse`（增量构建，不会运行过期二进制）；启动前若后端端口**已有监听者**（多半是上次遗留的旧后端），脚本立即失败并打印端口/排查命令（`netstat -ano | findstr :<port>`、`lsof -i :<port>`），**不会自动杀进程**，也不会把旧进程的 `/health` 当成“就绪”。请先停掉旧后端或换端口。
- **管理面凭证只存在于进程环境**：后端 `JUMCP_ADMIN_TOKENS="alice:tok_alice_000000000,bob:tok_bob_00000000000"`；浏览器打开管理台后在**登录页**里输入其中一个 token，换 HttpOnly 会话 cookie。前端不读任何 token 环境变量，也没有任何注入层。
- 前端代理目标由启动器设置：`/admin` 转发到管理面（`JUMCP_DEV_ADMIN_TARGET`，默认 `http://127.0.0.1:8080`）、`/mcp` 与 `/health` 转发到 MCP 面（`JUMCP_DEV_API_TARGET`，默认 `http://127.0.0.1:8000`）。两个变量只存在于 Node 进程环境；手动起前端时后端换了端口就要带上它们。
- `-ClickHouseDsn <dsn>` / `--clickhouse-dsn <dsn>` 可选：给了就生成 `.dev-data/policy.toml`（追加只读数据源 `dev-clickhouse`，`engine = "clickhouse"`，DSN 走 `JUMCP_DEV_CLICKHOUSE_DSN`）；DSN 形如 `http://<user>:<password>@<host>:<port>[/database]`，凭证只经环境变量传递。`allowed_schemas` 对 ClickHouse 是 **database 白名单**，脚本生成的是空数组（不限制）；不给 DSN 则用 `config/policy.toml` 原样。
- 脚本会同时构建 `jumcp-server` 与 `jumcp-connector-clickhouse`（数据源执行 agent；内置 MCP 包编译进 `jumcp-server`，没有独立二进制），并为 HTTP 模式设置固定测试用的 `JUMCP_SECRET_KEY` 与 `JUMCP_TRANSPORT_KEY`（仅本地；生产必须 `openssl rand -hex 32`）；host 默认在可执行文件同目录查找 agent，也可用 `JUMCP_CLICKHOUSE_AGENT_BIN` 指定绝对路径。
- `-ResetData` / `--reset-data` 启动前清空 `.dev-data/`（默认保留，便于连续试用）；`-NoWeb` / `--no-web` 只起后端。
- Ctrl+C 同时停掉前后端；日志在 `.dev-data/logs/`（`backend.log` / `web.log`），数据在 `.dev-data/{registry,audit}.sqlite3`。

### ClickHouse 试用

```
pwsh scripts/dev.ps1 -ClickHouseDsn 'http://<user>:<password>@<host>:<port>' -NoWeb
```

`-ClickHouseDsn` 只负责生成带 `dev-clickhouse` 只读数据源的 `.dev-data/policy.toml`；DSN 里的 `<user>`/`<password>` 只经环境变量传入，不写进配置文件、不提交进仓库。

需要查询 ClickHouse 时，在管理台「MCP 接入」页安装 `clickhouse-basic`：填写 ClickHouse HTTP 端点 `url`（如 `http://ch-host:8123`；无端口时 http 默认 8123、https 默认 8443；不接受带 userinfo 的 URL）、`username`（默认 `default`）与 `password`（页面用 `GET /admin/transport-key` 下发的传输密钥在浏览器里加密后提交，请求体里只有 `enc:v1:` 密文；落库前再用 `JUMCP_SECRET_KEY` 重新加密，页面只回显「已设置」），可选 `database` 与 `allowed_schemas`——后者是逗号分隔的 database 白名单，留空不限制，建议配合只读账号；白名单非空时，未指定库的查询以列表首个库为会话库，多库场景请写库前缀；库名按实际大小写填写，比较不区分大小写。选授权组即可——安装产物是普通 HTTP 上游，指向本进程内嵌端点（见 `docs/builtin-mcp.md`）。工具以 `<upstream_id>__<tool>` 暴露（默认 id 下是 `clickhouse__query` / `clickhouse__list_databases` / `clickhouse__list_tables`），需在令牌的 `builtin_tools` 里授权（`clickhouse__*` 覆盖这台上游的全部工具，`*` 覆盖全部工具；先 `POST /admin/upstreams/<id>/refresh` 拉取工具名后 `/admin/tokens/options` 才会列出它们）；上游工具可逐项加入 `basic` 组。

> ClickHouse 只读语义（只读账号、`readonly=1`、行数/单次执行限制、函数与外部表引擎黑名单）由上游 MCP server 自己保证；本服务只保留入口裁剪、组/令牌授权与调用审计。生产必须使用仅 SELECT 授权的只读账号。

## 构建与运行

```bash
cd backend
cargo build --workspace
cargo run -p jumcp-server                                    # stdio（默认）
JUMCP_MCP_TRANSPORT=streamable-http cargo run -p jumcp-server  # HTTP（POST /mcp）
```

配置走 `JUMCP_MCP_*`（`JUMCP_MCP_CONFIG_FILE`、`JUMCP_MCP_AUDIT_PATH`、`JUMCP_MCP_REGISTRY_PATH`、
`JUMCP_MCP_TEMPLATES_FILE`、`JUMCP_MCP_GROUPS_MODE`、`JUMCP_MCP_BUDGET_OVERFLOW=fail|truncate`、`JUMCP_SECRET_KEY`（HTTP 模式必需，64 位 hex，内置包密码字段的落库加密密钥）、`JUMCP_TRANSPORT_KEY`（HTTP 模式必需，64 位 hex，管理台提交 secret 字段时的传输加密密钥，见 `GET /admin/transport-key`）、`JUMCP_MCP_HOST`（默认 `0.0.0.0`）、`JUMCP_MCP_PORT`（MCP 面端口，默认 `8000`）、`JUMCP_ADMIN_PORT`（管理面端口，默认 `8080`，与 `JUMCP_MCP_PORT` 相同即拒绝启动）、`JUMCP_ADMIN_SESSION_TTL_SECS`（管理台会话 TTL，默认 `43200` = 12 小时）、`JUMCP_ADMIN_COOKIE_SECURE`（置 `1` 才给会话 cookie 加 `Secure`，HTTPS 部署才开；HTTP 下开启会导致浏览器不回传 cookie）、`JUMCP_ADMIN_TOKENS="id:token,id:token"`（管理面必需）、`JUMCP_AGENT_TOKENS="agent_id:role:token,..."`（**仅用于启动引导**：`access_tokens` 表为空时一次性导入为数据库 token，之后可移除；导入的身份工具范围为 `*`，即全部工具——实际可见性仍由授权组与上游开关决定））。

HTTP 模式起**两个监听**：MCP 面（`JUMCP_MCP_PORT`）只挂 `/mcp`、`/internal/mcp/*`、`/health`；管理面（`JUMCP_ADMIN_PORT`）只挂 `/admin/*`、`/health` 与管理台静态站。跨面访问一律 `404`（例如管理面 `POST /mcp`、MCP 面 `GET /admin/me`）。管理台自带登录页：`POST /admin/login {"token":"<admin token>"}` → `204` + HttpOnly 会话 cookie，脚本仍可用 `Authorization: Bearer`。完整契约（cookie 优先、CSRF 同源校验、登录限流、会话表）见 `docs/mcp-router.md` §9.0–9.1。

MCP 接入令牌由管理平台生成并持久化在 registry SQLite 的 `access_tokens` 表；`/mcp` 认 `Authorization: Bearer <平台 token>`，没有该头时回落到 `?access_token=<token>`（仅此参数名，仅 MCP 面；内网用，token 会出现在 URL 里，可随时重签发轮换），缺/错/撤销/过期一律 `MCP_UNAUTHORIZED`。创建示例：

```bash
curl -X POST http://127.0.0.1:8000/admin/tokens \
  -H "Authorization: Bearer tok_alice_000000000" -H "Content-Type: application/json" \
  -d '{"name":"分析只读","role":"analyst","groups":["db.read.core"],
       "builtin_tools":["search_capabilities","describe_capability","list_groups"],
       "source":"agent","caller":"agent-analyst","expires_at":null}'
# 201 { "token": "jumcp_…（只出现一次）", "item": { … } }
```

组与内置工具的双重约束：`groups` 是 token 的硬上限（header/`_meta` 只能收窄），`builtin_tools` 是工具名 allowlist（支持 `<server_id>__*` 与 `*`，安装的上游工具因此可以先授权后接入）；`admin_*` 还需 `groups` 含 `admin`。撤销用 `POST /admin/tokens/{id}/revoke`；只有已撤销或已过期的记录可用 `DELETE /admin/tokens/{id}` 硬删除（active 必须先撤销，删除写审计）。明文只在创建/重签发响应里出现一次，库里只有 hash：需要重新取回 MCP 接入配置时用 `POST /admin/tokens/{id}/reissue` 原地换钥匙（`id` 与全部授权不变，旧 token 立即失效，撤销/过期令牌 409）。完整契约见 `docs/audit-openness.md` §7.7–§7.11 与 §11。

组与角色都是 **code + name 双轨**：`code` 是权限标识（token 的 `role` / `groups`、`capability.group`、审计 `actor.role`、scope 一律用 code），`name` 只是中文展示名，**不参与权限判定**。`GET /admin/roles` 返回 `{code,name,default_groups,default_budget}`；`GET /admin/groups?q=&source=&role=` 支持按 code/展示名包含、来源（policy|managed|builtin）与角色 code 过滤；`POST /admin/groups` 收 `{code,name,priority,min_budget,roles}`，`PUT /admin/groups/{code}` 改 `name/priority/min_budget/roles`（code 不可改）。`basic` 是唯一的 `source=builtin` 条目：`GET /admin/groups` 固定返回 `{code:"basic", name:"基础工具", grantable:false, roles:[], token_count:0, capability_count: 3 + 上游 basic 覆盖数}`，只读；创建/改名/删除一律 409 `MCP_GROUP_RESERVED`，配置文件 `[[groups]]` 也不得定义 `basic` 或 `builtin`。配置里组写 `[[groups]] code=… name=…`，只写 `name` 的旧配置按 name=code 兼容。

**上游 MCP router**：同一个 `/mcp` 同时暴露内置工具与所有启用上游 MCP server 的工具，上游工具名为 `<server_id>__<tool>`（无 `/mcp/<upstream>` 透传端点）。内置工具按职责分为三个**展示分类**：`basic`（基础元工具：`search_capabilities`、`describe_capability`、`list_groups`）、`analysis`（工具列表分析与变更提交审计：`admin_review_queue`、`admin_register_template`、`admin_set_visibility`）、`admin`（仅审计操作：`admin_audit_queries`）；其中 `basic` 同时是代码内置组（3 个常驻元工具的 `group` 就是它，上游只读工具可逐工具加入），`analysis`/`admin` 只是分类，`admin_*` 仍需组 `admin` 在 scope 内。上游配置在管理面 `GET/POST /admin/upstreams`、`PUT/DELETE /admin/upstreams/{id}`、`POST /admin/upstreams/{id}/refresh` 维护（可安装包安装产生的是同一种上游，见 `docs/mcp-router.md` §3），写成功下一次 `tools/list` / `tools/call` 即生效（无需重启）；上游工具按注册时声明的组随 token 的 `groups` 收窄，且与内置工具共用 token 的 `builtin_tools` 名字白名单（要调用上游工具必须把 `<server_id>__<tool>` 列进去）；单个上游不可达只影响它自己。上游声明 `read_only=false` 需 `JUMCP_MCP_ALLOW_WRITE_UPSTREAM=1`；stdio 上游默认关闭，需 `JUMCP_MCP_UPSTREAM_STDIO=1`（只影响手工接入的 stdio 上游；内置包一律走 HTTP）。逐工具基础组：`GET /admin/upstreams/{id}/tools` 只读缓存列出每个工具的生效组（`group`、`group_source`、`basic`、`overridden`、`eligible`、`orphaned`），`PUT /admin/upstreams/{id}/tools/basic` 用 `{"tools":{"<原始工具名>":true|false|null}}` 合并式增删（幂等，无变化不写审计）；上游级 `group` 不得为 `basic`（400 `MCP_UPSTREAM_INVALID`），只能逐工具加入。加入 `basic` 的工具对所有 scope 可见/可调（`X-MCP-Tool-Groups` 收窄不能隐藏它们），但仍受 token `builtin_tools` allowlist 与上游 `enabled`/`read_only` 约束；只有只读、`category≠admin`、工具名不以 `admin_` 开头的工具可以加入，`basic` 因此不会成为绕过组 `admin` 的通道。非 basic 的上游工具仍按上游组 + allowlist 双重判定。字段、命名冲突、可安装包、脱敏与审计契约见 `docs/mcp-router.md`。

策略与 `[[datasources]]` 同文件；数据源只写
**环境变量名**（`dsn_env`），DSN 由部署注入。非法配置、DSN 缺失或 agent 握手失败时启动即退出（非零码）；L1 只读账号由部署与巡检独立验证，启动不执行写探测（当前联调账号为超级用户）。

**内置 MCP 包**：`clickhouse-basic` 这类包编译进 `jumcp-server`，安装产物是一台指向本进程内嵌端点的 HTTP 上游——没有子进程、没有随包二进制。配置字段在安装时按 key 校验（`url`/`username`/`password`/`database`/`allowed_schemas`），密码字段必须以 `JUMCP_TRANSPORT_KEY` 加密的 `enc:v1:` 密文提交（管理台从 `GET /admin/transport-key` 取密钥，在浏览器里加密），服务端解出后再用 `JUMCP_SECRET_KEY` 加密落库、目录与响应只回显 `secrets_set`；提交明文或换钥密文一律 400（`MCP_TRANSPORT_CIPHERTEXT_REQUIRED` / `MCP_TRANSPORT_DECRYPT_FAILED`）。`PATCH /admin/mcp/catalog/{id}/install/{upstream_id}/config` 改配置（secret 传空串=保持原值），包管理字段不能经通用 `PUT /admin/upstreams/{id}` 改写（409 `MCP_UPSTREAM_PACKAGE_MANAGED`）。旧 stdio 安装在 HTTP 模式启动时自动迁移成 HTTP。插件契约、端点协议与新增内置包的步骤见 `docs/builtin-mcp.md`。

### 容器与 CI/CD

镜像 `ghcr.io/nnyjk/jumcp`，只构建 `linux/amd64`：多阶段构建（Rust release 编译后端与 connector、Node 22 构建管理台、Debian bookworm-slim 运行时），非 root（UID/GID `10001`）、`/data` 为数据卷、`JUMCP_MCP_WEB_DIR=/app/web` 把管理台作为同源静态资源提供。`JUMCP_ADMIN_TOKENS`、`JUMCP_SECRET_KEY` 与 `JUMCP_TRANSPORT_KEY` **必须由运行时注入**：镜像里既没有默认值，也没有任何 token/secret（HTTP 模式缺任一把密钥都直接拒绝启动）。

内置 MCP 包（`clickhouse-basic`）安装后只是 `/data` 里 `registry.sqlite3` 的一条普通上游记录（带 `catalog_id` / `catalog_version`），**不需要额外卷**，也不需要任何随包二进制。容器内安装需要运行时注入两把 64 位 hex 密钥（生成方式 `openssl rand -hex 32`）：`JUMCP_SECRET_KEY` 加密落库，`JUMCP_TRANSPORT_KEY` 加密管理台提交的密码。镜像不提供默认值，缺失时 HTTP 模式直接拒绝启动，见下面 `docker run` 示例。

```bash
docker buildx build --platform linux/amd64 -t jumcp:local --load .
docker run -d --name jumcp -p 127.0.0.1:8000:8000 -p 127.0.0.1:8080:8080 \
  -e JUMCP_ADMIN_TOKENS="admin:<至少 16 位的 token>" \
  -e JUMCP_SECRET_KEY="$(openssl rand -hex 32)" \
  -e JUMCP_TRANSPORT_KEY="$(openssl rand -hex 32)" \
  -v jumcp-data:/data \
  jumcp:local
# 管理台 http://127.0.0.1:8080/ （登录页），健康检查 GET /health （两个面都有），MCP POST http://127.0.0.1:8000/mcp
```

- `/data` 存 `registry.sqlite3` 与 `audit.sqlite3`（管理台会话也在 `registry.sqlite3` 的 `admin_sessions` 表）：容器重建不丢治理、会话与审计数据，必须挂卷（`-v` 或 `--tmpfs`）。
- 管理台是薄前端，浏览器不发 `Authorization`：镜像只保证「同源静态资源 + API」可用，鉴权靠 `POST /admin/login` 换来的 HttpOnly 会话 cookie。没有网关/BFF，也没有镜像内的 token 注入后门。
- CI（`.github/workflows/ci.yml`）：在 `dev` 上跑日常校验——后端 `fmt`/`clippy -D warnings`/`cargo test --workspace --locked`；管理台 `lint`/`typecheck`/`build` + 无头 Web 冒烟；脚本 `bash -n` 与发布 tag 解析用例；`linux/amd64` 镜像构建 + 容器冒烟 + Swarm stack 配置校验（`docker stack config`）；PR 额外跑依赖审查（高危阻断）。CI 只读、不登录也不推送 GHCR。
- image CD（`.github/workflows/image-cd.yml`）：由 `dev` push（快照）、`main` push（正式）、`v*` tag 或手动触发，推 `ghcr.io/<owner>/jumcp`。tag 与 `VERSION` 由 `scripts/release-tags.sh` 确定性算出：`dev` 给 `dev`/`sha-<短 sha>`，`main` 给 `latest`/`main`/`sha-<短 sha>`，版本 tag 给精确版本与兼容 tag（`latest` 只给 `main`），带 provenance 与 SBOM，push 后按 digest 回拉再冒烟。`publish` job 限定 `refs/heads/dev`、`refs/heads/main` 与 `refs/tags/v*`：对 feature 分支手动 dispatch 会整 job 跳过；`v*` tag 还必须指向 `main` 历史（`merge-base --is-ancestor` 校验）。
- web CD（`.github/workflows/web-cd.yml`）：触发面与 image CD 相同，构建 `jumcp-admin-web-<version>.tar.gz`（`admin-web/dist` 打包）并上传 artifact（保留 30 天）；`v*` tag 时再把同一份包挂到该 tag 的 GitHub Release（只有这一步需要 `contents: write`）。
- 本地复现：`bash scripts/smoke-headless-web.sh` 校验 `admin-web/dist` 能当静态站点跑；`bash scripts/smoke-image.sh jumcp:local` 起容器跑完整断言（`/health` 探针 JSON 与 `Accept: text/html` 的 UI 页面两条分支、管理台静态资源、401/200 鉴权、`/mcp`、内置包链路：目录 `transport` → 安装（密码以传输密文提交）→ refresh 拉到 3 个工具 → 改配置（不回显密文）→ 卸载）。

完整的检查项、artifact 名称、tag 规则与分支保护建议见 `docs/ci-cd.md`。

**Portainer / Swarm**：`deploy/portainer-stack.yml` 是可直接粘贴进 Portainer（Stacks → Add stack）或 `docker stack deploy -c` 的部署定义——单副本、`stop-first` 更新与回滚（SQLite 单写者，不能多副本）、`/data` 挂命名卷、`JUMCP_ADMIN_TOKENS`、`JUMCP_SECRET_KEY` 与 `JUMCP_TRANSPORT_KEY` 必填 fail-fast、镜像与端口可用 `JUMCP_IMAGE` / `JUMCP_PORT` 覆盖。前置条件、Portainer 操作步骤、鉴权边界、备份升级与多节点限制见 `docs/portainer-swarm.md`。

分支模型：`main` 只做发布、不接受日常直接 push，且只接受 `dev` 的发版 PR（另有 `release source is dev` 检查来源）；`dev` 是默认分支与集成分支。日常提交走 `feature/* → dev`（PR 目标 `dev`），CI 在 `dev` 与发版 PR 上校验；发版时用 merge commit 把 `dev` 整体合到 `main`（PR 目标 `main`），合并触发 image CD 与 web CD。发版后要把 `main` 合回 `dev` 做同步，否则 `strict` 保护会挡住下一次发版 PR。流程与禁止事项见 `CONTRIBUTING.md`，目录职责见 `docs/repository-structure.md`。

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

- `backend/crates/jumcp-core/`：治理域与配置——`config`（`policy.toml` 解析与校验）、`registry`（能力注册表：双布尔可见性 + 版本审批元数据）、`template`（SQL 模板定义与渲染）、`access`（token / 角色 / 组模型与 allowlist 判定）、`entry`（三通道入口 + EffectiveScope）、`audit`（哈希链审计）、`budget`（调用预算与溢出策略）
- `backend/crates/jumcp-sqlguard/`：只读 SQL 校验（L4）与结果整形
- `backend/crates/jumcp-server/`：MCP 服务与传输层——`mcp/`（JSON-RPC 方法与 `/mcp` 分发、HTTP 传输）、`admin/`（`/admin/*` 管理面：令牌、组、能力审核、统计、审计查询、上游维护、内置 MCP 包目录/安装/配置编辑/卸载）、`upstream/`（动态上游 MCP server 接入，工具名 `<server_id>__<tool>`；手工 stdio 上游的可执行文件解析 `JUMCP_MCP_BUNDLED_BIN_DIR` → 服务同目录 → `PATH`；`builtin/` 是内嵌端点与内置包插件注册表）、`connector/`（`ConnectorExecutor`：拉起 `jumcp-connector-clickhouse` 子进程的 host 侧执行桥，做 L4 校验、database 收窄与结果整形，供数据源健康检查与执行器契约使用）；另有能力目录与渐进发现（`search_capabilities` / `describe_capability`）与 `JUMCP_MCP_WEB_DIR` 静态资源同源托管
- `backend/crates/jumcp-connector-protocol/`：host 与 connector agent 之间的行分隔 JSON-RPC 协议定义（两端共用）
- `backend/crates/jumcp-connector-clickhouse/`：ClickHouse connector agent（stdin/stdout 按行 JSON-RPC；只做连接/绑定/执行/取数）
- `backend/crates/jumcp-mcp-clickhouse/`：内置 MCP 包 `clickhouse-basic` 的库（MCP 2025-03-26，工具 `query` / `list_databases` / `list_tables`；无二进制、无 stdio 行循环，按字段 key 读 `BuiltinConfig`，校验 → 执行 → 整形三层；由 `jumcp-server` 的内嵌 HTTP 端点调用）
- `admin-web/`：React + Vite 管理台（jumcp 治理面）；`npm run typecheck` / `build` / `lint`
- `config/`：`policy.toml`（组、角色、数据源、预算与只读策略）
- `deploy/`：`portainer-stack.yml`（Portainer / Docker Swarm stack 定义，见 `docs/portainer-swarm.md`）
- `scripts/`：`dev.ps1` / `dev.sh`（一键启动）、`test-all.ps1` / `test-all.sh`（一键测试）、`release-tags.sh`（发布 tag 与 `VERSION` 的唯一规则来源，`test-release-tags.sh` 是其用例）
- `docs/`：当前契约与路由文档（见下方文档索引）
- `CONTRIBUTING.md`：分支模型、开发流程、PR 要求与发版步骤

## 管理台（admin-web）

Fluent 2 风格管理台，首页是**工作台 / 数据看板**：KPI（总调用、成功率、失败数、活跃工具、待审批、未提交草稿、审计链、数据源）、调用趋势（手写 SVG 折线/面积图）、健康分布（SVG 环形图）、生命周期/来源堆叠条、工具调用排行与失败榜、数据源、治理动态；支持「每 30 秒自动刷新」并显示「数据更新于 …」；图表不引第三方库，全部有加载/错误/空态。

其余页面：**审核队列**（「待审批」与「未提交草稿」两段，批准填写审批单号 + 发布后可见性两步）、**工具台账**（受管能力 / 内置工具分段的可筛选列表）、**能力档案**、**组维护**（策略组只读在来源徽标/卡头表达，不可编辑）、**访问令牌**（创建时明文只展示一次，Bearer 接入 `/mcp`；按角色/组/工具名收窄，已撤销/已过期可删除记录，组引用闭环；卡片「MCP 接入」一键复制 HTTP MCP JSON 配置——服务名 `jumcp`、URL 同源 `/mcp`、`Authorization: Bearer <token>`；明文不可回读，已有 active 令牌需确认重新签发（旧 token 立即失效）后再复制，revoked/expired 按钮禁用）、**MCP 接入**（单页两页签：「接入清单」维护 router 上游——测试连接 / 启停 / 删除，工具按 `<server_id>__<tool>` 暴露，并含「基础工具」逐工具开关/刷新/失效覆盖清理；「安装目录」列内置包目录，提供安装表单与已安装实例的编辑配置、卸载）、**审计查询**、**例外与可见性**（能力可见性只读清单）、**健康检查**（运行状态卡头「数据更新于 …」+「刷新」）。

**能力档案 `/capabilities/:id`**：可从工具台账、审核队列、例外与可见性点「详情」，在 content 视图栈内打开（URL 不变），也可直接访问 `/capabilities/:id`；「返回」回到来源列表，切侧栏即清空视图。它是单个能力的唯一工作面，分六个栏——概览（生效范围：组→角色；状态与可见性；治理时间线与窗口指标）、定义（结构化字段视图）、版本（版本行与定义哈希）、审批（提交 / 批准发布 + 第二步可见性 / 退回）、调用（最近 1 / 7 / 30 天的调用记录）、审计（该对象的事件流）。内置工具没有受管定义，只有「概览」与「调用」两栏；审核队列进入时直接落在「审批」栏。草稿「提交审核」（审核队列与档案内一致）走二次确认弹层，展示能力 ID / 版本 / 草稿→待审 / 提交后不可自行撤回。

**工具台账**：按受管能力 / 内置工具分段，列顺序按首屏可见性排：工具 ID（第二行类型/版本）→ 调用/失败 → 成功率 → 健康 → 生命周期 → 风险 → 可见性 → 组 → 分类 → 最近调用 → 操作；组显示中文名（内置工具显示「内置」），分类列只显示中文名（基础 / 分析 / 审计 / 自定义），筛选同样是中文分类；点「详情」在 content 视图内打开能力档案，URL 不变。**新建草案**是独立弹层（打开即聚焦第一个输入，组为必选），创建后进入草稿，需到「审批」栏提交审核。

**审计查询**：默认**最新优先**（`desc`，可切「最旧优先」）；从令牌、能力档案或工具台账进入时在 content 视图内打开并预填筛选（token_id / object_id / event_id），URL 不变，直接访问 `/audit` 路由仍可用并兼容这些查询参数，筛选与「重置」只改页内状态；表格列为 时间 → 摘要 → 主体 → 动作 → 对象 → 操作，结果与审批单由摘要与详情弹层承载（主体里的令牌 ID 表格中截断到 12 位，完整值在 `title`/详情）。时间统一显示 `YYYY-MM-DD HH:mm:ss`（本地时区，到秒），完整 RFC3339 保留在 `title` 与详情。详情里对 `tool_call`、`template_run` 展示入参、执行脚本与载荷结构版本。旧格式事件（没有 `object_type` / `object_id` / `action`）界面标注「旧格式事件，仅原始载荷」，不按旧字段推断语义，也不计入工具统计。

所有写操作经 admin API 落审计（`admin_change` / `policy_decision`），前端不直连数据库、不持有管理面凭证。审计事件结构、管理面字段与统计口径见 `docs/audit-openness.md`。本地试用请直接用上面的「快速开始」；手动起法是 `cd admin-web && npm install && npm run dev`（另开终端跑后端），按后端实际端口设 `JUMCP_DEV_ADMIN_TARGET=<管理面>` 与 `JUMCP_DEV_API_TARGET=<MCP 面>`，然后在 http://127.0.0.1:5173 的登录页里输入 `JUMCP_ADMIN_TOKENS="id:token,id:token"` 中的某个 token。`npm run build` 做类型检查与构建。

## 文档索引

- `docs/mcp-router.md`：架构与 router 模型——内置工具分类与 `basic` 组、上游配置与逐工具基础组、内置 MCP 包（`/admin/mcp/catalog*`：目录、安装、配置编辑、卸载）。
- `docs/audit-openness.md`：审计事件契约、管理面接口字段与统计口径（§7.7–§7.11、§11 令牌契约）。
- `docs/ci-cd.md`：CI 检查项、artifact、镜像 tag 规则、分支模型与发布流程、分支保护。
- `docs/portainer-swarm.md`：Portainer / Docker Swarm 部署（`deploy/portainer-stack.yml`）、数据与鉴权边界、升级回滚与单副本限制。
- `docs/web-hosting.md`：管理台静态资源同源托管的启动校验、路由优先级与缓存策略。
- `docs/repository-structure.md`：目录结构与各层职责（root、Rust workspace、管理台、config/scripts/docs/notes、`.github`）。
- `CONTRIBUTING.md`：分支模型、开发流程、本地校验、PR 与发版步骤。
- `admin-web/README.md`：管理台页面职责、契约要点与本地启动。
- `notes/mcp_plan/architecture_preview.md` / `notes/mcp_plan/implementation_plan.md`：历史设计记录，不作为当前契约。
