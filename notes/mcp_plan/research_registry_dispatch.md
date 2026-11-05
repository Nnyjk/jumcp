# 功能点：注册表分发（少工具 + 服务端路由）

问题：固定查询路径数以百千计，不能一一注册为 tool。

## 候选方案

| 方案 | 做法 | 关键点 |
|---|---|---|
| Cloudflare Code Mode（MCP 变体） | 服务端只暴露**一个** `code` 工具，描述里内联上游全部工具的 TypeScript 定义，模型写代码调用，代码在 isolate 内执行 | 官方口径"整个 API 放进 1,000 token"。[blog.cloudflare.com/code-mode-mcp](https://blog.cloudflare.com/code-mode-mcp) |
| MCP 官方 Registry + `server.json` | 服务发现层标准化：`server.json` 描述名称、包/远端地址、启动参数、capabilities | 社区驱动注册表，Anthropic/GitHub/Microsoft 等参与。[modelcontextprotocol.io/registry/about](https://modelcontextprotocol.io/registry/about) |
| IBM ContextForge（MCP Gateway） | 虚拟服务器（virtual server）只打包某用户/团队该见的工具；资源可见性 public/team/private；RBAC 如 `tools.execute` 针对可见资源判定 | 网关层做目录裁剪与授权。[ibm.github.io/mcp-context-forge](https://ibm.github.io/mcp-context-forge) |
| Docker MCP Gateway interceptors | 拦截器在工具调用前后检查/改写/阻断，按工具参数与时间做策略 | 策略上下文**不含调用方客户端属性**，无内建 per-client 过滤；per-client 需自行在拦截器内实现。[ajeetraina.com 解析](https://www.ajeetraina.com/a-quick-look-at-docker-mcp-gateway-interceptors) |

## 契合度与取舍

- **采纳（Code Mode 的同构思想）**：`db_run_template` / `invoke_capability` 就是本计划版本的"单入口 + 服务端分发"，区别是**不让模型执行代码**，而是传 `template_id`/`capability_id` + 结构化参数。理由：模型产代码需要沙箱与执行工具，本服务只做数据面，且代码执行会把 SQL 拼装责任转给模型，与 §4.3.1 的只读保证冲突。
- **采纳（ContextForge 的虚拟服务器）**：对应 §4.1 的 group 裁剪——同一份注册表，按调用方投影出不同目录；差异是 ContextForge 按资源可见性 + RBAC 在网关判定，本计划把"可见性裁剪（Header）"与"授权（policy 层）"显式分离（§2.1）。
- **采纳（Registry 的元数据格式）**：`template` / `endpoint` 注册项的字段设计（id、group、描述、参数 schema、版本、可发现/可调用）对齐 `server.json` 的"名称 + 位置 + 能力声明"结构，便于后续向官方 Registry 发布或对接。
- **反证（Docker Gateway 的缺口）**：其策略上下文没有调用方客户端字段，说明"客户端维度裁剪"在网关层天然缺失，必须在入口层（HTTP Header / `_meta`）传入——佐证 §4.1.1 双通道设计与其对 stdio 的兜底必要。

## 对本计划的具体约束

- 分发入口的入参必须是**标识 + 结构化参数**，禁止"传 SQL / 传 URL / 传命令"式入口（避免后退成通用执行器）。
- 注册表项必须与目录投影共用同一份元数据，禁止目录与执行两侧各维护一套（否则 ContextForge 类"工具丢失"问题会在本地复现）。
