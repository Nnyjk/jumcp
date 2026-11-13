//! 词法层扫描：切分语句、识别 AST 看不到的构造。
//!
//! sqlparser 的 AST 只覆盖它能表达的语法，MySQL 可执行注释、PostgreSQL 美元引号、
//! `INTO OUTFILE`、控制字符等必须绕开解析器、在字符/token 层拦截。
//! 本模块只做切词与定位，不做语义判定。

use crate::dialect::Dialect;

/// token 类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// 未加引号的标识符或关键字。
    Word,
    /// 加引号的标识符（`"x"`、`` `x` ``、`[x]`）。
    QuotedIdent,
    /// 字符串字面量。
    Str,
    /// 数字字面量。
    Number,
    /// 标点或运算符。
    Punct,
    /// 参数占位符（`:name`、`?`、`$1`）。
    Placeholder,
}

/// 一个词法 token。
#[derive(Debug, Clone)]
pub struct Token {
    /// token 类别。
    pub kind: TokenKind,
    /// token 文本（字符串为内容，标识符为原样大小写）。
    pub text: String,
    /// 在源串中的起始字节偏移。
    pub start: usize,
    /// 在源串中的结束字节偏移。
    pub end: usize,
}

impl Token {
    /// 是否是指定关键字（大小写不敏感）。
    pub fn is_word(&self, keyword: &str) -> bool {
        self.kind == TokenKind::Word && self.text.eq_ignore_ascii_case(keyword)
    }
}

/// 一条注释。
#[derive(Debug, Clone)]
pub struct Comment {
    /// 注释正文（不含注释定界符）。
    pub text: String,
    /// 是否为块注释。
    pub block: bool,
}

/// 切词结果。
#[derive(Debug, Clone, Default)]
pub struct Lexed {
    /// 有效 token（不含空白与注释）。
    pub tokens: Vec<Token>,
    /// 注释。
    pub comments: Vec<Comment>,
}

/// 一条语句（顶层分号之间的 token 片段）。
#[derive(Debug, Clone)]
pub struct StatementSegment {
    /// 在 `Lexed::tokens` 中的起始下标（含）。
    pub start: usize,
    /// 在 `Lexed::tokens` 中的结束下标（不含）。
    pub end: usize,
    /// 语句文本（已应用占位符改写）。
    pub text: String,
}

/// 切词。
///
/// 注释与字符串里的分号不是语句分隔符，因此切分必须在词法层完成。
pub fn lex(sql: &str, dialect: Dialect) -> Lexed {
    let mut tokens: Vec<Token> = Vec::new();
    let mut comments: Vec<Comment> = Vec::new();
    let mut i = 0usize;
    let len = sql.len();

    while i < len {
        let c = sql[i..].chars().next().unwrap();

        if c.is_whitespace() {
            i += c.len_utf8();
            continue;
        }

        // 行注释：PostgreSQL 系 `--` 恒为注释；MySQL 要求其后紧跟空白。
        if c == '-'
            && sql.get(i + 1..).is_some_and(|rest| rest.starts_with('-'))
            && dash_dash_is_comment(sql, i, dialect)
        {
            let start = i + 2;
            let end = line_end(sql, start);
            comments.push(Comment {
                text: sql[start..end].to_string(),
                block: false,
            });
            i = end;
            continue;
        }

        // `#` 行注释仅 MySQL/MariaDB 成立；其它方言里 `#` 是运算符，不能当注释，
        // 否则会把注释之后的语句从校验范围里抹掉。
        if c == '#' && matches!(dialect, Dialect::MySql | Dialect::MariaDb) {
            let start = i + 1;
            let end = line_end(sql, start);
            comments.push(Comment {
                text: sql[start..end].to_string(),
                block: false,
            });
            i = end;
            continue;
        }

        // 块注释：PostgreSQL 系允许嵌套，MySQL/SQLite/ClickHouse 不允许。
        if c == '/' && sql.get(i + 1..).is_some_and(|rest| rest.starts_with('*')) {
            let (text, end) = block_comment(sql, i, dialect);
            comments.push(Comment { text, block: true });
            i = end;
            continue;
        }

        // 字符串字面量
        if c == '\'' {
            let (content_start, content_end, end) = quoted_span(sql, i, '\'');
            tokens.push(Token {
                kind: TokenKind::Str,
                text: sql[content_start..content_end].to_string(),
                start: i,
                end,
            });
            i = end;
            continue;
        }

        // 双引号：PostgreSQL 系是标识符，MySQL 是字符串；两者都不是语句分隔符。
        if c == '"' {
            let (content_start, content_end, end) = quoted_span(sql, i, '"');
            tokens.push(Token {
                kind: TokenKind::QuotedIdent,
                text: sql[content_start..content_end].to_string(),
                start: i,
                end,
            });
            i = end;
            continue;
        }

        // 反引号标识符（MySQL）
        if c == '`' {
            let (content_start, content_end, end) = quoted_span(sql, i, '`');
            tokens.push(Token {
                kind: TokenKind::QuotedIdent,
                text: sql[content_start..content_end].to_string(),
                start: i,
                end,
            });
            i = end;
            continue;
        }

        if c == '$' {
            if let Some((content_start, content_end, end)) = dollar_quote(sql, i, dialect) {
                tokens.push(Token {
                    kind: TokenKind::Str,
                    text: sql[content_start..content_end].to_string(),
                    start: i,
                    end,
                });
                i = end;
                continue;
            }
            if let Some(end) = dollar_placeholder_end(sql, i) {
                tokens.push(Token {
                    kind: TokenKind::Placeholder,
                    text: sql[i..end].to_string(),
                    start: i,
                    end,
                });
                i = end;
                continue;
            }
        }

        // `?` 占位符
        if c == '?' {
            let mut end = i + 1;
            while end < len && sql[end..].starts_with(|ch: char| ch.is_ascii_digit()) {
                end += 1;
            }
            tokens.push(Token {
                kind: TokenKind::Placeholder,
                text: sql[i..end].to_string(),
                start: i,
                end,
            });
            i = end;
            continue;
        }

        // `:` 占位符与 `::` 类型转换
        if c == ':' {
            if sql.get(i + 1..).is_some_and(|rest| rest.starts_with("::")) {
                tokens.push(Token {
                    kind: TokenKind::Punct,
                    text: "::".to_string(),
                    start: i,
                    end: i + 2,
                });
                i += 2;
                continue;
            }
            if let Some(end) = colon_placeholder_end(sql, i) {
                tokens.push(Token {
                    kind: TokenKind::Placeholder,
                    text: sql[i..end].to_string(),
                    start: i,
                    end,
                });
                i = end;
                continue;
            }
        }

        // MySQL 会话变量 `@var`：是标识符，不是绑定占位符。
        if c == '@' {
            let mut end = i + 1;
            while end < len && sql[end..].starts_with(|ch: char| ch.is_alphanumeric() || ch == '_')
            {
                end += 1;
            }
            tokens.push(Token {
                kind: TokenKind::Word,
                text: sql[i..end].to_string(),
                start: i,
                end,
            });
            i = end;
            continue;
        }

        // 数字
        if c.is_ascii_digit() {
            let mut end = i;
            while end < len && sql[end..].starts_with(|ch: char| ch.is_ascii_digit() || ch == '.') {
                end += 1;
            }
            tokens.push(Token {
                kind: TokenKind::Number,
                text: sql[i..end].to_string(),
                start: i,
                end,
            });
            i = end;
            continue;
        }

        // 标识符与关键字
        if is_ident_start(c) {
            let mut end = i;
            while end < len
                && sql[end..].starts_with(|ch: char| ch.is_alphanumeric() || ch == '_' || ch == '$')
            {
                end += 1;
            }
            tokens.push(Token {
                kind: TokenKind::Word,
                text: sql[i..end].to_string(),
                start: i,
                end,
            });
            i = end;
            continue;
        }

        tokens.push(Token {
            kind: TokenKind::Punct,
            text: c.to_string(),
            start: i,
            end: i + c.len_utf8(),
        });
        i += c.len_utf8();
    }

    Lexed { tokens, comments }
}

/// 按顶层分号切分语句。
///
/// 只含注释或空白的片段不算语句；`?` 占位符在解析前改写为 `$n`
/// （PostgreSQL 系方言把 `?` 用作 jsonb 运算符，不改写则无法作为占位符解析）。
pub fn split_statements(sql: &str, lexed: &Lexed, dialect: Dialect) -> Vec<StatementSegment> {
    let mut segments = Vec::new();
    let mut current: Vec<usize> = Vec::new();

    let flush = |current: &mut Vec<usize>, segments: &mut Vec<StatementSegment>| {
        if current.is_empty() {
            return;
        }
        let start = current[0];
        let end = current[current.len() - 1] + 1;
        let tokens: Vec<Token> = lexed.tokens[start..end].to_vec();
        current.clear();
        segments.push(StatementSegment {
            start,
            end,
            text: render(sql, &tokens, dialect),
        });
    };

    for (index, token) in lexed.tokens.iter().enumerate() {
        if token.kind == TokenKind::Punct && token.text == ";" {
            flush(&mut current, &mut segments);
            continue;
        }
        current.push(index);
    }
    flush(&mut current, &mut segments);
    segments
}

/// 渲染语句文本，并把值位置的 `?` 占位符改写为 `$n`。
fn render(sql: &str, tokens: &[Token], dialect: Dialect) -> String {
    let mut rewritten: Vec<Option<String>> = vec![None; tokens.len()];
    if !dialect.supports_question_placeholder() {
        let mut counter = 0usize;
        for (index, token) in tokens.iter().enumerate() {
            if token.kind != TokenKind::Placeholder || !token.text.starts_with('?') {
                continue;
            }
            if !is_value_position(tokens, index) {
                continue;
            }
            counter += 1;
            rewritten[index] = Some(format!("${counter}"));
        }
    }

    let mut out = String::new();
    let mut cursor = tokens.first().map(|t| t.start).unwrap_or(0);
    for (index, token) in tokens.iter().enumerate() {
        out.push_str(&sql[cursor..token.start]);
        match &rewritten[index] {
            Some(replacement) => out.push_str(replacement),
            None => out.push_str(&sql[token.start..token.end]),
        }
        cursor = token.end;
    }
    out
}

/// `?` 是否处于值位置（占位符）而非运算符位置（PostgreSQL 的 jsonb 存在性运算符）。
///
/// 判定依据是前一个有效 token：运算符、标点或关键字之后是值位置；
/// 标识符、字面量之后是运算符位置。
fn is_value_position(tokens: &[Token], index: usize) -> bool {
    let Some(previous) = index.checked_sub(1).and_then(|i| tokens.get(i)) else {
        return true;
    };
    match previous.kind {
        TokenKind::Punct => true,
        TokenKind::Word => VALUE_INTRODUCING_KEYWORDS
            .iter()
            .any(|keyword| previous.text.eq_ignore_ascii_case(keyword)),
        TokenKind::QuotedIdent | TokenKind::Str | TokenKind::Number => false,
        TokenKind::Placeholder => false,
    }
}

/// 其后可以跟值的保留关键字。
///
/// 只收保留字：保留字不能作为未加引号的列名，
/// 因此不会把 `col ? 'key'` 误判成占位符位置。
const VALUE_INTRODUCING_KEYWORDS: &[&str] = &[
    "SELECT",
    "WHERE",
    "AND",
    "OR",
    "NOT",
    "IN",
    "VALUES",
    "LIKE",
    "ILIKE",
    "BETWEEN",
    "IS",
    "NULL",
    "TRUE",
    "FALSE",
    "RETURNING",
    "LIMIT",
    "OFFSET",
    "SET",
    "THEN",
    "ELSE",
    "WHEN",
    "AS",
    "ON",
    "BY",
    "HAVING",
    "USING",
    "ALL",
    "ANY",
    "EXISTS",
    "CASE",
    "DEFAULT",
    "DISTINCT",
    "UNION",
    "EXCEPT",
    "INTERSECT",
    "FROM",
    "JOIN",
    "INTO",
];

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

/// `--` 是否为注释起始。
///
/// MySQL 要求 `--` 后紧跟空白或行尾，否则是减号运算符。
fn dash_dash_is_comment(sql: &str, i: usize, dialect: Dialect) -> bool {
    match dialect {
        Dialect::MySql | Dialect::MariaDb => {
            match sql.get(i + 2..).and_then(|s| s.chars().next()) {
                None => true,
                Some(c) => c.is_whitespace(),
            }
        }
        _ => true,
    }
}

fn line_end(sql: &str, from: usize) -> usize {
    let mut end = from;
    while end < sql.len() {
        let c = sql[end..].chars().next().unwrap();
        if c == '\n' {
            break;
        }
        end += c.len_utf8();
    }
    end
}

/// 读取块注释，返回 (正文, 结束偏移)。
fn block_comment(sql: &str, i: usize, dialect: Dialect) -> (String, usize) {
    let nested = matches!(
        dialect,
        Dialect::Postgres | Dialect::Redshift | Dialect::DuckDb
    );
    let content_start = i + 2;
    let mut j = content_start;
    let mut depth = 1usize;
    while j < sql.len() {
        if nested && sql[j..].starts_with("/*") {
            depth += 1;
            j += 2;
            continue;
        }
        if sql[j..].starts_with("*/") {
            depth -= 1;
            j += 2;
            if depth == 0 {
                break;
            }
            continue;
        }
        j += sql[j..].chars().next().unwrap().len_utf8();
    }
    let closed = depth == 0;
    let content_end = if closed { j - 2 } else { sql.len() };
    (sql[content_start..content_end].to_string(), j)
}

/// 读取引号包裹的内容，返回 (内容起始, 内容结束, 引号后偏移)。
fn quoted_span(sql: &str, i: usize, quote: char) -> (usize, usize, usize) {
    let content_start = i + 1;
    let mut j = content_start;
    while j < sql.len() {
        let c = sql[j..].chars().next().unwrap();
        if c == '\\' {
            j += 1;
            if j < sql.len() {
                j += sql[j..].chars().next().unwrap().len_utf8();
            }
            continue;
        }
        if c == quote {
            if sql[j + 1..].starts_with(quote) {
                j += 2;
                continue;
            }
            return (content_start, j, j + 1);
        }
        j += c.len_utf8();
    }
    (content_start, sql.len(), sql.len())
}

/// 读取 PostgreSQL 美元引号字符串，返回 (内容起始, 内容结束, 结束偏移)。
fn dollar_quote(sql: &str, i: usize, dialect: Dialect) -> Option<(usize, usize, usize)> {
    if !matches!(
        dialect,
        Dialect::Postgres | Dialect::Redshift | Dialect::DuckDb
    ) {
        return None;
    }
    let mut j = i + 1;
    while j < sql.len() {
        let c = sql[j..].chars().next().unwrap();
        if c == '$' {
            break;
        }
        if !(c.is_alphanumeric() || c == '_') {
            return None;
        }
        j += c.len_utf8();
    }
    if j >= sql.len() || !sql[j..].starts_with('$') {
        return None;
    }
    let tag = &sql[i..=j];
    let content_start = j + 1;
    let tail = &sql[content_start..];
    let position = tail.find(tag)?;
    Some((
        content_start,
        content_start + position,
        content_start + position + tag.len(),
    ))
}

/// `$1` 形式的占位符结束偏移。
fn dollar_placeholder_end(sql: &str, i: usize) -> Option<usize> {
    let mut end = i + 1;
    while end < sql.len() && sql[end..].starts_with(|c: char| c.is_ascii_digit()) {
        end += 1;
    }
    if end == i + 1 { None } else { Some(end) }
}

/// `:name` 形式的占位符结束偏移。
fn colon_placeholder_end(sql: &str, i: usize) -> Option<usize> {
    let mut end = i + 1;
    while end < sql.len() && sql[end..].starts_with(|c: char| c.is_alphanumeric() || c == '_') {
        end += 1;
    }
    if end == i + 1 { None } else { Some(end) }
}
