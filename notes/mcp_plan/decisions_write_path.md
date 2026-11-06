# 写路径：行情取舍与被否决方案

记录写路径设计时对 MCP 生态既有写能力的调研结论、采纳/不采纳理由，以及被否决的方案。结论已上升为 `docs/plan/write-path.md` 与主文档 §4.4.6，本文件只保留理由与坑。

## 1. 调研来源与取舍

| 方案 / 事实 | 可核验来源 | 取舍 |
|---|---|---|
| Postgres MCP Pro 有 `--access-mode=restricted\|unrestricted`，**默认 unrestricted**（全量读写 + 任意 SQL） | https://github.com/crystaldba/postgres-mcp | **不采纳默认开写**；采纳"显式模式开关"，但默认值反转为 restricted/关闭 |
| Postgres MCP Pro 0.3.0 的 restricted 模式可被绕过（`RangeFunction` 节点未走函数名校验，可执行 `pg_read_file`） | CISA 周报 2026-08-31：https://www.cisa.gov/news-events/bulletins/sb26-250 | **约束**：AST 白名单（L8–L10）永远可能漏项，写面必须有独立的写账号权限（L7）作为强保证 |
| DBHub 在 `dbhub.toml` 用 tool 级 `readonly = true`（默认 false），只放行 SELECT/SHOW/DESCRIBE/EXPLAIN/PRAGMA，INSERT/UPDATE/DELETE 被拦；实现同时用关键字分类器与数据库只读 | https://dbhub.ai/config/toml、https://dbhub.ai/tools/execute-sql | **借鉴**"入口层只读天花板"；**不采纳**关键字分类器（`WITH ... DELETE` 一类可绕），执行层继续用 AST；**不采纳**工具级 readonly 作为唯一开关，改为五级开关 + 账号 |
| Supabase MCP 用 URL 参数 `read_only=true` 把全部查询降为只读用户，`features` 做工具组投影；危险 SQL 用客户端 elicitation 弹确认，可用 `skip_elicitations` 关闭；客户端不支持 elicitation 时**不会弹框**，权限与只读限制仍生效 | https://supabase.com/docs/guides/ai-tools/mcp、https://supabase.com/docs/guides/troubleshooting/sql-confirmations-do-not-appear-in-your-mcp-client-sQf7Kp | **采纳**部署级只读天花板与组投影；**不采纳**"确认交给客户端"——客户端可跳过、可无 UI，确认必须在服务端留下审批与意图记录。Supabase 没有 in-band `confirm` 参数；带 `confirm: true` 的入参是模型可伪造的自我确认，不构成控制 |
| MCP 规范 `annotations` 缺省：`readOnlyHint=false`、`destructiveHint=true`、`openWorldHint=true`、`idempotentHint=false` | https://modelcontextprotocol.io/specification/2025-06-18/server/tools | **采纳**"显式声明、不依赖缺省"；据此给写能力 `readOnlyHint=false`、UPDATE/DELETE `destructiveHint=true` |
| MCP 官方博客：注解是 hint，不是强制语义，客户端可忽略；它只应作为风险词汇的一部分 | https://blog.modelcontextprotocol.io/posts/2026-03-16-tool-annotations | **采纳**：注解只影响客户端体验，安全边界放在服务端开关 + 账号 + 审批 |
| `Idempotency-Key` 是 IETF httpapi 草案（已过期，未成 RFC），Stripe 普及：同键同参返回首次响应，重放带 `Idempotent-Replayed: true`，验证失败不入缓存 | https://datatracker.ietf.org/doc/html/draft-ietf-httpapi-idempotency-key-header、https://docs.stripe.com/api/idempotent_requests | **采纳**语义与"重放返回首次结果"；**不采纳**全局唯一键——按 `(tenant, actor, capability)` 作用域，避免跨租户/跨能力读到他人结果 |
| Transactional Outbox：本地事务内写 outbox 表，再由投递器异步外发，避免"写库成功、发消息失败" | https://microservices.io/patterns/data/transactional-outbox.html | **采纳**为"目标服务自己的事"；**不采纳**本服务内建 outbox（它不拥有业务库，建了也只是第二个不可靠队列） |
| DBX MCP 的 prepare/apply 单次确认令牌、`outcome: unknown` 禁止重放、`max_rows` 夹紧 | https://github.com/t8y2/dbx | **采纳**令牌单次 + 结果未知不重试；**不采纳**把 prepare/apply 做成两个工具（见 §2.1） |
| Google MCP Toolbox 只读模式"裁剪写工具，而不是注册后靠策略拒绝" | https://mcp-toolbox.dev/documentation/configuration/security/read-only/ | **采纳**：写开关关闭时写能力不进入目录与可见集 |
| OWASP MCP02（scope creep）/ MCP03（tool poisoning） | https://owasp.org/projects/mcp-top-10 | **采纳**权限只收敛、定义哈希钉住、变更重审；写能力定义变更不得沿用旧批准 |
| PostgreSQL `statement_timeout` / `lock_timeout` / `RETURNING`；`UPDATE ... LIMIT` 不是标准 PG 语法 | https://www.postgresql.org/docs/current/runtime-config-client.html、https://www.postgresql.org/docs/current/sql-update.html | **采纳**事务级超时 + RETURNING；**不采纳**用 `LIMIT` 截断写，行数上限改为提交前检查并回滚 |

## 2. 被否决的方案

### 2.1 新增 `db_run_write`（或 `db_execute`）万能写工具

否决理由：

- 与"每个 kind 有且仅有一个调用面"冲突：版本钉住、参数校验、审计、令牌、配额会在两个入口各写一遍，随后必然分叉。
- 常驻预算增加一个工具定义（100–270 tokens），收益只是省一次 `phase` 参数。
- 万能写工具等于把 SQL 组装权交给模型；模板的列白名单、行数上限、`operation` 校验全部失效。
- 静态客户端的工具数上限问题会重新出现。

替代：`db_run_template` / `invoke_capability` 增加 `phase`、`confirm_token`、`idempotency_key`。

### 2.2 写做成会话级、无逐次确认

否决理由：

- 会话开启后所有写都放行，撤销权限必须等下一次会话；`prepare` 的意图记录缺失，审计无法回答"这一次是谁批的"。
- 与 §4.4 的"注册审批"和"调用期确认"两层治理直接冲突：会话级开关只能回答"这个 Agent 能不能写"，不能回答"这一次要不要写"。
- Agent 的自循环（检索→写→再检索→再写）会放大误写，且没有可插入人工审批的位置。

替代：五级开关 + 逐次意图 + 一次性令牌；只有 staging 的低风险、主键定位、单行写允许策略自动批准。

### 2.3 跨系统两阶段提交 / 分布式事务

否决理由：

- 本服务不是事务协调者，目标系统（HTTP API、其他库）不支持 XA；引入协调器会带来悬挂事务与阻塞。
- 提交阶段失败时 2PC 的"未知"状态与 MCP 调用模型不兼容：Agent 无法持有事务句柄跨轮次。
- 云托管数据库普遍不支持跨实例 2PC。

替代：一个能力只写一个系统；跨系统一致性由目标服务本地事务 + outbox 承担；本服务只记录送达结果与错误码。

### 2.4 用客户端注解或 elicitation 做确认

否决理由：注解是 hint，客户端可忽略；不支持 elicitation 的客户端（含 stdio 与部分 HTTP 客户端）没有确认 UI；确认发生在模型与客户端之间，服务端无法审计"谁批的"。写路径的确认点是服务端意图 + 人工审批 + 令牌。

### 2.5 在 `db_query_readonly` 上加 `allow_write` 参数

否决理由：只读的唯一强保证是账号与连接级锁（L1/L2），入口参数是模型可控的；一旦参数能开关写，只读保证退化为提示词约束。写只能走独立入口 + 独立账号。

### 2.6 运行期 `SET transaction_read_only = off` 切换写

否决理由：可被 `COMMIT; SET ...; DROP ...` 链式绕过；在 PgBouncer 等池化场景会污染共享 socket，泄漏到其他服务的写请求（主文档 §4.3.1 已记录）。

### 2.7 `RETURNING` 结果先返回、后提交

否决理由：提交失败时调用方已拿到"成功"的行数据，无法回收；审计会记录一笔实际未落库的写。必须先 `COMMIT` 再组装响应。

### 2.8 行数超限时截断写入

否决理由：`UPDATE ... LIMIT` 非标准 SQL，子查询截断需要额外排序语义；"部分成功"无法被调用方或审计正确推理。改为提交前检查 + 整体回滚。

### 2.9 配额超限时排队等待

否决理由：排队会让审批时看到的行版本与执行时不一致，且 MCP 调用没有排队语义；写请求过期后应重新 prepare，而不是在队列里变旧。拒绝优先于排队。

### 2.10 审计只存参数哈希、意图表也不存参数

否决理由：审批人看不到"要改成什么"，只能盲批，四眼退化为盖章。改为：审计只存哈希与列名；意图表存短期加密参数，仅供审批视图解密，访问也写审计。

### 2.11 把 `idempotency_key` 当凭据

否决理由：它是模型生成的字符串，可猜测、可复用；只能用于去重，不能用于身份或授权。授权仍由角色、令牌、审批决定。

### 2.12 提交后审计失败按 fail-closed 处理（回滚 / 返回错误 / 标成未知）

否决理由：

- 数据已 `COMMIT`，回滚不存在；声称失败是在审计层撒谎。
- 把写结果标成未知（`MCP_WRITE_OUTCOME_UNKNOWN`）会让调用方与模型重试，而写已经发生，只会制造重复写。
- 返回顶层错误会掩盖"已成功"的事实，运维排障时无法区分"没写"和"写了但没记账"。

替代：返回 `ok:true` + `audit_pending:true`，把事件放进持久化对账队列；缺口通过 `/health` 与指标暴露；补录只补 apply/failed 侧，不伪造 intent。

### 2.13 审计链断链时拒绝启动 / 直接退出

否决理由：

- 已提交数据不可回滚，退出进程同时失去读面与对账能力，缺口会永久悬空。
- 断链是可修复状态，不是账号权限错误；启动自检的 fail-closed 语义只适用于"写账号配错"这类不能带着跑的状态。

替代：以 `audit_chain=broken` 启动，读可用、写关闭（`level=audit_broken`），修复并补录后人工恢复写开关。

### 2.14 读写共用一个启动自检

否决理由：两者期望相反——只读连接要求写失败，写连接要求 DML 成功且 DDL 失败。合并会导致任一账号配错时另一面被误判为正常；也不能用"写连接通过"来证明只读面安全。

替代：两套自检并列（只读 `pg_executor.rs:90-118`；写 W1–W7），任一套失败各自处置。

## 3. 仓库内的坑（改代码前先看）

| 坑 | 位置 | 后果 / 规避 |
|---|---|---|
| 所有工具共用一份 `readOnlyHint=true` 注解 | `backend/crates/hjzl-server/src/mcp.rs:16-17` | 写能力开启后注解说谎；必须按会话投影，能力级注解由 `describe_capability` 返回 |
| `readonly_dsn()` 往 DSN 追加只读锁参数 | `backend/crates/hjzl-server/src/pg_executor.rs:22-25` | 写池复用它会导致写全部失败；反过来"修掉"它会污染读池。读写池必须各自建 DSN |
| 只读执行器恒 `rollback`，从不提交 | `backend/crates/hjzl-server/src/pg_executor.rs:120-175` | 写执行器不能照抄该函数，否则返回 `affected_rows` 但数据未落库 |
| 模板注册硬拒 `risk != read` | `backend/crates/hjzl-core/src/template.rs:250-254` | 放开前必须先有写校验分支，否则写模板永远进不了库 |
| 模板渲染后仍用只读校验器 | `backend/crates/hjzl-core/src/template.rs:671-674` | 不能给 `validate()` 加"允许写"开关；新增 `validate_write`，读路径语义保持不变 |
| 审计事件类型是闭集 | `backend/crates/hjzl-core/src/audit.rs:12-21`、`audit.rs:111-116` | 不先登记 `write_*` 类型，第一次写审计就失败；顺序必须是"意图审计落盘 → 发令牌" |
| 四眼只比较字符串；admin 的 `approved_by` 以 token 派生为准 | `backend/crates/hjzl-core/src/registry.rs:242-250`、`backend/crates/hjzl-server/src/admin.rs:498-520` | 写审批必须复用派生身份，不能新增"请求体自述批准人" |
| `affected_rows` 方言语义不同 | PG 受触发器影响；MySQL 默认返回 changed rows（需 `CLIENT_FOUND_ROWS`）；ClickHouse 无事务 | 行数后检查与乐观锁只在 PG 上成立；方言扩面必须先补矩阵 |
| `UPDATE ... LIMIT` 在 PG 不存在 | PostgreSQL `UPDATE` 语法 | 不能靠 SQL 限行；用提交前检查 + 回滚 |
| 客户端不一定支持 elicitation | Supabase 文档明确：不支持时不弹确认，限制仍生效 | 确认不能依赖客户端；服务端审批是唯一确认点 |
| AST 校验有已知盲区 | Postgres MCP Pro 0.3.0 的 `RangeFunction` 绕过 | 写账号权限必须独立成立；黑名单只做纵深 |

## 4. 外部约束（不可协商）

- 云托管 PG 的会话只读锁是 `locked`（Cloud SQL `cloudsql_session_read_only`、AlloyDB `alloydb_session_read_only`），写连接不能带这些参数，因此写必须使用独立 DSN 或独立实例。
- MCP 注解缺省值对写不友好（`destructiveHint` 缺省为 true、`readOnlyHint` 缺省为 false），必须显式声明，避免客户端行为依赖实现缺省。
- `Idempotency-Key` 没有正式 RFC，语义按 Stripe 的事实标准实现，并在能力定义里写清窗口与重放行为。
- 跨系统一致性没有通用 2PC 可用；outbox 只能由拥有目标库的服务实现。
- `readonly=true` 只约束只读连接（DSN 锁与自检都围绕它），不能复用为"写开关"。写面独立由 `write_dsn_env` + `[datasources.write] enabled` 控制；把 `readonly` 读成"该数据源不可写"会让两个语义纠缠，任何一侧改动都可能反向破坏另一侧。
