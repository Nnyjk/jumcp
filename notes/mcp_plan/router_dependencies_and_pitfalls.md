# 上游 MCP router 的依赖取舍与实现坑

## 1. 为什么新增 `reqwest`

- 服务端只有 axum/`hyper` 的 server 侧，不能直接当 HTTP client 用；上游 MCP 需要自己的
  client（`initialize` → `notifications/initialized` → `tools/list` 翻页 → `tools/call`）。
- 选择 `reqwest = { default-features = false, features = ["json", "rustls-tls"] }`：
  - `rustls` 不引入 openssl，Windows/容器构建一致；
  - `json` 用于请求/响应编解码；
  - `Response::chunk()` 已经够读 SSE 响应体，因此**不启用** `stream` feature。
- 一处 client 覆盖全部上游：按上游 `timeout_ms` 逐请求设置超时，重定向用
  `Policy::custom` 限制同 host（跨 host 直接 stop），避免被上游重定向到内网。

## 2. 为什么没有加 `futures-util`

并行拉取多个上游与逐块读 SSE 这两个需求都能用已有依赖表达：

- 并行：`tokio::task::JoinSet`（`tokio` 已是既有依赖，且每个任务持 `Arc<UpstreamManager>`）；
- SSE：`reqwest::Response::chunk()` + 手写 `\n\n` 事件切分（只有 20 行，无通用 frame 解析需求）。

为两处调用点引入 `futures`/`futures-util`/`eventsource-stream` 只会增加依赖面，
按 AGENTS §5「纯粹为省几行代码引入的依赖不通过」否决。

## 3. 被否决的候选

| 候选 | 否决理由 |
|---|---|
| `hyper` + `hyper-util` 手写 client | 连接池、TLS、重定向、超时全部自建，错误面远大于收益 |
| `ureq` / 同步 client | 会把阻塞 IO 带进 async 请求链（要么 `spawn_blocking`，要么卡 reactor） |
| `reqwest` 默认 features（native-tls） | 多一套 openssl 构建依赖，与 rustls 无收益差 |
| `eventsource-stream` | 只需读取 JSON-RPC 响应，不需要完整 SSE 语义（重连/Last-Event-ID） |
| `mcp-sdk` / rmcp 作为 client | API 随版本漂移（本仓库手写 JSON-RPC 的既定理由），且只需 4 个方法 |

## 4. 实现坑（改代码前必读）

1. **`set_var` 在 edition 2024 是 unsafe**：测试里不要靠改进程环境变量来测 `env:NAME` 解析；
   改为断言引用原样透出，或用独立进程级夹具。
2. **`***` 是 PUT 的保留语义**：`GET` 对敏感 header/env 输出 `***`，`PUT` 收到 `***` 必须回填
   旧值。新建/新增键给 `***` 而无旧值 → 400；否则会把占位符写进库并让上游拿到字面量 `***`。
3. **上游工具必须计入常驻预算且视为常驻项**：否则一旦 managed 工具数超过平铺阈值、
   或预算走 `truncate`，router 的主能力会被静默裁掉。
4. **`tools/list` 的 SSE 解析要先去 `\r`**：先 `replace("\r\n", "\n")` 再找 `\n\n`，否则
   CRLF 事件永远不结束；忽略无法解析的 `data:` 行（上游可能混入日志事件）。
5. **stdio 上游的 stderr 必须丢弃**：`status_message` 会进 `GET /admin/upstreams`，上游把
   env 里的凭据打到 stderr 就会泄露；同时不读 stderr 也会因管道写满而卡死子进程。
6. **连接池按 `definition_hash()`（含 env/headers 值）失效**：只比较 `updated_at` 会漏掉
   "同一秒内改了 header"的情况；反过来，`updated_at` 变了但配置哈希没变（只改展示名）时
   不该断连接。
7. **命名冲突是整台上游拒绝**：局部丢弃会留下"半接入"的上游，且审计里无法解释为什么
   某个工具消失。冲突判定必须与"写入缓存"在同一把锁内——先并行拉取、再进临界区做
   "合法性 + 跨上游重名"检查，否则两台上游同时首拉会各自通过校验，随后被 id 去重静默吞掉。
8. **`tools/call` 的前缀回退不做工具存在性校验**：上游自己对未知工具报错，router 把
   `isError` 整形后返回；预校验反而会让"缓存旧列表"场景继续失败。
9. **`refresh` 必须连同工具缓存一起失效**：只丢连接会被 30s TTL 挡住，`refresh` 变成假探测。
   失败时保留旧工具并置 `degraded`，成功才替换工具集。
10. **暴露名不截断**：`<server_id>__<tool>` 超过 64 字符要整台拒绝。截断会让"旧名回退"
    拿截断后的名字去调上游，且两个长名可能截成同一个。server id 也禁止 `__`，否则
    `a__b__tool` 无法唯一反解。
11. **allowlist 同时约束上游**：token 的 `builtin_tools` 是名字白名单（内置 id 或
    `<server_id>__<tool>`），`tools/list`、发现面与 `tools/call` 三处共用
    `discovery::tool_allowed`；调用面拒绝要写 `policy_decision`。创建令牌的校验据此放宽为
    "内置 id 或符合上游暴露名形状"，因此白名单可以早于上游存在。
12. **冲突校验必须包含 managed 能力**：上游不写 `capabilities`，但注册表里可以有同名
    `capability_id`；只查 builtin 与其他上游会让 `merge_exposed` 静默去重。校验时把
    `store.all()` 的 id 一并纳入，命中即整台拒绝（读注册表失败按存储错误上抛）。
13. **allowlist 必须早于上游解析**：`tools/call` 先按 `matches_upstream`（只查 SQLite 前缀）
    判定"这个未授权名字确实属于某个可路由上游"，命中才拒绝；否则会为一次注定被拒的调用
    建立上游连接/拉取工具。
