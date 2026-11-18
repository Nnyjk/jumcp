# 仓库结构

jumcp 是一个 Rust workspace + React 管理台的单仓库：后端负责 MCP router 与治理，管理台是它的同源薄前端，`Dockerfile` 把两端打进同一个镜像。本文件说明每层目录的职责边界；怎么开发、怎么发版见 [CONTRIBUTING.md](../CONTRIBUTING.md)，能力与接口契约见 [README.md](../README.md) 与 `docs/` 其他文档。

## 根目录

| 路径 | 职责 |
|---|---|
| `backend/` | Rust workspace：MCP server、治理域、SQL 只读校验、ClickHouse connector agent |
| `admin-web/` | React + Vite 管理台（构建产物 `admin-web/dist` 由后端同源提供） |
| `config/` | 部署策略：`policy.toml`（组、角色、数据源、预算、只读策略） |
| `deploy/` | 部署定义：`portainer-stack.yml`（Portainer / Docker Swarm stack，单副本 + 命名卷），见 [portainer-swarm.md](portainer-swarm.md) |
| `scripts/` | 本地与 CI 共用的启动/测试/冒烟脚本（PowerShell + Bash 双份） |
| `docs/` | 当前有效的方案、契约与运维文档 |
| `notes/` | 工作笔记：踩坑、约束、被否决的决定（`notes/mcp_plan/` 的架构预览与落地计划是历史记录，不作为契约） |
| `.github/workflows/` | CI 与 release 流水线 |
| `Dockerfile` / `.dockerignore` | 多阶段构建：Rust release 编译 → Node 构建管理台 → `debian:bookworm-slim` 运行时 |
| `README.md` / `CONTRIBUTING.md` / `AGENTS.md` | 项目说明 / 协作与分支模型 / 文档与注释规范 |
| `LICENSE` | MIT |
| `.gitattributes` | 换行统一：仓库内文本 LF，`*.ps1`/`*.bat`/`*.cmd` 为 CRLF |
| `.dev-data/` `data/` `backend/data/` `.screenshots/` | 运行期与本地产物：SQLite（registry / audit）、日志、截图，已被 `.gitignore` 排除 |

## Rust workspace（`backend/`）

`backend/Cargo.toml` 是 workspace 根（`members = ["crates/*"]`，`resolver = "2"`），依赖锁定在 `backend/Cargo.lock`，所有 CI/镜像构建都带 `--locked`。

### `backend/crates/jumcp-core/`

治理域与配置：不含 IO 入口，不依赖 server。

| 模块 | 职责 |
|---|---|
| `config/` | `policy.toml` 的解析与校验：`tenant` 租户策略（组/角色定义、预算上限）、`principals` 管理面与 agent 主体令牌（常量时间比较）、`datasource` 数据源引用（DSN 只取环境变量） |
| `registry/` | 能力注册表：`record` 记录/版本/管理变更结构与定义 diff、`lifecycle` 注册→审核→发布→下架与可见性、`groups` 管理组 CRUD 与引用计数、`upstreams` 上游配置与逐工具组覆盖 |
| `builtin/` | 内置 MCP 插件类型：`BuiltinPackage` / `BuiltinField` / `BuiltinToolDef` / `BuiltinConfig` / `BuiltinError` 与 `BuiltinMcp` trait（只依赖 `serde_json` 与 `std`，不依赖任何具体数据库） |
| `secret.rs` | 配置字段加密：`SecretKey` 解析 64 位 hex 密钥，`enc:v1:` 前缀的 AES-256-GCM 加解密、`is_encrypted`（明文值原样兼容）与 `to_hex`（仅供 `GET /admin/transport-key` 下发传输密钥）；落库密钥 `JUMCP_SECRET_KEY` 与传输密钥 `JUMCP_TRANSPORT_KEY` 共用实现、用途分离 |
| `template/` | `definition` 模板定义与结构变体白名单、`render` 注册期门禁与运行期渲染（占位符改写、sqlguard 复核、参数绑定）、`registry` 模板注册表 |
| `access/` | `token.rs` 平台生成的 MCP access token 与哈希鉴权、`mod.rs` 持久化 / 撤销 / 组引用计数（明文只在创建或重签发时返回一次） |
| `entry.rs` | 平面 A 纯函数：三通道解析 → 越界判定 → `EffectiveScope`（组 → 角色 → 工具名收窄） |
| `audit.rs` | 哈希链审计事件 |
| `budget.rs` | 常量与按会话累计的调用预算、溢出策略 |
| `canonical.rs` | 规范化 JSON 与哈希（与 Python `json.dumps` 逐字节一致，排序显式按 key） |
| `errors.rs` | 统一错误码（与 Python `ERROR_CODES` 同名稳定契约）与消息映射 |

各模块的 `tests.rs` 放该域单元测试。

### `backend/crates/jumcp-sqlguard/`

只读 SQL 校验（L4）与结果整形：`dialect.rs` 方言差异、`lex.rs` 词法、`validate.rs` 规则判定、`shape.rs` 结果形状、`error.rs` 错误类型。纯函数式，可被 server 与 connector 同时引用。

### `backend/crates/jumcp-connector-protocol/`

host 与 connector agent 之间的按行 JSON-RPC 协议定义（`src/lib.rs`：消息类型与序列化），两端共用同一份契约。

### `backend/crates/jumcp-connector-clickhouse/`

ClickHouse connector agent：stdin/stdout 按行 JSON-RPC，只做连接、参数绑定、执行、取数。`src/lib.rs` 是被 host 拉起的实现，`src/main.rs` 是二进制入口（`jumcp-connector-clickhouse`）；`tests/protocol.rs` 覆盖协议编解码。

### `backend/crates/jumcp-mcp-clickhouse/`

内置 MCP 包 `clickhouse-basic` 的**库**（无二进制、无 stdio 行循环）：`src/plugin.rs` 是 `BuiltinMcp` 实现（静态包定义与字段、按配置指纹缓存 `ClickHouseAgent`）、`src/tools.rs` 是工具定义与参数校验、`src/executor.rs` 包装 connector 的只读执行、`src/lib.rs` 是 `Server::call_tool`（工具语义）；配置按字段 key 传入 `BuiltinConfig`。`tests/protocol.rs` 覆盖工具语义（协议层由 `jumcp-server` 的内嵌端点测试覆盖）。

### `backend/crates/jumcp-server/`

服务与传输层。

| 模块 | 职责 |
|---|---|
| `main.rs` / `lib.rs` | 进程入口（参数、启动装配）与可测试的库骨架 |
| `service.rs` | `Service` 聚合：配置、catalog、执行委托、审计与按会话预算 |
| `mcp/` | 手写 JSON-RPC MCP 服务：`mod` 的 tools/list（按 `EffectiveScope` 过滤 + 常驻预算）与元工具、维护通道；`http` 把 HTTP 请求翻成平台身份；`dispatch/{tools,upstream,admin}` 分别是工具、上游转发、管理类方法的处理臂 |
| `admin/` | `/admin/*` 管理面：`surface` 路由与 `/admin/me`，`capabilities`、`writes`（定义/提审/发布/驳回/可见性等写操作）、`stats`、`tokens`、`groups`、`upstreams` 各面实现，`packages` 内置 MCP 包目录、安装、配置编辑与卸载 |
| `builtin/` | 内置 MCP 运行时：`mod` 是插件注册表（`plugin(id)` / `plugins()`）、内嵌端点 `POST /internal/mcp/{package_id}/{upstream_id}`（Bearer 内存 token）与 `BuiltinEndpoint`（base + token，供上游客户端注入鉴权）；`migrate` 把旧 stdio 安装改写为 HTTP |
| `upstream/` | 动态上游 MCP server 接入：`client` 传输抽象与连接池（http / stdio 二选一）、`http` 与 `stdio` 两种传输实现；`http` 对命中内置端点 base 的请求注入 internal Bearer；`stdio::bundled_binary` 是手工 stdio 上游可执行文件的唯一解析点（`JUMCP_MCP_BUNDLED_BIN_DIR` → 服务同目录 → `PATH`） |
| `connector/` | 连接器桥：把已校验的只读执行委托给子进程 `jumcp-connector-clickhouse`（L4、database 白名单、行数夹紧、单格整形留在 host） |
| `executor.rs` | 只读执行委托契约（`QueryExecutor`）；未配置数据源时显式报错，绝不假执行 |
| `catalog.rs` | 能力抽象与内存目录（可见性 `published ∧ discoverable ∧ group ∈ scope`，不可见与不存在同构） |
| `discovery.rs` | 发现面唯一的可见集生产函数：`builtin ∪ 注册表可见行 ∪ 内存目录`，供 search / describe / list_groups / tools/list 共用 |
| `web.rs` | 管理台静态托管：HTTP 模式下提供 `admin-web/dist`（`JUMCP_MCP_WEB_DIR` 为空则不挂）、SPA 回退与路径空间隔离 |
| `templates_boot.rs` | 启动期从部署配置装载模板，走注册 → 预检 → 提审 → 发布 → 开放同一条链 |

### `backend/crates/jumcp-server/tests/`

契约与集成测试，共用真实 axum 链路但不需要真实数据源；真实 ClickHouse 用例是 `#[ignore]`。

| 路径 | 职责 |
|---|---|
| `contracts.rs`、`discovery.rs`、`discovery_projection.rs`、`identity_admin.rs`、`templates.rs`、`web_hosting.rs` | 顶层的工具表面、发现投影、身份与管理面、模板、静态托管契约 |
| `admin/`、`tokens/`、`upstream/` | 每个子目录是一个测试 target（`main.rs`），按面组织：鉴权、CRUD、审核与冲突、统计、上游传输与 scope（`upstream/packages.rs` 用真实回环 listener 覆盖内置包的目录契约、安装、配置编辑、包管理字段保护、卸载与旧 stdio 迁移） |
| `support/mod.rs` | 共享设施：真实 axum 链路、admin 鉴权请求、能力发布与审计样例 |
| `support/fake_upstream.rs` | 上游 MCP server 的假实现（stdio / HTTP 测试用） |

子目录各自的 `fixtures.rs` 放该面的请求/数据构造。

## 管理台（`admin-web/`）

React 19 + Vite + TypeScript，无 UI 组件库，样式与动画自持。

| 路径 | 职责 |
|---|---|
| `src/main.tsx` / `src/App.tsx` | 挂载、路由与全局外壳（侧栏 + content 视图栈） |
| `src/api/` | `client.ts`（fetch 封装与错误归一）、`queries.ts`（数据拉取）、`admin.ts`（管理面接口）、`types.ts`（契约类型） |
| `src/pages/` | 路由级页面，一个功能一个目录：`dashboard/`（工作台与图表）、`tools/`（台账与新建草案）、`groups/`、`capability-dossier/`（六分栏与审批）、`upstreams/`（MCP 接入单页两页签：「接入清单」含基础工具弹层，「安装目录」含内置包的安装表单、已安装实例的配置编辑与卸载）、`tokens/`、`audit-query/`、`review-queue/`；`ExceptionPage.tsx`、`HealthPage.tsx` 是单文件页面 |
| `src/components/` | 复用的展示与交互件（表格、卡片、弹层、侧栏、主题切换、提交确认等） |
| `src/motion/` | 过渡与动画原语（`useGsap.ts`） |
| `src/styles/` | `index.css` 为汇总入口，`components.css` 按顺序 `@import` `styles/components/*.css`（顺序即级联优先级，勿重排）；另有 `layout.css`、`modal.css`、`dashboard.css`、`tokens.css` 与主题变量 |
| `src/*.ts` | 跨页面工具：`audit.ts`、`objects.ts`、`labels.ts`、`format.ts`、`json.ts`、`notice.ts`、`theme.ts`、`viewStack.ts` |
| `public/` `index.html` `vite.config.ts` `eslint.config.js` `tsconfig.json` | 静态资源、构建与开发代理（`JUMCP_DEV_ADMIN_TOKEN` 注入、生产构建期拦截 `VITE_ADMIN_TOKEN`） |

页面职责与路由表见 `admin-web/README.md`；管理台不持有管理面凭证，浏览器不发 `Authorization`。

## 其他目录边界

- `config/`：只放部署策略文件。`policy.toml` 是唯一的策略入口（`[[groups]]`、`[[roles]]`、`[[datasources]]`、预算与只读开关），数据源只写 `dsn_env` 环境变量名，DSN 由部署注入。镜像把它复制到 `/app/config/policy.toml`。
- `deploy/`：只放部署定义。`portainer-stack.yml` 是 Portainer（Stacks）与 `docker stack deploy` 共用的 Swarm 定义：单副本、`stop-first` 更新/回滚、命名卷 `/data`、`JUMCP_ADMIN_TOKENS` fail-fast，`docker stack config` 可离线校验（CI 的 `image` job 会跑）。平台用 placement constraints 固定 linux/amd64，因为 Swarm 的 stack 格式不接受 `platform`。前置条件与运维边界见 [portainer-swarm.md](portainer-swarm.md)。
- `scripts/`：`dev.ps1`/`dev.sh`（一键启动前后端）、`test-all.ps1`/`test-all.sh`（一键测试）、`release-tags.sh`（发布 tag 与 `VERSION` 的唯一规则来源）、`test-release-tags.sh`（它的离线用例）、`smoke-headless-web.sh`（校验 `admin-web/dist` 能当静态站点跑）、`smoke-image.sh`（起容器跑完整断言）。CI 与本地共用同一份脚本，不另写 CI 专用变体。
- `docs/`：只写**当前有效**的结论（方案、契约、运维与 CI），结论变了就改原文，不保留阶段性设计与变更叙述；这类历史与被否决项放 `notes/`。文档索引在 `README.md`。
- `notes/`：只记踩坑、约束、被否决的决定与开发期重要信息，路径 `notes/<module>/<note_name>.md`；结论一旦稳定就上升进 `docs/`。规范见 `AGENTS.md`。
- `.github/workflows/`：`ci.yml`（`dev` push、PR 到 `dev`/`main`、手动，只读、不推镜像）与 `release.yml`（`main` push、`v*` tag、手动，推 GHCR）。分支模型与分支保护见 [docs/ci-cd.md](ci-cd.md)。

## 目录约定

- 新增代码放对应目录：新 crate 放 `backend/crates/`，core 领域模块放 `backend/crates/jumcp-core/src/<域>/`，server 的面放 `backend/crates/jumcp-server/src/<面>/`，新页面放 `admin-web/src/pages/<feature>/`，新脚本放 `scripts/`，新契约文档放 `docs/`，理由与被否决项放 `notes/`。
- 搬动既有路径必须先确认收益，并在同一次改动里同步所有引用方：CI workflow、`Dockerfile`、`scripts/`、`docs/` 与 `README.md` 的链接、`JUMCP_MCP_WEB_DIR` 静态资源路径。搬完 `scripts/test-all.*` 与镜像冒烟必须仍通过。
