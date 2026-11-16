# 贡献指南

面向 `Nnyjk/jumcp` 的分支模型、开发流程与发版步骤。目录职责见 [docs/repository-structure.md](docs/repository-structure.md)，CI 细节见 [docs/ci-cd.md](docs/ci-cd.md)，文档与注释规范见 `AGENTS.md`。

## 分支模型

| 分支 | 角色 | 规则 |
|---|---|---|
| `main` | 发布分支 | 只接受 `dev` 的发版 PR（merge commit）；禁止日常直接 push；合并即触发 release 并产出 GHCR 镜像 |
| `dev` | 默认分支、集成分支 | 日常改动的汇聚点；只接受 PR；每次 push 跑完整 CI（含镜像构建冒烟），但不登录、不推送 GHCR |
| `feature/*` | 单个改动的短期分支 | 从 `dev` 切出，PR 目标 `dev`，合并后可删 |

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
4. 等 `backend (fmt / clippy / test)`、`web (lint / typecheck / build / headless smoke)`、`image (linux/amd64 build / smoke)`、`dependency review` 全绿再合并；`dev` 受保护，只能 PR 合并。
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
npm run build
cd ..
bash scripts/smoke-headless-web.sh        # 构建产物能当静态站点起

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

发版就是「把 `dev` 整体合并到 `main`」，不做 cherry-pick、不单独重写提交：

```bash
gh pr create --base main --head dev --title "release: <版本或主题>" \
  --body "## 变更
<本批改动摘要>

## 验证
- dev CI: <run URL>
- 发版 PR checks: <run URL>"

# 等 PR checks 全绿后，用 merge commit（不要 squash / rebase）
gh pr merge <PR> --merge
```

合并后：

1. `main` push 自动触发 `release.yml`，产出 `ghcr.io/<owner>/jumcp` 的 `latest`、`main`、`sha-<短 sha>`。
2. 确认 release run 成功、镜像按 digest 回拉冒烟通过；`latest` 只由 `main` 产出，`dev` 的任何活动都不产生镜像。
3. 需要语义化版本时再打 tag：`git tag vX.Y.Z <merge-sha> && git push origin vX.Y.Z`，产出 `X.Y.Z` / `X.Y`（不改 `latest`）。
4. 发版后 `dev` 继续接收日常 PR，下一次发版重复上面的流程。

## 禁止事项

- 不直接 push `main`（也不直接 push `dev`，日常改动一律走 PR）。
- 不使用 `git push --force` / `--force-with-lease` 改写 `main` 或 `dev` 历史（受保护分支会拒绝，也不要尝试绕过）。
- 发版 PR 不用 squash / rebase 合并，避免 `main` 与 `dev` 历史分叉。
- 不在 `dev` 上触发或手工运行 `release.yml`：`dev` 不产出镜像，手工发布只用于 `main` 或 tag。
- 不提交 token/secret，不在 workflow 里打印凭证；GHCR 登录只用 `GITHUB_TOKEN`。
- 不发未经 CI 验证的版本：required checks 未全绿不合并。
