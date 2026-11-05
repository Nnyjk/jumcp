# 功能点：数据库类 MCP Server 的既有形态

问题：模板注册表、数据源抽象、参数类型、组划分——这些是否已有成熟范式可直接对齐。

## 候选方案

| 方案 | 配置形态 | 关键能力 |
|---|---|---|
| Google MCP Toolbox for Databases | 单个 `tools.yaml`：`sources`（postgres / bigquery / cloud-sql-postgres…）+ `tools`（`kind: postgres-sql`、`parameters` 带类型、`statement` 用 `$1` 占位）+ `toolsets` 分组 | 声明式注册、多数据源统一、按工具的 OAuth identity passthrough（公共工具免 token）。[github.com/googleapis/mcp-toolbox](https://github.com/googleapis/mcp-toolbox)、[认证模型](https://mcp-toolbox.dev/dev/documentation/configuration/toolbox_mcp_auth) |
| Postgres MCP Pro | 运行参数（access mode）+ 连接串 | 只读事务 + SQL 解析防逃逸（见 `research_readonly_sql.md`）。[github.com/crystaldba/postgres-mcp](https://github.com/crystaldba/postgres-mcp) |
| Bytebase DBHub | `dbhub.toml`：DSN + 每连接 `readonly` / `max_rows`；工具极少（`execute_sql` 一类） | 极简工具面 + 单二进制多库。[github.com/bytebase/dbhub](https://github.com/bytebase/dbhub) |
| Supabase MCP | 连接 URL 参数：`read_only` / `features` / `project_ref` | 运行时投影与只读开关放在入口层。[supabase.com/docs/guides/ai-tools/mcp](https://supabase.com/docs/guides/ai-tools/mcp) |

## 契合度与取舍

- **高度同构（Toolbox）**：`sources`≈`executor`（数据源/连接，含只读账号）、`tools`≈`template`（命名参数化 SQL + 参数 schema + 描述）、`toolsets`≈`group`。本计划的模板注册表采用同一结构，字段命名尽量对齐，便于把现成 `tools.yaml` 迁移进来，也便于未来接 Toolbox 生态。
- **采纳（参数类型子集）**：Toolbox 的参数类型为 string / integer / float / boolean / array / map 这一级别；本计划 §4.3.3 在此之上加了 `enum`、`date`、`timestamp`、`column_ref`——`column_ref` 是只读白名单换来的安全能力，属于必要超集。
- **拒绝（identity passthrough）**：数据源以内部只读账号为主，用户身份不映射到数据库账号；授权在 MCP 层判定（§7），数据库层只有服务身份。理由：内部库通常没有逐用户 DB 账号，passthrough 会退化为虚假隔离。
- **拒绝（`execute_sql` 单工具形态）**：DBHub 证明"极简工具面"可行，但它把 SQL 组装责任交给模型，与本计划 §4.3.1 的只读与成本目标冲突；本计划保留 `db_query_readonly` 但以"模板为主、裸查为例外"分层约束。
- **借鉴（入口层只读开关）**：Supabase 的 `read_only` 参数与本计划"只读是部署属性、不是入参"一致——模型不能自己请求写权限。

## 接入 vs 自建（结论）

| 选项 | 内容 | 结论 |
|---|---|---|
| A. 直接部署 MCP Toolbox 作为数据库 MCP Server | 用它跑 `tools.yaml`，把 MCP 面直接给 Agent | **否决**。它自身就是一个 MCP Server（自带 `tools/list` 面），与本计划的目录分层与"每 kind 唯一调用面"冲突；只读是实例级开关，无法按租户/组投影；没有版本钉住、审批流、`admin_change` 审计、预算与结果上限；`tools.yaml` 改动即生效，缺少不可变版本语义（§4.3.5 无从实现） |
| B. 复用其声明结构与只读模型，自建执行器 | 本服务实现 sources/tools/toolsets + 三层只读 + 注解 | **采纳（阶段一）**。治理能力是主要缺口，而它恰好是 Toolbox 不提供的部分 |
| C. 本服务管治理与目录，执行下沉 Toolbox | 内部按工具名 + 参数调用其 SDK/CLI 接口 | **保留为阶段三可选项**。前置条件：需核验其非 MCP 调用入口（SDK / CLI）是否提供稳定、可版本化的调用契约；未核验前不选，否则会出现两个 `tools/list` 面 |

## Toolbox 的只读实现（比 Postgres MCP Pro 更完整，直接支撑 §4.3.1）

官方把"软锁"失效原因写得很清楚，与本计划的分层模型一致但更细：

- **提示词约束、正则/SQL 字符串校验不可靠**：`WITH d AS (DELETE FROM users RETURNING *) SELECT * FROM d` 以 `WITH` 开头、`SELECT` 结尾；`SELECT purge_inactive_records()` 看似只读，实际在函数内写入。
- **运行期 `SET default_transaction_read_only = on` 可被绕过**：`COMMIT; SET ... = off; DROP TABLE ...`；且在 PgBouncer 等池化场景会污染共享 socket，泄漏到其他服务的写请求。
- **三层防线**：① 引擎级会话锁（Cloud SQL/AlloyDB 用 DSN 参数 `*_session_read_only=locked`，不可被事务命令覆盖；MySQL 用 driver 级只读事务；BigQuery 用 `dryRun`）② 只读模式下**裁剪**写工具（不进入目录与工具组）③ 工具清单标注 `readOnlyHint: true`，让客户端免人工确认自动执行。

来源：[Read-Only Tools](https://mcp-toolbox.dev/documentation/configuration/security/read-only/)、[Tools](https://mcp-toolbox.dev/documentation/configuration/tools/)、[Groups](https://mcp-toolbox.dev/documentation/configuration/groups/)。

## 对本计划的具体约束

- 模板注册表文件结构：`sources`（连接 + 账号属性）、`tools`（模板 + 参数 + 组 + 风险）、`toolsets`（组 → 模板集合）三层，与 Toolbox 同构。
- 数据源只读账号是**部署配置**，不进入模板定义，也不进入工具入参。
- 多库差异集中在 `executor`（方言、占位符风格、超时单位），模板正文不写方言特例。
