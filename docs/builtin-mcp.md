# 内置 MCP 插件标准

内置 MCP 是**编译进 `jumcp-server` 的 MCP server 实现**：没有独立二进制、没有子进程、没有 stdio
行循环。每个插件声明自己的配置字段与工具清单，安装后表现为一台指向本进程内嵌端点的普通 HTTP
上游（`POST /internal/mcp/{package_id}/{upstream_id}`）。

```
admin 安装 → mcp_upstreams 一行（transport=http） → /admin/upstreams 可见
                                                   ↓
agent tools/list / tools/call → 上游客户端 → HTTP（Bearer internal token）
                                            → 内嵌端点 → 插件 call()
```

## 1. trait 契约

`jumcp-core::builtin` 只依赖 `serde_json` 与 `std`（不引入 async-trait，也不依赖任何具体数据库）：

```rust
pub trait BuiltinMcp: Send + Sync {
    fn package(&self) -> &'static BuiltinPackage;
    fn tools(&self) -> Vec<BuiltinToolDef>;
    fn call<'a>(
        &'a self,
        config: &'a BuiltinConfig,
        tool: &'a str,
        arguments: &'a Value,
    ) -> BuiltinFuture<'a, Result<Value, BuiltinError>>;
}
```

- `package()` 返回静态包定义：`id`、展示名、摘要、版本、`tags`、`default_upstream_id`、
  `fields`（`BuiltinField { key, label, kind, required, secret, default, placeholder, help, env }`；
  `kind` 只有 `text` 与 `password`，后者配 `secret=true`）、
  `tools`（目录展示用 `{name, summary}`）。`version` 取 crate 版本（`env!("CARGO_PKG_VERSION")`），
  `catalog_version` 由此写入。
- `tools()` 是 MCP `tools/list` 的**单一来源**：返回 `BuiltinToolDef { name, description, input_schema }`，
  schema 必须是完整 JSON Schema（管理面与 agent 面看到的是同一份）。目录里的 `tools[].summary`
  是简短展示文案，不参与协议。
- `call()` 返回 MCP `tools/call` 的 `result` 对象：
  - 成功：`{"content":[…], "structuredContent": {…}}`；
  - 失败：`{"content":[{"type":"text","text":"<紧凑 JSON>"}], "isError":true}`，其中 text 里的
    `code` 是稳定 `MCP_*` 码（如 `MCP_VALIDATION_ERROR`、`MCP_SQL_REJECTED`），`message` 面向人，
    `data` 放结构化细节。工具级失败**不**抛 JSON-RPC error：agent 需要看到可解释的失败对象。
- `BuiltinConfig` 按**字段 key** 取值（`config.get("password")`），值来自上游 `env` 且已解密；
  它只打印 key 列表，不打印值——日志里不会出现明文密码。
- `BuiltinError { code, message, data }` 由插件在"配置不可用/参数不合法"时返回；内嵌端点把它
  转成上面的 `isError` 形状。

插件内部可以缓存重资源（连接池、客户端）。缓存键必须是配置指纹（对含密文/明文的配置做 sha256，
不落盘、不进日志），配置一变指纹就变，旧条目自然失效，不需要显式失效通知。

## 2. 内嵌 HTTP 端点

`POST /internal/mcp/{package_id}/{upstream_id}`，单条 JSON-RPC 2.0（请求体是报文，响应体也是报文），
不支持批处理，不发 SSE。

鉴权：`Authorization: Bearer <internal token>`。token 在进程启动时随机生成（32 字节 hex），
**只存在内存**，不经配置/环境变量暴露；比较用常量时间。上游客户端在 URL 前缀命中内嵌端点
（`BuiltinEndpoint.matches`）时注入该头，并**覆盖**用户配置的同名 header——没有这个头，端点一律
`401 MCP_UNAUTHORIZED`。

方法分派：

| 方法 | 响应 |
|---|---|
| `initialize` | `{protocolVersion:"2025-03-26", capabilities:{tools:{}}, serverInfo:{name:<package id>, version:<package version>}}` |
| 任何无 `id` 的请求（含 `notifications/initialized`） | `202` 空体（通知不回包；包与上游行校验通过后才静默成功） |
| `ping` | `{}` |
| `tools/list` | `{tools:[{name, description, inputSchema}]}`（无 cursor，一次给全） |
| `tools/call` | 插件的 `result` 对象；缺 `params.name` → `-32602` |
| 其他方法 | `-32601`（message「未知方法: …」） |

请求前置校验（顺序固定）：Bearer → JSON 可解析（`-32700`）→ 对象且有 `method`（`-32600`）→
包已注册（`404 MCP_CATALOG_NOT_FOUND`）→ 上游行存在（`404 MCP_UPSTREAM_NOT_FOUND`）→
`catalog_id` 与包 id 相符（`404`，hint「该上游不是这个包安装的」）→ `transport == "http"`
（`403 MCP_BUILTIN_UNAVAILABLE`）→ `enabled == true`（`403 MCP_UPSTREAM_DISABLED`）。

错误文案与审计里都不得出现配置值：消息只带字段 `label`（缺字段、解密失败），数据只带字段 key。

## 3. 配置与密钥

两把密钥，用途分离，都是 64 位 hex（32 字节），HTTP 模式**必需**、缺失/非法在启动时直接失败
（fail-closed），生成方式：`openssl rand -hex 32`。

- `JUMCP_SECRET_KEY`：**落库**加密。安装/改配置时，`secret=true` 的字段用它加密后写进上游 `env`。
- `JUMCP_TRANSPORT_KEY`：**传输**加密。管理台从 `GET /admin/transport-key` 取到同一把密钥，
  在浏览器里加密 `secret` 字段后提交；服务端解密再用 `JUMCP_SECRET_KEY` 重新加密落库。

线格式两处完全一致（复用同一个 `enc:v1` 实现）：`enc:v1:` + `hex(nonce12 ‖ ciphertext ‖ tag)`，
AES-256-GCM，AAD 为空，hex 小写，12 字节随机 nonce。

- `secret=true` 的非空值必须是 `enc:v1:` 传输密文：明文 → `MCP_TRANSPORT_CIPHERTEXT_REQUIRED`，
  用错密钥/被篡改 → `MCP_TRANSPORT_DECRYPT_FAILED`，解出空串 → `MCP_VALIDATION_ERROR`。
  拒绝路径都显式，不存在「解不开就当明文用」或降级空串；PATCH 传空串仍表示保持原密文。
- 非加密值原样兼容：迁移前留下的明文 env 仍可读；读取端看到 `enc:` 前缀才解密。
- 解密失败是显式错误（`MCP_CONFIG_ERROR`，message 指名字段），绝不静默返回空串把错误配置跑起来。
- 目录与安装响应只回显非密字段；密字段只给 `secrets_set: {key: bool}`；日志/审计/错误响应都不出现
  明文或密文（审计仍记 `***`）。

## 4. 生命周期与状态

- 启动（HTTP 模式）：`Settings::validate` 校验密钥 → `builtin::migrate_legacy_packages`
  把旧的 stdio 安装改写为 HTTP（在构造 `UpstreamManager` 之前）→ 生成 `BuiltinEndpoint`
  （监听通配地址时用回环 `127.0.0.1`）→ 注入 `UpstreamManager` → 挂载端点 router。
- 安装/改配置/卸载都拒绝把包管理字段交给通用 `PUT`：`transport`、`url`、`command`、`args`、
  `env`、`headers` 任一被改动即 `409 MCP_UPSTREAM_PACKAGE_MANAGED`；`name`/`group`/`enabled`/
  `timeout_ms` 仍可改。
- 上游缓存按 `definition_hash`（完整配置 sha256）失效：改配置后下一次 `tools/list`/`tools/call`
  自动重连，不需要重启进程，也不需要调用 refresh。
- 卸载只删 `catalog_id` 匹配的行；手工上游不受影响。

## 5. 如何新增一个内置 MCP

1. 新建 crate（如 `jumcp-mcp-foo`），`Cargo.toml` 只依赖 `jumcp-core` + 该 server 需要的库；
   **不要**加 `[[bin]]`，也不要有 stdio 行循环。
2. 实现 `BuiltinMcp`：静态 `BuiltinPackage`（字段的 `env` 名自己定，前缀建议 `JUMCP_FOO_`）、
   工具定义、`call()`；连接/客户端按配置指纹缓存。
3. 导出 `pub fn plugin() -> &'static dyn BuiltinMcp`（`static` + `LazyLock` 或 unit struct）。
4. 在 `jumcp-server` 的 `builtin::plugins()` 里加一行 `jumcp_mcp_foo::plugin()`，并在
   `jumcp-server/Cargo.toml` 加依赖。目录、安装、内嵌端点、鉴权、加密、迁移全部自动生效。
5. 测试：插件 crate 内测工具语义（直接调 `call()`/内部 Server，不起协议层）；`jumcp-server`
   的 `tests/upstream/packages.rs` 用真实 listener 覆盖 IO 链路（安装 → refresh → 卸载）。
   协议层（initialize/tools/list/错误码）由内嵌端点的单测覆盖，插件不必重复实现。

## 6. 迁移与限制

- **迁移**：HTTP 模式启动时，`catalog_id='clickhouse-basic' AND transport='stdio'` 的旧行会被改写：
  env 里的 `JUMCP_CLICKHOUSE_DSN` 拆成 `URL`/`USER`/`PASSWORD`（加密）/`DATABASE`，
  `ALLOWED_SCHEMAS` 保留，`transport=http`、`url` 指向内嵌端点、`command`/`args` 清空。
  DSN 解析失败时只 warn 并保留原行（不静默删除/停用），需要人工重装。迁移写 `admin_change` 审计。
- **密钥轮换**：落库密钥 `JUMCP_SECRET_KEY` 换掉后旧密文不可解，必须在管理台重填密码字段；
  轮换前请先备份 `registry.sqlite3`。传输密钥 `JUMCP_TRANSPORT_KEY` 换掉只影响未重新提交的
  传输密文（重新填一次密码即可），库里的密文不受影响。
- **额外一跳**：上游客户端经回环 HTTP 调用本进程端点，每个 `tools/call` 多一次本地 HTTP 往返；
  仍受 `timeout_ms` 约束，但对延迟敏感的内置包应把工具实现做薄。
- **TLS**：传输加密只让请求体里的密码是密文，不替代 TLS——共享密钥随管理页面下发，能读到页面
  的中间人同样能拿到它。生产必须在前面做 TLS 终止（反向代理 / 负载均衡），不要把管理面或密码字段暴露在明文 HTTP 上。
- 内嵌端点不实现 `resources`/`prompts`/`sampling`/`logging`/`completion`；也不支持批处理与
  `tools/list_changed` 推送（与 `/mcp` 面一致）。
