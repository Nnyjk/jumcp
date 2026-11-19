# CI/CD

面向 `Nnyjk/jumcp` 的持续集成与发布流水线。三个 workflow：

| workflow | 触发 | 作用 |
|---|---|---|
| `.github/workflows/ci.yml` | push `dev`、`pull_request`（目标 `dev` / `main`）、`workflow_dispatch` | 后端静态检查与测试、管理台构建与无头 Web 冒烟、脚本语法与发布 tag 解析、镜像构建与冒烟、Swarm stack 配置校验、PR 依赖审查。只读：不登录、不推任何 registry |
| `.github/workflows/image-cd.yml` | push `dev`、push `main`、tag `v*`（必须指向 `main` 历史）、`workflow_dispatch`（只有 `dev` / `main` / `v*` tag 真正构建） | 构建 `linux/amd64` 镜像并推 GHCR（`dev` 推快照 tag，`main` 推 `latest`，tag 推版本号），按 digest 回拉再冒烟 |
| `.github/workflows/web-cd.yml` | 同上 | 构建无头 Web 包（`jumcp-admin-web-<version>.tar.gz`）并上传 artifact；tag 触发时再挂到该 tag 的 GitHub Release |

CI 只订阅 `dev` 的 push 与 PR：`main` 上的改动只能经 `dev → main` 的发版 PR 进入，合并后的产物发布由 `image-cd` / `web-cd` 承担，避免同一 ref 上重复跑全量校验。PR 则同时接受目标 `dev`（日常）与 `main`（发版 PR）。

`image-cd.yml` 在 `github.ref` 触发条件之外还有两道来源防线，`dev` 不会被当成正式版发布：

- `publish` job 的 `if: github.ref == 'refs/heads/dev' || github.ref == 'refs/heads/main' || startsWith(github.ref, 'refs/tags/v')`：`workflow_dispatch` 可以选任意 ref，选到 feature 分支时整个 job 跳过，不构建、不推镜像。
- tag 触发先跑 `git fetch --no-tags origin main`，再用 `git merge-base --is-ancestor "$GITHUB_SHA" origin/main` 校验：tag 必须打在 `main` 的提交上，在 `dev` 提交上打 `v*` tag 会失败退出（所以 checkout 用 `fetch-depth: 0`）。

权限与并发：CI 只有 `contents: read`；`image-cd` 额外要 `packages: write`、`attestations: write`、`id-token: write`；`web-cd` 默认 `contents: read`，只有 tag 场景的 `release` job 提升到 `contents: write`（GitHub 权限是 job 级的，所以拆成两个 job）。并发：CI 取消同 ref 的旧运行，`image-cd` / `web-cd` 不取消（`cancel-in-progress: false`）。

## 分支模型与发布流程

| 分支 | 角色 | 谁能推 |
|---|---|---|
| `main` | 发布分支，只有从 `dev` 合入的发版提交 | 仅 PR 合并（禁止直接 push） |
| `dev` | 默认分支、集成分支，日常改动的汇聚点 | 仅 PR 合并（禁止直接 push） |
| `feature/*` | 单个改动的短期分支，从 `dev` 切出 | 作者本人 |

流程：

1. 从 `dev` 切 `feature/<主题>`，在其中开发、本地校验（见 `CONTRIBUTING.md`）。
2. 开 PR 到 `dev`：CI 跑 `backend` / `web` / `scripts` / `image` / `dependency review`，全部通过后合并（squash 或 merge 均可，历史留在 `dev`）。合并后 `image-cd` 推 `dev` 快照镜像，`web-cd` 上传 `dev` 的 Web 包 artifact。
3. 发版前确认 `dev` 已包含 `main`（上一轮发版后做过第 7 步的同步）；若 `main` 领先，先做一次 `main → dev` 的同步 PR 再发版。
4. 发版：`dev` 的 PR 目标改为 `main`，**必须用 merge commit**（`gh pr merge --merge`，不用 squash/rebase），保留 `dev` 的完整提交历史。这个 PR 除了常规检查，还会跑 `release source is dev`（来源分支不是 `dev` 直接失败）。
5. 合并进 `main` 即触发 `image-cd.yml` 与 `web-cd.yml`：镜像产出 `latest` + `main` + `sha-<短 sha>`，Web 包产出 `jumcp-admin-web-main.tar.gz` artifact。
6. 需要正式版本号时在 `main` 的发布提交上打 `v*` tag：`image-cd` 改产版本号 tag（不动 `latest`），`web-cd` 把同一份 Web 包挂到该 tag 的 GitHub Release。
7. 发版后把 `main` 合回 `dev`（`sync/main-to-dev` 分支 → PR 到 `dev`，merge commit）。`main` 因发版多一个 merge commit，`dev` 若不含它，`strict` 会让下一次发版 PR 卡在「分支不是最新」。

关键约束：

- `main` 不接受日常直接 push，也不承接 `feature/*` 的 PR（`release source is dev` 会拒绝）。
- `image-cd` 由 `dev` push（快照）、`main` push（正式）、`v*` tag 与 `workflow_dispatch` 触发；`web-cd` 触发面相同。对 feature 分支手动 dispatch 时两个 workflow 的构建 job 都整体跳过。
- `v*` tag 必须打在 `main` 历史内的提交上：tag 触发会跑 `merge-base --is-ancestor`，在 `dev` 提交上打 tag 会失败退出。
- `latest` 只由 `main` 的 push 产出（规则写在 `scripts/release-tags.sh`，只有 `main` 分支序列才带 `latest`）；`dev` 快照只有 `dev` 与 `sha-<7 位>`，不能把 `dev` 当正式版本 pin。
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
2. `bash scripts/test-release-tags.sh`：发布 tag 解析的离线用例（`dev` 快照 / `main` / `v1.2.3` / `v1.2.3.4` / `v0.0.1.000` / 非法格式 / `--image` 前缀 / `--github-output`），不依赖网络与 docker

### `image`（timeout 90 分钟）

`needs: [backend, web]`：上游失败时不再排队一次 90 分钟的镜像构建；镜像仍从源码各自重编，不消费上游 artifact。镜像冒烟同时覆盖内置 MCP 包的安装链路（起容器时注入固定测试用的 `JUMCP_SECRET_KEY` 与 `JUMCP_TRANSPORT_KEY`，见 `scripts/smoke-image.sh` 第 8 步）。

1. `docker/setup-buildx-action`
2. `JUMCP_ADMIN_USERNAME=ci-placeholder JUMCP_ADMIN_PASSWORD=ci-placeholder-password JUMCP_SECRET_KEY=<64 个 0> JUMCP_TRANSPORT_KEY=<64 个 0> docker stack config -c deploy/portainer-stack.yml >/dev/null`（步骤名 `Validate stack file`）：只做配置解析，校验 `deploy/portainer-stack.yml` 落在 **Swarm** 接受的 compose 子集内（见 [portainer-swarm.md](portainer-swarm.md)）；不部署、不登录 registry。dummy 账号/密钥只为满足 `${JUMCP_ADMIN_USERNAME:?…}`、`${JUMCP_ADMIN_PASSWORD:?…}`、`${JUMCP_SECRET_KEY:?…}` 与 `${JUMCP_TRANSPORT_KEY:?…}` 的 fail-fast——`platform` 这类 Swarm 会拒绝的键在这里就直接失败，不用等真部署
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
- tag 与 `VERSION` 构建参数由 `scripts/release-tags.sh` 唯一决定，`image-cd` 流水线调它写 `GITHUB_OUTPUT`，本地可用同一脚本复算：

| 触发 | `version`（即 `VERSION` build arg） | tags |
|---|---|---|
| `dev` push / 在 `dev` 上手动 dispatch | `dev` | `dev`、`sha-<短 sha>`（快照，不带 `latest`） |
| `main` push / 在 `main` 上手动 dispatch | `main` | `latest`、`main`、`sha-<短 sha>` |
| tag `v1.2.3` | `1.2.3` | `1.2.3`、`1.2`、`sha-<短 sha>` |
| tag `v0.0.1.000`（四段） | `0.0.1.000` | `0.0.1.000`、`0.0.1`、`0.0`、`sha-<短 sha>` |

- `dev` 是集成分支快照：`dev` tag 会被下一次 push 覆盖，只能用来试跑与回滚参考，不适合任何「固定版本」的部署
- 四段版本额外产出三段兼容 tag，供只认 semver `X.Y.Z` 的消费方 pin；精确 `0.0.1.000` 始终存在
- `latest` 只在 `main` 分支序列出现；tag push 不改 `latest`
- tag 名（`v` + 3/4 段数字）、分支（只有 `dev` 与 `main`）在构建前校验，非法输入 fail-fast
- 每次发布带 `provenance: true` 与 `sbom: true`（attestation 随镜像 push 到 GHCR）
- 发布门禁：push 之后按 `ghcr.io/<owner>/jumcp@<digest>` 回拉，再跑同一份 `scripts/smoke-image.sh`；任一断言失败即发布失败

### 平台

只构建并验证 `linux/amd64`。镜像不带 `arm64` manifest；在 arm64 主机上 `docker pull` 会拿到 amd64 镜像，需要 QEMU 模拟（不保证可用/性能）。

### 运行时约定

- 多阶段构建：`rust:bookworm` 编译 `jumcp-server` + `jumcp-connector-clickhouse`（release、`--locked`），`node:22-bookworm-slim` 构建 `admin-web`，运行镜像 `debian:bookworm-slim`
- 三个基础镜像用浮动 tag 跟随 Debian 12 / Node 22 / Rust stable，与 CI 的 `dtolnay/rust-toolchain@stable` 保持一致；需要钉死时用 `--build-arg RUST_IMAGE=<tag|digest>`、`NODE_IMAGE=…`、`RUNTIME_IMAGE=…` 覆盖（`Dockerfile` 只认这三个 ARG）
- 运行时只装 `ca-certificates`、`curl`（HEALTHCHECK）、`tini`（ENTRYPOINT，`STOPSIGNAL SIGTERM` 才能正常转发）
- 非 root：UID/GID `10001`（`docker run` 里可用 `id -u` 验证）
- `/data` 是 `VOLUME` 且属主 `10001`：registry/audit 两个 SQLite 都在这里；内置包安装出来的是 registry SQLite 里的一条上游记录（带 `catalog_id` / `catalog_version` 坐标），因此安装状态随数据卷持久化，不需要额外卷
- 镜像在 `/usr/local/bin` 提供 `jumcp-server` 与 `jumcp-connector-clickhouse`；内置 MCP 包（`clickhouse-basic`）编译进 `jumcp-server`，没有独立二进制
- 内置包安装需要 `JUMCP_SECRET_KEY`（落库）与 `JUMCP_TRANSPORT_KEY`（请求体）两把 64 位 hex 密钥：镜像不提供默认值，HTTP 模式缺任一把都拒绝启动；未挂内嵌端点（如以 stdio 模式启动）时安装返回 503 `MCP_BUILTIN_UNAVAILABLE`
- 默认 env（镜像内置）：
  - `JUMCP_MCP_WEB_DIR=/app/web`（管理台静态资源，同源提供）
  - `JUMCP_MCP_TRANSPORT=streamable-http`、`JUMCP_MCP_HOST=0.0.0.0`、`JUMCP_MCP_PORT=8000`
  - `JUMCP_MCP_CONFIG_FILE=/app/config/policy.toml`
  - `JUMCP_MCP_REGISTRY_PATH=/data/registry.sqlite3`、`JUMCP_MCP_AUDIT_PATH=/data/audit.sqlite3`
- `JUMCP_ADMIN_USERNAME` / `JUMCP_ADMIN_PASSWORD` **没有任何默认值**：HTTP 模式下空库首启就必须提供（口令至少 12 位），否则拒绝启动（fail-closed）。首个账号落库为 Argon2id 哈希，之后这两个变量不再生效，改口令走 `PATCH /admin/admins/{id}`。镜像里不含任何账号/口令/secret
- 部署形态：单容器 `docker run` 见下文「运行命令」；Portainer / Docker Swarm 用 `deploy/portainer-stack.yml`（单副本、`stop-first`、命名卷），前置条件与边界见 [portainer-swarm.md](portainer-swarm.md)

## 无头 Web 包（`web-cd.yml`）

管理台的静态产物单独发一份，不必等镜像、也不依赖镜像构建。`build` job（Node 22 + npm 缓存，`timeout 20 分钟`）：

1. `npm ci` → `npm run lint` → `npm run typecheck` → `npm run test:transport` → `npm run build`（工作目录 `admin-web`）
2. 在仓库根跑 `bash scripts/smoke-headless-web.sh`：产物必须能当静态站点提供
3. `bash scripts/release-tags.sh --github-output` 取 `version`（与镜像同一份规则，不带 `--image` 前缀）
4. `tar -C admin-web/dist -czf jumcp-admin-web-$version.tar.gz .`
5. 上传 artifact `jumcp-admin-web-headless-<run_id>`（保留 30 天，`if-no-files-found: error`）

`release` job 只在 `v*` tag 上运行（`needs: build`，`permissions: contents: write`）：下载同一 artifact，用 `softprops/action-gh-release@v2` 创建/更新该 tag 的 Release（`generate_release_notes: true`、`fail_on_unmatched_files: true`）。分支运行不会拿到写权限。

产物矩阵：

| 触发 | `version` | 产物 |
|---|---|---|
| `dev` push | `dev` | `jumcp-admin-web-dev.tar.gz` artifact |
| `main` push | `main` | `jumcp-admin-web-main.tar.gz` artifact |
| tag `v0.0.1.000` | `0.0.1.000` | `jumcp-admin-web-0.0.1.000.tar.gz` artifact + 该 tag 的 Release 附件 |

解包即 `dist` 内容（`index.html`、`assets/`），可直接交给任意静态服务器；`tar -C admin-web/dist .` 解包时落在当前目录，不要指望有顶层目录。

## 运行命令

### 本机构建镜像

```bash
docker buildx build --platform linux/amd64 -t jumcp:local --load .
```

### 起容器

```bash
docker run -d --name jumcp -p 127.0.0.1:8000:8000 \
  -e JUMCP_ADMIN_USERNAME=admin \
  -e JUMCP_ADMIN_PASSWORD="$(openssl rand -hex 12)" \
  -e JUMCP_SECRET_KEY="$(openssl rand -hex 32)" \
  -e JUMCP_TRANSPORT_KEY="$(openssl rand -hex 32)" \
  -v jumcp-data:/data \
  jumcp:local
```

`JUMCP_SECRET_KEY` 负责落库加密，`JUMCP_TRANSPORT_KEY` 负责管理台提交 `secret` 字段时的请求体加密（两把都是 HTTP 模式启动必需，`openssl rand -hex 32` 生成）。落库密钥轮换会让旧密文不可解，需要在管理台重填密码；传输密钥轮换只影响未重新提交的密文，重填一次密码即可。

`JUMCP_AGENT_TOKENS` 只在 `access_tokens` 表为空时把静态 token 一次性导入数据库（引导用），生产令牌请用 `POST /admin/tokens` 在管理面生成。

### `scripts/release-tags.sh`

```
bash scripts/release-tags.sh [--ref-type branch|tag] [--ref-name <ref>] [--sha <sha>] \
                             [--image <registry/repo>] [--github-output]
```

- 输入缺省取 `GITHUB_REF_TYPE` / `GITHUB_REF_NAME` / `GITHUB_SHA`（两条 CD 流水线直接用环境变量），本地模拟用参数
- 输出 `version=…` 与 `tags=…`（逗号分隔）；`--github-output` 写 `$GITHUB_OUTPUT`，`--image` 给每个 tag 加仓库前缀
- 分支规则：`dev` → `dev` + `sha-<7 位>`；`main` → `latest` + `main` + `sha-<7 位>`；其它分支退出码 2
- 本地复算发版 tag：`bash scripts/release-tags.sh --ref-type tag --ref-name v0.0.1.000 --sha <sha>`
- 退出码：0 成功、2 参数/ref 非法（非 `dev`/`main` 分支、tag 不是 `v` + 3/4 段数字）
- 用例：`bash scripts/test-release-tags.sh`（CI 的 `scripts` job 跑同一份，覆盖 `dev` 快照、`main`、三段/四段 tag、`--image` 前缀、`--github-output` 与各类非法输入）

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
- 注入凭证：`JUMCP_ADMIN_USERNAME=smoke` + `JUMCP_ADMIN_PASSWORD=<至少 12 位>`（首次启动在空库里播种管理员）、`JUMCP_AGENT_TOKENS=agent-analyst:analyst:<token>`；可用 `JUMCP_SMOKE_ADMIN_USERNAME`、`JUMCP_SMOKE_ADMIN_PASSWORD`、`JUMCP_SMOKE_AGENT_TOKEN` 覆盖
- 容器注入固定测试用的 `JUMCP_SECRET_KEY` 与 `JUMCP_TRANSPORT_KEY`（内置包安装链路的前提，见断言第 8 条）
- 断言：
  1. `/health` 探针分支（无 `Accept` / `Accept: application/json`）200 + `application/json` 且 `audit_chain == "valid"`——监控与容器探针不会被前端劫持
  2. `GET /health` 且 `Accept: text/html` → 200 `text/html` 且含 `id="root"`（浏览器直接导航 `/health` 时落同源 SPA 外壳，不是 JSON 探针）
  3. `/` 200 `text/html` 且含 `id="root"`
  4. `/review` 200 SPA 回退
  5. `/assets/*.js` 200 且非空
  6. 管理面鉴权链路：匿名 `/admin/review-queue` → 401 `MCP_UNAUTHORIZED`；`POST /admin/login` 用户名口令 → 204 + `HttpOnly` 会话 cookie；错误口令 → 401 且 message 为 `用户名或密码错误`；带 cookie 的 `/admin/session` 返回 `actor.id == smoke`；带 cookie 的 `/admin/admins` → 200、`items` 是数组、`total >= 1`、不含 `$argon2id$` 与口令原文；带 cookie 的 `/admin/review-queue` → 200 且 `items` 是数组；`POST /admin/logout` 后旧 cookie → 401
  7. `/mcp` 用 agent token `initialize` 200、`tools/list` 200 且含 `search_capabilities`
  8. 内置包链路：容器注入 `JUMCP_SECRET_KEY` 与 `JUMCP_TRANSPORT_KEY`；断言 `/admin/mcp/catalog` 里 `clickhouse-basic` 的 `transport` 为 `http` 且没有二进制/stdio 开关字段；安装（`url=http://127.0.0.1:1`，密码提交预先算好的 `enc:v1:` 传输密文，只验证 HTTP 端点与工具缓存，不验证真库；响应里的密码是 `***`）→ 同一端点再提交明文密码必须 400 `MCP_TRANSPORT_CIPHERTEXT_REQUIRED` → `POST /admin/upstreams/{id}/refresh` 后 `status=ok`、`tool_count=3`；`PATCH .../config` 改 `url` 且 `secrets_set.password=true`、响应不含明文/密文；agent token 的 `tools/list` 含 `clickhouse__query`；最后 `DELETE` 卸载且 `deleted=true`
  9. `id -u` = `10001`
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

- 管理台是薄前端，浏览器**不发** `Authorization`：管理面自带登录页，`POST /admin/login` 提交用户名口令换 HttpOnly 会话 cookie，此后 `/admin/*` 只认 cookie（没有 Bearer 回落，脚本也用同一条登录链路）。镜像只负责「同源静态资源 + API」一起可用，不提供任何凭证注入后门，也没有网关
- 开发期不注入任何凭证：Vite 只做纯转发（`JUMCP_DEV_ADMIN_TARGET` 指管理面、`JUMCP_DEV_API_TARGET` 指 MCP 面），两个变量都只存在于 Node 进程环境
- `scripts/smoke-image.sh` 走真实登录链路（口令换 cookie 后打 `/admin/review-queue` 与 `/admin/admins`），验证的是**后端鉴权边界**（匿名 401、错口令 401、有效会话 200），不是浏览器行为
- 发布流水线不写任何 token 到仓库：GHCR 登录用 `GITHUB_TOKEN`，镜像构建不需要管理面凭证
