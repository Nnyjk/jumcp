# MCP 计划：评审理由、被否决方案与外部约束

配套文档：`architecture_preview.md`（历史设计记录，只放结论与理由）。本文件保存结论背后的理由，避免同一议题反复提案。

## 外部约束（不可违反）

- MCP `tools/list` 是每轮快照，服务端无法在会话中途让模型"看见"新注入的工具定义；`notifications/tools/list_changed` 的客户端支持度差异极大，非 HTTP 传输（stdio）也没有 Header。
- JSON-RPC 的 `tools/list` / `tools/call` 没有独立的"越权"结果通道，只能用 error(-32602) + `data` 传递；模型无法据此自行恢复。
- PostgreSQL 没有运行时可强制的行扫描上限，也无法强制索引命中；`EXPLAIN` 只在实际执行时（`ANALYZE`）才真实，绑定参数后计划可能变化。ClickHouse 才有 `max_rows_to_read` / `max_bytes_to_read`。
- PG 的只读校验必须考虑可写 CTE（`WITH ... DELETE RETURNING`）、`FOR UPDATE`、`nextval`、`set_config`、`pg_read_file`、`dblink`、`EXPLAIN ANALYZE`。
- 工具定义与工具结果**都**常驻 transcript，因此预算必须按会话累计，而非按轮。

## 被否决的方案（勿再提）

| 候选 | 否决理由 |
|---|---|
| 越界 group 静默取交集 | 掩盖配置错误，且与"拒绝静默降级"目标直接冲突；intersect 只允许租户级显式开启 |
| 线性三态（Registered → Discoverable → Callable） | 表达不了"不可发现但可调用"（报表服务/定时任务直调），改为 `status` + `discoverable`/`callable` 双布尔 |
| 审计 admin 与普通调用物理分库 | 切断"变更 ↔ 权限判定 ↔ 执行"因果链；改为单追加表 + 哈希链 + 分类保留策略 |
| 每次执行前校验表结构存在性 | 系统目录开销不可接受；改为异步 schema 快照 + `suspect`/`deprecated` 流转 |
| 运行期每次调用做 EXPLAIN 预检 | 开销翻倍；发布期全量、运行期按指纹缓存 + 低采样 |
| 用 AST 校验作为只读保证 | 单条 SELECT 校验不足以覆盖可写 CTE 与危险函数；唯一强保证是只读账号 + 只读事务 |
| 预算溢出默认截断 | 截断即静默降级；默认 `fail`，`truncate` 需显式开启并返回 `dropped_groups` |
| 两套检索入口（`search_tools` + `db_find_templates`） | 语义重叠、模型需学两套、易长出第三个入口；合并为 `search_capabilities` |
| 只发现不定义调用面 | `kind=endpoint` 曾只有检索面，静态客户端拿到 schema 也调不到；新增 `invoke_capability` 并确立"每 kind 唯一调用面" |
| tool result 只设单轮上限 | 多轮即破峰值；改为"单次 1,500 且会话累计 2,500" |
| Jinja 条件块内插值 | 与"参数化预编译"冲突且是注入面；只允许三类结构变体，禁止值插值 |
| 自建完整注册中心 UI（全套 CRUD 前端） | 注册走声明文件 + 审核队列，人只需要审核与例外处理；完整 CRUD 会长成第二个调用通道且与 CI 化注册冲突（`research_web_admin.md`） |
| 管理台直连数据库改状态 | 绕过 admin API 即绕过策略与审计，切断 §4.4.3 的因果链 |
| 按调用者生成工具定义（Dynamic FastMCP 式） | 同一 id 对不同租户给出不同定义，破坏审计可复现性与缓存键；改为"定义唯一、可见性投影"（`research_dynamic_tool_editing.md`） |
| 数据源 identity passthrough（用户身份映射为 DB 账号） | 内部库无逐用户账号，passthrough 退化为虚假隔离；授权在 MCP 层判定（`research_db_tool_servers.md`） |
| 运行时从 OpenAPI 自动生成工具 | 未审核的接口面会直接暴露给模型；`endpoint` 类必须注册 + 审批 + 显式声明 base URL |
| 依赖 `list_changed` 实现热更新 | 客户端支持不一，只能作为尽力通知；正确性依赖下一次会话重新拉取 |
| 状态化会话 / 显式事务（DBX 式 `open_session` + `begin/commit`） | 模板路径必须无状态；开放会话状态等于开放 `USE`/`SET` 语义，与只读 + 版本钉住冲突（`research_dbx.md`） |
| 把桌面 UI 操作做成 MCP 工具（`open_table` 类） | 本服务无桌面端，UI 回写会成为第二条调用通道 |
| 用 `max_rows` 夹紧语义处理 token 超限 | 行数/单格字符是载荷整形，可夹紧回显；token 是预算约束，必须显式失败或摘要模式，混用即静默降级 |
| 业务数据写路径（agent 改接入数据源） | 与产品边界冲突：全部 tool 对数据源只读；agent 只能维护本项目的能力注册表（`decisions_write_scope.md`） |

## 评审处置记录

| 编号 | 问题 | 处置 |
|---|---|---|
| P0-1 | 解析流程相邻两步"取交集"与"越界显式失败"自相矛盾 | 先判越界、后裁剪；`X-MCP-Tool-Groups-Mode` 默认 `strict` |
| P0-2 | 渐进发现是 G1 的前提却排在风险表末行 | 前置为客户端能力矩阵；预算按客户端分口径；新增阶段〇实测 |
| P0-3 | 线性三态无法表达系统直调 | 双布尔 + 生命周期 |
| P0-4 | 检索面成为能力探测通道；越权错误不可恢复 | 检索前硬过滤、不可见=not-found、错误通道显式化 |
| P0-5 | `max_scan_rows` / `required_index_hit` 在 PG 无法强制 | 分层保证模型；字段改 `preflight.*`；附录给出数据源能力矩阵 |
| P1-1 | 预算未计工具结果 | 会话累计结果预算 |
| P1-2 | 分组索引随规模膨胀 | 索引只含组名+计数，组数 ≤20 |
| P1-3 | 双检索入口 | 统一 `search_capabilities` |
| P1-4 | 模板分支与参数化冲突 | 三类结构变体白名单 |
| P1-5 | 参数类型系统过薄 | 补长度/范围/枚举/时区/数组上限 |
| P1-6 | 版本解析未定义 | 会话内钉住版本 |
| P1-7 | 失败/降级路径缺失 | 失败与降级路径表 |
| P1-8 | 审计物理分离破坏因果链 | 单追加表 + 哈希链 |
| P1-9 | 验收指标不可测 | golden 集/基线/分母/窗口 |
| P1-10 | 运行期 EXPLAIN 开销 | 预检缓存 + 低采样 |
| P1-11 | 阶段顺序把最高风险放最前 | 阶段一限内测 + 审批最小版提前 |
| P1-12 | stdio 无 Header | 双通道（Header / `_meta` / profile） |
| P1-13 | `kind=endpoint` 无调用面 | 新增 `invoke_capability` + 调用面矩阵 |
| P1-14 | result 预算按轮计 | 改会话累计 |
| P2×9 | 文档卫生、预算算术、优先级字段、G4 拆段、能力抽象、结果缓存、readonly 语义、授权租约、结构漂移 | 已内化进文档 |

## 已作废的判断

- "计划 §6 整段重复粘贴"：误判。原文各标题仅出现一次，重复系读取窗口重叠（1–300 与 300–433）造成的假象。教训：读长文件分段时先核标题计数，再下"重复/缺失"结论。
