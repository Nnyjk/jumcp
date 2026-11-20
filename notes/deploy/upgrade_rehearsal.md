# 升级复演：0.0.1.003 → 0.0.1.004

本文件记录 0.0.1.003 → 0.0.1.004 升级复演中发现的坑与约束，部署与契约见 docs/。

## 管理面不再接受 Bearer（唯一破坏性变更）

- `/admin/*` 只认 `POST /admin/login` 换来的 HttpOnly cookie；`Authorization: Bearer` 不再被接受。
- 非 GET 请求还要求 `Origin` / `Referer` 同源，**缺头直接 403**（不是 401）。
- 用 Bearer 打管理面的脚本、巡检必须同时改两处：登录换 cookie + 补 Origin。只改一处会分别得到 401（无 cookie）或 403（有 cookie 无 Origin）。
- `/mcp` 不受影响，已有 MCP 令牌原样可用。

## 空库播种的硬前置

- `admin_users` 为空时必须提供 `JUMCP_ADMIN_USERNAME` / `JUMCP_ADMIN_PASSWORD`（口令 ≥12 位），缺任一拒绝启动。
- 旧 `JUMCP_ADMIN_TOKENS` 被忽略，但留在 service env 里不会报错 —— 现象上像"配置生效了"，排查时不能拿它当证据。

## 令牌白名单在旧版本上是"拒绝"而非"隐藏"

- 0.0.1.003 上 `POST /admin/tokens` 带 `db_run_template` 直接 400：`MCP_VALIDATION_ERROR`，`data.unknown: ["db_run_template"]`，`data.allowed` 只列 7 个内置名。
- 令牌没有 PATCH；`reissue` 只换明文不改配置 ⇒ 给新内置工具授权只能"升级后再建"，不能"先建后补"。

## 发布与可见性是两步

- `POST /admin/capabilities/{id}/publish` 成功后 `callable` 仍是 `false`。
- 必须再 `POST /admin/capabilities/{id}/visibility {"discoverable":true,"callable":true}`，否则 agent 侧 `tools/call` 得到 `MCP_CAPABILITY_NOT_FOUND`。
- 管理台 UI 一次点击背后是两个请求；脚本漏第二步的现象很像"发布失败"，不要去查发布链路。

## 模板执行臂不走上游

- `db_run_template` 读 policy 里的 `datasource`，镜像默认 `datasources: []`，此时恒报 `MCP_STORE_ERROR 未配置只读数据源：请在 policy 中声明 engine="clickhouse" 的 datasource`。
- 内置 clickhouse MCP 包（HTTP 上游）与模板执行是两条独立链路：装好包不代表模板能取数。

## `MCP_UPSTREAM_UNREACHABLE` 的触发面

- 该码只由上游 MCP 传输层抛出。
- 内置 clickhouse 包是进程内回环、端点可达，连不上 ClickHouse 时报的是连接器的 `MCP_STORE_ERROR`。
- 想复现 `MCP_UPSTREAM_UNREACHABLE` 必须让回环端点本身不可达。

## 审计 `surface` 列

- `ensure_surface_column` 自 v0.0.1.002 起才有；003 → 004 上这条路径不触发（列已存在），要验 ALTER 必须用 000/001 的库。
- 老行 `surface=""` 不重写、不重算哈希，`?surface=admin|mcp` 查不到它们，只在缺省/`all` 下可见 —— 告警规则不能写成"按面查不到就是丢了"。

## 本地复演环境坑

- Windows 上 `bash` 解析到 `C:\WINDOWS\system32\bash.exe`（WSL，无 `/d` 挂载），必须显式用 `/d/softs/Git/bin/bash.exe`。
- 宿主 curl 要 `--noproxy '*'`，否则走本机代理打内网 502/超时。
- 旧版 `JUMCP_ADMIN_TOKENS=admin:<hex>` 的 Bearer 只填**冒号后**的 token 段；填整串 `admin:<hex>` 会 401（比的是 token 段）。

## 被否决的决定

- "先建 10 工具版令牌、升级后再补 `db_run_template`"——否决。令牌不可改，补授权只能再轮换一次令牌，等于多一个 agent 切换窗口。
