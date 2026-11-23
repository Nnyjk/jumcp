# Vite dev 代理：cookie 身份与 `/admin` 前缀

## 现象

本地 `npm run dev` 时，管理台能加载，但登录 POST 返回 403（`MCP_FORBIDDEN`），或直接访问 `/admins` 显示后端 404。

## 根因

1. **同源校验看的是 Origin 与 Host**：管理面把 cookie 身份的非 GET 请求当作 CSRF 风险，要求 `Origin`/`Referer` 的 host 与 `Host` 一致。代理若开 `changeOrigin: true`，后端收到的 `Host` 是 `127.0.0.1:8080`，而浏览器带的 `Origin` 是 `http://127.0.0.1:5173`，两者不一致 → 403。登录、注销、所有写操作都会中招，读接口不受影响。
2. **代理 key 是字面量前缀**：`'/admin'` 会把 SPA 路由 `/admins`（账户管理页）也代理到后端，后端没有该路由 → 404。管理台的 `/admin/*` 才是后端 API。

## 规避

`admin-web/vite.config.ts` 的 `/admin` 代理：

- `changeOrigin: false`（保持 Host 为 5173，与浏览器 Origin 一致）；
- key 用正则 `'^/admin(?:/|$)'`，只代理 API 路径，`/admins` 留给 SPA 回退。

验证：`curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:5173/admins` 应为 200；`/admin/session` 未登录应为 401；登录 POST 应为 204。

## 边界

这套代理只用于本地开发。线上由 `jumcp-server` 同源提供静态产物与 `/admin/*`，不存在跨端口 Origin 问题，也不要把 `changeOrigin: false` 当成生产配置搬走。
