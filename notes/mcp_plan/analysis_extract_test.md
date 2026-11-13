# “分析提取 tool”机制验证（2026-11-10，本地 127.0.0.1:8000）

> 历史快照：本文记录的是改造前（审计 v1 载荷、发布请求体含 `approved_by`）的实测结果，保留原文不再更新。当前契约见 `docs/audit-openness.md`。

## 结论：仓库里没有叫“分析提取”的 tool

代码/前端/文档证据：

- MCP 常驻工具只有五个：`search_capabilities` / `describe_capability` / `list_groups` / `db_run_template` / `db_query_readonly`（`backend/crates/jumcp-server/src/mcp.rs:34-77`）。admin scope 追加四个：`admin_register_template` / `admin_review_queue` / `admin_audit_queries` / `admin_set_visibility`（同文件 `admin_tool_definitions`）。
- 全仓 `rg '分析提取|extract|analy'`（.rs/.ts/.tsx）无工具实现；只有 `json_extract` / “analyst” / “analytics” 等无关命中。
- 管理台导航只有：工作台 / 审核队列 / 工具（含统计+新建草案）/ 审计查询 / 例外处理 / 健康（`admin-web/src/App.tsx:21-67`），没有“分析提取”按钮。
- 文档里最接近的两条：
  - §4.2 平面 B 渐进发现：`search_capabilities` / `describe_capability` / `list_groups`（“分析工具目录、提取 schema”）。
  - §4.4.2 + §6.2 固化闭环：采集审计 → 归一化/指纹/聚类 → `admin_register_template` 落 draft → HTTP 审核发布（外部审批单号）；文档明确“查询固化不设独立工具”（`docs/plan/jumcp-preview.md:555`）。

因此用户所说“分析提取 tool”实际指 **A（检索三件套）+ B（审计→草案固化链路）**，二者都做了真实 HTTP 验证。

## A. 渐进发现三件套（实测通过）

- 身份：`Authorization: Bearer <agent token>`；`POST /mcp` JSON-RPC。
- analyst `tools/list` → 5 个工具、无完整 schema；admin → 9 个（多 4 个 admin_*）。
- 返回体含 `tools/groups/token_estimate/truncated/flattened/flattened_count`；`flattened=true, flattened_count=0`（阈值 40，注册表无可见 tool 能力）。
- `search_capabilities`：`query` 必填；大小写不敏感子串匹配 id/one_line/description；`kind`/`group` AND 过滤；`limit` 默认 5、上限 50（`min(50)`），`limit:0` 返回空（不报错）；结果只含 `id/kind/group/one_line/param_summary/token_cost`，无 schema。
- `describe_capability`：返回完整 `params_schema` + `schema_tokens`；每次按 token 计费；预算 2000 的 analyst 连续 11 次（11×178=1958）成功，第 12 次 `MCP_CONTEXT_BUDGET_EXHAUSTED`。
- 不可见/不存在同构：analyst describe `admin_audit_queries` → `MCP_CAPABILITY_NOT_FOUND`；describe `no_such_cap` → 同一错误；admin_audit_queries 对 analyst → `MCP_CAPABILITY_NOT_FOUND {tool:...}`。
- `list_groups`：analyst/admin 均返回 `[]`（无 managed 能力；builtin 被排除）。
- scope 请求头越界：analyst 带 `X-MCP-Tool-Groups: db.admin.schema` → JSON-RPC error `MCP_GROUP_FORBIDDEN` + `offending_groups`（strict 不裁剪）。

审计：`capability_search` payload `{query, returned_ids}`；`capability_describe` payload `{capability_id, schema_tokens}`。

## B. 审计查询 → 草案 → 审核（实测通过）

1. `admin_audit_queries`（admin agent token）
   - `limit` 夹紧 1..=200（传 999 → `limit:200`）；`offset` 生效；`filter` 白名单 `tenant/event_type/session_id/actor_role/ts_after/ts_before`，未知键 → `MCP_FILTER_UNSUPPORTED`。
   - 返回 `{events:[完整审计 envelope], total, limit, offset}`；`total` 精确；`actor_role` 过滤正常（analyst 43 / admin 39 / 全量 81）。
   - `tool_call` 中 `db_query_readonly` 的 payload 只有 `tool/sql_hash/row_count/truncated/elapsed_ms/tokens/summary/outcome`。
2. `admin_register_template`（admin agent token）
   - 合法定义 → `{id, status:"draft", version:1, definition_hash}`；同时写 `admin_change`（`before_hash:"none"`, `diff:"create"`, `approval_id:"draft:<actor>"`）。
   - 门禁：`DELETE` SQL → `MCP_TEMPLATE_INVALID/guard_rejected`；非点号 id → `template_id_format`；`risk=write`（HTTP 面）→ `risk_not_readonly`。
   - 注意：用 `db_query_readonly`（builtin）当 template_id 时，先被 `template_id_format` 拒绝，而不是 `MCP_BUILTIN_IMMUTABLE`；HTTP `POST /admin/capabilities` 同样输入才给 409 immutable。
3. `admin_review_queue` → draft 可见（`items[0].status="draft"`，字段 `id/kind/group/risk/status/version/updated/definition_hash`）。
4. `admin_set_visibility`：对 draft 可成功写 `discoverable=true`（`admin_change` 落审计，approval_id 派生 `visibility:<actor>`）；但可见谓词仍要求 `status=published`，analyst/admin 的 `search_capabilities` 仍查不到。之后已复位为 `discoverable=false`。
5. 发布校验（HTTP 管理面，alice/bob token）：
   - publish 缺 `approval_id` / 缺 `approved_by` → 400 `MCP_APPROVAL_REQUIRED`；
   - `approved_by` 与 token 派生身份不一致 → 400 `MCP_BAD_REQUEST`（`derived/declared` 回显）；
   - draft 直接 publish → 409 `MCP_STATE_TRANSITION_INVALID`；
   - submit（alice）→ review；alice 自批（approved_by=alice）→ 400 `MCP_APPROVAL_REQUIRED`（快照行为）；bob reject → 回 draft（reason 记录）。
   - MCP 无发布工具：`tools/call name=publish` → `-32602 Unknown tool`。
   - agent token 调 `/admin/*` → 401；无 token → 401。
6. 新建草案 UI 路径：`POST /admin/capabilities`，body `{definition}`。重复 `template_id` → **HTTP 500 `MCP_STORE_ERROR` “UNIQUE constraint failed”**，未映射为 409/幂等。

探针草案 `test.analysis.extract_probe` 已留在 `draft`、`discoverable=false`、`callable=false`；未发布任何模板，未 deprecate 任何模板；`/health` 仍 `audit_chain:"valid"`。

## 发现的缺口

1. **没有“分析提取”工具/页面**；它只是“检索三件套 + 审计→草案”的描述性叫法。若用户要一键分析/提取，产品上不存在。
2. **固化链路 ①→④ 断在数据上**：`tool_call` 审计只存 `sql_hash`，没有 SQL 原文、参数形状或 schema 信息；`/admin/tools/{id}/calls` 也只给 hash/时延/token。admin agent 无法仅凭审计重建模板定义，只能靠外部知识重写，与 §4.4.2 “采集 sql_hash + 参数形状”“归一化→指纹→聚类”不符。
3. **指纹/聚类/归一化未实现**：`fingerprint` 全仓仅出现在文档，代码零实现；也没有候选频次接口。`admin_audit_queries` 只能人工翻页，无 group-by sql_hash。
4. **草稿可见性可被提前打开**：`admin_set_visibility` 对 draft 不校验状态，能把 `discoverable=true` 写进 draft；一旦发布立刻可见，绕过 §6.2 的灰度步骤（当前只是不可见谓词兜底）。
5. **重复注册是 500**：`CapabilityStore::register` 直接 INSERT，主键冲突冒成 `MCP_STORE_ERROR`/500，缺少 §R12/D7 提到的幂等/去重口径（`(group, sql_fingerprint, params_shape)`）。
6. **`list_groups` 恒为空**：只统计 managed 能力且排除 builtin；在无模板的默认部署里，agent 无法从组索引得知 `db.read.analytics` 等可用组，只能靠 Header 试错（越界直接 403）。
7. **检索实现弱于契约**：§4.2.2 要求关键词+向量混合、§4.2.4 要求 5 分钟结果缓存；实际是大小写不敏感子串、无缓存。`limit=0` 静默返回空而非夹紧到 1。
8. **`truncated` 语义混用**：`/admin/tools/stats` 把“列表超过 limit”也置 `truncated=true`，与注释里的“扫描命中 50k 上限”不同源（实测 10 个工具、limit=5 即 true）。

## 可复现 curl（token 用环境变量，勿写入历史）

```bash
BASE=http://127.0.0.1:8000
AGENT=...   # analyst token
ADMIN_AGENT=...  # admin agent token
ALICE=...; BOB=...  # admin tokens

# A: 目录
curl -s $BASE/mcp -H "Authorization: Bearer $AGENT" -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}'
curl -s $BASE/mcp -H "Authorization: Bearer $AGENT" -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"search_capabilities","arguments":{"query":"","kind":"tool","limit":5}}}'
curl -s $BASE/mcp -H "Authorization: Bearer $AGENT" -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"describe_capability","arguments":{"capability_id":"db_query_readonly"}}}'
curl -s $BASE/mcp -H "Authorization: Bearer $AGENT" -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"describe_capability","arguments":{"capability_id":"admin_audit_queries"}}}'
curl -s $BASE/mcp -H "Authorization: Bearer $AGENT" -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"list_groups","arguments":{}}}'

# B: 审计 → 草案 → 队列
curl -s $BASE/mcp -H "Authorization: Bearer $ADMIN_AGENT" -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"admin_audit_queries","arguments":{"filter":{"event_type":"tool_call"},"limit":2}}}'
curl -s $BASE/mcp -H "Authorization: Bearer $ADMIN_AGENT" -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"admin_register_template","arguments":{"definition":{"template_id":"probe.demo.tpl","group":"db.read.analytics","risk":"read","sql":"SELECT 1 AS probe","params":[]}}}}'
curl -s $BASE/mcp -H "Authorization: Bearer $ADMIN_AGENT" -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"admin_review_queue","arguments":{}}}'

# 发布校验（只对自有 draft，不发布）
curl -s -X POST $BASE/admin/capabilities/probe.demo.tpl/publish -H "Authorization: Bearer $ALICE" \
  -H 'Content-Type: application/json' -d '{}'
curl -s -X POST $BASE/admin/capabilities/probe.demo.tpl/submit -H "Authorization: Bearer $ALICE"
curl -s -X POST $BASE/admin/capabilities/probe.demo.tpl/publish -H "Authorization: Bearer $ALICE" \
  -H 'Content-Type: application/json' -d '{"approval_id":"apr-1","approved_by":"alice"}'
curl -s -X POST $BASE/admin/capabilities/probe.demo.tpl/reject -H "Authorization: Bearer $BOB" \
  -H 'Content-Type: application/json' -d '{"reason":"cleanup"}'
```
