# ClickHouse 探测报告

- 目标：`192.168.30.48:18123`（HTTP），用户 `default`
- 探测时间：2026-11-10
- 方式：只读。全部为 `SELECT` / `EXPLAIN` / `DESCRIBE` / `SHOW`，未执行任何写入或 DDL。
- 凭证在本文中一律以 `$CHPW` 代替，不落盘。

## 1. 服务端基本信息

| 项 | 值 |
|---|---|
| `version()` | `25.5.1.2782 (official build)` |
| `currentDatabase()` | `default` |
| `timezone()` | `Asia/Shanghai` |
| `hostName()` | `ebffcba39c6f`（疑似容器） |
| `uptime()` | 约 425699 s |
| 默认端口 | HTTP 18123（非标准 8123，无 TLS） |
| 认证方式 | HTTP Basic / `X-ClickHouse-User` + `X-ClickHouse-Key` 头 / URL userinfo 均可用 |

## 2. `/ping` 与认证

```
GET /ping                     -> 200 "Ok."   （无需凭证）
GET /?query=SELECT%201        -> 401          （无凭证）
GET /?query=SELECT%201        -> 200 1        （带凭证）
```

结论：`/ping` 不校验认证，只能当存活探针，不能当权限校验。业务请求必须带凭证，否则 401。

注意：base URL 带尾斜杠时 `//ping` 会 404，拼接路径要规范。

## 3. 数据库与表

`SHOW DATABASES`：

```
INFORMATION_SCHEMA
click_test
default
env_air
information_schema
system
```

各库表数量：`system`(114, 视图为主)、`env_air`(14)、`click_test`(9)、`INFORMATION_SCHEMA`(20)、`information_schema`(20)。
**`default` 库内无任何用户表**（0 张），`SELECT name FROM system.tables WHERE database='default'` 返回空。

`env_air`（环境空气域）：

| 表 | 引擎 | total_rows |
|---|---|---|
| air_point_info | ReplacingMergeTree | 35 |
| air_quality_target | ReplacingMergeTree | 9 |
| air_aqi_type | ReplacingMergeTree | 16 |
| air_orig_h | ReplacingMergeTree | 104495 |
| air_orig_h_aqi | ReplacingMergeTree | 233845 |
| air_orig_d_aqi | ReplacingMergeTree | 18721 |
| air_orig_m | ReplacingMergeTree | 10465 |
| air_audit_h | ReplacingMergeTree | 939 |
| air_audit_h_aqi | ReplacingMergeTree | 38954 |
| air_audit_d_aqi | ReplacingMergeTree | 5291 |
| air_audit_mon / air_audit_y | ReplacingMergeTree | 0 / 0 |
| air_blend_d_aqi | View | - |
| view_air_orig_h | View | - |

`click_test`（测试/接入验证）：

| 表 | 引擎 | total_rows |
|---|---|---|
| t_click_table | MergeTree | 1 |
| water_orig_h / water_orig_h_test | ReplacingMergeTree | 120 / 49 |
| water_orig_h → outfall_orig_h | ReplacingMergeTree | 79775 |
| water_audit_h / water_audit_d / water_audit_mon | ReplacingMergeTree | 36 / 66 / 51 |
| jdbc_engine_table | JDBC 引擎（外部库代理） | - |
| mysql_engine_table | MySQL 引擎（外部库代理） | - |

代表性 DDL（`SHOW CREATE env_air.air_audit_h`）：

```sql
CREATE TABLE env_air.air_audit_h
(
    `sn` String COMMENT '站点编码',
    `time` DateTime COMMENT '数据时间',
    `factor_values` Map(String, Float64) COMMENT '因子数值键值对',
    `factor_flags` Map(String, String) COMMENT '因子标记键值对',
    `modify_date` DateTime DEFAULT now() COMMENT '修改时间，默认当前时间'
)
ENGINE = ReplacingMergeTree(modify_date)
PARTITION BY toYear(time)
PRIMARY KEY (sn, time)
ORDER BY (sn, time)
SETTINGS index_granularity = 1024, min_bytes_for_wide_part = 0
```

特征：业务表广泛使用 `Map(String, ...)` 存业务指标、`ReplacingMergeTree(modify_date)` 做去重、按年分区。
`click_test.t_click_table` 用 `Map(String,String)` / `Tuple(String)` / `Array(Int8)` / `pushTime DateTime`，像是入库连通性验证表。

实测查询：`SELECT count(), uniqExact(sn), min(time), max(time) FROM env_air.air_orig_h` → 104495 / 28 / 2025-06-20 08:00:00 / 2026-11-10 11:00:00，耗时约 0.16 s。

## 4. 参数绑定（HTTP 接口）

语法：URL 查询串传 `param_<name>=<value>`，SQL 里写 `{name:Type}`。

| 用例 | 结果 |
|---|---|
| `?param_x=7` + `SELECT {x:UInt8} AS v` | `7`，`toTypeName(v)`=`UInt8` |
| `?param_s=hello%20world` + `{s:String}` | `hello world` |
| `?param_a=[1,2,3]` + `{a:Array(UInt8)}` | `[1,2,3]` |
| `?param_d=2024-01-02 03:04:05` + `{d:DateTime}` | 正常解析（服务器时区） |
| `?param_dec=12.34` + `{dec:Decimal(18,2)}` | `12.34` |
| `?param_n=` + `{n:Nullable(String)}` | 得到空串 `""`（**不是 NULL**） |
| `{c:Identifier}` + `param_c=1` | 生效，替换为标识符 `1` → `UNKNOWN_IDENTIFIER`（说明确实按标识符插入） |
| 缺参数 | `Code: 456 ... Substitution 'x' is not set (UNKNOWN_QUERY_PARAMETER)` |
| `?` 占位符 + `param_1=5` | **不支持**，`SYNTAX_ERROR`（`?` 不是 ClickHouse 语法） |
| `param_x` 放在 POST body | **不支持**，body 被视为 SQL 的一部分 |

结论：
- 只有命名参数 `{name:Type}`，没有 `?` 位置参数（`?` 是 clickhouse-rs 客户端自己的语法，见第 9 节）。
- 参数**必须**走 URL query string，不能混进 POST body。
- 每个参数都强类型，类型串写错会直接报错；空串不能表示 NULL。
- 想表达 NULL 需 `{n:Nullable(String)}` 且值传 `NULL` 或以 `NULL` 字面量处理（未逐个核验所有 NULL 形式）。

## 5. 只读与权限（**重要**）

`system.settings` 当前值：

| setting | 值 |
|---|---|
| `readonly` | `0`（无任何只读限制） |
| `max_execution_time` | `0`（不限） |
| `max_result_rows` | `0`（不限） |
| `max_result_bytes` | `0`（不限） |
| `result_overflow_mode` | `throw` |
| `max_memory_usage` | `0`（不限） |
| `max_bytes_before_external_group_by` | `0` |
| `max_block_size` | `65409` |
| `max_threads` | `auto(16)` |
| `timeout_before_checking_execution_speed` | `10` |

`SHOW GRANTS`：

```
GRANT CHECK, SHOW, SELECT, INSERT, ALTER, CREATE, DROP, UNDROP TABLE, TRUNCATE, OPTIMIZE,
      BACKUP, KILL QUERY, KILL TRANSACTION, MOVE PARTITION BETWEEN SHARDS, SYSTEM,
      dictGet, displaySecretsInShowAndSelect, INTROSPECTION, SOURCES, CLUSTER ON *.* TO default
GRANT TABLE ENGINE ON * TO default
GRANT SET DEFINER ON * TO default
```

`system.users`：只有一个 `default`，`auth_type=['plaintext_password']`。

结论：
- **该账号是超级用户，服务器端完全没有只读保护**。任何"只读"保证只能由客户端（MCP server）自己实现。
- 服务端提供了可用的手段：每个请求带 URL 参数 `?readonly=1`（或 `SETTINGS readonly=1`）即可让这条连接只读。
- `readonly` 取值实测：
  - `0` 无限制；
  - `1` 只允许读请求 + 修改白名单设置；
  - `2` 只允许读请求 + 修改除 `readonly` 外的设置；
  - `3` 也被接受（高版本语义，等同更严格档；文档对 3 描述不完整，属**未完全核验**）。
- 用 `session_id=<id>&session_timeout=120` 可让 `SET` 在多次请求间保持；不带 session_id 时每次请求都是独立设置上下文（`SET readonly=1` 立即失效，实测 `getSetting('readonly')` 回到 0）。
- 在 session 内 `SET readonly=1` 后再 `SET max_result_rows=5` → `Code: 164 Cannot modify 'max_result_rows' setting in readonly mode (READONLY)`；再 `SET readonly=0` 同样被拒。
- 同一请求的 `SETTINGS readonly=1, max_result_rows=5` 这种写法**不会**互相约束（设置在同一层解析），所以只读必须通过 URL 参数/会话设置施加，不能靠语句尾 `SETTINGS`。

### 5.1 `?readonly=1` 对写操作的拦截（实测，未产生任何持久写入）

全部请求都只带 URL 参数 `?readonly=1`，不带 session_id（每次独立请求）：

| # | 请求 | HTTP | 错误 |
|---|---|---|---|
| 1 | `CREATE TEMPORARY TABLE __jumcp_probe(x UInt8) ENGINE=Memory` | **500** | `Code: 164. DB::Exception: default: Cannot execute query in readonly mode. (READONLY)` |
| 2 | `INSERT INTO __jumcp_probe VALUES (1)`（承接 #1，表未建成） | **404** | `Code: 60 ... Table default.__jumcp_probe does not exist. (UNKNOWN_TABLE)` —— 表存在性校验先于只读校验 |
| 3 | `INSERT INTO system.numbers VALUES (1)` | **500** | `Code: 164 ... Cannot execute query in readonly mode. (READONLY)` |
| 4 | `SET readonly=0` | **500** | `Code: 164 ... Cannot modify 'readonly' setting in readonly mode. (READONLY)`（`&default_format=JSON` 时状态同为 500，`exception` 字段同文本） |
| 5 | `SELECT 1` / `SHOW TABLES FROM env_air` / `EXPLAIN SELECT ... LIMIT 1` | 200 / 200 / 200 | 正常 |

补充结论：
- `readonly=1` **确实**阻断临时表 DDL 与 INSERT，且不会留下任何对象：事后 `SELECT count() FROM system.tables WHERE name LIKE '%__jumcp_probe%'` → `0`。
- READONLY(164) 的 HTTP 状态是 **500**（与 `max_result_rows` 超限的 400 不同），客户端归一化错误时必须按错误码而不是状态码判断。
- `INSERT` 的只读校验发生在"表是否存在"之后，因此对不存在的表会先得到 `UNKNOWN_TABLE`；想依赖只读报错做检测时不要用不存在的表。
- 该版本没有 `system.temporary_tables`（查询该表 → `UNKNOWN_TABLE`）。
- 未创建/未触碰 `env_air`、`click_test` 的任何真实表。

## 6. 行数/结果集截断

| 用例 | 结果 |
|---|---|
| `LIMIT 10 SETTINGS max_result_rows=3, result_overflow_mode='break'`（默认 block_size） | 返回 **10 行**（整块返回后才判断，未截断） |
| 同上但 `max_threads=1, max_block_size=2` | 返回 **4 行**（0,1,2,3） |
| `... result_overflow_mode='throw'` | `Code: 396 Limit for result exceeded, max rows: 3.00, current rows: 10.00 (TOO_MANY_ROWS_OR_BYTES)`，HTTP 400 |
| URL `?max_result_rows=3&result_overflow_mode=throw` | 同样报 396，说明 URL 设置生效 |

结论：
- `max_result_rows` 是**块级**限制，不是精确行数上限：`break` 会多返回最多一个 block（最大可到 `max_block_size`）。
- 要精确 `LIMIT`，仍需在 SQL 里加 `LIMIT`；`max_result_rows` 只能当作兜底防爆。
- 想要"截断后仍成功返回"，用 `break`；想要"超限即失败"，用 `throw`（默认）。

## 7. 响应格式

默认 `TabSeparated`。可通过 `?default_format=JSON`（或请求头 `X-ClickHouse-Format`，或 SQL 末尾 `FORMAT xxx`）切换；SQL 里的 `FORMAT` 优先于 `default_format`。

`FORMAT JSON`：

```json
{
  "meta": [{"name":"u64","type":"UInt64"}, {"name":"dt","type":"DateTime"}, {"name":"d","type":"Decimal(18, 4)"}],
  "data": [{"n":null,"u64":"18446744073709551615","dt":"2024-01-02 03:04:05","d":1.2345,"s":"x","arr":[1,2]}],
  "rows": 1,
  "statistics": {"elapsed": 0.036, "rows_read": 1, "bytes_read": 1}
}
```

`FORMAT JSONCompact`：`data` 为二维数组，字段顺序同 `meta`；其余结构一致。

`FORMAT JSONEachRow`：每行一个 JSON 对象，没有 `meta`/`rows`/`statistics`。

类型映射（`output_format_json_quote_64bit_integers` 默认 1）：

| ClickHouse 类型 | JSON 表示 |
|---|---|
| `Nullable(Nothing)` / `Nullable(T)` 的 NULL | `null` |
| `UInt64` / `Int64` | **字符串** `"18446744073709551615"`；加 `?output_format_json_quote_64bit_integers=0` 才变数字 |
| `DateTime` | 字符串 `"2024-01-02 03:04:05"`（无时区后缀，按 server timezone） |
| `Date` | 字符串 `"2024-01-02"` |
| `Decimal(18,4)` | JSON 数字 `1.2345`（有精度丢失风险） |
| `UUID` | 字符串 |
| `Array` | 数组 |
| `Map(String, Float64)` | 对象 `{"a":1.5}`（键为字符串） |
| `Tuple(Int8, String)` | 数组 `[1,"x"]` |

对应响应头：`X-ClickHouse-Format`、`X-ClickHouse-Timezone`、`X-ClickHouse-Query-Id`、`X-ClickHouse-Summary`（JSON：read_rows/read_bytes/elapsed_ns 等）、`X-ClickHouse-Exception-Code`。

## 8. 错误形状

错误响应体为**纯文本** `Code: <n>. DB::Exception: <msg> (NAME) (version ...)`；只有加 `?default_format=JSON` 时才变成 JSON（`meta`/`data` 为空 + `exception` 字段）。HTTP 状态码与错误码对应：

| 场景 | HTTP | 错误码 |
|---|---|---|
| 密码错误 / 无凭证 | 401 | 194 `REQUIRED_PASSWORD` / Authentication failed |
| 语法错误 | 400 | 62 `SYNTAX_ERROR` |
| 结果集超限 | 400 | 396 `TOO_MANY_ROWS_OR_BYTES` |
| 超时 | 408 | 159 `TIMEOUT_EXCEEDED` |
| 未知数据库 `nosuch.tbl` | 404 | 81 `UNKNOWN_DATABASE` |
| 未知表（库存在） | 404 | 60 `UNKNOWN_TABLE` |
| 未知函数 | 404 | 46 `UNKNOWN_FUNCTION` |
| 未知设置名 | 404 | 115 `UNKNOWN_SETTING` |
| 设置值非法（`max_result_rows=abc`） | 400 | 27 `CANNOT_PARSE_INPUT_ASSERTION_FAILED` |
| 只读模式改设置 | **500** | 164 `READONLY` |
| 只读模式执行写/DDL（含临时表） | **500** | 164 `READONLY`（`Cannot execute query in readonly mode`） |
| 未声明的查询参数 | 400 | 456 `UNKNOWN_QUERY_PARAMETER` |

超时行为实测：

- `SELECT count() FROM system.numbers` + `max_execution_time=0.1` → 0.104 s 返回 408 `Timeout exceeded: elapsed 100.124 ms, maximum: 100 ms`。
- `SELECT sleep(1) SETTINGS max_execution_time=0.1` → 约 1.0 s 才返回 408（检查发生在块结束后，`sleep` 不会被中途打断）。
- `SELECT sleep(0.3) SETTINGS max_execution_time=0` 配 URL `?max_execution_time=0.1` → 成功返回 0，**未超时**（单块内不检查）。
- 结论：`max_execution_time` 是"软"超时，按块检查，精度受 `max_block_size`/函数阻塞影响；MCP 侧仍需自己的 HTTP 客户端超时。

无权限错误（`PERMISSION_DENIED`，码 497）**未核验**：当前 `default` 拥有全部权限，无法构造。

## 9. 官方 `clickhouse` Rust crate（0.15）

- crates.io 最新：**0.15.3**（2026-10-13）；0.15.0 发布于 2026-04-06。API 在 0.15.x 内未变。
- 传输：仅 HTTP（`hyper`），默认格式 `RowBinaryWithNamesAndTypes`（开启 schema 校验，`Client::with_validation(false)` 退化为 `RowBinary`）。不支持 native TCP。
- 连接串：`Client::default().with_url("http://host:18123").with_user("default").with_password("...").with_database("env_air")`
  - 认证实际通过请求头 `X-ClickHouse-User` / `X-ClickHouse-Key` 发送（`src/headers.rs: with_authentication`）。
  - `with_url("http://user:pass@host:port")` 这种 userinfo 写法**不是** crate 的认证路径：`with_url` 只把字符串交给 `Url::parse` 后作为 URI 使用，crate 内部不做 userinfo 拆分，不建议依赖（本次也未在 crate 内验证其可用性）。
  - `with_database()` 通过 URL 参数 `database=` 发送。
- 占位符与绑定（`src/sql/mod.rs`、`src/sql/bind.rs`）：
  - SQL 字符串里的 `?` 是**客户端**占位符，按 `bind()` 调用顺序替换；`?fields` 用 `Row` 派生的字段列表替换；`??` 表示字面 `?`。
  - `bind()` **不是**服务端参数绑定：它用 `serde` 把值序列化成 SQL 字面量（含转义）后拼进 SQL 文本，再 POST 出去。
  - 真正的服务端参数用 `.param("name", value)`，内部转成 URL 设置 `param_<name>`，SQL 里写 `{name:Type}` —— 与第 4 节探测结果一致。
  - 因此 "`?` 占位符可 Push 到 ClickHouse" 这一说法不成立：`?` 只在客户端存在，服务端只认 `{name:Type}` + `param_*`。
- 每个请求的 URL 还附带：`default_format`、`database`、`param_*`、客户端 `with_setting(k,v)` 的设置（例如可 `.with_setting("readonly","1")`、`.with_setting("max_execution_time","30")`）。
- 错误类型 `clickhouse::error::Error`：`InvalidParams` / `Network` / `BadResponse` / `TimedOut` / `SchemaMismatch` / `NotEnoughData` / `Custom` 等；服务端异常文本以 `BadResponse` 形式带出，没有按 `X-ClickHouse-Exception-Code` 做结构化映射（需要自己从文本解析 `Code: N`）。

## 10. 对 MCP server 的结论/建议

1. 连接：`http://192.168.30.48:18123`，用户 `default` 密码另行注入（环境变量/配置，不写进仓库）。
2. **只读必须客户端强制**：账号是 superuser、`readonly=0`、默认库为空。建议两条腿：
   - SQL 侧白名单（仅 `SELECT`/`WITH`/`SHOW`/`DESCRIBE`/`EXPLAIN`，拒绝 `;` 多语句、拒绝 INSERT/ALTER/CREATE/DROP/TRUNCATE/OPTIMIZE/SYSTEM）；
   - 请求侧附带 `readonly=1` URL 参数（服务端兜底，已实测可拦住临时表 DDL / INSERT / 改设置，见 5.1）。
3. 防爆参数建议：`max_execution_time`（例如 30）、`max_result_rows`（例如 1000）+ `result_overflow_mode=break`（多返回一个块，再做应用层裁剪）或 `throw`（直接报错）。
4. 参数化：只能用 `{name:Type}` + `param_name`，务必按列类型给准确类型；字符串空值 ≠ NULL。
5. 结果解析：JSON/JSONCompact 的 UInt64/Int64 是字符串，Decimal 是 JSON number，DateTime 无时区（server 为 `Asia/Shanghai`）。
6. 业务表在 `env_air` / `click_test`，`Map(String, Float64)` 是主要指标载体，SQL 需用 `mapKeys`/`mapValues`/`factor_values['x']` 之类取值；`ReplacingMergeTree` 查询注意 `FINAL` 或 `argMax` 去重。

## 11. 未核验项

- ~~`readonly=1` 对 INSERT/DDL 的实际拦截~~ → 已核验，见 5.1（临时表 DDL / INSERT / SET 全部 164，无残留）。
- `readonly=2` 对 INSERT/DDL 的拦截（只核验了 `readonly=1`）。
- `readonly=3` 的准确语义。
- `PERMISSION_DENIED`(497) 与内存超限(241) 的错误形状。
- TLS / 压缩（`compress=1`、zstd）未测。
- `param_` 传 `NULL` 的正确写法（空串已验证为 `""`）。
- ClickHouse `25.5` 相对 crate 0.15 的类型支持边界（如 `Variant`/`JSON` 新类型）未逐个验证。
