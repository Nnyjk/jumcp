# HJZL MCP 管理台（骨架）

后端 admin API 的薄前端（§4.4.5）。当前仅只读视图：四个页面展示数据/空态/错误态，写操作按钮标注「未接通」。

## 启动

```bash
npm install
npm run dev      # http://127.0.0.1:5173
npm run build    # 产物在 dist/
npm run typecheck
npm run lint
```

## 后端地址与鉴权配置

- `VITE_API_BASE`：后端 admin API 根地址，缺省空串（走同源相对路径）。例：`VITE_API_BASE=http://127.0.0.1:8000`
- `VITE_ADMIN_TOKEN`：管理面 Bearer token（对应后端 `HJZL_ADMIN_TOKEN`），非空时所有请求带 `Authorization: Bearer`。
- `VITE_ADMIN_ACTOR`：管理操作者标识（对应后端 `X-Admin-Actor`），仅写操作使用；只读请求不带该头。
- 开发期也可用 Vite 代理：`vite.config.ts` 已把 `/admin`、`/health` 代理到 `http://127.0.0.1:8000`，此时无需设 `VITE_API_BASE`；后端需以 streamable-http 模式启动（`HJZL_MCP_TRANSPORT=streamable-http`），`/admin/*` 缺 Bearer token 返回 401。

## 边界（不可放宽）

- 不提供「以某租户/某 Agent 身份执行工具」的入口，也不提供工具调试器（排障走 MCP Inspector + 审计回放）。
- 审计视图只读：无编辑控件，不得导出后回写状态；UI 不直连数据库，变更须经 admin API 并产生 `admin_change` 审计。
