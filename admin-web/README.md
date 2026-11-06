# HJZL MCP 管理台

后端 admin API 的薄前端（§4.4.5）。界面为只读联调版，写操作未接通：四个页面展示数据/空态/错误态，写操作按钮标注「未接通」。

## 启动

推荐用仓库根目录的一键启动脚本（会同时拉起后端并注入管理面凭证）：

```bash
pwsh scripts/dev.ps1      # Windows / PowerShell 7+（后端 + 本前端）
scripts/dev.sh            # Linux / macOS / CI
```

手动起前端时必须带上管理面 token，否则所有请求 401：

```bash
npm install
HJZL_DEV_ADMIN_TOKEN=tok_alice_000000000 npm run dev   # http://127.0.0.1:5173
npm run build    # 产物在 dist/
npm run typecheck
npm run lint
```

## 后端地址与鉴权配置

- `VITE_API_BASE`：后端 admin API 根地址，缺省空串（走同源相对路径）。例：`VITE_API_BASE=http://127.0.0.1:8000`
- 前端不读取也不发送任何管理面凭证：没有 `VITE_ADMIN_*` 变量，`Authorization` / `X-Admin-Actor` 均不由浏览器发出，操作者身份由后端按 token 派生。
- `HJZL_DEV_API_TARGET`：开发期 Vite 代理的后端地址，默认 `http://127.0.0.1:8000`。与 `HJZL_DEV_ADMIN_TOKEN` 一样只存在于 Node 进程环境，不是 `VITE_*` 变量、不会进浏览器产物；后端换端口时用它覆盖，例如 `HJZL_DEV_API_TARGET=http://127.0.0.1:8100 HJZL_DEV_ADMIN_TOKEN=... npm run dev`。
- 本地开发：Vite 代理把 `/admin`、`/health` 转发到 `HJZL_DEV_API_TARGET`（默认 `http://127.0.0.1:8000`）并注入 Bearer，token 只存在于 Node 进程环境 —— `HJZL_DEV_ADMIN_TOKEN=... npm run dev`（后端 token 由 `HJZL_ADMIN_TOKENS="alice:tok1,bob:tok2"` 配置）。**一键启动脚本已经代你设好这个变量**：`pwsh scripts/dev.ps1`（或 `scripts/dev.sh`）会同时拉起后端与前端并注入 `HJZL_DEV_ADMIN_TOKEN`，直接开 http://127.0.0.1:5173 即可；手动起则必须自己 export，否则请求全部 401。
- 生产部署：静态产物与后端同源部署，由网关/BFF 在 `/admin` 路径注入 `Authorization`（或改用会话 Cookie），前端不做任何凭证处理；缺凭证时后端返回 401。

## 边界（不可放宽）

- 不提供「以某租户/某 Agent 身份执行工具」的入口，也不提供工具调试器（排障走 MCP Inspector + 审计回放）。
- 审计视图只读：无编辑控件，不得导出后回写状态；UI 不直连数据库，变更须经 admin API 并产生 `admin_change` 审计。
