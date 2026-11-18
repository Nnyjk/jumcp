# 内置包的实现取舍、坑与限制

接口契约、目录字段、安装/改配置/卸载语义与错误码见 `docs/mcp-router.md` 的「3. 内置 MCP 包」，
插件标准见 `docs/builtin-mcp.md`。本文只留 docs 不适合承载的内容。

## 1. 为什么是代码内静态目录

- **安装等于一次写操作**：`POST` 会写 `mcp_upstreams`，而任意 URL 安装意味着目录内容来自
  不可信远端。让远端决定"装什么、工具清单长什么样"会把治理面变成开放代理。
- **封闭且可审计**：每个包都是随 `jumcp-server` 编译发布的代码，字段、工具、版本与
  `catalog_version` 一起进 git；用户装了什么，读源码就能知道全貌。
- **不做包签名**：没有签名体系就不假装能安全加载第三方描述。新增包 = 新 crate + 注册表一行。

## 2. 为什么不用 rmcp / MCP SDK

仓库已有一套手写 JSON-RPC 栈（`/mcp` 的 `mcp/` 与上游 `upstream/http`），错误码、身份、
scope 与审计都长在这套栈上。引 SDK 会出现第二套协议实现：两套 `initialize` 能力协商、
两套错误形状、两套传输细节，且 SDK 的类型无法直接复用我们的 `EffectiveScope` 与审计原语。
内嵌端点只实现 5 个方法（initialize / initialized / ping / tools/list / tools/call），
手写量远小于对齐两套抽象的长期成本。同理不引 `async-trait`：`Pin<Box<dyn Future>>` 足够，
且少一个宏依赖。

唯一新增的运行时依赖是 `aes-gcm`（rustcrypto）。密码字段必须加密落库，自己写 AES-GCM 是
明确禁止的；`aes-gcm` 只带来 `aead`/`cipher`/`ghash`/`universal-hash` 等 rustcrypto 传递依赖，
随机数复用其 re-export 的 `OsRng`，不需要额外的 `rand`。

## 3. 测试里的真实 listener（坑）

内嵌端点必须经 TCP 才能被 reqwest 打通，`tower::ServiceExt::oneshot` 拿不到 `127.0.0.1:<port>`。
`tests/support/mod.rs` 的 `BuiltinHarness` 因此先 `TcpListener::bind("127.0.0.1:0")`，用实际
addr 构造 `BuiltinEndpoint`，再用同一个 Router 的 clone 做 admin 请求：安装产物里的 URL 指向
真实端口，`refresh` 才真的走「上游客户端 → 注入 Bearer → 内嵌端点」。测试结束 `Drop` 里
`abort()`，不依赖生产代码提供懒初始化或测试开关。

## 4. 旧 stdio 安装的迁移

- 迁移在 HTTP 模式启动、构造 `UpstreamManager` 之前执行：`catalog_id='clickhouse-basic' AND
  transport='stdio'` 的行按 `JUMCP_CLICKHOUSE_DSN` 拆成 `URL`/`USER`/`PASSWORD`（加密）/`DATABASE`。
  DSN 解析失败只 warn 并保留原行——迁移不能把用户配置弄丢，哪怕它已经跑不起来。
- 迁移只覆盖 `clickhouse-basic` 的旧 DSN 形状；别的包、或手工把 DSN 写进别的环境变量名，
  不会被猜。手工 stdio 上游（无 `catalog_id`）完全不动。
- 密钥轮换的两个方向（`JUMCP_SECRET_KEY` / `JUMCP_TRANSPORT_KEY`）见
  `notes/builtin_mcp/transport_encryption.md`，这里不再复述。

## 5. 已知限制

- **多一跳**：agent 的 `tools/call` 经上游客户端走回环 HTTP 调本进程，比进程内直调多一次
  本地往返与一次 JSON 编解码；代价可忽略，但为延迟敏感的包做工具实现时要薄。
- **TLS 由网关负责**：内嵌端点只监听本机/内网明文 HTTP；安装/改配置的请求体密码是传输密文
  （能防日志与抓包直接读），但密钥随管理页面下发，**不替代 TLS**。生产必须由网关终止 TLS，
  且不要把 `JUMCP_SECRET_KEY` / `JUMCP_TRANSPORT_KEY` 写进镜像。
- **迁移不验证连通性**：拆分 DSN 只做语法解析，`url` 是否可连由
  `POST /admin/upstreams/{id}/refresh` 的 `status` 表达。
- **没有包升级通道**：`catalog_version` 只记录安装时的版本，字段升级不做自动迁移，卸载重装是
  唯一路径；改配置用 PATCH，不必重装。
- **`config` 只接受字符串值**：`kind` 只决定表单控件（`text` / `password`），JSON 数字/布尔会被拒
  （请求体契约就是 `{fieldKey: string}`）。
- **`installations` 只按 `catalog_id` 聚合**：不猜 command/url。手工建一个指向内嵌端点的上游
  也不会被认作本包安装——刻意如此。
- **`dictGet('db.dict', …)` / `joinGet('db.join', …)` 绕过白名单**：这类函数用字符串命名字典或
  Join 表，sqlguard 的库白名单与会话钉库都覆盖不到，仍可作跨库读数通道；要彻底封住只能靠只读
  账号把授权收到库级。
- 未做批量安装 / 依赖解析：一个包一个上游，互不引用。
