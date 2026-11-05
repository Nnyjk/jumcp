# hjzl-mcp-server

Harness Agent 平台的 MCP 服务：入口裁剪（§4.1）、渐进发现（§4.2）、只读分发（§4.3）与治理审计（§4.4）。
架构见 `docs/plan/hjzl-mcp-server-preview.md`。

## 构建与运行

```bash
cd backend
cargo build --workspace
cargo run -p hjzl-server                                    # stdio（默认）
HJZL_MCP_TRANSPORT=streamable-http cargo run -p hjzl-server  # HTTP（POST /mcp）
```

配置走 `HJZL_MCP_*`（`HJZL_MCP_CONFIG_FILE`、`HJZL_MCP_AUDIT_PATH`、`HJZL_MCP_REGISTRY_PATH`、
`HJZL_MCP_TEMPLATES_FILE`、`HJZL_MCP_GROUPS_MODE`、`HJZL_MCP_BUDGET_OVERFLOW=fail|truncate`、`HJZL_ADMIN_TOKEN`）。策略与 `[[datasources]]` 同文件；数据源只写
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

## 目录

- `backend/crates/hjzl-core/`：`config`、`entry`（三通道 + EffectiveScope）、`audit`（哈希链）、`registry`（双布尔 + 四眼）、`budget`（§5 上限）
- `backend/crates/hjzl-sqlguard/`：只读 SQL 校验（L4）与结果整形
- `backend/crates/hjzl-server/`：JSON-RPC MCP 服务、`PgExecutor`（L1–L3，只读事务 + 自检）、stdio / HTTP 双入口

## 管理台（admin-web）

`cd admin-web && npm install && npm run dev` 起本地开发服务；`npm run build` 做类型检查与构建。
