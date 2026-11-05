# 功能点：运行时动态编辑 MCP tool

问题：模板/工具定义需要持续新增与修改，运行时如何安全地"改工具"。

## 候选方案

| 方案 | 做法 | 风险/限制 |
|---|---|---|
| 静态注册（FastMCP `@mcp.tool`） | 启动期注册，定义随代码发布 | 变更需发版；对运营侧不友好。[gofastmcp.com/servers/tools](https://gofastmcp.com/servers/tools) |
| Dynamic FastMCP | 运行时按 user / tenant 计算描述甚至注册新工具，协议层 `list/tools` 动态生成 | 定义随调用者变化，审计与复现困难；同一 id 语义不稳定。[github.com/ragieai/dynamic-fastmcp](https://github.com/ragieai/dynamic-fastmcp) |
| 从 OpenAPI 运行时生成（`FastMCP.from_openapi`） | 读 spec 直接生成工具集 | 官方自述对复杂 API 手写 server 效果更好；不自动读 `securitySchemes`，认证需另接。[gofastmcp.com/integrations/openapi](https://gofastmcp.com/integrations/openapi) |
| 协议通知 | `notifications/tools/list_changed` + 客户端重拉 `tools/list` | 客户端支持度不一，部分实现只在连接时拉一次。 |
| OWASP MCP03 / 防 rug pull | 对完整定义（含 schema、annotations）做哈希钉住，定义变更必须重新评审；推荐签名、内容寻址、不可变版本化注册表 | 这是"允许动态编辑"的前提条件，不是可选项。[owasp.org MCP03](https://owasp.org/www-project-mcp-top-10/2025/MCP03-2025%E2%80%93Tool-Poisoning)、[OWASP MCP Cheat Sheet](https://cheatsheetseries.owasp.org/cheatsheets/MCP_Security_Cheat_Sheet.html) |

## 契合度与取舍

- **采纳（不可变版本 + 哈希钉住）**：动态编辑的唯一合法形式是"发布新 version"。已发布 `(template_id, version)` 的定义**永久只读**，`definition_hash` 随审计落库；会话内版本钉住（§4.3.5）因此天然阻断 rug pull——同一会话里定义不会中途变形。
- **采纳（OWASP 的评审触发条件）**：只要 `definition_hash` 变化（描述、参数、SQL 任一字节），就必须重新走审批；仅改 `discoverable`/`callable`/预算不触发重审，但必须记 `admin_change`。
- **放弃（按调用者生成定义）**：Dynamic FastMCP 那种"同 id 不同人不同定义"会破坏审计可复现性与缓存键语义；本计划改为"按组投影目录"——定义唯一，可见性不同。
- **放弃（依赖 `list_changed` 实现热更新）**：只能作为尽力通知；正确性依赖"下一次会话重新拉取"，不依赖客户端实时刷新。
- **放弃（运行时从 OpenAPI 自动生成工具）**：`endpoint` 类能力必须注册、审核、显式声明 base URL 与路径模板（§4.3.8）；自动生成会把未审核的接口面直接暴露给模型。

## 对本计划的具体约束

- 编辑即新版本：`published` 记录不可 UPDATE 定义字段，只允许 INSERT 新 version；旧 version 保留可查、不可执行（除已绑定的在途会话）。
- 下架只允许 `deprecated` + `successor_id`，不允许物理删除（审计与在途会话需要）。
- 定义哈希、审批人、生效时间三者同时落审计；缺任一项视为非法变更。
- 动态注册（含 admin agent 生成草案）只写目录，不自动授权——与 §4.4.1 一致。
