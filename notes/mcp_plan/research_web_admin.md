# 功能点：Web 管理台与 MCP 服务的关系

问题：注册、审批、授权、预算、审计需要人（和 admin agent）操作，管理面是不是要自建一套完整前端。

## 候选方案

| 方案 | 形态 | 可借鉴点 |
|---|---|---|
| MCP Inspector（官方） | Web 调试 UI，transport-agnostic，Tab 分 resources / prompts / tools / notifications | 开发期验证 `tools/list` 与调用链路的现成工具，不需要自建调试界面。[modelcontextprotocol.io/docs/tools/debugging](https://modelcontextprotocol.io/docs/draft/tools/debugging) |
| IBM ContextForge | Admin UI + 同一套 REST API；虚拟服务器创建即 `POST /servers` 带 `associated_tools`；另有桌面客户端与 CLI | **UI 与 API 同源**：UI 只是 API 的一个客户端，不存在"只有 UI 能做"的操作。[ibm.github.io/mcp-context-forge](https://ibm.github.io/mcp-context-forge)、[管理端说明](https://heidloff.net/article/context-forge-client) |
| Supabase MCP | 不做管理台，用连接 URL 的 query 参数声明运行时投影：`read_only`、`features=<groups>`、`project_ref` | 声明式裁剪放在入口 URL，等价于本计划的 Header 通道，说明"入口即配置"是可接受的生态做法。[supabase.com/docs/guides/ai-tools/mcp](https://supabase.com/docs/guides/ai-tools/mcp) |
| OWASP MCP Top 10 | MCP02 = Privilege Escalation via Scope Creep | 管理面本身就是提权入口：谁能改 scope/组，谁就在改别人的能力边界。[owasp.org/projects/mcp-top-10](https://owasp.org/projects/mcp-top-10) |

## 契合度与取舍

- **采纳（ContextForge 的同源原则）**：管理台 = admin API 的薄前端，且 admin API 与 agent 侧 admin group **共用同一套策略判定与审计写入**。UI 不得直连数据库改状态。
- **采纳（Inspector 分工）**：开发期联调、回归验证用官方 Inspector；管理台只做治理动作（注册、审批、授权、预算、审计查询），不做工具调试台——否则会长成第二个调用通道。
- **借鉴（Supabase 声明式裁剪）**：管理台保存的"角色 → 组可见性 / 预算"配置，最终必须能被入口层解释成 Header/`_meta` 的等价约束，而不是只存在于 UI 里。
- **放弃（自建完整注册中心 UI）**：目录检索（search/describe/list_groups）已经给 agent 用，人只需要"审核 + 例外处理"两个视图；完整 CRUD 界面成本高、且与"注册走声明文件/CI"冲突。
- **放弃（UI 直连库）**：绕过 API 等于绕过策略与审计，直接违反 §4.4.3 的因果链要求。

## 对本计划的具体约束

- 管理台写操作必须产生 `admin_change` 审计事件，且与 agent 侧 admin 调用**同一事件结构**（§4.4.3）。
- 管理台不提供"以某租户/某 Agent 身份执行工具"的能力；排障一律走 Inspector + 只读审计回放。
- 审批与发布必须由不同主体完成（四眼），UI 上不允许同一账号既提交又批准。
- 审计视图只读，且不允许导出后回写任何状态（避免审计库变成配置源）。
