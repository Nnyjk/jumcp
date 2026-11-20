//! 第三方 SQL 的前置拦截：调用上游 MCP 之前的最后一道闸门。
//!
//! 上游 MCP 是第三方实现，是否校验 SQL 不由本服务决定；本服务是唯一收口点，
//! 因此恶意注入与大范围扫描必须在转发前就被挡下，并把原因作为可读错误回给 agent。
//!
//! 为什么不用 `jumcp-sqlguard::validate`：那条路径面向**已知方言**的通用只读入口，
//! 规则集（可执行注释、行锁、函数黑名单）与本层契约（固定 10 条 rule、LIMIT 上限、
//! 纯聚合放行、审计按 rule 聚合）不对应；且它要求先选定解析方言，而第三方工具的
//! SQL 方言不可静态确定。这里只做与契约一一对应的词法级判定，不解析 AST。

use serde_json::Value;

/// 拦截结果。`rule` 是稳定标识：管理台/告警按它聚合，不解析文案。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlRejection {
    /// 命中的规则名（见 [`check`] 的规则表）。
    pub rule: &'static str,
    /// 面向调用方的一句话原因。
    pub message: String,
    /// 可执行的改法；agent 拿到它就能自己修正查询。
    pub hint: String,
}

impl SqlRejection {
    fn new(rule: &'static str, message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            rule,
            message: message.into(),
            hint: hint.into(),
        }
    }
}

/// 前置拦截配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardConfig {
    /// 允许访问的库；空表示不做库范围限定（`schema_out_of_scope` 规则整体跳过）。
    pub allowed_schemas: Vec<String>,
    /// 单次查询允许的最大 LIMIT；无 LIMIT 的宽查询按 `unbounded` 拒绝。
    pub max_limit: i64,
}

impl Default for GuardConfig {
    fn default() -> Self {
        Self {
            allowed_schemas: Vec::new(),
            max_limit: 1000,
        }
    }
}

impl GuardConfig {
    /// 部署侧配置：`JUMCP_SQL_ALLOWED_SCHEMAS`（逗号分隔，空 = 不做库范围限定）。
    ///
    /// `max_limit` 不从 env 开放：它是"一次调用能带走多少行"的安全承诺，
    /// 可配置就等于把它降级成运维便利项；要改必须走代码评审。
    pub fn from_env() -> Self {
        Self {
            allowed_schemas: parse_allowed_schemas(
                &std::env::var("JUMCP_SQL_ALLOWED_SCHEMAS").unwrap_or_default(),
            ),
            ..Self::default()
        }
    }
}

/// 逗号分隔的库名列表：去空白、丢空项、去重（大小写不敏感，判定时统一小写比较）。
fn parse_allowed_schemas(raw: &str) -> Vec<String> {
    let mut schemas: Vec<String> = Vec::new();
    for entry in raw.split(',') {
        let schema = entry.trim();
        if schema.is_empty() || schemas.iter().any(|kept| kept.eq_ignore_ascii_case(schema)) {
            continue;
        }
        schemas.push(schema.to_string());
    }
    schemas
}

/// 工具入参里代表 SQL 的字段名（按顺序取第一个非空字符串）。
const SQL_ARGUMENT_KEYS: &[&str] = &["sql", "query", "statement"];

/// 写/DDL/管理类关键字：任何位置出现都说明不是只读查询。
const WRITE_KEYWORDS: &[&str] = &[
    "INSERT", "UPDATE", "DELETE", "ALTER", "DROP", "CREATE", "TRUNCATE", "RENAME", "GRANT",
    "REVOKE", "ATTACH", "DETACH", "OPTIMIZE", "SYSTEM", "KILL", "SET",
];

/// 绕过表/库白名单的函数：直接按字典/Join 键取值，不经过表引用校验。
const BYPASS_FUNCTIONS: &[&str] = &["dictget", "dictgetordefault", "joinget"];

/// 聚合函数：无 WHERE、无 LIMIT 但只返回聚合值时不会是全表结果集。
const AGGREGATE_FUNCTIONS: &[&str] = &["count", "sum", "avg", "min", "max", "uniq", "quantile"];

/// FROM/JOIN 引用区的结束词：到这里表引用已经结束，后续是条件/分组/排序等。
const TABLE_CLAUSE_BOUNDARIES: &[&str] = &[
    "where",
    "prewhere",
    "group",
    "order",
    "having",
    "limit",
    "offset",
    "union",
    "except",
    "intersect",
    "join",
    "on",
    "using",
    "settings",
    "format",
    "sample",
    "window",
    "qualify",
    "into",
    "from",
    "select",
    "with",
    "final",
];

/// 从工具入参里取出待校验的 SQL：`sql` → `query` → `statement`，优先取第一个非空字符串。
///
/// 只认这三个字段：其余字段（表名、过滤条件）由上游自己解释，本层无权也不该猜。
/// 三个字段都存在但全为空白时**仍返回空白串**：那是"声明了 SQL 却什么都没给"，
/// 必须走 `empty` 拒绝而不是被当成非 SQL 调用放过去。
pub fn sql_argument(arguments: &Value) -> Option<&str> {
    let map = arguments.as_object()?;
    let mut blank: Option<&str> = None;
    for key in SQL_ARGUMENT_KEYS {
        let Some(sql) = map.get(*key).and_then(Value::as_str) else {
            continue;
        };
        if !sql.trim().is_empty() {
            return Some(sql);
        }
        blank.get_or_insert(sql);
    }
    blank
}

/// 校验一条即将发往第三方的 SQL 是否允许执行。
///
/// 规则（每条固定 `rule` 名，顺序即优先级）：
///
/// | rule | 触发 |
/// |---|---|
/// | `empty` | trim 后为空 |
/// | `comment` | 含 `--` 或 `/*`（注释是常见的注入/绕过载体） |
/// | `not_select` | 首个关键字不是 `SELECT` / `WITH`（允许前置括号） |
/// | `multiple_statements` | 去掉字符串字面量与结尾分号后仍含 `;` |
/// | `write_statement` | 出现写/DDL/管理关键字（跳过字符串字面量与 `db.x` 限定名里的库名） |
/// | `system_table` | 引用 `system.` 前缀的表 |
/// | `bypass_function` | 调用 `dictGet` / `dictGetOrDefault` / `joinGet` |
/// | `schema_out_of_scope` | `FROM` / `JOIN` 引用区的 `db.table` 里 `db` 不在白名单（白名单为空则跳过） |
/// | `limit_too_large` | 存在 `LIMIT n` 且 `n > max_limit` |
/// | `unbounded` | 既无 `LIMIT` 也无 `WHERE`，且不是纯聚合查询 |
///
/// 判定全部基于**词法**：字符串字面量先被抹成空白，因此字面量里的 `;`、`DROP`、
/// `--` 都不会误伤；注释本身被拒绝，所以不需要在注释里做关键字判定。
pub fn check(sql: &str, cfg: &GuardConfig) -> Result<(), SqlRejection> {
    if sql.trim().is_empty() {
        return Err(SqlRejection::new(
            "empty",
            "SQL 为空：没有可执行的语句。",
            "传入一条 SELECT 查询后重试。",
        ));
    }

    let masked = mask(sql);
    if masked.has_comment {
        return Err(SqlRejection::new(
            "comment",
            "SQL 含注释（-- 或 /* */）：注释常用于隐藏注入片段。",
            "删除注释后重试。",
        ));
    }

    let tokens = tokenize(&masked.text);
    match tokens.iter().find(|token| **token != Token::Punct('(')) {
        Some(Token::Word(word))
            if word.eq_ignore_ascii_case("select") || word.eq_ignore_ascii_case("with") => {}
        _ => {
            return Err(SqlRejection::new(
                "not_select",
                "只允许 SELECT / WITH 查询，当前语句不是查询。",
                "把语句改写为 SELECT ...（或 WITH ... SELECT ...）后重试；写操作请走其他通道。",
            ));
        }
    }

    reject_multiple_statements(&tokens)?;
    reject_write_keywords(&tokens)?;
    reject_system_tables(&tokens)?;
    reject_bypass_functions(&tokens)?;
    reject_schema_out_of_scope(&tokens, cfg)?;
    reject_bounded(&tokens, cfg)
}

/// 掩码后的 SQL：字符串字面量内容替换为空格，注释同样抹掉并只保留"出现过"的标记。
struct Masked {
    text: String,
    has_comment: bool,
}

/// 把字符串字面量与注释从 SQL 里抹掉（保持长度无关，只用空白占位）。
///
/// 为什么必须抹字符串：`WHERE name = 'a; DROP TABLE t'` 里的分号与关键字是数据，
/// 不是语句结构；不抹就会把合法查询当成注入，也会把真正的注入藏进字面量里漏判。
fn mask(sql: &str) -> Masked {
    let chars: Vec<char> = sql.chars().collect();
    let mut text = String::with_capacity(sql.len());
    let mut has_comment = false;
    let mut index = 0;
    while index < chars.len() {
        let current = chars[index];
        if matches!(current, '\'' | '"' | '`') {
            let quote = current;
            text.push(' ');
            index += 1;
            while index < chars.len() {
                let inner = chars[index];
                if inner == '\\' && index + 1 < chars.len() {
                    text.push(' ');
                    text.push(' ');
                    index += 2;
                    continue;
                }
                if inner == quote {
                    // 连写两个引号是转义，不是字面量结束。
                    if index + 1 < chars.len() && chars[index + 1] == quote {
                        text.push(' ');
                        text.push(' ');
                        index += 2;
                        continue;
                    }
                    text.push(' ');
                    index += 1;
                    break;
                }
                text.push(' ');
                index += 1;
            }
            continue;
        }
        if current == '-' && chars.get(index + 1) == Some(&'-') {
            has_comment = true;
            while index < chars.len() && chars[index] != '\n' {
                text.push(' ');
                index += 1;
            }
            continue;
        }
        if current == '/' && chars.get(index + 1) == Some(&'*') {
            has_comment = true;
            text.push_str("  ");
            index += 2;
            while index < chars.len() {
                if chars[index] == '*' && chars.get(index + 1) == Some(&'/') {
                    text.push_str("  ");
                    index += 2;
                    break;
                }
                text.push(' ');
                index += 1;
            }
            continue;
        }
        text.push(current);
        index += 1;
    }
    Masked { text, has_comment }
}

/// 词法单元：只有"单词"与"标点"两类，空白一律丢弃。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token<'a> {
    Word(&'a str),
    Punct(char),
}

fn tokenize(text: &str) -> Vec<Token<'_>> {
    let mut tokens = Vec::new();
    let mut chars = text.char_indices().peekable();
    while let Some((start, current)) = chars.next() {
        if current.is_whitespace() {
            continue;
        }
        if is_word_char(current) {
            let mut end = start + current.len_utf8();
            while let Some(&(next_index, next)) = chars.peek() {
                if !is_word_char(next) {
                    break;
                }
                end = next_index + next.len_utf8();
                chars.next();
            }
            tokens.push(Token::Word(&text[start..end]));
            continue;
        }
        tokens.push(Token::Punct(current));
    }
    tokens
}

fn is_word_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

/// 该单词是否属于 `x.word` / `word.x` 形式的限定名（此时它是库名/列名，不是语句关键字）。
fn is_qualified(tokens: &[Token<'_>], index: usize) -> bool {
    let previous_is_dot = index > 0 && tokens[index - 1] == Token::Punct('.');
    let next_is_dot = tokens.get(index + 1) == Some(&Token::Punct('.'));
    previous_is_dot || next_is_dot
}

fn reject_multiple_statements(tokens: &[Token<'_>]) -> Result<(), SqlRejection> {
    let mut body = tokens;
    if body.last() == Some(&Token::Punct(';')) {
        body = &body[..body.len() - 1];
    }
    if body.contains(&Token::Punct(';')) {
        return Err(SqlRejection::new(
            "multiple_statements",
            "一次只允许执行一条 SQL，检测到多条语句。",
            "只保留其中一条 SELECT，去掉其它语句后重试。",
        ));
    }
    Ok(())
}

fn reject_write_keywords(tokens: &[Token<'_>]) -> Result<(), SqlRejection> {
    for (index, token) in tokens.iter().enumerate() {
        let Token::Word(word) = token else {
            continue;
        };
        let upper = word.to_ascii_uppercase();
        if !WRITE_KEYWORDS.contains(&upper.as_str()) || is_qualified(tokens, index) {
            continue;
        }
        return Err(SqlRejection::new(
            "write_statement",
            format!("SQL 含写/DDL/管理关键字 {upper}，只允许只读查询。"),
            format!("去掉 {upper} 对应的语句，只保留 SELECT 后重试。"),
        ));
    }
    Ok(())
}

fn reject_system_tables(tokens: &[Token<'_>]) -> Result<(), SqlRejection> {
    for (index, token) in tokens.iter().enumerate() {
        let Token::Word(word) = token else {
            continue;
        };
        if !word.eq_ignore_ascii_case("system") {
            continue;
        }
        // 只跳过 `db.system` 这种被别人限定的写法：`system.tables` 本身就是要拒的形状。
        if index > 0 && tokens[index - 1] == Token::Punct('.') {
            continue;
        }
        if tokens.get(index + 1) == Some(&Token::Punct('.'))
            && matches!(tokens.get(index + 2), Some(Token::Word(_)))
        {
            return Err(SqlRejection::new(
                "system_table",
                "禁止查询 system.* 系统表。",
                "改为查询业务库中的表，不要引用 system.*。",
            ));
        }
    }
    Ok(())
}

fn reject_bypass_functions(tokens: &[Token<'_>]) -> Result<(), SqlRejection> {
    for (index, token) in tokens.iter().enumerate() {
        let Token::Word(word) = token else {
            continue;
        };
        let lowered = word.to_ascii_lowercase();
        if !BYPASS_FUNCTIONS.contains(&lowered.as_str()) {
            continue;
        }
        if tokens.get(index + 1) != Some(&Token::Punct('(')) {
            continue;
        }
        return Err(SqlRejection::new(
            "bypass_function",
            format!("禁止调用 {word}：它绕过表名/库白名单校验。"),
            "改用普通 SELECT ... FROM <表> 读取同样的数据。",
        ));
    }
    Ok(())
}

/// 校验 FROM/JOIN 引用区的库名：`db.table` 的 `db` 必须在白名单内。
///
/// 只看表引用区而不是全句的 `x.y`：SELECT 列表与 WHERE 里的 `t.column` 是别名引用，
/// 按它们判库名会把正常查询误杀。
fn reject_schema_out_of_scope(tokens: &[Token<'_>], cfg: &GuardConfig) -> Result<(), SqlRejection> {
    if cfg.allowed_schemas.is_empty() {
        return Ok(());
    }
    let allowed: Vec<String> = cfg
        .allowed_schemas
        .iter()
        .map(|schema| schema.to_ascii_lowercase())
        .collect();
    for (index, token) in tokens.iter().enumerate() {
        let Token::Word(keyword) = token else {
            continue;
        };
        if !(keyword.eq_ignore_ascii_case("from") || keyword.eq_ignore_ascii_case("join")) {
            continue;
        }
        let mut cursor = index + 1;
        while cursor < tokens.len() {
            if let Token::Word(boundary) = tokens[cursor]
                && TABLE_CLAUSE_BOUNDARIES.contains(&boundary.to_ascii_lowercase().as_str())
            {
                break;
            }
            if let Some(Token::Word(schema)) = tokens.get(cursor).copied()
                && tokens.get(cursor + 1) == Some(&Token::Punct('.'))
                && matches!(tokens.get(cursor + 2), Some(Token::Word(_)))
                && starts_like_identifier(schema)
                && !allowed.contains(&schema.to_ascii_lowercase())
            {
                return Err(SqlRejection::new(
                    "schema_out_of_scope",
                    format!("SQL 引用了允许范围外的库 {schema}。"),
                    format!(
                        "只查询允许的库（{}），或联系管理员加入 JUMCP_SQL_ALLOWED_SCHEMAS。",
                        cfg.allowed_schemas.join(", ")
                    ),
                ));
            }
            cursor += 1;
        }
    }
    Ok(())
}

/// 库名必须像标识符：排除 `1.5` 这类数字字面量被误当成 `db.table`。
fn starts_like_identifier(word: &str) -> bool {
    word.chars()
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
}

/// LIMIT 上限与"大范围扫描"判定。
///
/// 这里的 `WHERE` 视为范围收敛：点查（`WHERE id = 1`）不需要 LIMIT 也不构成大范围扫描；
/// 无 WHERE 的宽查询必须显式 LIMIT，上限由配置钉死。
fn reject_bounded(tokens: &[Token<'_>], cfg: &GuardConfig) -> Result<(), SqlRejection> {
    let mut largest_limit: Option<i64> = None;
    let mut too_large: Option<i64> = None;
    for (index, token) in tokens.iter().enumerate() {
        let Token::Word(word) = token else {
            continue;
        };
        if !word.eq_ignore_ascii_case("limit") || is_qualified(tokens, index) {
            continue;
        }
        let Some(value) = limit_value(tokens, index) else {
            continue;
        };
        largest_limit = Some(largest_limit.map_or(value, |current| current.max(value)));
        if value > cfg.max_limit {
            too_large = Some(value);
        }
    }

    if let Some(value) = too_large {
        return Err(SqlRejection::new(
            "limit_too_large",
            format!("LIMIT {value} 超过单次查询上限 {}。", cfg.max_limit),
            format!(
                "把 LIMIT 改为 ≤{}（或缩小 WHERE 范围分页取数）后重试。",
                cfg.max_limit
            ),
        ));
    }

    let has_limit = largest_limit.is_some();
    let has_where = tokens
        .iter()
        .any(|token| matches!(token, Token::Word(word) if word.eq_ignore_ascii_case("where")));
    if has_limit || has_where {
        return Ok(());
    }
    let aggregate_only = !has_select_star(tokens) && has_aggregate(tokens);
    if aggregate_only {
        return Ok(());
    }
    Err(SqlRejection::new(
        "unbounded",
        "查询既无 LIMIT 也无 WHERE，会全表/全范围扫描。",
        format!("加上 LIMIT（≤{}）或 WHERE 过滤条件后重试。", cfg.max_limit),
    ))
}

/// 取 `LIMIT` 后的行数：`LIMIT n`、`LIMIT offset, n`（取第二个数字）、`LIMIT n OFFSET m`（取 n）。
fn limit_value(tokens: &[Token<'_>], index: usize) -> Option<i64> {
    let Some(Token::Word(first)) = tokens.get(index + 1).copied() else {
        return None;
    };
    let first_value = first.parse::<i64>().ok()?;
    if tokens.get(index + 2) == Some(&Token::Punct(','))
        && let Some(Token::Word(second)) = tokens.get(index + 3).copied()
    {
        return second.parse::<i64>().ok().or(Some(first_value));
    }
    Some(first_value)
}

fn has_select_star(tokens: &[Token<'_>]) -> bool {
    tokens.windows(2).any(|pair| {
        matches!(pair[0], Token::Word(word) if word.eq_ignore_ascii_case("select"))
            && pair[1] == Token::Punct('*')
    })
}

fn has_aggregate(tokens: &[Token<'_>]) -> bool {
    tokens.windows(2).any(|pair| {
        matches!(pair[0], Token::Word(word)
            if AGGREGATE_FUNCTIONS.contains(&word.to_ascii_lowercase().as_str()))
            && pair[1] == Token::Punct('(')
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn allow(sql: &str) -> Result<(), SqlRejection> {
        check(sql, &GuardConfig::default())
    }

    /// 字面量里的分号/关键字/注释都是数据：抹不干净会误杀合法查询。
    #[test]
    fn string_literals_are_not_statement_structure() {
        assert_eq!(
            allow("SELECT id FROM env_air.t WHERE name = 'a; DROP TABLE t' AND id = 1"),
            Ok(())
        );
        assert_eq!(
            allow("SELECT id FROM env_air.t WHERE note = '-- x' AND id = 1"),
            Ok(())
        );
    }

    #[test]
    fn cte_is_a_query() {
        assert_eq!(
            allow("WITH x AS (SELECT 1 AS id) SELECT id FROM env_air.t WHERE id = 1"),
            Ok(())
        );
    }

    /// `LIMIT offset, rows` 的行数是第二个数字：按 offset 判上限会把分页查询全部误杀。
    #[test]
    fn limit_offset_form_reads_the_row_count() {
        assert_eq!(allow("SELECT id FROM env_air.t LIMIT 5, 1000"), Ok(()));
        assert_eq!(
            allow("SELECT id FROM env_air.t LIMIT 5, 5000")
                .expect_err("行数超限必须拒绝")
                .rule,
            "limit_too_large"
        );
    }

    #[test]
    fn schema_rule_skips_alias_references() {
        let cfg = GuardConfig {
            allowed_schemas: vec!["env_air".into()],
            ..GuardConfig::default()
        };
        // SELECT 列表与 WHERE 里的 `t.col` 是别名引用，不是库名。
        assert_eq!(
            check(
                "SELECT t.id FROM env_air.t AS t WHERE t.kind = 'a' LIMIT 10",
                &cfg
            ),
            Ok(())
        );
        assert_eq!(
            check("SELECT id FROM other.t LIMIT 10", &cfg)
                .expect_err("白名单外的库必须拒绝")
                .rule,
            "schema_out_of_scope"
        );
    }

    #[test]
    fn blank_sql_argument_still_counts_as_sql() {
        assert_eq!(sql_argument(&json!({"sql": "  "})), Some("  "));
        assert_eq!(
            sql_argument(&json!({"sql": "  ", "query": "SELECT 1 LIMIT 1"})),
            Some("SELECT 1 LIMIT 1")
        );
        assert_eq!(sql_argument(&json!({"msg": "hi"})), None);
        assert_eq!(sql_argument(&json!({"sql": 5})), None);
    }
}
