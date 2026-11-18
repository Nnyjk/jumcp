# 管理台网关（admin-gateway）

浏览器打不开管理台的面板、所有请求都是 `HTTP 401 / MCP_UNAUTHORIZED`，这不是部署坏了，是设计：
管理面 `/admin/*` 只认 `Authorization: Bearer <admin token>`，而管理台是薄前端，
`admin-web/src/api/client.ts` 的 `buildInit` 明确**不发** `Authorization`（凭证只存在于网关与后端之间）。
开发期由 Vite 代理代填这个头（`admin-web/vite.config.ts` 的 `devAdminAuth`，读 `JUMCP_DEV_ADMIN_TOKEN`）；
生产环境没有这层代理，所以必须自己补一个**同源注入 Bearer 的反向代理** —— 就是本目录。

## 架构

```
浏览器 ──basic auth──▶ admin-gateway (nginx:1.27-alpine, :8080 → 宿主 52913, mode: host)
                          │  /admin/*   → 注入 Authorization: Bearer <admin token>
                          │  /          → 原样转发（管理台静态资源 + SPA 路由）
                          │  /health    → 仅容器回环（Swarm healthcheck）
                          └─ /mcp, /internal/* → 404（不经网关）
                                   ▼
                            jumcp:8000（同 stack，overlay 网络内按服务名解析）
```

- 只注入 `/admin/*`。`/` 与静态资源不注入任何凭证。
- MCP 客户端不走网关：仍直连 `jumcp:8000/mcp`，带自己的 agent token。
- 网关不缓存：`/admin/*` 响应带 `Cache-Control: no-store`（审计、令牌、工单都是实时数据）。

## 变量

| 变量 | 用途 | 谁提供 |
|---|---|---|
| `JUMCP_UPSTREAM_TOKEN` | 注入到 `/admin/*` 的 Bearer 值，**等于 `JUMCP_ADMIN_TOKENS` 里某一个 `id:token` 的 token 部分** | 部署者；不要新建令牌（管理面审计按 token 派生操作者身份） |
| `ADMIN_GATEWAY_USER` | basic auth 用户名 | 部署者 |
| `ADMIN_GATEWAY_PASSWORD` | basic auth 密码 | 部署者（建议 24 字节 hex） |

三者都在 Portainer 的 **stack 环境变量**里注入，编排里只留 `${...}` 引用：写进栈文件会把凭证落到
Portainer 的 stack 版本记录里，`Update/Rollback` 也会把它一起翻回去，无法单独轮换。

## 为什么配置内联在 `command` 里

Swarm 栈在节点上没有仓库检出，`bind mount` 仓库里的 `nginx.conf` 不可行；用 swarm `config` 对象又会
让网关配置漂移出 stack（Update/Rollback 不能一并覆盖）。所以 nginx 配置以单引号 heredoc 内联在
`command` 里，**`stack-snippet.yml` 就是它的唯一来源**，`portainer-stack.yml` 与
`docs/portainer-swarm.md` 只做指引。

改写注意：栈文件先做 compose 变量插值，单个 `$` 会被吃掉（`$host` → 空串）。因此
nginx 自己的变量写 `$$host` / `$$jumcp_upstream` / `$$request_uri`，shell 写 `$$(cmd)` / `$${VAR}`；
注入值用不含 `$` 的 `__JUMCP_UPSTREAM_TOKEN__` 占位符交给 `sed`。用
`docker stack config -c <file>` dry-run 校验（`version:` 键必须保留，Portainer 2.19 用 compose v1 解析器）。

## 能力与镜像

- 镜像 `nginx:1.27-alpine`；`cap_drop: [ALL]` + `cap_add: [CHOWN, SETGID, SETUID]`（实测最小集：
  缺 `CHOWN` 写 htpasswd 失败，缺 `SETGID`/`SETUID` worker 起不来；监听 8080，不需要 `NET_BIND_SERVICE`）。
- 镜像里没有 `openssl`/`htpasswd`，htpasswd 用 BusyBox `mkpasswd -m apr1 -P 0` 在容器启动时生成
  （`-P 0` 从 stdin 读密码，避免密码进 `ps` 命令行）。
- `resolver 127.0.0.11` + 变量形式 upstream：网关先于 jumcp 就绪也不会因为
  `host not found in upstream` 起不来。

## 访问

```
http://192.168.12.81:52913/          # 管理台（浏览器弹 basic auth）
http://192.168.12.81:52913/admin/*   # 管理面 API，网关注入 Bearer
http://192.168.12.81:52912/mcp       # MCP 直连，不经网关
```

网段白名单只放行 `10.0.0.0/8`、`172.16.0.0/12`、`192.168.0.0/16`，公网一律 403。

## 风险与边界

- 这是 **HTTP 明文**（同 stack 其它服务一致）：basic auth 凭证与 Bearer 都在明文链路上传播，
  只适用于内网。要公网暴露，请在更外层加 TLS（或直接把整栈搬到 TLS 反代后）。
- basic auth 是**共享口令**，不是按人身份；网关注入的 Bearer 决定审计里的操作者。
  多人使用应各自部署网关或改用能下发独立身份的反代。
- 网关上任何一个操作员都能用管理员身份调 `/admin/*`：控制器只做「内网可达 + 共享口令」两层，
  细粒度授权仍由 jumcp 的 `policy.toml` 角色决定（`/admin/me` 可查当前主体）。

## 轮换

1. 在 jumcp 里新增一个 admin token（管理台 `/admin` 令牌页，或直接改 `JUMCP_ADMIN_TOKENS`
   后 update stack），拿到 token 部分；
2. 在 Portainer 的 stack 环境变量里改 `JUMCP_UPSTREAM_TOKEN`（以及 `ADMIN_GATEWAY_PASSWORD`）；
3. **Update the stack** → 只有 `admin-gateway` 任务重建（`stop-first` 对网关无数据风险）；
4. 验证：`curl -sS -o /dev/null -w '%{http_code}' http://<node>:52913/admin/me` 无凭证 → 401；
   `curl -sS -u <user>:<pass> http://<node>:52913/admin/me` → `{"id":...,"role":"admin",...}`。
