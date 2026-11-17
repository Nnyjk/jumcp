# CI/CD

面向 `Nnyjk/jumcp` 的持续集成与发布流水线。两个 workflow：

| workflow | 触发 | 作用 |
|---|---|---|
| `.github/workflows/ci.yml` | push `dev`、`pull_request`（目标 `dev` / `main`）、`workflow_dispatch` | 后端静态检查与测试、管理台构建与无头 Web 冒烟、脚本语法与发布 tag 解析、镜像构建与冒烟、Swarm stack 配置校验、PR 依赖审查 |
| `.github/workflows/release.yml` | push `main`、tag `v*`（必须指向 `main` 历史）、`workflow_dispatch`（只有 `main` / `v*` tag 真正发布） | 构建并推送 GHCR 镜像，回拉 digest 再冒烟 |

CI 只订阅 `dev` 的 push：`main` 上的改动只能经 `dev → main` 的发版 PR 进入，合并后的全量发布校验由 release 承担，避免同一 ref 上重复跑两遍。PR 则同时接受目标 `dev`（日常）与 `main`（发版 PR）。

`release.yml` 在 `github.ref` 触发条件之外还有两道来源防线，`dev` 绕不过去：

- `publish` job 的 `if: github.ref == 'refs/heads/main' || startsWith(github.ref, 'refs/tags/v')`：`workflow_dispatch` 可以选任意 ref，选到 `dev` 时整个 job 跳过，不构建、不推镜像。
- tag 触发先跑 `git fetch --no-tags origin main`，再用 `git merge-base --is-ancestor "$GITHUB_SHA" origin/main` 校验：tag 必须打在 `main` 的提交上，在 `dev` 提交上打 `v*` tag 会失败退出（所以 checkout 用 `fetch-depth: 0`）。

CI 权限只有 `contents: read`；release 额外要 `packages: write`、`attestations: write`、`id-token: write`。两个 workflow 都用 `concurrency` 控并发：CI 取消同 ref 的旧运行，release 不取消。

## 分支模型与发布流程

| 分支 | 角色 | 谁能推 |
|---|---|---|
| `main` | 发布分支，只有从 `dev` 合入的发版提交 | 仅 PR 合并（禁止直接 push） |
| `dev` | 默认分支、集成分支，日常改动的汇聚点 | 仅 PR 合并（禁止直接 push） |
| `feature/*` | 单个改动的短期分支，从 `dev` 切出 | 作者本人 |

流程：

1. 从 `dev` 切 `feature/<主题>`，在其中开发、本地校验（见 `CONTRIBUTING.md`）。
2. 开 PR 到 `dev`：CI 跑 `backend` / `web` / `scripts` / `image` / `dependency review`，全部通过后合并（squash 或 merge 均可，历史留在 `dev`）。
3. 发版前确认 `dev` 已包含 `main`（上一轮发版后做过第 6 步的同步）；若 `main` 领先，先做一次 `main → dev` 的同步 PR 再发版。
4. 发版：`dev` 的 PR 目标改为 `main`，**必须用 merge commit**（`gh pr merge --merge`，不用 squash/rebase），保留 `dev` 的完整提交历史。这个 PR 除了常规检查，还会跑 `release source is dev`（来源分支不是 `dev` 直接失败）。
5. 合并进 `main` 即触发 `release.yml`，产出 `latest` + `main` + `sha-<短 sha>` 镜像。
6. 发版后把 `main` 合回 `dev`（`sync/main-to-dev` 分支 → PR 到 `dev`，merge commit）。`main` 因发版多一个 merge commit，`dev` 若不含它，`strict` 会让下一次发版 PR 卡在「分支不是最新」。

关键约束：

- `main` 不接受日常直接 push，也不承接 `feature/*` 的 PR（`release source is dev` 会拒绝）。
- release 只由 `main` push、`v*` tag、`workflow_dispatch` 触发；`dev` 的任何活动都不登录、不推送 GHCR——对 `dev` 手动 `workflow_dispatch` 时 `publish` job 直接跳过。
- `v*` tag 必须打在 `main` 历史内的提交上：tag 触发会跑 `merge-base --is-ancestor`，在 `dev` 提交上打 tag 会失败退出。
- `latest` 只由 `main` 的 push 产出（规则写在 `scripts/release-tags.sh`，只有 `main` 分支序列才带 `latest`）；默认分支是 `dev`，不能用「默认分支即发布分支」的假设。
- 版本 tag 必须是 `v` + 3 或 4 段数字：`v1.2.3` 产出 `1.2.3` / `1.2`，`v0.0.1.000` 产出 `0.0.1.000` / `0.0.1` / `0.0`；tag push 不改 `latest`。
- tag 名非法（`v1.2`、缺 `v` 前缀）时 `scripts/release-tags.sh` 直接失败退出，不构建、不推送。

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

### `scripts`（timeout 5 分钟）

1. 逐个 `bash -n scripts/*.sh`：语法错误在任何脚本上都会先在这里暴露
2. `bash scripts/test-release-tags.sh`：发布 tag 解析的离线用例（`main` / `v1.2.3` / `v1.2.3.4` / `v0.0.1.000` / 非法格式 / `--image` 前缀 / `--github-output`），不依赖网络与 docker

### `image`（timeout 90 分钟）

`needs: [backend, web]`：上游失败时不再排队一次 90 分钟的镜像构建；镜像仍从源码各自重编，不消费上游 artifact。

1. `docker/setup-buildx-action`
2. `JUMCP_ADMIN_TOKENS=ci-placeholder docker stack config -c deploy/portainer-stack.yml >/dev/null`（步骤名 `Validate stack file`）：只做配置解析，校验 `deploy/portainer-stack.yml` 落在 **Swarm** 接受的 compose 子集内（见 [portainer-swarm.md](portainer-swarm.md)）；不部署、不登录 registry。dummy token 只为满足 `${JUMCP_ADMIN_TOKENS:?…}` 的 fail-fast——`platform` 这类 Swarm 会拒绝的键在这里就直接失败，不用等真部署
   本地可另跑 `docker compose -f deploy/portainer-stack.yml config -q` 作 YAML/插值旁证：它按 compose 语义求值，会为 `version` 键给废弃警告，也不会像 `docker stack config` 那样拒收 `platform` 这类 Swarm 非法键。部署门禁以 `docker stack config` 为准，CI 只跑后者。
3. `docker/build-push-action`：`context: .`、`platforms: linux/amd64`、`tags: jumcp:ci`、`load: true`、`provenance: false`、`sbom: false`（load 模式不支持这两项），GHA 缓存 `type=gha,scope=jumcp-image`
4. `bash scripts/smoke-image.sh jumcp:ci`（见下文）
5. `docker save jumcp:ci | gzip -1 > jumcp-linux-amd64-image.tar.gz`
6. 上传 artifact `jumcp-linux-amd64-image`（上面的压缩镜像，保留 7 天）

不登录 registry、不 push。

### `dependency-review`（仅 PR，timeout 10 分钟）

`actions/dependency-review-action@v4`，`fail-on-severity: high`：高危依赖变更直接阻断 PR。没有引入其他扫描器。

### `release-source`（仅目标为 `main` 的 PR，timeout 5 分钟）

job 名 `release source is dev`，`if: github.event_name == 'pull_request' && github.base_ref == 'main'`：断言 `github.head_ref` 等于 `dev`，否则失败退出。PR 目标为 `dev` 时整个 job 跳过（跳过不算失败，也不影响 `dev` 的 required checks）。作用是挡住 `feature/*` 直接 PR 到 `main`——`main` 只接受 `dev` 的发版 PR。

## 镜像

### 地址与 tag

- 地址：`ghcr.io/nnyjk/jumcp`（`ghcr.io/<owner>/jumcp`，owner 一律转小写）
- tag 与 `VERSION` 构建参数由 `scripts/release-tags.sh` 唯一决定，release 流水线调它写 `GITHUB_OUTPUT`，本地可用同一脚本复算：

| 触发 | `version`（即 `VERSION` build arg） | tags |
|---|---|---|
| `main` push / 在 `main` 上手动 dispatch | `main` | `latest`、`main`、`sha-<短 sha>` |
| tag `v1.2.3` | `1.2.3` | `1.2.3`、`1.2`、`sha-<短 sha>` |
| tag `v0.0.1.000`（四段） | `0.0.1.000` | `0.0.1.000`、`0.0.1`、`0.0`、`sha-<短 sha>` |

- 四段版本额外产出三段兼容 tag，供只认 semver `X.Y.Z` 的消费方 pin；精确 `0.0.1.000` 始终存在
- `latest` 只在 `main` 分支序列出现；tag push 不改 `latest`
- tag 名（`v` + 3/4 段数字）、分支（只有 `main`）在构建前校验，非法输入 fail-fast
- 每次发布带 `provenance: true` 与 `sbom: true`（attestation 随镜像 push 到 GHCR）
- 发布门禁：push 之后按 `ghcr.io/<owner>/jumcp@<digest>` 回拉，再跑同一份 `scripts/smoke-image.sh`；任一断言失败即发布失败

### 平台

只构建并验证 `linux/amd64`。镜像不带 `arm64` manifest；在 arm64 主机上 `docker pull` 会拿到 amd64 镜像，需要 QEMU 模拟（不保证可用/性能）。

### 运行时约定

- 多阶段构建：`rust:bookworm` 编译 `jumcp-server` + `jumcp-connector-clickhouse`（release、`--locked`），`node:22-bookworm-slim` 构建 `admin-web`，运行镜像 `debian:bookworm-slim`
- 三个基础镜像用浮动 tag 跟随 Debian 12 / Node 22 / Rust stable，与 CI 的 `dtolnay/rust-toolchain@stable` 保持一致；需要钉死时用 `--build-arg RUST_IMAGE=<tag|digest>`、`NODE_IMAGE=…`、`RUNTIME_IMAGE=…` 覆盖（`Dockerfile` 只认这三个 ARG）
- 运行时只装 `ca-certificates`、`curl`（HEALTHCHECK）、`tini`（ENTRYPOINT，`STOPSIGNAL SIGTERM` 才能正常转发）
- 非 root：UID/GID `10001`（`docker run` 里可用 `id -u` 验证）
- `/data` 是 `VOLUME` 且属主 `10001`：registry/audit 两个 SQLite 都在这里
- 默认 env（镜像内置）：
  - `JUMCP_MCP_WEB_DIR=/app/web`（管理台静态资源，同源提供）
  - `JUMCP_MCP_TRANSPORT=streamable-http`、`JUMCP_MCP_HOST=0.0.0.0`、`JUMCP_MCP_PORT=8000`
  - `JUMCP_MCP_CONFIG_FILE=/app/config/policy.toml`
  - `JUMCP_MCP_REGISTRY_PATH=/data/registry.sqlite3`、`JUMCP_MCP_AUDIT_PATH=/data/audit.sqlite3`
- `JUMCP_ADMIN_TOKENS` **没有任何默认值**：HTTP 模式下不配就拒绝启动（fail-closed）。镜像里不含任何 token/secret
- 部署形态：单容器 `docker run` 见下文「运行命令」；Portainer / Docker Swarm 用 `deploy/portainer-stack.yml`（单副本、`stop-first`、命名卷），前置条件与边界见 [portainer-swarm.md](portainer-swarm.md)

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

### `scripts/release-tags.sh`

```
bash scripts/release-tags.sh [--ref-type branch|tag] [--ref-name <ref>] [--sha <sha>] \
                             [--image <registry/repo>] [--github-output]
```

- 输入缺省取 `GITHUB_REF_TYPE` / `GITHUB_REF_NAME` / `GITHUB_SHA`（release 流水线直接用环境变量），本地模拟用参数
- 输出 `version=…` 与 `tags=…`（逗号分隔）；`--github-output` 写 `$GITHUB_OUTPUT`，`--image` 给每个 tag 加仓库前缀
- 本地复算发版 tag：`bash scripts/release-tags.sh --ref-type tag --ref-name v0.0.1.000 --sha <sha>`
- 退出码：0 成功、2 参数/ref 非法（非 `main` 分支、tag 不是 `v` + 3/4 段数字）
- 用例：`bash scripts/test-release-tags.sh`（CI 的 `scripts` job 跑同一份）

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

required status checks 的名字必须与 workflow 里的 job `name` **完全一致**，否则 GitHub 会一直等一个永不出现的检查。两个分支的列表**不同**：

`dev`（5 项）：

- `backend (fmt / clippy / test)`
- `web (lint / typecheck / build / headless smoke)`
- `scripts (bash -n / release tags)`
- `image (linux/amd64 build / smoke)`
- `dependency review`（只在 PR 上出现；不要设成只对 `main` 生效，否则发版 PR 会缺这一项）

`main`（上面 5 项 + 1 项来源校验）：

- `release source is dev`（只在目标为 `main` 的 PR 上运行，PR 到 `dev` 时跳过）。`dev` 上**不要**加这一项，否则 `dev` 的检查永远等不到它。

`dependency review` 还依赖仓库的 Dependency graph：未开启时该 job 会立即失败并报 `Dependency review is not supported on this repository`，与代码无关。在仓库 Settings → Code security and analysis 开启 Dependency graph（启用 Dependabot alerts 会一并打开它）后重跑即可。

一次性配置（`gh api`；`main` 在 `dev` 的 payload 上追加来源检查）：

```bash
payload='{
  "required_status_checks": {
    "strict": true,
    "contexts": [
      "backend (fmt / clippy / test)",
      "web (lint / typecheck / build / headless smoke)",
      "scripts (bash -n / release tags)",
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

# main 多一项：只接受 dev 的发版 PR
printf '%s' "$payload" \
  | jq '.required_status_checks.contexts += ["release source is dev"]' \
  | gh api --method PUT repos/Nnyjk/jumcp/branches/main/protection --input -
```

- `required_approving_review_count: 0` 表示「必须走 PR」但不要求人工 approve：单人仓库也能自行合并，同时直接 push 被拒。
- `enforce_admins: true` 是「禁止直接 push」真正生效的关键：否则仓库管理员（owner）可以绕过保护直接推 `main`/`dev`。
- `strict: true` 即合并前要求分支最新（PR 必须基于目标分支最新提交）。
- `strict: true` 与「发版用 merge commit」配合的后果：每次发版后 `main` 都比 `dev` 多一个 merge commit，必须按「分支模型与发布流程」第 6 步做 `main → dev` 同步，否则下一次发版 PR 会被「分支不是最新」挡住。
- `restrictions: null` 表示不额外限制可推送人员（PR 要求已经挡住直接 push）；需要按人限制时再填白名单。

生效情况以返回结果为准：`gh api repos/Nnyjk/jumcp/branches/main/protection`、`gh api repos/Nnyjk/jumcp/branches/dev/protection`。计划或权限不允许时不要假设已生效。

## 管理台鉴权边界

- 管理台是薄前端，浏览器**不发** `Authorization`：管理面 API 的 Bearer 由网关/BFF 在同源路径注入（或改用会话 Cookie）。镜像只负责「同源静态资源 + API」一起可用，不提供任何 token 注入后门
- 开发期凭证注入只存在 Node 进程环境：`JUMCP_DEV_ADMIN_TOKEN`（Vite 代理注入），不是 `VITE_*`，不进 bundle；`vite.config.ts` 在 production 构建期检测 `VITE_ADMIN_TOKEN`/`VITE_ADMIN_ACTOR` 就直接终止构建
- `scripts/smoke-image.sh` 直接带 Bearer 打 `/admin/review-queue`，验证的是**后端鉴权边界**（匿名 401、带 token 200），不是浏览器行为
- 发布流水线不写任何 token 到仓库：GHCR 登录用 `GITHUB_TOKEN`，镜像构建不需要管理面凭证
