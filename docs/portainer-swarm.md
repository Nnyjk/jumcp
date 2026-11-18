# Portainer / Docker Swarm 部署

用 `deploy/portainer-stack.yml` 把 jumcp 部署成单副本 Swarm service：Portainer 的 **Stacks** 页面（Swarm 模式）直接用它当 stack 定义，命令行等价于 `docker stack deploy`。镜像、平台与运行时约定见 [ci-cd.md](ci-cd.md)「镜像」段。

## 前置条件

- 目标环境已 `docker swarm init`（单节点也可以：`docker swarm init --advertise-addr <内网 IP>`），部署节点是 **manager**；Portainer 已连到该 Swarm。
- 调度节点必须是 **linux/amd64**：镜像只发布 `linux/amd64`（没有 `arm64` manifest）。stack 用 placement constraints 限定，只有满足条件的节点会跑任务；全是 arm64 节点时会一直 `pending`，`docker service ps jumcp_jumcp` 会显示 `no suitable node`。
- 镜像可拉取：`ghcr.io/nnyjk/jumcp`。package 为 public 时无需登录；为 private 时在 Portainer 里注册 GHCR registry（用户名 + PAT，scope 至少 `read:packages`），并确保 stack 的镜像地址与凭据对应同一个 registry。
- `latest` 只由 `main` 的 push 产出（见 ci-cd.md 的 tag 规则）。生产建议固定版本 tag（如 `0.0.1.000`，或兼容的三段 `0.0.1`）或 digest，不要跟 `latest`，否则每次 `main` 合并后 redeploy 都会换版本。`dev` 快照 tag（`dev`、`sha-<短 sha>`）会被下一次 `dev` push 覆盖，只用于试跑，不要用于生产。

## 用 Portainer 部署

1. **Stacks → Add stack**，Name 填 `jumcp`（服务名会变成 `jumcp_jumcp`，卷 `jumcp_jumcp-data`）。
2. Build method 二选一：
   - **Web editor**：把 `deploy/portainer-stack.yml` 的内容粘进去；
   - **Repository**：连 Git，Compose path 填 `deploy/portainer-stack.yml`，分支填要部署的分支（生产用 `main` 的 tag，不要用 `dev`）。
3. 同一页面的 **Environment variables** 里填（Portainer 会把它作为 stack 的 interpolation 输入）：

   | 变量 | 必填 | 说明 |
   |---|---|---|
   | `JUMCP_ADMIN_TOKENS` | ✅ | 管理面凭证，`id:token,id:token` 形式，每个 token 至少 16 位（如 `admin:<openssl rand -hex 24>`）。未设置或为空时 `docker stack deploy` / Portainer 会在 interpolation 阶段直接失败 |
   | `JUMCP_AGENT_TOKENS` | — | `agent_id:role:token,agent_id:role:token`，**只在 `access_tokens` 表为空时**一次性导入（引导用）。表非空后不再生效，令牌请用管理面 `POST /admin/tokens` 生成 |
   | `JUMCP_IMAGE` | — | 覆盖镜像，默认 `ghcr.io/nnyjk/jumcp:latest`。生产填固定 tag 或 `@sha256:` digest |
   | `JUMCP_SECRET_KEY` | ✅ | 64 位 hex（`openssl rand -hex 32`）。内置 MCP 包密码字段的**落库**加密密钥；HTTP 模式缺失或不合法会直接拒绝启动。**轮换后旧密文不可解**，需在管理台重填密码 |
   | `JUMCP_TRANSPORT_KEY` | ✅ | 64 位 hex（`openssl rand -hex 32`）。**传输**加密密钥：管理台从 `GET /admin/transport-key` 取到它，在浏览器里加密 `secret` 字段的请求体；HTTP 模式缺失或不合法同样拒绝启动。与落库密钥用途分离，轮换后只需在管理台重填密码 |
   | `JUMCP_MCP_UPSTREAM_STDIO` | — | 默认 `1`（stack 里写作 `${JUMCP_MCP_UPSTREAM_STDIO:-1}`），只影响**手工接入的 stdio 上游**；内置包走内嵌 HTTP 端点，不需要它。不允许容器拉起子进程的部署可显式设 `0` |
   | `JUMCP_MCP_BUNDLED_BIN_DIR` | — | 可选。覆盖**手工 stdio 上游**可执行文件的查找目录。查找顺序：该目录 → `jumcp-server` 同目录（镜像里是 `/usr/local/bin`）→ `PATH`；目录里没有同名文件就继续回落，所以指向一个空目录不会破坏内置包 |
   | `JUMCP_PORT` | — | MCP 面对外发布的端口，默认 `8000`（ingress，任意节点该端口都可达） |
   | `JUMCP_ADMIN_PORT_PUBLISHED` | — | 管理面对外发布的端口，默认 `8080`（ingress）。浏览器访问 `http://<节点>:8080/` 即登录页；只在内网暴露 |
   | `JUMCP_ADMIN_SESSION_TTL_SECS` | — | 管理台会话 TTL，默认 `43200`（12 小时）。`POST /admin/login` 下发的 cookie `Max-Age` 取这个值，同时也写进 `admin_sessions.expires_at` |
   | `JUMCP_ADMIN_COOKIE_SECURE` | — | 默认 `0`。置 `1` 时会话 cookie 带 `Secure`（浏览器只在 HTTPS 回传）。经公网/TLS 暴露管理面时必须置 `1` |
   | `RUST_LOG` | — | 默认 `info` |

   **不要把 token 提交进仓库**：stack 文件里只有变量引用、没有任何默认凭证。Portainer 把这些变量随 stack 存在自己的数据库里，不要改成写死在 YAML 里。
4. **Deploy the stack**。首次启动等 healthcheck 转为 `healthy`（`start_period: 20s` + `interval: 30s`）。

## 访问与鉴权边界

进程在 HTTP 模式下监听两个端口，`deploy/portainer-stack.yml` 两个都发布：

| 面 | 端口（栈变量） | 挂载 | 谁访问 |
|---|---|---|---|
| MCP 面 | 容器 8000，发布 `${JUMCP_PORT:-8000}`（线上 52912） | `/mcp`、`/internal/mcp/*`、`/health` | agent（直连，无网关） |
| 管理面 | 容器 8080，发布 `${JUMCP_ADMIN_PORT_PUBLISHED:-8080}`（线上 52913） | `/admin/*`、`/admin/login`、`/health`、管理台静态站 | 浏览器 |

线上惯例是把两个栈变量分别设成 **52912（MCP 面）/ 52913（管理面）**：52913 只给浏览器（自带登录页），
52912 只给 agent。**不要**把两个发布端口设成同一个值——容器内两个监听本来就不同，发布重合只会让一面被遮蔽。

- 管理台 UI：`http://<任意 swarm 节点>:<JUMCP_ADMIN_PORT_PUBLISHED>`（默认 8080，线上 52913），首屏是**登录页**
  （`POST /admin/login` 换 HttpOnly 会话 cookie）。管理台不再依赖反向代理注入 Bearer——这是本次分面带来的最大变化。
- 健康检查：`GET /health` **两个面都有**；镜像与栈的 healthcheck 都各探一次，避免"另一面没监听"被判健康。
- MCP endpoint：`POST /mcp`，客户端带 `Authorization: Bearer <agent token>`，走 MCP 面发布端口（默认 8000，线上 52912）。
- 分面是硬边界：管理面访问 `/mcp`、`/internal/*` 返回 `404 MCP_NOT_FOUND`；MCP 面访问 `/admin/*` 与 `/`
  同样 404（MCP 面不托管任何页面）。不要为了"方便"把两个端口映射到同一个发布端口。
- 管理面是浏览器入口，默认 cookie **不带** `Secure`（内网 HTTP）。要经公网暴露，请前置 TLS 终止并设置
  `JUMCP_ADMIN_COOKIE_SECURE=1`，同时用防火墙/安全组限制 8080 的来源。Swarm `ingress` 会在每个节点上
  开放发布端口，只在内网可达需要额外收口，或改成 `mode: host` 并配合 placement 固定节点。

### 没有网关：浏览器直连管理面

`deploy/admin-gateway/` 已删除（分面前的产物）。它之所以存在，是因为旧的单端口模型下 `/admin/*` 只认
Bearer 而浏览器不发凭据：只能用 nginx 做 basic auth 外层并在 `/admin/*` 注入 `Authorization`。

现在管理面独立在 8080 且自带登录面，浏览器直接访问 `http://<任意 swarm 节点>:8080/`，登录后靠
HttpOnly 会话 cookie 认身份——**中间没有任何反向代理注入凭据**：

- 管理面入口的安全边界由端口可达性决定：只对内网暴露 8080，或前置一层 TLS 终止（并设
  `JUMCP_ADMIN_COOKIE_SECURE=1`），不要再用共享口令式的 basic auth 冒充操作者身份。
- MCP 客户端带各自 agent token 直连 MCP 面（8000），也不经任何中间层。
- 线上若仍有旧网关服务（把管理面的发布端口反代到 8000 并注入 Bearer），迁移时直接删掉该 stack/服务：
  把管理面发布端口指向容器 8080 即可。保留网关只会带来"网关注入的身份盖过用户会话身份"的双重身份问题。
- **本轮只改文档**：线上已有的网关与栈不动，迁移按本节说明单独执行。

开发期同理：Vite 代理只做纯转发——`/admin` 转管理面、`/mcp` 与 `/health` 转 MCP 面，
不注入任何 token（`JUMCP_DEV_ADMIN_TARGET` / `JUMCP_DEV_API_TARGET` 两个变量只指定目标地址）。

## 数据与升级

- `/data` 是命名卷 `jumcp-data`（Compose 项目下实际名 `jumcp_jumcp-data`），存 `registry.sqlite3` 与 `audit.sqlite3`。
- **命名卷是节点本地卷**（`driver: local`）：卷跟着节点走，不跟着任务走。任务被重调度到另一个节点时会挂上一个**新的空卷**，表现为治理数据与审计链「消失」。单节点部署没有这个问题；多节点部署请用 placement 把任务固定到持有数据的节点（`node.hostname == <node>`），或改用共享存储。
- 备份：库里是两个 SQLite 文件，**先停服务**（`docker service scale jumcp_jumcp=0`）再拷 `/data`，或用 `sqlite3 <db> ".backup '<目标>'"` 做一致性备份；直接 `cp` 正在被写的库可能拿到损坏副本。注意 `/data/registry.sqlite3` 里的上游 `env` 保存配置值：密码类字段是 `enc:v1:` 密文（依赖 `JUMCP_SECRET_KEY`），其余字段明文；脱敏只发生在 API 输出侧；备份副本与 `/data` 卷权限都要按敏感数据处理（加密存放、限定可读范围）。
- 内置 MCP 包（`clickhouse-basic`）：安装产物就是 `/data/registry.sqlite3` 里的一条上游记录（`catalog_id` / `catalog_version` 标记来源），**不需要新增卷**，随 `/data` 一起备份/迁移即可；包编译进 `jumcp-server`，没有随包二进制、也不起子进程。能否安装只取决于是否注入了 `JUMCP_SECRET_KEY` 与 `JUMCP_TRANSPORT_KEY`（缺任一把时进程不会以 HTTP 模式启动）。
- 若确实要把宿主上的新版**手工 stdio 上游**可执行文件挂进容器（不重建镜像）：只读 bind 到 `/opt/jumcp/bin`，并设置 `JUMCP_MCP_BUNDLED_BIN_DIR`。`docker run` 等价命令：

  ```bash
  docker run -d --name jumcp -p 127.0.0.1:8000:8000 -p 127.0.0.1:8080:8080 \
    -e JUMCP_ADMIN_TOKENS="admin:<至少 16 位的 token>" \
    -e JUMCP_SECRET_KEY="$(openssl rand -hex 32)" \
    -e JUMCP_TRANSPORT_KEY="$(openssl rand -hex 32)" \
    -e JUMCP_MCP_BUNDLED_BIN_DIR=/opt/jumcp/bin \
    -v /opt/jumcp/bin:/opt/jumcp/bin:ro \
    -v jumcp-data:/data \
    jumcp:local
  ```

  Stack 里的等价写法就是 `portainer-stack.yml` 中 volumes 下注释掉的那行 `/opt/jumcp/bin:/opt/jumcp/bin:ro`，外加 environment 里的 `JUMCP_MCP_BUNDLED_BIN_DIR: /opt/jumcp/bin`。查找顺序是该目录 → 服务可执行文件同目录（`/usr/local/bin`）→ `PATH`，目录里没有同名文件就继续回落，所以挂一个空目录不会破坏内置包。
- 换镜像后已安装的上游仍在（那行记录还在 `/data` 里）；旧版 stdio 安装在 HTTP 模式启动时自动迁移成内嵌 HTTP 上游（DSN 解析失败的行保留原样并 warn，需要重装）。不需要的上游用 `DELETE /admin/mcp/catalog/{id}/install/{upstream_id}` 卸载（只删该包装出来的上游）。
- 升级：Portainer 里改 `JUMCP_IMAGE` 后 **Update the stack**（勾选 re-pull image），或命令行 `docker service update --image ghcr.io/nnyjk/jumcp:<tag> jumcp_jumcp`。
- 回滚：Portainer 的 **Rollback the stack**，或 `docker service rollback jumcp_jumcp`。
- `update_config.order: stop-first`（回滚同样）意味着升级是**先停后起**：会有短暂停机，换来的是同一时刻只有一个进程写 SQLite。`failure_action: rollback` + `monitor: 30s` 让新任务起不来时自动回滚到上一个 spec。

## 多节点 / 高可用边界

- 当前存储是 SQLite 单写者：**不支持水平扩展**。不要 `--replicas >1`，也不要给多个 service 挂同一个 `/data`——并发写会破坏库。
- rollback/update 全部是 `stop-first`；多副本即使能起，也会因为卷共享或各写各的副本产生数据分裂。
- 需要真正的 HA 必须先改存储方案（外部数据库或单写者代理），属于存储层改造，不在本部署定义范围内。当前推荐形态：单节点 + 可靠备份 + 快速重建。

## 本地 / 命令行验证

只做配置解析（不改本机 Docker 状态、不需要 Swarm）：

```bash
JUMCP_ADMIN_TOKENS=ci-placeholder JUMCP_SECRET_KEY=$(printf '0%.0s' {1..64}) \
  JUMCP_TRANSPORT_KEY=$(printf '0%.0s' {1..64}) \
  docker stack config -c deploy/portainer-stack.yml
JUMCP_ADMIN_TOKENS=ci-placeholder JUMCP_SECRET_KEY=$(printf '0%.0s' {1..64}) \
  JUMCP_TRANSPORT_KEY=$(printf '0%.0s' {1..64}) \
  docker compose -f deploy/portainer-stack.yml config
```

两者都退出 0 即 YAML 合法、变量引用可解析。`docker compose config` 会提示 `version` 已废弃（Compose v2+ 的警告，可忽略；`docker stack` 仍按 3.8 解析）。

真正部署（需要已初始化的 Swarm manager）：

```bash
JUMCP_ADMIN_TOKENS="admin:$(openssl rand -hex 24)" \
  docker stack deploy -c deploy/portainer-stack.yml jumcp
docker service ps jumcp_jumcp          # 任务状态与调度失败原因
docker service logs -f jumcp_jumcp     # 日志
docker stack rm jumcp                  # 移除服务（命名卷保留）
```

CI 在 `image` job 里用同一份 `docker stack config` 校验该文件（见 ci-cd.md），因此写错键（例如 Swarm 不接受的 `platform`）会在构建前失败。
