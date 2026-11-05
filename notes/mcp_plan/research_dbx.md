# 参考实现：DBX（t8y2/dbx）的 MCP 设计

对象：`t8y2/dbx`（Rust + Tauri 桌面数据库客户端，Apache-2.0，约 2.9 万 star），MCP Server 为独立 Rust 二进制，支持桌面 / Web / Docker / CLI 四种后端，100+ 数据源（MySQL、PG、ClickHouse、MongoDB、Redis、Nacos、Consul、Kafka…）。
来源：[仓库](https://github.com/t8y2/dbx)；文档正文来自 `docs/content/docs/mcp.mdx`（经 GitHub contents API 读取：[mcp.mdx](https://api.github.com/repos/t8y2/dbx/contents/docs/content/docs/mcp.mdx)、[mcp-server README](https://api.github.com/repos/t8y2/dbx/contents/packages/mcp-server/README.md)）。本地 `blob` 链接被代理拒连，故改用 API 入口核验。

它和本项目是**同一类问题**（把数据库能力交给 Agent，且必须防越权），但它的取舍点不同：它替代的是"人用的客户端 + 顺带开个 MCP"，本项目是"给 Agent 的数据面"。因此下面只记可直接复用的机制。

## 可复用机制

| 机制 | DBX 做法 | 本计划落点 |
|---|---|---|
| 权限只做收敛 | 权威策略存于设置；生效权限取**最具体作用域**（库 → 连接 → 组 → 全局），但连接只读、生产保护、允许清单、数据源账号权限是**硬上限**，任何更具体设置都不能越过；`DBX_MCP_ALLOW_WRITES=1` 也"never widens the central policy" | §2 原则 10（新增）、§4.1.3 |
| 工具开关与权限分离 | Capabilities 决定"哪些工具可见"，Permission 决定"能做什么"；两者都在每次调用时服务端复核，客户端持有旧清单会被拒绝 | §4.4.1 双布尔（已具备） |
| 读入口不可绕过写确认 | `execute_query` 遇到写直接 `SALESFORCE_DML_REQUIRES_CONFIRMATION` | §3.3 唯一调用面（已具备） |
| 两阶段写 + 单次令牌 | `prepare_write` 返回摘要 + `confirm_token`（单次、5 分钟、绑定该条语句）；`apply_write` 时复查连接范围、允许清单、只读/生产保护与 DML 开关，撤销优先 | §4.4.6（新增） |
| 高风险调用即时确认 | 高风险 SQL / 破坏性 MongoDB / `full` 级工具调用前请求人工确认；批准不升权、不产生时间豁免，批准后复查权限 | §4.4.6 |
| 会话不放大策略 | 会话绑定单连接 + 单库；未知/过期 ID 直接失败（不静默回退）；空闲 30 分钟回收、并发上限 32；会话内每次请求仍复核写/DDL/生产保护 | §4.3.5 版本钉住（语义不同，但"不回退"一致） |
| 结果集按行夹紧 + 单格可续读 | `max_rows` 默认 100、上限 1000，超限**夹紧并回显**而非报错；`cell_char_limit` 默认 200（上限 4000）、`cell_char_offset` 支持滑动读取单个长值 | §4.3.2（新增参数）、§5.1 |
| 策略以整个请求为单位校验 | 批处理中只要出现一条写/DDL，在只读策略下整个调用失败，而不是逐语句放行 | §4.3.2 / §6.3（新增） |
| 超时后 outcome unknown | 超时/取消/传输失败/丢 COMMIT 确认 → 结果标记 `unknown`、永久禁止该会话继续执行、丢弃连接且**不重放** | §6.3（新增行） |
| 撤销后仍可清理 | 写权限被撤销后，rollback / close 仍可用 | §4.4.6 |
| 渐进发现的双档 | 插件工具 ≤40 个直接平铺，超过则只暴露 `plugin_list` / `plugin_tools` / `plugin_call` 三个元工具；也支持 `both`；旧配置保留原行为 | §5.2（新增阈值） |
| 子作用域与越界拒绝 | Nacos 命名空间、数据库范围作为子作用域；越界请求 `DATABASE_OUT_OF_SCOPE`；存在按库差异化权限时拒绝跨库 SQL | §4.3.1 / §4.3.3（新增 `allowed_schemas`） |
| 不暴露无法分类风险的能力 | Nacos/Consul 的集群、ACL、License 级操作明确不提供 | §4.4.5 禁止项 |

## 明确不采纳

- **状态化会话与显式事务**：本项目走模板 + 只读，模板路径无状态；开放会话状态等于开放 `USE`/`SET` 语义，与"只读 + 版本钉住"冲突。
- **写路径整体**：§1.3 已明确本期不注册 `risk = write|sensitive` 能力、不暴露 `prepare_call`/`apply_call`；§4.4.6 只是启用写路径时必须满足的契约（启用需重算 §5.1 常驻预算）。
- **桌面 UI 工具**（`open_table` / `execute_and_show`）：本项目无桌面端，且 UI 回写会成为第二条调用通道（§4.4.5 已禁止）。
- **多后端镜像**（桌面/Web/Docker 能力不同）：本服务是单一服务端，能力矩阵只按"数据源 + 客户端"两维展开。

## 差异点（不要照搬）

- DBX 的权限是**用户配置树**（组/连接/库），本项目是**租户策略 + 角色**；它的"最具体作用域"在这里对应"租户 cap ≤ 角色默认 ≤ 请求声明"，方向一致但落点不同。
- DBX 的 `max_rows` 夹紧是**产品默认**（宁可少给几行也不报错），本项目的 token 预算默认 `fail`；两者不冲突：行数上限可夹紧（可回显 effective），token 预算必须显式失败或显式摘要模式。
- DBX 的 `cell_char_*` 是针对"人工浏览大字段"设计的；本项目借用其**可续读**语义，但默认值应更小（200 字符即够定位），并把 `next_offset` 作为结果元信息必填项。
