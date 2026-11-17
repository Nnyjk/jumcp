//! 只读 SQL 校验契约（单语句 + 只读 AST + 函数黑名单 + schema 白名单）。
//!
//! 覆盖三类判定：语句条数（单语句且注释/字符串里的分号不误判）、根节点与 AST 结构
//! （写语句、可写 CTE、行锁）、以及 schema 范围（越界不做隐式降级）。

use jumcp_sqlguard::{Dialect, GuardError, Kind, validate};

fn schemas(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

/// 执行校验并断言其失败，返回错误供进一步断言。
fn reject(sql: &str, dialect: Dialect, allowed_schemas: &[String]) -> GuardError {
    match validate(sql, dialect, allowed_schemas) {
        Ok(validated) => panic!("期望拒绝，但校验通过：{sql} -> {validated:?}"),
        Err(error) => error,
    }
}

fn reason(error: &GuardError) -> &str {
    error
        .data
        .get("reason")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
}

// ---------- 语句条数：只允许单条 ----------

macro_rules! multi_statement_case {
    ($name:ident, $sql:expr) => {
        #[test]
        fn $name() {
            let error = reject($sql, Dialect::Postgres, &[]);
            assert_eq!(error.code, "MCP_MULTI_STATEMENT");
            assert!(
                error.data["statement_count"]
                    .as_u64()
                    .is_some_and(|count| count >= 2),
                "statement_count 缺失或小于 2：{}",
                error.data
            );
            assert!(error.to_string().contains("MCP_MULTI_STATEMENT"));
        }
    };
}

multi_statement_case!(multi_statement_select_then_drop, "SELECT 1; DROP TABLE t");
multi_statement_case!(multi_statement_two_selects, "SELECT 1; SELECT 2");
multi_statement_case!(multi_statement_drop_then_select, "DROP TABLE t; SELECT 1");
multi_statement_case!(multi_statement_extra_semicolon, "SELECT 1;; SELECT 2");

macro_rules! single_statement_case {
    ($name:ident, $sql:expr) => {
        #[test]
        fn $name() {
            let result = validate($sql, Dialect::Postgres, &schemas(&["public"]))
                .unwrap_or_else(|error| panic!("期望通过，但被拒绝：{} -> {error}", $sql));
            assert!(
                matches!(result.kind, Kind::Select | Kind::Union | Kind::Subquery),
                "kind 非法：{:?}",
                result.kind
            );
        }
    };
}

single_statement_case!(single_statement_plain, "SELECT 1");
single_statement_case!(single_statement_semicolon_in_string, "SELECT ';'");
single_statement_case!(
    single_statement_semicolon_in_literal,
    "SELECT ';' AS s FROM t WHERE x = 'a;b'"
);
single_statement_case!(
    single_statement_semicolon_in_line_comment,
    "SELECT 1 -- ; not a statement"
);
single_statement_case!(
    single_statement_semicolon_in_block_comment,
    "SELECT 1 /* ; */"
);
single_statement_case!(
    single_statement_trailing_semicolon_with_comment,
    "SELECT 1; -- 尾部注释"
);
single_statement_case!(single_statement_redundant_semicolons, "SELECT 1;;");
single_statement_case!(
    single_statement_readonly_cte,
    "WITH x AS (SELECT 1) SELECT * FROM x"
);
single_statement_case!(single_statement_union, "SELECT 1 UNION ALL SELECT 2");

// ---------- 根节点：仅 SELECT / UNION / WITH ----------

macro_rules! root_rejection_case {
    ($name:ident, $sql:expr, $dialect:expr, $reason:expr) => {
        #[test]
        fn $name() {
            let error = reject($sql, $dialect, &[]);
            assert_eq!(error.code, "MCP_SQL_REJECTED");
            assert_eq!(reason(&error), $reason);
        }
    };
}

root_rejection_case!(
    root_reject_insert,
    "INSERT INTO t VALUES (1)",
    Dialect::Postgres,
    "write_statement"
);
root_rejection_case!(
    root_reject_update,
    "UPDATE t SET a = 1",
    Dialect::Postgres,
    "write_statement"
);
root_rejection_case!(
    root_reject_delete,
    "DELETE FROM t",
    Dialect::Postgres,
    "write_statement"
);
root_rejection_case!(
    root_reject_merge,
    "MERGE INTO t USING s ON t.id = s.id WHEN MATCHED THEN UPDATE SET t.a = 1",
    Dialect::Postgres,
    "write_statement"
);
root_rejection_case!(
    root_reject_create_table,
    "CREATE TABLE t(i int)",
    Dialect::Postgres,
    "ddl_statement"
);
root_rejection_case!(
    root_reject_create_temp_table,
    "CREATE TEMP TABLE t(i int)",
    Dialect::Postgres,
    "ddl_statement"
);
root_rejection_case!(
    root_reject_drop_table,
    "DROP TABLE t",
    Dialect::Postgres,
    "ddl_statement"
);
root_rejection_case!(
    root_reject_alter_table,
    "ALTER TABLE t ADD COLUMN c int",
    Dialect::Postgres,
    "ddl_statement"
);
root_rejection_case!(
    root_reject_truncate,
    "TRUNCATE t",
    Dialect::Postgres,
    "ddl_statement"
);
root_rejection_case!(
    root_reject_set,
    "SET default_transaction_read_only = off",
    Dialect::Postgres,
    "set_statement"
);
root_rejection_case!(
    root_reject_copy,
    "COPY t TO '/tmp/out.csv'",
    Dialect::Postgres,
    "copy_statement"
);
root_rejection_case!(
    root_reject_grant,
    "GRANT SELECT ON t TO r",
    Dialect::Postgres,
    "not_select"
);
root_rejection_case!(
    root_reject_revoke,
    "REVOKE SELECT ON t FROM r",
    Dialect::Postgres,
    "not_select"
);
root_rejection_case!(
    root_reject_call,
    "CALL my_proc()",
    Dialect::Postgres,
    "call"
);
root_rejection_case!(
    root_reject_do,
    "DO $$ BEGIN END $$",
    Dialect::Postgres,
    "do"
);
root_rejection_case!(
    root_reject_explain_analyze,
    "EXPLAIN ANALYZE SELECT 1",
    Dialect::Postgres,
    "explain_analyze"
);
root_rejection_case!(
    root_reject_lock_table,
    "LOCK TABLE t IN ACCESS EXCLUSIVE MODE",
    Dialect::Postgres,
    "unsupported_command"
);

// ---------- 可写 CTE：AST 任意位置出现写节点 ----------

macro_rules! writable_cte_case {
    ($name:ident, $sql:expr) => {
        #[test]
        fn $name() {
            let error = reject($sql, Dialect::Postgres, &schemas(&["public"]));
            assert_eq!(error.code, "MCP_SQL_REJECTED");
            assert_eq!(reason(&error), "writable_cte");
        }
    };
}

writable_cte_case!(
    writable_cte_delete_returning,
    "WITH d AS (DELETE FROM users RETURNING *) SELECT * FROM d"
);
writable_cte_case!(
    writable_cte_insert_returning,
    "WITH d AS (INSERT INTO users VALUES (1) RETURNING *) SELECT * FROM d"
);
writable_cte_case!(
    writable_cte_update_returning,
    "WITH d AS (UPDATE users SET a = 1 RETURNING *) SELECT * FROM d"
);
writable_cte_case!(
    writable_cte_merge_returning,
    "WITH d AS (MERGE INTO users u USING src s ON u.id = s.id WHEN MATCHED THEN UPDATE SET a = 1 RETURNING *) SELECT * FROM d"
);

// ---------- 行锁 ----------

macro_rules! lock_case {
    ($name:ident, $sql:expr) => {
        #[test]
        fn $name() {
            let error = reject($sql, Dialect::Postgres, &schemas(&["public"]));
            assert_eq!(error.code, "MCP_SQL_REJECTED");
            assert_eq!(reason(&error), "row_lock");
        }
    };
}

lock_case!(lock_for_update, "SELECT * FROM t FOR UPDATE");
lock_case!(lock_for_share, "SELECT * FROM t FOR SHARE");
lock_case!(lock_in_share_mode, "SELECT * FROM t LOCK IN SHARE MODE");
lock_case!(
    lock_for_update_with_where,
    "SELECT * FROM t WHERE id = 1 FOR UPDATE"
);

// ---------- 黑名单函数：只拦名单内的函数名 ----------

macro_rules! blacklisted_function_case {
    ($name:ident, $sql:expr, $dialect:expr, $function:expr) => {
        #[test]
        fn $name() {
            let error = reject($sql, $dialect, &schemas(&["public"]));
            assert_eq!(error.code, "MCP_SQL_REJECTED");
            assert_eq!(reason(&error), "blacklisted_function");
            assert_eq!(error.data["function"].as_str(), Some($function));
        }
    };
}

blacklisted_function_case!(
    blacklist_pg_read_file,
    "SELECT pg_read_file('/etc/passwd')",
    Dialect::Postgres,
    "pg_read_file"
);
blacklisted_function_case!(
    blacklist_pg_ls_dir,
    "SELECT pg_ls_dir('/')",
    Dialect::Postgres,
    "pg_ls_dir"
);
blacklisted_function_case!(
    blacklist_lo_import,
    "SELECT lo_import('/etc/passwd')",
    Dialect::Postgres,
    "lo_import"
);
blacklisted_function_case!(
    blacklist_lo_export,
    "SELECT lo_export(1, '/tmp/x')",
    Dialect::Postgres,
    "lo_export"
);
blacklisted_function_case!(
    blacklist_dblink_conn,
    "SELECT dblink_conn('host=x')",
    Dialect::Postgres,
    "dblink_conn"
);
blacklisted_function_case!(
    blacklist_dblink,
    "SELECT dblink('host=x', 'select 1')",
    Dialect::Postgres,
    "dblink"
);
blacklisted_function_case!(
    blacklist_nextval,
    "SELECT nextval('s')",
    Dialect::Postgres,
    "nextval"
);
blacklisted_function_case!(
    blacklist_setval,
    "SELECT setval('s', 1)",
    Dialect::Postgres,
    "setval"
);
blacklisted_function_case!(
    blacklist_set_config,
    "SELECT set_config('a', 'b', false)",
    Dialect::Postgres,
    "set_config"
);
blacklisted_function_case!(
    blacklist_pg_sleep,
    "SELECT pg_sleep(10)",
    Dialect::Postgres,
    "pg_sleep"
);
blacklisted_function_case!(
    blacklist_pg_sleep_for,
    "SELECT pg_sleep_for('10 seconds')",
    Dialect::Postgres,
    "pg_sleep_for"
);
blacklisted_function_case!(
    blacklist_pg_advisory_lock,
    "SELECT pg_advisory_lock(1)",
    Dialect::Postgres,
    "pg_advisory_lock"
);
blacklisted_function_case!(
    blacklist_pg_terminate_backend,
    "SELECT pg_terminate_backend(1)",
    Dialect::Postgres,
    "pg_terminate_backend"
);
blacklisted_function_case!(
    blacklist_load_file,
    "SELECT LOAD_FILE('/etc/passwd')",
    Dialect::MySql,
    "load_file"
);
blacklisted_function_case!(blacklist_sleep, "SELECT SLEEP(10)", Dialect::MySql, "sleep");

macro_rules! non_blacklisted_function_case {
    ($name:ident, $sql:expr, $dialect:expr) => {
        #[test]
        fn $name() {
            let result = validate($sql, $dialect, &schemas(&["public"]))
                .unwrap_or_else(|error| panic!("期望通过，但被拒绝：{} -> {error}", $sql));
            assert_eq!(result.kind, Kind::Select);
        }
    };
}

non_blacklisted_function_case!(
    not_blacklisted_custom_function,
    "SELECT purge_inactive_records()",
    Dialect::Postgres
);
non_blacklisted_function_case!(
    not_blacklisted_builtin_aggregate,
    "SELECT count(*) FROM t",
    Dialect::Postgres
);
non_blacklisted_function_case!(
    not_blacklisted_current_timestamp,
    "SELECT current_timestamp",
    Dialect::Postgres
);
non_blacklisted_function_case!(
    not_blacklisted_mysql_builtin,
    "SELECT db_name()",
    Dialect::MySql
);
non_blacklisted_function_case!(
    not_blacklisted_string_literal_not_function,
    "SELECT * FROM t WHERE slug = 'sleep'",
    Dialect::Postgres
);

// ---------- MySQL 词法级绕过 ----------

macro_rules! executable_comment_case {
    ($name:ident, $sql:expr, $dialect:expr) => {
        #[test]
        fn $name() {
            let error = reject($sql, $dialect, &schemas(&["public"]));
            assert_eq!(error.code, "MCP_SQL_REJECTED");
            assert_eq!(reason(&error), "executable_comment");
        }
    };
}

executable_comment_case!(
    executable_comment_whole_statement_postgres,
    "/*!50000 SELECT 1 */",
    Dialect::Postgres
);
executable_comment_case!(
    executable_comment_whole_statement_mysql,
    "/*!50000 SELECT 1 */",
    Dialect::MySql
);
executable_comment_case!(
    executable_comment_after_select_postgres,
    "SELECT /*!50000 x */ 1",
    Dialect::Postgres
);
executable_comment_case!(
    executable_comment_after_select_mysql,
    "SELECT /*!50000 x */ 1",
    Dialect::MySql
);
executable_comment_case!(
    executable_comment_trailing_postgres,
    "SELECT 1 /*!50000 x */",
    Dialect::Postgres
);
executable_comment_case!(
    executable_comment_trailing_mysql,
    "SELECT 1 /*!50000 x */",
    Dialect::MySql
);
executable_comment_case!(
    executable_comment_set_names_postgres,
    "/*!40101 SET NAMES utf8 */",
    Dialect::Postgres
);
executable_comment_case!(
    executable_comment_set_names_mysql,
    "/*!40101 SET NAMES utf8 */",
    Dialect::MySql
);

#[test]
fn control_character_rejected() {
    let error = reject("SELECT 1\u{0b} FROM t", Dialect::Postgres, &[]);
    assert_eq!(error.code, "MCP_SQL_REJECTED");
    assert_eq!(reason(&error), "control_character");
    assert_eq!(error.data["codepoint"].as_u64(), Some(0x0b));
}

#[test]
fn ordinary_whitespace_accepted() {
    let result = validate(
        "SELECT\n\t1\r\nFROM t",
        Dialect::Postgres,
        &schemas(&["public"]),
    )
    .unwrap_or_else(|error| panic!("期望通过，但被拒绝：{error}"));
    assert_eq!(result.kind, Kind::Select);
}

#[test]
fn into_outfile_rejected() {
    let error = reject(
        "SELECT * FROM t INTO OUTFILE '/tmp/out.txt'",
        Dialect::MySql,
        &schemas(&["public"]),
    );
    assert_eq!(error.code, "MCP_SQL_REJECTED");
    assert_eq!(reason(&error), "into_outfile");
}

// ---------- schema 范围 ----------

#[test]
fn schema_out_of_scope() {
    let error = reject(
        "SELECT * FROM other_schema.t",
        Dialect::Postgres,
        &schemas(&["public"]),
    );
    assert_eq!(error.code, "MCP_SCHEMA_OUT_OF_SCOPE");
    assert_eq!(
        error.data["offending_tables"][0].as_str(),
        Some("other_schema.t")
    );
    assert_eq!(error.data["allowed_schemas"][0].as_str(), Some("public"));
}

#[test]
fn schema_in_scope_accepted() {
    let result = validate(
        "SELECT * FROM public.t",
        Dialect::Postgres,
        &schemas(&["public"]),
    )
    .unwrap_or_else(|error| panic!("期望通过，但被拒绝：{error}"));
    assert_eq!(result.tables, vec!["public.t".to_string()]);
}

#[test]
fn unqualified_table_uses_dialect_default_schema() {
    let result = validate("SELECT * FROM t", Dialect::Postgres, &schemas(&["public"]))
        .unwrap_or_else(|error| panic!("期望通过，但被拒绝：{error}"));
    assert_eq!(result.tables, vec!["t".to_string()]);
}

#[test]
fn unqualified_table_out_of_scope_when_default_not_allowed() {
    let error = reject("SELECT * FROM t", Dialect::Postgres, &schemas(&["other"]));
    assert_eq!(error.code, "MCP_SCHEMA_OUT_OF_SCOPE");
    assert_eq!(error.data["offending_tables"][0].as_str(), Some("t"));
}

#[test]
fn no_allowed_schemas_disables_scope_check() {
    let result = validate("SELECT * FROM any_schema.t", Dialect::Postgres, &[])
        .unwrap_or_else(|error| panic!("期望通过，但被拒绝：{error}"));
    assert_eq!(result.tables, vec!["any_schema.t".to_string()]);
}

#[test]
fn mysql_unqualified_table_resolves_to_connection_database() {
    // MySQL 无前缀表名落在连接当前库，是否越界由连接声明决定，不静态误杀。
    let result = validate("SELECT * FROM t", Dialect::MySql, &schemas(&["shop"]))
        .unwrap_or_else(|error| panic!("期望通过，但被拒绝：{error}"));
    assert_eq!(result.tables, vec!["t".to_string()]);
}

#[test]
fn mysql_qualified_table_out_of_scope() {
    let error = reject("SELECT * FROM other.t", Dialect::MySql, &schemas(&["shop"]));
    assert_eq!(error.code, "MCP_SCHEMA_OUT_OF_SCOPE");
    assert_eq!(error.data["offending_tables"][0].as_str(), Some("other.t"));
}

#[test]
fn cte_alias_is_not_counted_as_table() {
    let result = validate(
        "WITH d AS (SELECT 1) SELECT * FROM d",
        Dialect::Postgres,
        &schemas(&["public"]),
    )
    .unwrap_or_else(|error| panic!("期望通过，但被拒绝：{error}"));
    assert!(result.tables.is_empty());
}

#[test]
fn tables_are_deduplicated_in_order() {
    let result = validate(
        "SELECT * FROM a JOIN b ON a.id = b.id JOIN a ON 1 = 1",
        Dialect::Postgres,
        &schemas(&["public"]),
    )
    .unwrap_or_else(|error| panic!("期望通过，但被拒绝：{error}"));
    assert_eq!(result.tables, vec!["a".to_string(), "b".to_string()]);
}

#[test]
fn two_level_qualified_table_requires_both_parts_allowed() {
    let error = reject(
        "SELECT * FROM mydb.public.t",
        Dialect::Postgres,
        &schemas(&["public"]),
    );
    assert_eq!(error.code, "MCP_SCHEMA_OUT_OF_SCOPE");
    assert_eq!(
        error.data["offending_tables"][0].as_str(),
        Some("mydb.public.t")
    );

    let result = validate(
        "SELECT * FROM mydb.public.t",
        Dialect::Postgres,
        &schemas(&["mydb", "public"]),
    )
    .unwrap_or_else(|error| panic!("期望通过，但被拒绝：{error}"));
    assert_eq!(result.tables, vec!["mydb.public.t".to_string()]);
}

// ---------- 返回值 ----------

#[test]
fn validated_statement_fields() {
    let result = validate(
        "SELECT id FROM t WHERE id = :id",
        Dialect::Postgres,
        &schemas(&["public"]),
    )
    .unwrap_or_else(|error| panic!("期望通过，但被拒绝：{error}"));
    assert_eq!(result.kind, Kind::Select);
    assert_eq!(result.tables, vec!["t".to_string()]);
    assert!(result.has_params);
}

macro_rules! has_params_case {
    ($name:ident, $sql:expr, $dialect:expr) => {
        #[test]
        fn $name() {
            let result = validate($sql, $dialect, &[])
                .unwrap_or_else(|error| panic!("期望通过，但被拒绝：{} -> {error}", $sql));
            assert!(result.has_params, "未识别出绑定参数：{}", $sql);
        }
    };
}

has_params_case!(
    has_params_named_placeholder,
    "SELECT * FROM t WHERE id = :id",
    Dialect::Postgres
);
has_params_case!(
    has_params_positional_parameter,
    "SELECT * FROM t WHERE id = $1",
    Dialect::Postgres
);
has_params_case!(
    has_params_question_mark,
    "SELECT * FROM t WHERE id = ?",
    Dialect::MySql
);

#[test]
fn has_params_false_without_placeholders() {
    let result = validate("SELECT 1", Dialect::Postgres, &[])
        .unwrap_or_else(|error| panic!("期望通过，但被拒绝：{error}"));
    assert!(!result.has_params);
}

#[test]
fn mysql_session_variable_is_not_a_param() {
    // MySQL 的 @var 是会话变量，不是需要绑定的占位符。
    let result = validate("SELECT @a", Dialect::MySql, &[])
        .unwrap_or_else(|error| panic!("期望通过，但被拒绝：{error}"));
    assert!(!result.has_params);
}

#[test]
fn union_kind() {
    let result = validate("SELECT 1 UNION ALL SELECT 2", Dialect::Postgres, &[])
        .unwrap_or_else(|error| panic!("期望通过，但被拒绝：{error}"));
    assert_eq!(result.kind, Kind::Union);
}

#[test]
fn subquery_kind() {
    let result = validate("(SELECT 1)", Dialect::Postgres, &[])
        .unwrap_or_else(|error| panic!("期望通过，但被拒绝：{error}"));
    assert_eq!(result.kind, Kind::Subquery);
}

#[test]
fn parse_error_rejected() {
    let error = reject("SELECT FROM WHERE (", Dialect::Postgres, &[]);
    assert_eq!(error.code, "MCP_SQL_REJECTED");
    assert_eq!(reason(&error), "parse_error");
}

// ---------- ClickHouse 外部访问/执行函数 ----------

#[test]
fn clickhouse_external_functions_are_rejected() {
    let functions = [
        "url",
        "file",
        "s3",
        "s3Cluster",
        "remote",
        "remoteSecure",
        "jdbc",
        "mysql",
        "postgresql",
        "sqlite",
        "mongodb",
        "redis",
        "odbc",
        "hdfs",
        "azureBlobStorage",
        "gcs",
        "iceberg",
        "deltaLake",
        "hudi",
        "executable",
        "format",
        "input",
        "cluster",
        "clusterAllReplicas",
        "distributed",
        "merge",
        "zookeeper",
        "arrowFlight",
    ];
    for function in functions {
        let sql = format!("SELECT * FROM {function}('x')");
        let error = reject(&sql, Dialect::ClickHouse, &[]);
        assert_eq!(error.code, "MCP_SQL_REJECTED", "{function}: {sql}");
        assert_eq!(reason(&error), "blacklisted_function", "{function}: {sql}");
    }
}

#[test]
fn clickhouse_external_function_is_rejected_inside_select_expression() {
    let error = reject(
        "SELECT url('http://example.invalid/x')",
        Dialect::ClickHouse,
        &[],
    );
    assert_eq!(error.code, "MCP_SQL_REJECTED");
    assert_eq!(reason(&error), "blacklisted_function");
}

#[test]
fn clickhouse_internal_table_functions_still_allowed() {
    for sql in [
        "SELECT count() FROM system.numbers LIMIT 1",
        "SELECT * FROM numbers(3)",
    ] {
        validate(sql, Dialect::ClickHouse, &[])
            .unwrap_or_else(|error| panic!("期望通过，但被拒绝：{sql} -> {error:?}"));
    }
}

#[test]
fn clickhouse_other_schema_table_functions_are_rejected() {
    // cluster/merge 的目标库写在参数里，不是 AST 里的表引用，schema 白名单看不到它们。
    for sql in [
        "SELECT * FROM cluster('c', other_db.events)",
        "SELECT * FROM cluster('c', 'other_db', 'events')",
        "SELECT merge('other_db', '.*') FROM numbers(1)",
    ] {
        let error = reject(sql, Dialect::ClickHouse, &schemas(&["default"]));
        assert_eq!(error.code, "MCP_SQL_REJECTED", "{sql}");
        assert_eq!(reason(&error), "blacklisted_function", "{sql}");
    }
}

#[test]
fn clickhouse_table_named_like_dangerous_function_is_allowed() {
    validate(
        "SELECT * FROM default.merge",
        Dialect::ClickHouse,
        &schemas(&["default"]),
    )
    .unwrap_or_else(|error| panic!("无参数调用不是表函数，应通过：{error:?}"));
}

#[test]
fn clickhouse_settings_clause_is_rejected() {
    for sql in [
        "SELECT 1 SETTINGS readonly=0",
        "SELECT number FROM system.numbers LIMIT 10 SETTINGS max_result_rows=0",
        "SELECT 1 SETTINGS max_block_size = 100000",
    ] {
        let error = reject(sql, Dialect::ClickHouse, &[]);
        assert_eq!(error.code, "MCP_SQL_REJECTED", "{sql}");
        assert_eq!(reason(&error), "settings_clause", "{sql}");
    }
    // 列名/别名含 settings 不是子句，不能误伤。
    validate("SELECT 1 AS settings", Dialect::ClickHouse, &[])
        .unwrap_or_else(|error| panic!("应通过，但被拒绝：{error:?}"));
    validate("SELECT 1", Dialect::ClickHouse, &[])
        .unwrap_or_else(|error| panic!("应通过，但被拒绝：{error:?}"));
}
