//! 旧模型 → 组暴露矩阵的数据迁移。
//!
//! 迁完之后**删除**旧列/旧表：留着它们就会在下次启动被再次灌回（管理员删掉的授权会复活），
//! 也会让"读哪一列"变成永久歧义。迁移在 `CapabilityStore::open` 里、任何读写之前执行，
//! 并且只按旧结构是否存在判断幂等，不额外记标记。
//!
//! 迁移只搬"原来已经生效的授权"，不补任何新授权：旧令牌的工具白名单没有等价物，
//! 升级后需要管理员给令牌显式加上 `basic` 组才能看见常驻元工具。

use crate::errors::McpError;
use rusqlite::Connection;

pub(super) fn run(conn: &Connection) -> Result<(), McpError> {
    migrate_tool_groups(conn)?;
    migrate_upstream_group_column(conn)?;
    drop_token_allowlist(conn)?;
    Ok(())
}

/// `mcp_upstream_tool_groups`（只允许 basic 的逐工具覆盖）→ `mcp_group_tool_rules`。
///
/// 旧行语义是"把该工具纳入 basic"，正好等价于第二层的 `include`。
fn migrate_tool_groups(conn: &Connection) -> Result<(), McpError> {
    if !table_exists(conn, "mcp_upstream_tool_groups")? {
        return Ok(());
    }
    conn.execute(
        "INSERT OR IGNORE INTO mcp_group_tool_rules (group_code, upstream_id, tool_name, state, updated_at) \
         SELECT group_code, upstream_id, tool_name, 'include', updated_at FROM mcp_upstream_tool_groups",
        [],
    )
    .map_err(|e| McpError::store(format!("迁移 mcp_upstream_tool_groups 失败: {e}")))?;
    conn.execute("DROP TABLE mcp_upstream_tool_groups", [])
        .map_err(|e| McpError::store(format!("删除 mcp_upstream_tool_groups 失败: {e}")))?;
    Ok(())
}

/// `mcp_upstreams.group_code`（整台上游一个组）→ `mcp_group_upstreams`（第一层授予）。
///
/// 旧列是 `NOT NULL` 且无默认值，不删就会让新写入缺列失败。
fn migrate_upstream_group_column(conn: &Connection) -> Result<(), McpError> {
    if !column_exists(conn, "mcp_upstreams", "group_code")? {
        return Ok(());
    }
    conn.execute(
        "INSERT OR IGNORE INTO mcp_group_upstreams (group_code, upstream_id, updated_at) \
         SELECT group_code, id, updated_at FROM mcp_upstreams WHERE group_code <> ''",
        [],
    )
    .map_err(|e| McpError::store(format!("迁移 mcp_upstreams.group_code 失败: {e}")))?;
    conn.execute("DROP INDEX IF EXISTS idx_upstream_group", [])
        .map_err(|e| McpError::store(format!("删除 idx_upstream_group 失败: {e}")))?;
    conn.execute("ALTER TABLE mcp_upstreams DROP COLUMN group_code", [])
        .map_err(|e| McpError::store(format!("删除 mcp_upstreams.group_code 失败: {e}")))?;
    Ok(())
}

/// `access_tokens.builtin_tools_json`（工具白名单）→ 删除。
///
/// 授权面只剩 `groups`，白名单没有等价列；旧值不可解读，直接丢弃。
fn drop_token_allowlist(conn: &Connection) -> Result<(), McpError> {
    if !column_exists(conn, "access_tokens", "builtin_tools_json")? {
        return Ok(());
    }
    conn.execute(
        "ALTER TABLE access_tokens DROP COLUMN builtin_tools_json",
        [],
    )
    .map_err(|e| McpError::store(format!("删除 access_tokens.builtin_tools_json 失败: {e}")))?;
    Ok(())
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool, McpError> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?",
            [table],
            |r| r.get(0),
        )
        .map_err(|e| McpError::store(e.to_string()))?;
    Ok(count > 0)
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool, McpError> {
    if !table_exists(conn, table)? {
        return Ok(false);
    }
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|e| McpError::store(e.to_string()))?;
    let mapped = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(|e| McpError::store(e.to_string()))?;
    for row in mapped {
        if row.map_err(|e| McpError::store(e.to_string()))? == column {
            return Ok(true);
        }
    }
    Ok(false)
}
