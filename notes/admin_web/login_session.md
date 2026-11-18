# 管理台登录态：HttpOnly 会话 cookie

管理面从「网关/BFF 注入 Bearer」改为**自带登录页**：浏览器把管理员令牌 `POST /admin/login` 一次性交给后端，后端返回 `Set-Cookie: jumcp_session=<opaque>; HttpOnly; SameSite=Strict; Path=/; Max-Age=<ttl>`，此后所有 `/admin/*` 请求靠这个 cookie 认身份。前端不再有任何持有凭证的代码路径。

## 为什么是 HttpOnly cookie，而不是 localStorage

- **XSS 拿不走**：`HttpOnly` 的 cookie 对 `document.cookie` 不可见，页面里任何脚本都无法读出会话值再外带；`localStorage` 里的令牌对同源所有脚本可读，一次注入就等于长期失守。
- **没有「刷新即丢」的取舍**：`localStorage` 方案要么每次刷新重登，要么长期存令牌；cookie 由浏览器按 `Max-Age` 管理，前端代码不参与过期判定。
- **不是前端自己造会话**：`GET /admin/session` 是唯一身份来源，登录成功后 `AuthContext` 也会再探一次，而不是把登录响应里的字段当成事实。
- 令牌本身（用户输入的那串）只作为 `POST /admin/login` 的请求体瞬时存在：不写 `localStorage`/`sessionStorage`，不放进 URL，提交后立刻从表单 state 清掉。

## CSRF：SameSite=Strict + 后端 Origin 校验

Cookie 方案的经典风险是跨站请求伪造。两层兜底：

1. `SameSite=Strict`：跨站发起的请求（包括顶级导航、表单 POST、`fetch`）不会带上 `jumcp_session`，所以第三方页面无法借助用户已登录的浏览器完成管理操作。
2. 后端对管理面请求做 Origin/来源校验：即使将来为了某些场景放宽 SameSite，非同源来源也会被拒绝，而不是只依赖浏览器行为。

前端不做 CSRF token：管理面是独立端口（52913）同源提供静态产物与 API，没有需要跨源的合法调用方；加一套双提交 token 只会增加状态，收益被上面两层覆盖。

## HTTP 部署没有 Secure

内网部署走纯 HTTP（`http://<内网 IP>:52913`），所以 cookie **不能**带 `Secure`，否则浏览器不会在 http 下回传。这意味着：

- 同一网段的被动嗅探者可以看到 `jumcp_session` 明文（HTTP 无传输加密）；
- 因此后端把会话 TTL 压得较短，并且 `POST /admin/logout` 可随时吊销；
- 若后续对外暴露或跨不可信网络，必须在前面加 TLS 终止并给 cookie 补 `Secure`——前端代码无需改动（cookie 属性由后端下发）。

## 401 的统一处理

`src/api/client.ts` 在 `credentials: 'include'` 下发出所有请求；任一响应 401 会调用可注入的 `onUnauthorized`（`src/auth/AuthContext.tsx` 注册），把会话打成匿名态，再由 `App.tsx` 的闸门带着当前路径跳 `/login?from=<path>`。`client.ts` 因此不依赖 router。

`/admin/login`、`/admin/logout`、`/admin/session` 用 `skipUnauthorized` 跳过这个回调：它们的 401 是业务语义（令牌无效 / 本来就没登录），不是「会话在页面上过期」。

无 `?from=` 与 history `state.from` 的回跳规则集中在 `src/auth/redirect.ts`（`loginUrl` / `loginTarget`），登录页与闸门共用一份，避免两处各推导一遍。
