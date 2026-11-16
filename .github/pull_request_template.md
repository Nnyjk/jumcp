## 变更说明

<!-- 做了什么、为什么；关联的 issue/讨论 -->

## 验证

<!-- 贴实际跑过的命令与结果；只写跑过的 -->
- [ ] `cd backend && cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`
- [ ] `cd admin-web && npm ci && npm run lint && npm run typecheck && npm run build`
- [ ] `bash scripts/smoke-headless-web.sh`
- [ ] `bash scripts/smoke-image.sh jumcp:local`（改了镜像/运行时行为时）

## 目标分支

- [ ] 日常改动 → `dev`（`feature/*` → `dev`）
- [ ] 发版 → `main`（`dev` → `main`，merge commit）

## 风险与回滚

<!-- 影响面、兼容性、需要同步的配置/迁移；出问题怎么回滚 -->
