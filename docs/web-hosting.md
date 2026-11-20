# 管理台静态托管（同源无头 Web）

后端在 HTTP 模式下可以把管理台的构建产物（`admin-web/dist`）当作静态资源直接提供，
让管理台页面与管理 API、`/mcp`、`/health` 同源，不需要额外的 Nginx。

## 环境变量

| 变量 | 默认 | 语义 |
| --- | --- | --- |
| `JUMCP_MCP_WEB_DIR` | 空 | 空 = 不托管静态资源，行为与未引入该能力时完全一致。 |

只在 `JUMCP_MCP_TRANSPORT=streamable-http` 时挂载路由。stdio 模式没有 HTTP 面，不会挂载任何静态路由，
但 `JUMCP_MCP_WEB_DIR` 仍参与启动校验（校验规则不分传输，避免同一份 env 在两种传输下行为不一致）。

构建产物来自管理台构建：`cd admin-web && npm run build`（产物目录 `admin-web/dist`）。

## 启动校验

`JUMCP_MCP_WEB_DIR` 非空时，启动期（`Settings::from_env` → `Settings::validate` → `validate_web_dir`）必须满足：

1. 路径存在且是目录，否则进程非零退出，错误形如 `JUMCP_MCP_WEB_DIR 不是目录: <path>`；
2. 目录内含 `index.html`，否则非零退出，错误形如
   `JUMCP_MCP_WEB_DIR 缺少 index.html（管理台未构建或目录错误）: <path>`。

校验失败即拒绝启动，不会静默降级成运行期 404。挂载静态路由时（`jumcp_server::web::attach`）再校验一次同样的条件。

## 路由优先级

请求先经过后端路由，只有**后端没有匹配**的路径才进入静态处理：

| 路径 | 处理方 |
| --- | --- |
| `/admin`、`/admin/*` | 管理 API（要求 `POST /admin/login` 下发的会话 cookie） |
| `/mcp` | MCP 接入（POST） |
| `/internal/mcp/*` | 内置 MCP 内嵌端点（POST，只接受进程内 Bearer token；不对静态资源开放） |
| `GET`/`HEAD` `/health` 且 `Accept` 含 `text/html` | 同源静态站的 `index.html`（浏览器导航，`Cache-Control: no-cache`） |
| 其他 `/health`（探针、curl、XHR） | 后端健康检查 JSON |
| `/health/*` | 后端路径空间：JSON 404 |
| `GET /`、`GET /index.html` | `index.html` |
| 不带扩展名的其他 GET 路径 | SPA 深链，回退到 `index.html` |
| 带扩展名的其他路径 | 对应静态文件；文件不存在即 404 |

两条硬边界：

- 未知的 API 路径（如 `GET /admin/unknown`）返回 `404` + JSON
  `{"error":{"code":"MCP_NOT_FOUND","message":"未找到该路径","data":{"path":"..."}}}`，
  **不会**回退成 `index.html`。否则路径拼错的调用方会拿到 `200 text/html`。
- 带扩展名但缺失的静态文件（如 `/assets/missing.js`）返回 404，同样不回退成 `index.html`，
  避免浏览器把 HTML 当脚本执行、把真正的 404 掩盖成语法错误。
- 非 `GET`/`HEAD` 的静态请求返回 `405`（响应头 `Allow: GET, HEAD`）。

前端路由不占用 `/health`。启用静态托管时，`GET`/`HEAD /health` 且 `Accept` 含 `text/html`
（浏览器地址栏导航、刷新）返回静态站的 `index.html` 外壳；不带 `text/html` 的请求
（监控探针、`curl`、前端 XHR）仍返回后端 JSON。
这与开发期 Vite 代理的 `bypass` 规则完全一致，dev 与生产同源行为相同。
拦截只作用于精确路径 `/health`，`/health/*` 与 `POST /health` 不受影响。

两条分支都有容器级回归：`scripts/smoke-image.sh` 在镜像里分别断言「无 `Accept` / `Accept: application/json` → 200 `application/json` 且 `audit_chain == "valid"`」与「`Accept: text/html` → 200 `text/html` 且含 `id="root"`」，既保证浏览器导航能拿到静态站，也保证监控探针不会被前端劫持。

## 静态文件与缓存

- 路径解析由 `tower-http` 的 `ServeDir` / `ServeFile` 完成，后端不手写路径拼接，
  `..`、编码后的 `%2e%2e` 等路径穿越由上游拒绝，读不到托管目录之外的文件。
- `/assets/` 下是 Vite 生成的内容哈希产物：`Cache-Control: public, max-age=31536000, immutable`。
- 其余静态文件（含 `index.html` 与 SPA 深链回退）：`Cache-Control: no-cache`，
  每次回源校验，部署新版本后不会继续跑旧入口。

## 容器内的目录

镜像把 `admin-web/dist` 的内容放到 `/app/web`，并设置 `JUMCP_MCP_WEB_DIR=/app/web`
（构建阶段产物 `cp -R dist/. /app/web/`）。镜像只包含这一份静态产物，不做运行期构建。

## 鉴权边界

- 静态资源免鉴权，不含任何业务数据；静态站挂在**管理面**端口（`JUMCP_ADMIN_PORT`，默认 8080）。
- `/admin/*` 只认 `POST /admin/login` 下发的 HttpOnly 会话 cookie：登录提交 `admin_users` 里的
  用户名口令，成功后 cookie 优先于一切、无效即 401（不回落 Bearer）。脚本与浏览器走同一条链路。
- 后端**不**向静态响应注入 `Authorization`，也不下发任何凭证：管理台前端不持有凭证，
  用户只在登录页里把用户名口令交一次，此后由同源请求自动带 cookie。**没有网关/BFF 参与**，
  开发期同理（Vite 只做纯转发，`JUMCP_DEV_ADMIN_TARGET` 指管理面端口）。
