# 仓库结构

jumcp 是一个 Rust workspace + React 管理台的单仓库：后端负责 MCP router 与治理，管理台是它的同源薄前端，`Dockerfile` 把两端打进同一个镜像。本文件说明每层目录的职责边界；怎么开发、怎么发版见 [CONTRIBUTING.md](../CONTRIBUTING.md)，能力与接口契约见 [README.md](../README.md) 与 `docs/` 其他文档。

## 根目录

| 路径 | 职责 |
|---|---|
| `backend/` | Rust workspace：MCP server、治理域、SQL 只读校验、ClickHouse connector agent |
| `admin-web/` | React + Vite 管理台（构建产物 `admin-web/dist` 由后端同源提供） |
| `config/` | 部署策略：`policy.toml`（组、角色、数据源、预算、只读策略） |
| `scripts/` | 本地与 CI 共用的启动/测试/冒烟脚本（PowerShell + Bash 双份） |
| `docs/` | 当前有效的方案、契约与运维文档 |
| `notes/` | 工作笔记：踩坑、约束、被否决的决定（不进 `docs/`） |
| `.github/workflows/` | CI 与 release 流水线 |
| `Dockerfile` / `.dockerignore` | 多阶段构建：Rust release 编译 → Node 构建管理台 → `debian:bookworm-slim` 运行时 |
| `README.md` / `CONTRIBUTING.md` / `AGENTS.md` | 项目说明 / 协作与分支模型 / 文档与注释规范 |
| `LICENSE` | MIT |
| `.gitattributes` | 换行统一：仓库内文本 LF，`*.ps1`/`*.bat`/`*.cmd` 为 CRLF |
| `.dev-data/` `backend/data/` `.screenshots/` | 运行期与本地产物，已被 `.gitignore` 排除（SQLite、日志、截图） |

## Rust workspace（`backend/`）

`backend/Cargo.toml` 是 workspace 根（`members = ["crates/*"]`，`resolver = "2"`），依赖锁定在 `backend/Cargo.lock`，所有 CI/镜像构建都带 `--locked`。

### `backend/crates/jumcp-core/`

治理域与配置：不含 IO 入口，不依赖 server。

| 模块 | 职责 |
|---|---|
| `config.rs` | `policy.toml` 解析与校验（组、角色、数据源、预算、只读开关） |
| `entry.rs` | 三通道入口与 `EffectiveScope`（组 → 角色 → 工具名收窄） |
| `access.rs` | token / 角色 / 组模型与 allowlist 判定 |
| `registry.rs` | 能力注册表（双布尔可见性 + 版本审批元数据） |
| `audit.rs` | 哈希链审计事件 |
| `budget.rs` | 调用预算与溢出策略（§5 上限） |
| `template.rs` / `canonical.rs` / `errors.rs` | SQL 模板、规范化序列化（哈希稳定）、错误码 |

### `backend/crates/jumcp-sqlguard/`

只读 SQL 校验（L4）与结果整形，`dialect.rs` / `lex.rs` / `shape.rs` / `validate.rs` 分工到方言、词法、结果形状与规则判定。纯函数式，可被 server 与 connector 同时引用。

### `backend/crates/jumcp-connector-protocol/`

host 与 connector agent 之间的按行 JSON-RPC 协议定义（消息类型与序列化），两端共用同一份契约，`tests/protocol.rs` 覆盖编解码。

### `backend/crates/jumcp-connector-clickhouse/`

ClickHouse connector agent：stdin/stdout 按行 JSON-RPC，只做连接、参数绑定、执行、取数。`lib.rs` 是被 host 拉起的实现，`main.rs` 是二进制入口（`jumcp-connector-clickhouse`）。

### `backend/crates/jumcp-server/`

服务与传输层。

| 文件 | 职责 |
|---|---|
| `main.rs` / `lib.rs` | 进程入口与可测试的库骨架 |
| `service.rs` / `mcp.rs` | JSON-RPC MCP 服务装配、`/mcp` 方法与工具分发 |
| `admin.rs` | `/admin/*` 管理面：令牌、组、能力审核、审计查询、上游维护 |
| `catalog.rs` / `discovery.rs` | 能力目录与渐进发现（`search_capabilities` / `describe_capability`） |
| `executor.rs` / `connector.rs` | `ConnectorExecutor`：拉起 `jumcp-connector-clickhouse` 子进程，施 L4 校验、database 收窄、结果整形、预算与审计 |
| `upstream.rs` | 动态上游 MCP server 接入（工具名 `<server_id>__<tool>`） |
| `web.rs` | 管理台静态资源同源托管（`JUMCP_MCP_WEB_DIR`）与 SPA 回退 |
| `templates_boot.rs` | 启动时装载 SQL 模板 |

`backend/crates/jumcp-server/tests/` 放契约与集成测试（`contracts.rs`、`tokens.rs`、`admin.rs`、`upstream.rs`、`web_hosting.rs` 等），不需要数据库；真实 ClickHouse 用例是 `#[ignore]`。

## 管理台（`admin-web/`）

React 18 + Vite + TypeScript，无 UI 组件库，样式与动画自持。

| 路径 | 职责 |
|---|---|
| `src/main.tsx` / `src/App.tsx` | 挂载、路由与全局外壳（侧栏 + content 视图栈） |
| `src/api/` | `client.ts`（fetch 封装与错误归一）、`queries.ts`（数据拉取）、`admin.ts`（管理面接口）、`types.ts`（契约类型） |
| `src/pages/` | 路由级页面：`DashboardPage`、`ReviewQueuePage`、`ToolsPage`、`CapabilityDossierPage`、`GroupsPage`、`TokensPage`、`UpstreamsPage`、`AuditQueryPage`、`ExceptionPage`、`HealthPage` |
| `src/components/` | 复用的展示与交互件（表格、卡片、弹层、侧栏、主题切换、提交确认等） |
| `src/motion/` | 过渡与动画原语 |
| `src/styles/` | 全局样式与主题变量 |
| `src/*.ts` | 跨页面工具：`audit.ts`、`objects.ts`、`labels.ts`、`format.ts`、`json.ts`、`notice.ts`、`theme.ts`、`viewStack.ts` |
| `public/` `index.html` `vite.config.ts` `eslint.config.js` `tsconfig.json` | 静态资源、构建与开发代理（`JUMCP_DEV_ADMIN_TOKEN` 注入、生产构建期拦截 `VITE_ADMIN_TOKEN`） |

页面职责与路由表见 `admin-web/README.md`；管理台不持有管理面凭证，浏览器不发 `Authorization`。

## 其他目录边界

- `config/`：只放部署策略文件。`policy.toml` 是唯一的策略入口（`[[groups]]`、`[[roles]]`、`[[datasources]]`、预算与只读开关），数据源只写 `dsn_env` 环境变量名，DSN 由部署注入。镜像把它复制到 `/app/config/policy.toml`。
- `scripts/`：`dev.ps1`/`dev.sh`（一键启动前后端）、`test-all.ps1`/`test-all.sh`（一键测试）、`smoke-headless-web.sh`（校验 `admin-web/dist` 能当静态站点跑）、`smoke-image.sh`（起容器跑完整断言）。CI 与本地共用同一份脚本，不另写 CI 专用变体。
- `docs/`：只写**当前有效**的结论（方案、契约、运维与 CI），结论变了就改原文；`docs/plan/` 放阶段性设计与实施计划。文档索引在 `README.md`。
- `notes/`：只记踩坑、约束、被否决的决定与开发期重要信息，路径 `notes/<module>/<note_name>.md`；结论一旦稳定就上升进 `docs/`。规范见 `AGENTS.md`。
- `.github/workflows/`：`ci.yml`（`dev` push、PR 到 `dev`/`main`、手动，只读、不推镜像）与 `release.yml`（`main` push、`v*` tag、手动，推 GHCR）。分支模型与分支保护见 [docs/ci-cd.md](ci-cd.md)。

## 不改目录结构的前提

源码目录划分是当前契约的一部分：新增 crate 放在 `backend/crates/`、新页面放 `admin-web/src/pages/`、新脚本放 `scripts/`、新契约文档放 `docs/`。不要为了「更整齐」搬动既有路径——CI、`Dockerfile`、脚本与文档链接都按现路径硬编码。
