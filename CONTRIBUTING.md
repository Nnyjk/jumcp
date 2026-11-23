# 贡献指南

面向 `Nnyjk/jumcp` 的分支模型、开发流程与发版步骤。目录职责见 [docs/repository-structure.md](docs/repository-structure.md)，CI 细节见 [docs/ci-cd.md](docs/ci-cd.md)，文档与注释规范见 `AGENTS.md`。

## 分支模型

| 分支 | 角色 | 规则 |
|---|---|---|
| `main` | 发布分支 | 只接受 `dev` 的发版 PR（merge commit，另有 `release source is dev` 校验来源）；禁止日常直接 push；合并即触发 image CD（GHCR 镜像）与 web CD（Web 包） |
| `dev` | 默认分支、集成分支 | 日常改动的汇聚点；只接受 PR；每次 push 跑完整 CI（含镜像构建冒烟，不登录、不推送 GHCR），同时 image CD 推 `dev` 快照、web CD 上传 `dev` 的 Web 包 |
| `feature/*` | 单个改动的短期分支 | 从 `dev` 切出，PR 目标 `dev`，合并后可删 |
| `sync/main-to-dev` | 发版后的同步分支 | 从 `dev` 切出、合入 `main`，PR 目标 `dev`；保证 `dev` 含 `main` 的发布 merge commit |

`dev` 是仓库默认分支：克隆后默认在 `dev`，CI badge 也指向它。

## 开发流程

```bash
git switch dev && git pull --ff-only origin dev
git switch -c feature/<主题>
# 改动 + 本地校验
git add -A && git commit -m "feat(<scope>): <中文摘要>"
git push -u origin feature/<主题>
gh pr create --base dev --title "<中文标题>" --body "<变更说明 / 验证命令 / 风险>"
```

1. 分支从 `dev` 切出，基线始终是 `dev` 最新提交。
2. 提交信息用 Conventional Commits 类型前缀（`feat` / `fix` / `ci` / `docs` / `refactor` / `test` / `chore`），摘要用中文。
3. PR 目标 `dev`，模板要求写清变更说明、验证命令、目标分支与风险/回滚。
4. 等 `backend (fmt / clippy / test)`、`web (lint / typecheck / build / headless smoke)`、`scripts (bash -n / release tags)`、`image (linux/amd64 build / smoke)`、`dependency review` 全绿再合并；`dev` 受保护，只能 PR 合并。目标为 `main` 的 PR 还会多跑 `release source is dev`（来源必须是 `dev`）。
5. 分支落后目标分支时先 rebase/merge 到最新（保护规则要求分支最新）。

## 本地校验

改动提交前至少跑与 CI 对应的检查：

```bash
# 后端（fmt / clippy / 测试，与 CI backend job 一致）
cd backend
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cd ..

# 管理台（与 CI web job 一致）
cd admin-web
npm ci
npm run lint
npm run typecheck
npm run test:transport
npm run build
cd ..
bash scripts/smoke-headless-web.sh        # 构建产物能当静态站点起

# 脚本（与 CI scripts job 一致）
for f in scripts/*.sh; do bash -n "$f"; done
bash scripts/test-release-tags.sh

# 镜像（与 CI image job 一致；需要 docker）
docker buildx build --platform linux/amd64 -t jumcp:local --load .
bash scripts/smoke-image.sh jumcp:local
```

一条命令跑完静态检查 + 测试 + E2E：`scripts/test-all.sh`（Linux/macOS/CI）或 `pwsh scripts/test-all.ps1`（Windows）；`-SkipE2E` / `--skip-e2e` 只跑静态检查与测试。真实 ClickHouse 用例是 `#[ignore]`，需要时用 `-ClickHouseDsn` / `--clickhouse-dsn` 打开。

改了 workflow 或文档也要自检：YAML 能被解析、文档里的命令与检查名能和 workflow 对上。

## PR 要求

- 目标分支：日常 PR → `dev`；只有发版 PR → `main`。
- 标题中文、能自解释；描述用 `.github/pull_request_template.md`：变更说明、验证命令（贴实际跑过的）、目标分支确认、风险与回滚。
- 一个 PR 只做一件事；不夹带无关重构、格式化或依赖升级。
- 不提交 `.dev-data/`、`backend/data/`、`admin-web/node_modules`、`admin-web/dist`、截图或任何 token/secret。
- PR 会跑 `dependency review`：高危依赖变更会被阻断，引入依赖需说明理由。

## 发版步骤

发版就是「把 `dev` 整体合并到 `main`」，不做 cherry-pick、不单独重写提交。

**第 0 步：确认 `dev` 已包含 `main`。** `main` 用 merge commit 发版，每发一次就比 `dev` 多一个 merge commit；保护规则要求 PR 分支最新，不先同步的话发版 PR 会被「分支不是最新」永久挡住。

```bash
git fetch origin
git merge-base --is-ancestor origin/main origin/dev && echo "dev 已包含 main"
```

不包含时先做同步 PR（**不要**直接 push `dev`）：

```bash
git switch dev && git pull --ff-only origin dev
git switch -c sync/main-to-dev
git merge --no-ff origin/main -m "merge: 发版后把 main 同步回 dev"
git push -u origin sync/main-to-dev
gh pr create --base dev --head sync/main-to-dev --title "chore(sync): 发版后把 main 同步回 dev"
# 等 required checks 全绿后 merge
```

然后发版：

```bash
gh pr create --base main --head dev --title "release: <版本或主题>" \
  --body "## 变更
<本批改动摘要>

## 验证
- dev CI: <run URL>
- 发版 PR checks: <run URL>"

# 等 PR checks 全绿（含 release source is dev）后用 merge commit，不要 squash / rebase
gh pr merge <PR> --merge
```

合并后：

1. `main` push 自动触发 `image-cd.yml` 与 `web-cd.yml`：镜像产出 `ghcr.io/<owner>/jumcp` 的 `latest`、`main`、`sha-<短 sha>`，Web 包产出 `jumcp-admin-web-main.tar.gz` artifact。发版 PR 上 `release source is dev` 校验来源分支必须是 `dev`，`feature/*` 直接 PR 到 `main` 会被拒绝。
2. 确认两个 CD run 成功、镜像按 digest 回拉冒烟通过；`latest` 只由 `main` 产出，`dev` 的 push 只推 `dev` 快照（`dev`/`sha-<短 sha>`），对 feature 分支手动 `workflow_dispatch` 时两个 workflow 的构建 job 都直接跳过。
3. 需要版本 tag 时再打：`git tag v0.0.1.000 <merge-sha> && git push origin v0.0.1.000`，产出 `0.0.1.000` / `0.0.1` / `0.0`（不改 `latest`），同时把 `jumcp-admin-web-0.0.1.000.tar.gz` 挂到该 tag 的 Release；tag 必须是 `v` + 3 或 4 段数字，`v1.2.3` 产出 `1.2.3` / `1.2`。规则与复算方式见 `scripts/release-tags.sh`。tag 必须指向 `main` 历史内的提交——tag 触发会跑 `merge-base --is-ancestor`，打在 `dev` 提交上会失败退出。
4. 发版后立刻回到第 0 步，把 `main` 同步回 `dev`；`dev` 继续接收日常 PR，下一轮发版重复上面的流程。

## 禁止事项

- 不直接 push `main`（也不直接 push `dev`，日常改动一律走 PR）。
- 不使用 `git push --force` / `--force-with-lease` 改写 `main` 或 `dev` 历史（受保护分支会拒绝，也不要尝试绕过）。
- 发版 PR 不用 squash / rebase 合并，避免 `main` 与 `dev` 历史分叉。
- 不在 `dev` 上手工发布正式版：对 `dev` 手动 `workflow_dispatch` 只会得到 `dev` 快照（`dev`/`sha-<短 sha>`），对 feature 分支手动 dispatch 时两个 CD 的构建 job 会整体跳过；正式版只由 `main` 产出。
- 不在 `dev` 的提交上打 `v*` tag：image CD 会校验 tag 是否在 `main` 历史内，不满足直接失败；发布 tag 一律打在 `main` 的发布提交上。
- 发版合入 `main` 后不做 `main → dev` 同步就开下一轮发版：`strict` 保护会以「分支不是最新」挡住发版 PR。
- 不提交 token/secret，不在 workflow 里打印凭证；GHCR 登录只用 `GITHUB_TOKEN`。
- 不发未经 CI 验证的版本：required checks 未全绿不合并。
