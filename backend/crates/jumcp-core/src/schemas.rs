//! schema 白名单收窄：请求声明的 schema 只能落在连接声明之内，不能扩大。
//!
//! 两处调用面（模板执行臂与自由查询参数解析）必须用同一份判定：各写一遍的代价不是重复
//! 代码，而是"一处补了缺省语义、另一处没补"的静默越界。

use crate::errors::McpError;
use serde_json::json;

/// 计算本次请求实际生效的 schema 集合。
///
/// - `requested` 为空 = 不额外收窄，沿用连接白名单（连接白名单也为空 = 不限制）；
/// - 连接白名单非空时，`requested` 的每一项都必须落在其中，否则
///   `MCP_SCHEMA_OUT_OF_SCOPE`（`offending_schemas` 列出越界项，`allowed` 回连接声明）。
pub fn narrow_schemas(
    connection: &[String],
    requested: &[String],
) -> Result<Vec<String>, McpError> {
    if requested.is_empty() {
        return Ok(connection.to_vec());
    }
    if !connection.is_empty() {
        let offending: Vec<&String> = requested
            .iter()
            .filter(|schema| !connection.contains(schema))
            .collect();
        if !offending.is_empty() {
            return Err(McpError::new(
                "MCP_SCHEMA_OUT_OF_SCOPE",
                json!({"offending_schemas": offending, "allowed": connection}),
            ));
        }
    }
    Ok(requested.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn empty_request_inherits_connection() {
        let connection = owned(&["public", "analytics"]);
        assert_eq!(
            narrow_schemas(&connection, &[]).unwrap(),
            owned(&["public", "analytics"])
        );
        assert_eq!(narrow_schemas(&[], &[]).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn request_must_stay_inside_connection() {
        let connection = owned(&["public"]);
        assert_eq!(
            narrow_schemas(&connection, &owned(&["public"])).unwrap(),
            owned(&["public"])
        );
        let err = narrow_schemas(&connection, &owned(&["public", "secret"])).unwrap_err();
        assert_eq!(err.code, "MCP_SCHEMA_OUT_OF_SCOPE");
        assert_eq!(err.data["offending_schemas"], json!(["secret"]));
        assert_eq!(err.data["allowed"], json!(["public"]));
    }

    #[test]
    fn empty_connection_means_unrestricted() {
        assert_eq!(
            narrow_schemas(&[], &owned(&["anything"])).unwrap(),
            owned(&["anything"])
        );
    }
}
