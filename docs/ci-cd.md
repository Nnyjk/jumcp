# CI/CD

面向 `Nnyjk/jumcp` 的持续集成与发布流水线。两个 workflow：

| workflow | 触发 | 作用 |
|---|---|---|
| `.github/workflows/ci.yml` | push `dev`、`pull_request`（目标 `dev` / `main`）、`workflow_dispatch` | 后端静态检查与测试、管理台构建与无头 Web 冒烟、镜像构建与冒烟、PR 依赖审查 |
| `.github/workflows/release.yml` | push `main`、tag `v*`、`workflow_dispatch` | 构建并推送 GHCR 镜像，回拉 digest 再冒烟 |

CI 只订阅 `dev` 的 push：`main` 上的改动只能经 `dev → main` 的发版 PR 进入，合并后的全量发布校验由 release 承担，避免同一 ref 上重复跑两遍。PR 则同时接受目标 `dev`（日常）与 `main`（发版 PR）。

CI 权限只有 `contents: read`；release 额外要 `packages: write`、`attestations: write`、`id-token: write`。两个 workflow 都用 `concurrency` 控并发：CI 取消同 ref 的旧运行，release 不取消。

## 分支模型与发布流程

| 分支 | 角色 | 谁能推 |
|---|---|---|
| `main` | 发布分支，只有从 `dev` 合入的发版提交 | 仅 PR 合并（禁止直接 push） |
| `dev` | 默认分支、集成分支，日常改动的汇聚点 | 仅 PR 合并（禁止直接 push） |
| `feature/*` | 单个改动的短期分支，从 `dev` 切出 | 作者本人 |

流程：

1. 从 `dev` 切 `feature/<主题>`，在其中开发、本地校验（见 `CONTRIBUTING.md`）。
2. 开 PR 到 `dev`：CI 跑 `backend` / `web` / `image` / `dependency review`，全部通过后合并（squash 或 merge 均可，历史留在 `dev`）。
3. 发版：`dev` 的 PR 目标改为 `main`，**必须用 merge commit**（`gh pr merge --merge`，不用 squash/rebase），保留 `dev` 的完整提交历史。
4. 合并进 `main` 即触发 `release.yml`，产出 `latest` + `main` + `sha-<短 sha>` 镜像。

关键约束：

- `main` 不接受日常直接 push，也不承接 `feature/*` 的 PR。
- release 只由 `main` push、`v*` tag、`workflow_dispatch` 触发；`dev` 的任何活动都不登录、不推送 GHCR。
- `latest` 只由 `main` 的 push 产出（`enable=${{ github.ref == 'refs/heads/main' }}`）；默认分支是 `dev`，所以不能用 `is_default_branch` 判断。
- `v*` tag 推送产出 `X.Y.Z` / `X.Y`，不改 `latest`。

## CI 检查项

所有 job 都在 `ubuntu-latest` 上跑。

### `backend`（timeout 45 分钟）

工作目录 `backend`，Rust 工具链取 stable（`dtolnay/rust-toolchain`，含 `clippy`/`rustfmt`），cargo 缓存用 `Swatinem/rust-cache`（`backend -> target`，key `backend-debug`）。

1. `cargo fmt --all --check`
2. `cargo clippy --workspace --all-targets --locked -- -D warnings`
3. `cargo test --workspace --locked`

真实 ClickHouse 用例是 `#[ignore]`，CI 不跑（没有 DSN）。

### `web`（timeout 20 分钟）

工作目录 `admin-web`，Node 22，npm 缓存按 `admin-web/package-lock.json`。

1. `npm ci`
2. `npm run lint`（`eslint .`）
3. `npm run typecheck`（`tsc --noEmit`）
4. `npm run build`（`tsc --noEmit && vite build`）
5. `bash scripts/smoke-headless-web.sh`（在仓库根执行）：起 `vite preview` 提供 `admin-web/dist`，断言 `/` 返回 HTML 且含 `id="root"`、`/review` 是 200 的 SPA 回退、页面引用的 `/assets/*.js` 可取
6. 上传 artifact `jumcp-admin-web-headless`（`admin-web/dist`，保留 14 天）

第 5 步是「无头 Web 服务」产物校验：只有构建产物能当静态站点跑起来，才算这一步通过。

### `image`（timeout 90 分钟）

1. `docker/setup-buildx-action`
2. `docker/build-push-action`：`context: .`、`platforms: linux/amd64`、`tags: jumcp:ci`、`load: true`、`provenance: false`、`sbom: false`（load 模式不支持这两项），GHA 缓存 `type=gha,scope=jumcp-image`
3. `bash scripts/smoke-image.sh jumcp:ci`（见下文）
4. `docker save jumcp:ci | gzip -1 > jumcp-linux-amd64-image.tar.gz`
5. 上传 artifact `jumcp-linux-amd64-image`（上面的压缩镜像，保留 7 天）

不登录 registry、不 push。

### `dependency-review`（仅 PR，timeout 10 分钟）

`actions/dependency-review-action@v4`，`fail-on-severity: high`：高危依赖变更直接阻断 PR。没有引入其他扫描器。

## 镜像

### 地址与 tag

- 地址：`ghcr.io/nnyjk/jumcp`（`ghcr.io/<owner>/jumcp`，owner 一律转小写）
- tag（`docker/metadata-action`）：
  - `main`：`main` push 时打上（`type=ref,event=branch`）
  - `sha-<短 sha>`：每次发布 push（`type=sha`）
  - `X.Y.Z`、`X.Y`：tag `vX.Y.Z` push（`type=semver`）
  - `latest`：只在 `main` push 时打（`type=raw,value=latest,enable=${{ github.ref == 'refs/heads/main' }}`，且 `flavor: latest=false`，tag push 不会改 `latest`）。默认分支是 `dev`，因此不用 `{{is_default_branch}}`
- 每次发布带 `provenance: true` 与 `sbom: true`（attestation 随镜像 push 到 GHCR）
- 发布门禁：push 之后按 `ghcr.io/<owner>/jumcp@<digest>` 回拉，再跑同一份 `scripts/smoke-image.sh`；任一断言失败即发布失败

### 平台

只构建并验证 `linux/amd64`。镜像不带 `arm64` manifest；在 arm64 主机上 `docker pull` 会拿到 amd64 镜像，需要 QEMU 模拟（不保证可用/性能）。

### 运行时约定

- 多阶段构建：`rust:bookworm` 编译 `jumcp-server` + `jumcp-connector-clickhouse`（release、`--locked`），`node:22-bookworm-slim` 构建 `admin-web`，运行镜像 `debian:bookworm-slim`
- 运行时只装 `ca-certificates`、`curl`（HEALTHCHECK）、`tini`（ENTRYPOINT，`STOPSIGNAL SIGTERM` 才能正常转发）
- 非 root：UID/GID `10001`（`docker run` 里可用 `id -u` 验证）
- `/data` 是 `VOLUME` 且属主 `10001`：registry/audit 两个 SQLite 都在这里
- 默认 env（镜像内置）：
  - `JUMCP_MCP_WEB_DIR=/app/web`（管理台静态资源，同源提供）
  - `JUMCP_MCP_TRANSPORT=streamable-http`、`JUMCP_MCP_HOST=0.0.0.0`、`JUMCP_MCP_PORT=8000`
  - `JUMCP_MCP_CONFIG_FILE=/app/config/policy.toml`
  - `JUMCP_MCP_REGISTRY_PATH=/data/registry.sqlite3`、`JUMCP_MCP_AUDIT_PATH=/data/audit.sqlite3`
- `JUMCP_ADMIN_TOKENS` **没有任何默认值**：HTTP 模式下不配就拒绝启动（fail-closed）。镜像里不含任何 token/secret

## 运行命令

### 本机构建镜像

```bash
docker buildx build --platform linux/amd64 -t jumcp:local --load .
```

### 起容器

```bash
docker run -d --name jumcp -p 127.0.0.1:8000:8000 \
  -e JUMCP_ADMIN_TOKENS="admin:$(openssl rand -hex 24)" \
  -v jumcp-data:/data \
  jumcp:local
```

`JUMCP_AGENT_TOKENS` 只在 `access_tokens` 表为空时把静态 token 一次性导入数据库（引导用），生产令牌请用 `POST /admin/tokens` 在管理面生成。

### `scripts/smoke-headless-web.sh`

```
bash scripts/smoke-headless-web.sh [--port <port>] [--timeout <seconds>]
```

- 前置：`admin-web/node_modules`（`npm ci`）与 `admin-web/dist`（`npm run build`）
- 环境变量：`JUMCP_SMOKE_WEB_PORT`（默认 `4173`）
- 断言：`/` 200 `text/html` 且含 `id="root"`；`/review` 200 SPA 回退；页面里的 `/assets/*.js` 200 且非空
- 退出码：0 通过、1 断言失败、2 参数或前置缺失；退出时杀掉 `vite preview`

### `scripts/smoke-image.sh`

```
bash scripts/smoke-image.sh <image> [--port <port>] [--timeout <seconds>]
```

- 需要：`docker`、`curl`、`jq`
- 容器参数：`/data` 用 `--tmpfs /data:rw,mode=1777,size=64m`（非 root 可写 + 每次干净空库），端口默认随机（`-p 127.0.0.1::8000`，也可 `--port` 指定）
- 注入凭证：`JUMCP_ADMIN_TOKENS=smoke:<token>`、`JUMCP_AGENT_TOKENS=agent-analyst:analyst:<token>`；可用 `JUMCP_SMOKE_ADMIN_TOKEN`、`JUMCP_SMOKE_AGENT_TOKEN` 覆盖
- 断言：
  1. `/health` 探针分支（无 `Accept` / `Accept: application/json`）200 + `application/json` 且 `audit_chain == "valid"`——监控与容器探针不会被前端劫持
  2. `GET /health` 且 `Accept: text/html` → 200 `text/html` 且含 `id="root"`（浏览器导航落管理台健康页）
  3. `/` 200 `text/html` 且含 `id="root"`
  4. `/review` 200 SPA 回退
  5. `/assets/*.js` 200 且非空
  6. 匿名 `/admin/review-queue` → 401；带管理 token → 200 且 `items` 是数组
  7. `/mcp` 用 agent token `initialize` 200、`tools/list` 200 且含 `search_capabilities`
  8. `id -u` = `10001`
- 失败时打印容器状态与日志尾部，然后清理容器与临时目录
- 退出码：0 通过、1 断言失败、2 参数或工具缺失

## 分支保护

`dev` 与 `main` 都要保护：只允许 PR 合并、禁止 force push、禁止删除分支、合并前要求分支最新。`main` 另需禁止直接 push（发版提交只能来自 `dev` 的 PR 合并）。不强制要求人工 approve——单人仓库会把合并卡死。

required status checks 的名字必须与 workflow 里的 job `name` **完全一致**，否则 GitHub 会一直等一个永不出现的检查：

- `backend (fmt / clippy / test)`
- `web (lint / typecheck / build / headless smoke)`
- `image (linux/amd64 build / smoke)`
- `dependency review`（只在 PR 上出现；不要设成只对 `main` 生效，否则发版 PR 会缺这一项）

一次性配置（`gh api`，两个分支同一份 payload）：

```bash
payload='{
  "required_status_checks": {
    "strict": true,
    "contexts": [
      "backend (fmt / clippy / test)",
      "web (lint / typecheck / build / headless smoke)",
      "image (linux/amd64 build / smoke)",
      "dependency review"
    ]
  },
  "enforce_admins": true,
  "required_pull_request_reviews": { "required_approving_review_count": 0 },
  "restrictions": null,
  "allow_force_pushes": false,
  "allow_deletions": false,
  "required_conversation_resolution": false
}'

printf '%s' "$payload" | gh api --method PUT repos/Nnyjk/jumcp/branches/dev/protection --input -
printf '%s' "$payload" | gh api --method PUT repos/Nnyjk/jumcp/branches/main/protection --input -
```

- `required_approving_review_count: 0` 表示「必须走 PR」但不要求人工 approve：单人仓库也能自行合并，同时直接 push 被拒。
- `enforce_admins: true` 是「禁止直接 push」真正生效的关键：否则仓库管理员（owner）可以绕过保护直接推 `main`/`dev`。
- `strict: true` 即合并前要求分支最新（PR 必须基于目标分支最新提交）。
- `restrictions: null` 表示不额外限制可推送人员（PR 要求已经挡住直接 push）；需要按人限制时再填白名单。

生效情况以返回结果为准：`gh api repos/Nnyjk/jumcp/branches/main/protection`、`gh api repos/Nnyjk/jumcp/branches/dev/protection`。计划或权限不允许时不要假设已生效。

## 管理台鉴权边界

- 管理台是薄前端，浏览器**不发** `Authorization`：管理面 API 的 Bearer 由网关/BFF 在同源路径注入（或改用会话 Cookie）。镜像只负责「同源静态资源 + API」一起可用，不提供任何 token 注入后门
- 开发期凭证注入只存在 Node 进程环境：`JUMCP_DEV_ADMIN_TOKEN`（Vite 代理注入），不是 `VITE_*`，不进 bundle；`vite.config.ts` 在 production 构建期检测 `VITE_ADMIN_TOKEN`/`VITE_ADMIN_ACTOR` 就直接终止构建
- `scripts/smoke-image.sh` 直接带 Bearer 打 `/admin/review-queue`，验证的是**后端鉴权边界**（匿名 401、带 token 200），不是浏览器行为
- 发布流水线不写任何 token 到仓库：GHCR 登录用 `GITHUB_TOKEN`，镜像构建不需要管理面凭证
