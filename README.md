# hjzl-mcp-server

Harness Agent 平台的 MCP 服务：入口裁剪（§4.1）、渐进发现（§4.2）、只读分发（§4.3）与治理审计（§4.4）。
架构见 `docs/plan/hjzl-mcp-server-preview.md`。

## 本地一键启动

一条命令拉起「后端（streamable-http）+ 管理台（Vite dev）」，打开 http://127.0.0.1:5173 即可试用。

```
pwsh scripts/dev.ps1            # Windows / PowerShell 7+
scripts/dev.sh                  # Linux / macOS / CI
```

- 后端默认 http://127.0.0.1:8000（`-BackendPort` / `--backend-port` 可改），前端默认 5173（`-WebPort` / `--web-port` 被占用时自动 +1 顺延）。
- **管理面凭证只存在于进程环境**：后端 `HJZL_ADMIN_TOKENS="alice:tok_alice_000000000,bob:tok_bob_00000000000"`，前端由 Vite 代理注入 `HJZL_DEV_ADMIN_TOKEN` —— 不设这个变量，管理台请求会全部 401。
- `-PgDsn <dsn>` / `--pg-dsn <dsn>` 可选：给了就生成 `.dev-data/policy.toml`（追加只读数据源 `dev-pg`，DSN 走 `HJZL_DEV_PG_DSN`）；不给则用 `config/policy.toml` 原样，`db_query_readonly` 返回 `MCP_NOT_IMPLEMENTED`（属预期）。
- `-ResetData` / `--reset-data` 启动前清空 `.dev-data/`（默认保留，便于连续试用）；`-NoWeb` / `--no-web` 只起后端。
- Ctrl+C 同时停掉前后端；日志在 `.dev-data/logs/`（`backend.log` / `web.log`），数据在 `.dev-data/{registry,audit}.sqlite3`。

## 构建与运行

```bash
cd backend
cargo build --workspace
cargo run -p hjzl-server                                    # stdio（默认）
HJZL_MCP_TRANSPORT=streamable-http cargo run -p hjzl-server  # HTTP（POST /mcp）
```

配置走 `HJZL_MCP_*`（`HJZL_MCP_CONFIG_FILE`、`HJZL_MCP_AUDIT_PATH`、`HJZL_MCP_REGISTRY_PATH`、
`HJZL_MCP_TEMPLATES_FILE`、`HJZL_MCP_GROUPS_MODE`、`HJZL_MCP_BUDGET_OVERFLOW=fail|truncate`、`HJZL_ADMIN_TOKENS="id:token,id:token"`）。策略与 `[[datasources]]` 同文件；数据源只写
**环境变量名**（`dsn_env`），DSN 由部署注入。非法配置或只读自检失败时启动即退出（非零码）。

## 测试

```bash
cd backend
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace              # 契约测试不需要数据库
```

真实 PG 用例默认 `#[ignore]`，需要只读 DSN：

```bash
export HJZL_TEST_PG_DSN='postgres://reader:pw@host:5432/db'
cargo test -p hjzl-server -- --ignored
```

## 一键测试

一条命令跑完「后端 fmt/clippy/测试 + 前端 typecheck/build/lint + E2E 冒烟」并输出 PASS/FAIL 汇总表。

```
pwsh scripts/test-all.ps1      # Windows / PowerShell 7+
scripts/test-all.sh            # Linux / macOS / CI
```

参数（两脚本等价）：`-SkipE2E`/`--skip-e2e` 只跑静态检查与测试；`-PgDsn <dsn>`/`--pg-dsn <dsn>` 设 `HJZL_TEST_PG_DSN` 并额外跑 `#[ignore]` 真库用例；`-NoClean`/`--no-clean` 保留临时目录与服务日志。

## 目录

- `backend/crates/hjzl-core/`：`config`、`entry`（三通道 + EffectiveScope）、`audit`（哈希链）、`registry`（双布尔 + 四眼）、`budget`（§5 上限）
- `backend/crates/hjzl-sqlguard/`：只读 SQL 校验（L4）与结果整形
- `backend/crates/hjzl-server/`：JSON-RPC MCP 服务、`PgExecutor`（L1–L3，只读事务 + 自检）、stdio / HTTP 双入口

## 管理台（admin-web）

只读联调前端（Fluent 2 风格）：四个页面展示数据/空态/错误态，写操作按钮标注「未接通」。本地试用请直接用上面的「本地一键启动」；手动起法是 `cd admin-web && npm install && npm run dev`，但**必须**带上 `HJZL_DEV_ADMIN_TOKEN=<管理面 token>`（Vite 代理据此注入 `Authorization`，缺失则请求全部 401），后端 token 由 `HJZL_ADMIN_TOKENS="id:token,id:token"` 配置。`npm run build` 做类型检查与构建。
