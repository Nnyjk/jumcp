# 上游 read_only 声明已移除

## 结论

`mcp_upstreams.read_only` 与部署开关 `JUMCP_MCP_ALLOW_WRITE_UPSTREAM` 都删除。上游可达性只由
`enabled`、授权组、token allowlist 决定；转发前的 SQL/文件/接口前置拦截保留。`basic` 资格只看
`category≠admin`、工具名不以 `admin_` 开头、暴露名合法。

## 为什么否决"上游自己声明只读"

- 声明只是布尔值，本服务既不能验证也不能强制第三方 MCP 的行为。真实案例：simple-post 把端点
  URL/method 存在服务端配置里，MCP 工具入参只有业务字段，前置拦截看不到写意图——`read_only=true`
  没有任何运行期含义，只制造"这条链路是只读的"错觉。
- 声明还会污染三处无关逻辑：`basic` 资格、risk 标签、审计字段，全部要跟着这个不可信的布尔值走。
- 写控制的真实落点在接入侧：只读账号/只读会话、上游权限策略、网络出口。这几点本来就写在
  `notes/mcp_plan/decisions_write_scope.md`。
- 内置包 `clickhouse-basic` 不需要声明：它固定 `readonly=1` 并过 `jumcp-sqlguard`，这是实现保证，
  不是配置。

## 兼容

旧库的 `read_only` 列保留但不读不写；INSERT 省略该列，靠列默认值 `1` 满足 NOT NULL。
不做迁移，不做回写。
