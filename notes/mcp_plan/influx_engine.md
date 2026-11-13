# InfluxDB 1.8 引擎接入的取舍与坑

只记理由、限制与坑；对外契约与能力矩阵见 `docs/plan/jumcp-preview.md` §4.3.2 与附录 A。

## 为什么复用 QueryExecutor + `db_query_readonly`

- 现成的 `db_query_readonly` 已经承担了权限、审计、token 预算、行/格截断、错误码映射；再开一个 `influx_query` 会把上述每一条都要复制一遍，且两个入口的 `sql_hash`/审计字段会分叉。
- InfluxQL 与 SQL 的执行语义差异集中在「编译请求 + 解析响应」，把这层做成 `QueryExecutor` 的一个实现，L4 守卫、结果整形、审计链路都能原样复用。
- 否决：新 crate / 现成 influx client。1.8 的 HTTP `/query` 就是一个 GET/POST + JSON，本次接入只为 jumcp-server 增了 `reqwest` 0.12（`rustls-tls` + `json`）作为唯一 HTTP 客户端；再引 `influxdb` crate 只会换来一层薄封装，且 1.8 的 `chunked` 行为仍要自己处理。

## `database` 为什么是工具入参而不是 DSN 的一部分

- InfluxQL 的库是**查询作用域**（`db` 参数），不是连接作用域：同一份数据源天然要跨库查询（`SHOW DATABASES`、按库取测量项）。
- 若把库写进 DSN，就得为每个库开一条 `[[datasources]]`，而启动期只允许一个 readonly 数据源——直接堵死。
- `SHOW DATABASES` 是唯一不需要 `database` 的语句（库级内省）；其余 `SHOW`/`SELECT` 都按请求声明的库执行，`SHOW ... ON <db>` 还要与 `database` 一致，否则两处口径会打架。
- 代价：入参可控，所以必须靠 `allowed_schemas` 白名单兜住；PostgreSQL 数据源收到 `database` 一律拒绝，避免「参数存在但语义不同」的歧义。

## `allowed_schemas` 复用为 database 白名单

- 配置结构里已有「允许对象范围」的概念，PG 是 schema，influx 自然对应 database；新造 `allowed_databases` 会让两个引擎的策略字段各说各话，且管理台/审阅面要改两处。
- 空数组 = 不限制：与本仓库其它数据源「缺省不声明即不限」的处理保持一致，也让 Trial 不需要先枚举 40+ 个库。

## 为什么只允许一个 readonly 主数据源

- `db_query_readonly` 没有数据源选择参数：多数据源时无法决定这次调用打到哪个引擎，只能靠隐式顺序，属于不可解释的行为。
- 只读自检、`statement_timeout_ms` 的落地方式、错误码前缀都按单数据源设计；两个 readonly 并存会让启动期自检在语义上无法收敛。故启动即失败，而不是运行时挑一个。

## 为什么 influx 暂不支持模板

- 模板是「命名占位符 + 类型化参数」的绑定模型；InfluxQL 1.8 没有参数占位符，只能做字符串拼接，等于把注入面重新打开。
- 模板缓存键 `(template_id, version, normalized_params_hash)` 不含 `database`，跨库复用同 id 会串结果。要支持得先改缓存键与导入/导出映射，收益不抵复杂度。

## 守卫是纵深防御，不是账号级保证

- 1.8 没有 PG 那种 `default_transaction_read_only` 等价开关，会话级只读不存在；只读靠「READ 授权用户 + L4 单条只读 InfluxQL + 客户端超时」三层叠加。
- 因此 L4 的拒绝是**唯一**能拦住 `SELECT INTO`、`CREATE/DROP`、`DELETE/DROP SERIES` 的运行时手段；不能因为账号是 READ 就放行可疑结构。
- 跨库三段式 `FROM`（`db.rp.measurement`）一律拒绝：它绕过 `database` 参数的单一作用域，也让白名单只校验到一个维度。

## 凭据只走 env

- `policy.toml` 会被管理面读取、被 admin API 回显、被 `.dev-data` 备份；DSN 的 userinfo 里带密码，写进文件等于把密码落到可读面。
- 所以 `dsn_env` 是**变量名**，不是值；脚本注入 `JUMCP_DEV_INFLUX_DSN` 只存在于后端进程环境（dev.sh / dev.ps1 均如此）。

## 坑

- **鉴权可能整体关闭**：实测目标实例在 Basic auth（`-u <user>:<password>`）、query 参数（`?u=<user>&p=<password>`）、完全无凭证三种情况下都返回 200 与同一份数据库列表。即「能查」不代表鉴权生效，联调时不能把 200 当鉴权证据；反过来，别把 `u/p` 放进 query string，那会进 access log。
- **不存在/无数据的 measurement 不是错误**：实测 `SELECT * FROM "no_such_measurement"` 在 `logs` 库返回 `{"series":[],"row_count":0,"truncated":false,"effective_max_rows":100,...,"isError":false}`；服务端的 `SHOW MEASUREMENTS` 对空库则直接省略 `series` 键（`{"results":[{"statement_id":0}]}`），解析必须容忍缺键，别按 `series[0]` 直接取。
- **超时是客户端超时**：`statement_timeout_ms` 在这里只能落到 HTTP 客户端超时；请求被放弃后服务端查询仍在跑，长查询会持续占资源，所以超时值不能当成资源保护。
- **响应可能是分块 NDJSON**：带 `chunked=true` 时 `/query` 会分多行 JSON 返回，每行一个 `results` 片段；客户端必须逐行解析或显式关闭 chunked，否则按单文档解析会失败。
- **时间字段是 RFC3339 字符串**：实测 `SELECT * FROM "changelogs" LIMIT 3` 的 `time` 列是 `"2025-08-15T03:32:00Z"`，执行器不传 `epoch`/`precision`；若将来传 `epoch=ns` 会变成整数，下游解析要能容忍两种类型。
- **字段值保持 InfluxDB 原始类型**：同一次查询里 `field`/`n`/`o`/`series`/`sn`/`ts` 都是 InfluxDB 存储时的字符串（`"67 "` 带尾部空格，`null` 与 `""` 并存），不是 SQL 那种强类型列；不要把 `rows` 当二维同构数组做类型假设。
- **`max_rows` 与 `LIMIT` 是两回事**：`max_rows=3` 时回显 `effective_max_rows=3`，而 `LIMIT 3` 只返回 2 行只是因为该 measurement 只有 2 个点——别拿「返回行数等于 LIMIT」当截断未生效的证据。
- **`SHOW DATABASES` 结果只有 `name` 一列**，1.8 原生 values 是 `[[name], ...]`；执行器已统一成 `series[].columns` + `series[].rows`。
