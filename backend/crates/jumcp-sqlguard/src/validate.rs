//! 只读 SQL 的 AST 层校验（L4 纵深防御）。
//!
//! 设计约束：
//!
//! - 只做**语法级**判定：单语句、根节点为查询、命中函数黑名单、schema 不越界。
//! - 不做**语义级**判定：能否读到数据由数据源账号权限（L1）保证，AST 层无权也无从保证。
//! - 解析只用 sqlparser，不用正则：正则无法可靠区分字符串字面量、注释与标识符；
//!   sqlparser 看不到的部分（可执行注释、INTO OUTFILE、控制字符）另走词法层拦截。

use std::ops::ControlFlow;

use serde_json::json;
use sqlparser::ast::{
    Expr, Ident, ObjectName, Query, Select, Statement, Value, ValueWithSpan, Visit, Visitor,
};
use sqlparser::parser::Parser;

use crate::dialect::Dialect;
use crate::error::{CODE_MULTI_STATEMENT, CODE_SCHEMA_OUT_OF_SCOPE, CODE_SQL_REJECTED, GuardError};
use crate::lex::{self, Lexed, Token, TokenKind};

/// 函数黑名单：精确函数名（小写比较）。
const BLACKLIST_FUNCTIONS: &[&str] = &[
    "pg_read_file",
    "pg_ls_dir",
    "lo_import",
    "lo_export",
    "nextval",
    "setval",
    "set_config",
    "pg_terminate_backend",
    "load_file",
    "sleep",
];

/// 函数黑名单：前缀族。
const BLACKLIST_FUNCTION_PREFIXES: &[&str] = &["dblink", "pg_sleep", "pg_advisory_lock"];

/// ClickHouse 外部访问/执行类函数（表函数与标量函数共用名字）。
///
/// 这些函数能读写服务器本地文件、访问任意 URL/其他数据库或执行进程，`readonly=1`
/// 不会拦截它们；必须在 L4 按函数名拦截。`numbers` / `system.*` / `values` / `null`
/// 等正常只读表函数不在名单内。
const CLICKHOUSE_DANGEROUS_FUNCTIONS: &[&str] = &[
    "url",
    "file",
    "s3",
    "s3cluster",
    "remote",
    "remotesecure",
    "jdbc",
    "mysql",
    "postgresql",
    "sqlite",
    "mongodb",
    "redis",
    "odbc",
    "hdfs",
    "azureblobstorage",
    "gcs",
    "iceberg",
    "deltalake",
    "hudi",
    "executable",
    "format",
    "input",
    "cluster",
    "clusterallreplicas",
    "distributed",
    "merge",
    "zookeeper",
    "arrowflight",
];

/// 通过校验的只读语句。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Validated {
    /// 根节点语义类别。
    pub kind: Kind,
    /// 语句引用的表（限定名，去重且保持出现顺序；CTE 别名不计入）。
    pub tables: Vec<String>,
    /// 是否含参数占位符（`:name` / `?` / `$1`），决定调用方是否必须绑定参数。
    pub has_params: bool,
}

/// 根节点语义类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// 普通查询。
    Select,
    /// 集合运算（UNION / EXCEPT / INTERSECT）。
    Union,
    /// 括号包裹的子查询。
    Subquery,
}

impl Kind {
    /// 语义类别的稳定字符串形式。
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Select => "select",
            Kind::Union => "union",
            Kind::Subquery => "subquery",
        }
    }
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 校验单条只读 SQL。
///
/// 校验顺序固定：控制字符 → 词法层绕过（可执行注释、INTO OUTFILE）→ 语句条数 →
/// 根节点类型 → AST 结构（可写 CTE、行锁、SELECT INTO）→ 黑名单函数 → schema 范围。
///
/// # 参数
///
/// - `sql`：待校验的 SQL 文本。
/// - `dialect`：解析所用方言。
/// - `allowed_schemas`：允许访问的 schema 列表；为空表示不做 schema 限定。
///
/// # 错误
///
/// `MCP_MULTI_STATEMENT`（不是单条语句）、`MCP_SQL_REJECTED`（写语句/黑名单/解析失败）、
/// `MCP_SCHEMA_OUT_OF_SCOPE`（引用了未授权 schema）。
pub fn validate(
    sql: &str,
    dialect: Dialect,
    allowed_schemas: &[String],
) -> Result<Validated, GuardError> {
    reject_control_characters(sql)?;

    let lexed = lex::lex(sql, dialect);
    reject_executable_comments(&lexed)?;
    reject_into_outfile(&lexed)?;
    reject_clickhouse_external_functions(&lexed.tokens, dialect)?;
    reject_clickhouse_settings_clause(&lexed.tokens, dialect)?;

    let segments = lex::split_statements(sql, &lexed, dialect);
    if segments.len() != 1 {
        return Err(multi_statement(segments.len()));
    }

    let segment = &segments[0];
    let tokens = &lexed.tokens[segment.start..segment.end];

    if let RootClass::Rejected {
        reason,
        statement_type,
    } = classify_root(tokens)
    {
        return Err(rejected(reason, &statement_type));
    }

    let statement = match parse_statement(&segment.text, dialect) {
        Ok(statement) => statement,
        Err(ParseFailure::Multi(count)) => return Err(multi_statement(count)),
        Err(ParseFailure::Error(message)) => {
            // 解析器覆盖不到的构造（MySQL 的 LOCK IN SHARE MODE 等）在词法层兜底。
            if let Some(error) = lexical_rejection(tokens) {
                return Err(error);
            }
            return Err(parse_error(&message));
        }
    };

    let mut collector = Collector::default();
    let _ = statement.visit(&mut collector);

    let Statement::Query(query) = &statement else {
        return Err(rejected("not_select", "query"));
    };

    if let Some(node) = collector.write_node {
        return Err(GuardError::new(
            CODE_SQL_REJECTED,
            "禁止写语句与可写 CTE",
            json!({ "reason": "writable_cte", "node": node }),
        ));
    }
    if collector.row_lock {
        return Err(GuardError::new(
            CODE_SQL_REJECTED,
            "禁止行锁（FOR UPDATE / FOR SHARE）",
            json!({ "reason": "row_lock" }),
        ));
    }
    if collector.select_into {
        return Err(GuardError::new(
            CODE_SQL_REJECTED,
            "禁止 SELECT INTO（会创建新表）",
            json!({ "reason": "select_into" }),
        ));
    }

    for function in &collector.functions {
        if is_blacklisted_function(function) {
            return Err(GuardError::new(
                CODE_SQL_REJECTED,
                format!("禁止调用危险函数 {function}"),
                json!({ "reason": "blacklisted_function", "function": function }),
            ));
        }
    }

    let table_parts = collect_tables(&collector);
    check_schema_scope(&table_parts, dialect, allowed_schemas)?;

    let kind = classify_kind(query, tokens);
    let has_params = collector
        .placeholders
        .iter()
        .any(|placeholder| !placeholder.starts_with('@'));
    let tables = table_parts.iter().map(|parts| parts.join(".")).collect();

    Ok(Validated {
        kind,
        tables,
        has_params,
    })
}

/// 拒绝控制字符。
///
/// 解析器会把 `\x0b` 之类当空白丢掉，解析结果与数据源实际执行的可能不一致；
/// 数据源本身也不接受这类字符，直接拒绝比放行后行为不可预期更安全。
/// 制表符、换行、回车是正常 SQL 空白，不在此列。
fn reject_control_characters(sql: &str) -> Result<(), GuardError> {
    for character in sql.chars() {
        if matches!(character, '\t' | '\n' | '\r') {
            continue;
        }
        let codepoint = character as u32;
        if codepoint < 0x20 || codepoint == 0x7F {
            return Err(GuardError::new(
                CODE_SQL_REJECTED,
                format!("SQL 含有控制字符 U+{codepoint:04X}"),
                json!({ "reason": "control_character", "codepoint": codepoint }),
            ));
        }
    }
    Ok(())
}

/// 拒绝 MySQL 可执行注释 `/*! ... */`。
///
/// 解析器把 `/*! ... */` 当普通注释丢弃，但 MySQL 服务端会执行其中内容，
/// 因此 AST 里看不到的部分必须在词法层拦下。
fn reject_executable_comments(lexed: &Lexed) -> Result<(), GuardError> {
    for comment in &lexed.comments {
        if comment.block && comment.text.starts_with('!') {
            return Err(GuardError::new(
                CODE_SQL_REJECTED,
                "禁止使用可执行版本注释（/*! ... */）",
                json!({ "reason": "executable_comment" }),
            ));
        }
    }
    Ok(())
}

/// 拒绝 ClickHouse 外部访问/执行类函数。
///
/// 为什么走词法层：表函数（`FROM url(...)`、`FROM s3(...)`）在 sqlparser 的 AST 里
/// 不一定表现为 `Expr::Function`，只靠 visitor 会漏；函数名后紧跟 `(` 是可靠判据，
/// 字符串字面量与注释已被切词排除。
fn reject_clickhouse_external_functions(
    tokens: &[Token],
    dialect: Dialect,
) -> Result<(), GuardError> {
    if dialect != Dialect::ClickHouse {
        return Ok(());
    }
    for (index, token) in tokens.iter().enumerate() {
        if !matches!(token.kind, TokenKind::Word | TokenKind::QuotedIdent) {
            continue;
        }
        let name = token.text.to_ascii_lowercase();
        if !CLICKHOUSE_DANGEROUS_FUNCTIONS.contains(&name.as_str()) {
            continue;
        }
        let called = tokens
            .get(index + 1)
            .is_some_and(|next| next.kind == TokenKind::Punct && next.text == "(");
        if called {
            return Err(GuardError::new(
                CODE_SQL_REJECTED,
                format!("禁止调用 ClickHouse 外部访问函数 {name}"),
                json!({ "reason": "blacklisted_function", "function": name }),
            ));
        }
    }
    Ok(())
}

/// 拒绝 ClickHouse 查询级 `SETTINGS` 子句。
///
/// 只读与资源上限（`readonly` / `max_result_rows` / `max_block_size` 等）由 agent 在请求层
/// 统一施加；SQL 末尾的 `SETTINGS` 会在同一请求里覆盖这些值（`SETTINGS readonly=0` 可解除只读）。
/// 只读查询不需要该子句，因此在 L4 一律拒绝。
fn reject_clickhouse_settings_clause(tokens: &[Token], dialect: Dialect) -> Result<(), GuardError> {
    if dialect != Dialect::ClickHouse {
        return Ok(());
    }
    for (index, token) in tokens.iter().enumerate() {
        if token.kind != TokenKind::Word || !token.text.eq_ignore_ascii_case("SETTINGS") {
            continue;
        }
        let Some(name) = tokens.get(index + 1) else {
            continue;
        };
        if !matches!(name.kind, TokenKind::Word | TokenKind::QuotedIdent) {
            continue;
        }
        let assignment = tokens
            .get(index + 2)
            .is_some_and(|next| next.kind == TokenKind::Punct && next.text == "=");
        if assignment {
            return Err(GuardError::new(
                CODE_SQL_REJECTED,
                "禁止查询级 SETTINGS 子句：只读与资源上限由服务端统一施加".to_string(),
                json!({ "reason": "settings_clause" }),
            ));
        }
    }
    Ok(())
}

/// 拒绝 `INTO OUTFILE` / `INTO DUMPFILE`。
///
/// 该语法把结果集写到数据源服务器本地文件，解析器无法解析它，
/// 但 token 流可识别，报错原因比笼统的解析失败更准确。
fn reject_into_outfile(lexed: &Lexed) -> Result<(), GuardError> {
    let tokens = &lexed.tokens;
    for index in 0..tokens.len() {
        if !tokens[index].is_word("INTO") {
            continue;
        }
        let Some(name) = tokens.get(index + 1) else {
            continue;
        };
        if !(name.is_word("OUTFILE") || name.is_word("DUMPFILE")) {
            continue;
        }
        if tokens
            .get(index + 2)
            .is_some_and(|token| token.kind == TokenKind::Str)
        {
            return Err(GuardError::new(
                CODE_SQL_REJECTED,
                "禁止使用 INTO OUTFILE / DUMPFILE",
                json!({ "reason": "into_outfile" }),
            ));
        }
    }
    Ok(())
}

/// 根节点判定结果。
enum RootClass {
    /// 查询类语句，进入 AST 层判定。
    Query,
    /// 非查询语句，直接拒绝。
    Rejected {
        /// 拒绝原因（稳定标识）。
        reason: &'static str,
        /// 语句类型（用于审计与错误回显）。
        statement_type: String,
    },
}

/// 根节点类型判定。
///
/// 只看首个有效 token：查询语句以 `SELECT` / `WITH` / `(` 开头，
/// 其余按写语句、DDL、会话控制等分类拒绝。
fn classify_root(tokens: &[Token]) -> RootClass {
    let Some(first) = tokens.first() else {
        return RootClass::Rejected {
            reason: "not_select",
            statement_type: "empty".to_string(),
        };
    };
    if first.kind == TokenKind::Punct && first.text == "(" {
        return RootClass::Query;
    }
    if first.kind != TokenKind::Word {
        return RootClass::Rejected {
            reason: "not_select",
            statement_type: "expression".to_string(),
        };
    }

    let keyword = first.text.to_ascii_uppercase();
    let reason: &'static str = match keyword.as_str() {
        "SELECT" | "WITH" => return RootClass::Query,
        "INSERT" | "UPDATE" | "DELETE" | "MERGE" => "write_statement",
        "CREATE" | "DROP" | "ALTER" | "TRUNCATE" => "ddl_statement",
        "SET" => {
            // SET ROLE 是角色切换命令，不是变量赋值。
            if tokens.get(1).is_some_and(|token| token.is_word("ROLE")) {
                "unsupported_command"
            } else {
                "set_statement"
            }
        }
        "COPY" => "copy_statement",
        "CALL" => "call",
        "DO" => "do",
        // 会话与对象管理类语句：不是查询，一律拒绝，标签与参考实现保持一致。
        "GRANT" | "REVOKE" | "VALUES" | "TABLE" | "COMMIT" | "ROLLBACK" | "BEGIN" | "ANALYZE"
        | "COMMENT" | "LISTEN" | "NOTIFY" | "UNLISTEN" | "CLUSTER" | "DISCARD" | "DEALLOCATE" => {
            "not_select"
        }
        "LOCK" | "RENAME" | "RESET" | "SHOW" => "unsupported_command",
        "EXPLAIN" => {
            return RootClass::Rejected {
                reason: if has_analyze(tokens) {
                    "explain_analyze"
                } else {
                    "explain"
                },
                statement_type: "explain".to_string(),
            };
        }
        "DESCRIBE" | "DESC" => {
            return RootClass::Rejected {
                reason: if has_analyze(tokens) {
                    "explain_analyze"
                } else {
                    "describe_or_explain"
                },
                statement_type: "describe".to_string(),
            };
        }
        _ => "unsupported_command",
    };
    RootClass::Rejected {
        reason,
        statement_type: keyword.to_ascii_lowercase(),
    }
}

/// `EXPLAIN` / `DESCRIBE` 之后是否带 `ANALYZE`。
fn has_analyze(tokens: &[Token]) -> bool {
    tokens
        .iter()
        .skip(1)
        .take(5)
        .any(|token| token.is_word("ANALYZE"))
}

/// 解析失败时的词法层兜底判定。
fn lexical_rejection(tokens: &[Token]) -> Option<GuardError> {
    if has_row_lock(tokens) {
        return Some(GuardError::new(
            CODE_SQL_REJECTED,
            "禁止行锁（FOR UPDATE / FOR SHARE）",
            json!({ "reason": "row_lock" }),
        ));
    }
    if let Some(node) = has_writable_cte(tokens) {
        return Some(GuardError::new(
            CODE_SQL_REJECTED,
            "禁止写语句与可写 CTE",
            json!({ "reason": "writable_cte", "node": node }),
        ));
    }
    None
}

/// 是否含行锁（`FOR UPDATE` / `FOR SHARE` / `LOCK IN SHARE MODE`）。
fn has_row_lock(tokens: &[Token]) -> bool {
    for index in 0..tokens.len() {
        if tokens[index].is_word("FOR")
            && tokens
                .get(index + 1)
                .is_some_and(|next| next.is_word("UPDATE") || next.is_word("SHARE"))
        {
            return true;
        }
        if tokens[index].is_word("LOCK")
            && tokens.get(index + 1).is_some_and(|next| next.is_word("IN"))
            && tokens
                .get(index + 2)
                .is_some_and(|next| next.is_word("SHARE"))
            && tokens
                .get(index + 3)
                .is_some_and(|next| next.is_word("MODE"))
        {
            return true;
        }
    }
    false
}

/// 是否含可写 CTE（括号包裹的写语句），返回写语句类型。
fn has_writable_cte(tokens: &[Token]) -> Option<&'static str> {
    for index in 0..tokens.len() {
        if tokens[index].kind != TokenKind::Punct || tokens[index].text != "(" {
            continue;
        }
        let Some(next) = tokens.get(index + 1) else {
            continue;
        };
        if next.kind != TokenKind::Word {
            continue;
        }
        match next.text.to_ascii_uppercase().as_str() {
            "INSERT" => return Some("insert"),
            "UPDATE" => return Some("update"),
            "DELETE" => return Some("delete"),
            "MERGE" => return Some("merge"),
            _ => {}
        }
    }
    None
}

/// 解析失败的原因。
enum ParseFailure {
    /// 解析出多条语句。
    Multi(usize),
    /// 语法错误。
    Error(String),
}

/// 解析单条语句。
fn parse_statement(sql: &str, dialect: Dialect) -> Result<Statement, ParseFailure> {
    let mut statements = Parser::parse_sql(&*dialect.sqlparser(), sql)
        .map_err(|error| ParseFailure::Error(error.to_string()))?;
    match statements.len() {
        1 => Ok(statements.remove(0)),
        0 => Err(ParseFailure::Error("空语句".to_string())),
        count => Err(ParseFailure::Multi(count)),
    }
}

/// AST 采集器：一次遍历收集判定所需的全部信息。
#[derive(Default)]
struct Collector {
    cte_names: Vec<String>,
    relations: Vec<Vec<String>>,
    functions: Vec<String>,
    placeholders: Vec<String>,
    write_node: Option<&'static str>,
    row_lock: bool,
    select_into: bool,
}

impl Visitor for Collector {
    type Break = ();

    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<()> {
        if !query.locks.is_empty() {
            self.row_lock = true;
        }
        if let Some(with) = &query.with {
            for cte in &with.cte_tables {
                self.cte_names.push(normalize_ident(&cte.alias.name));
            }
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_select(&mut self, select: &Select) -> ControlFlow<()> {
        if select.into.is_some() {
            self.select_into = true;
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_relation(&mut self, relation: &ObjectName) -> ControlFlow<()> {
        self.relations
            .push(relation.0.iter().filter_map(normalize_part).collect());
        ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
        if let Expr::Function(function) = expr
            && let Some(name) = function.name.0.last()
            && let Some(ident) = name.as_ident()
        {
            self.functions.push(ident.value.to_ascii_lowercase());
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_value(&mut self, value: &ValueWithSpan) -> ControlFlow<()> {
        if let Value::Placeholder(text) = &value.value {
            self.placeholders.push(text.clone());
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_statement(&mut self, statement: &Statement) -> ControlFlow<()> {
        if self.write_node.is_none() {
            self.write_node = match statement {
                Statement::Insert(_) => Some("insert"),
                Statement::Update(_) => Some("update"),
                Statement::Delete(_) => Some("delete"),
                Statement::Merge(_) => Some("merge"),
                _ => None,
            };
        }
        ControlFlow::Continue(())
    }
}

/// 标识符归一化：未加引号的标识符按小写比较，加引号的保留原样。
fn normalize_ident(ident: &Ident) -> String {
    if ident.quote_style.is_none() {
        ident.value.to_ascii_lowercase()
    } else {
        ident.value.clone()
    }
}

/// 限定名片段归一化；非标识符片段（如函数生成的名称）不参与表判定。
fn normalize_part(part: &sqlparser::ast::ObjectNamePart) -> Option<String> {
    part.as_ident().map(normalize_ident)
}

/// 收集语句引用的表（限定名），排除 CTE 别名引用。
fn collect_tables(collector: &Collector) -> Vec<Vec<String>> {
    let mut tables: Vec<Vec<String>> = Vec::new();
    for parts in &collector.relations {
        if parts.is_empty() {
            // 全部片段都是方言生成的函数名（非标识符）时既无 schema 也无表名可判定，
            // 记录为空会让下游按下标取 qualifier 时越界。
            continue;
        }
        if parts.len() == 1 && collector.cte_names.contains(&parts[0]) {
            continue;
        }
        if !tables.contains(parts) {
            tables.push(parts.clone());
        }
    }
    tables
}

/// 校验表引用不越出 `allowed_schemas`。
///
/// 带 schema 前缀的表按显式 schema 判定；无前缀的表按方言默认 schema 解释
/// （Postgres 系为 public，MySQL 系为连接当前库）。默认 schema 不在允许列表内即视为越界，
/// 不做"自动降级到某个允许的 schema"的隐式处理。
fn check_schema_scope(
    tables: &[Vec<String>],
    dialect: Dialect,
    allowed_schemas: &[String],
) -> Result<(), GuardError> {
    if allowed_schemas.is_empty() {
        return Ok(());
    }
    let allowed: Vec<String> = allowed_schemas
        .iter()
        .map(|schema| schema.to_ascii_lowercase())
        .collect();

    let mut offending: Vec<String> = Vec::new();
    for parts in tables {
        let qualifiers = &parts[..parts.len() - 1];
        if !qualifiers.is_empty() {
            let out_of_scope = qualifiers
                .iter()
                .any(|qualifier| !allowed.contains(&qualifier.to_ascii_lowercase()));
            if out_of_scope && !offending.contains(&parts.join(".")) {
                offending.push(parts.join("."));
            }
            continue;
        }
        if dialect.is_connection_scoped() {
            // 连接当前库不可静态确定，由数据源侧的只读账号与库级授权兜底。
            continue;
        }
        let default_allowed = dialect
            .default_schema()
            .is_some_and(|schema| allowed.iter().any(|item| item == schema));
        if !default_allowed && !offending.contains(&parts[0]) {
            offending.push(parts[0].clone());
        }
    }

    if offending.is_empty() {
        return Ok(());
    }
    Err(GuardError::new(
        CODE_SCHEMA_OUT_OF_SCOPE,
        format!("SQL 引用了允许范围外的 schema：{}", offending.join(", ")),
        json!({ "offending_tables": offending, "allowed_schemas": allowed_schemas }),
    ))
}

/// 根节点语义类别判定。
fn classify_kind(query: &Query, tokens: &[Token]) -> Kind {
    if matches!(*query.body, sqlparser::ast::SetExpr::SetOperation { .. }) {
        return Kind::Union;
    }
    let parenthesized = tokens
        .first()
        .is_some_and(|token| token.kind == TokenKind::Punct && token.text == "(");
    if parenthesized {
        Kind::Subquery
    } else {
        Kind::Select
    }
}

/// 函数名是否命中黑名单。
fn is_blacklisted_function(name: &str) -> bool {
    let lowered = name.to_ascii_lowercase();
    BLACKLIST_FUNCTIONS.contains(&lowered.as_str())
        || BLACKLIST_FUNCTION_PREFIXES
            .iter()
            .any(|prefix| lowered.starts_with(prefix))
}

fn multi_statement(count: usize) -> GuardError {
    GuardError::new(
        CODE_MULTI_STATEMENT,
        format!("只允许执行单条语句，当前解析出 {count} 条"),
        json!({ "statement_count": count }),
    )
}

fn rejected(reason: &'static str, statement_type: &str) -> GuardError {
    GuardError::new(
        CODE_SQL_REJECTED,
        format!("只允许 SELECT / UNION / WITH 查询，当前为 {statement_type}"),
        json!({ "reason": reason, "statement_type": statement_type }),
    )
}

fn parse_error(detail: &str) -> GuardError {
    GuardError::new(
        CODE_SQL_REJECTED,
        format!("SQL 解析失败：{detail}"),
        json!({ "reason": "parse_error", "detail": detail }),
    )
}
