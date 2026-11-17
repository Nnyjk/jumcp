# 可安装 MCP 包：理由、坑与限制

接口契约、目录字段、安装/卸载语义与错误码见 `docs/mcp-router.md` 的「3. 可安装 MCP 包」
（§3.1 安装、§3.2 可执行文件解析、§3.3 卸载、§3.4 错误码、§3.5 与手工上游的关系）。
本文只留 docs 不适合承载的内容。

## 1. 为什么是代码内静态目录，而不是"填 URL 装任意包"

- **安装等于一次写操作**：`POST` 会写 `mcp_upstreams`，而任意 URL 安装意味着目录内容来自
  不可信远端。目录里带着 `command` / `transport` / `env` 名，让远端决定这些字段等于把
  "以服务身份执行任意进程"的能力开放给网络。
- **可审计的封闭集合**：静态目录的每个条目都是随二进制发布的代码，评审、版本、二进制名
  与 `catalog_version` 一起进 git；用户装了什么，读源码就能知道全貌。
- **与签名/供应链无关**：我们不做包签名体系，就不该假装能安全地装第三方描述。需要新增
  包时改 `packages.rs` 并随版本发布——这不是限制，是把"谁能定义可执行内容"收回仓库。

## 2. 测试为什么要换 PATH（坑）

`cargo test` 会把 `backend/target/debug` 加进被测进程的 PATH，而该目录里就有
`jumcp-mcp-clickhouse.exe`。测试若只把 `JUMCP_MCP_BUNDLED_BIN_DIR` 指向空目录，
`binary_present` 仍会在 PATH 里命中真二进制，"可执行文件缺失"这条分支永远测不到。

所以 `tests/upstream/packages.rs` 的暂存夹具同时做两件事：把 `JUMCP_MCP_BUNDLED_BIN_DIR`
指向临时目录，并**临时把 PATH 也指到该目录**（用例在静态 `Mutex` 内串行，作用域结束恢复）。
夹具只放空占位文件、从不 spawn 进程，所以不需要往 `target/` 写文件，也不会引入端口竞争。

## 3. 已知限制

- **没有真实 ClickHouse 联调**：安装只负责"配置正确 + 二进制在"，不验证 DSN 可连。
  DSN 是否可用由 `POST /admin/upstreams/{id}/refresh` 的 `status` / `status_message` 表达。
- **没有包升级通道**：`catalog_version` 只是记录安装时的版本；二进制升级后不会自动改写已安装
  实例的 `catalog_version`，也不做字段迁移。需要新参数时，卸载重装是唯一路径。
- **`installations` 只按 `catalog_id` 聚合**：不按 `command` 猜归属。手工建一个
  `jumcp-mcp-clickhouse` 上游不会被认作"本包安装"——这是刻意的。
- **`config` 只接受字符串值**：`kind` 只决定表单控件，服务端不做数值校验；当前唯一的包只有
  text / password 字段。
- **`dictGet('db.dict', …)` / `joinGet('db.join', …)` 绕过白名单**：这类函数用字符串命名字典或
  Join 表，sqlguard 的库白名单与会话钉库都覆盖不到，仍可作跨库读数通道；要彻底封住只能靠只读
  账号把授权收到库级。
- 未做批量安装 / 依赖解析：一个包一个上游，互不引用。
