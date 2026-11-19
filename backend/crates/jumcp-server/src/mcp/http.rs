//! `/mcp` 传输：把一次 HTTP 请求翻译成平台身份，再交给 `McpServer::handle_authenticated`。

use super::McpServer;
use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, Uri};
use axum::routing::post;
use jumcp_core::entry::Actor;
use jumcp_core::errors::McpError;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;

/// `/mcp` 路由：per-request 身份来自 `Authorization: Bearer <平台生成的 access token>`，
/// 没有该头时回落到 `?access_token=<token>`（仅本路由；管理面与内置端点面只认头）。
///
/// 放在库内（而不是 main 里）是为了让集成测试能走真实的 axum 链路验证身份派生，
/// 而不是在测试里复制一份鉴权逻辑。生产环境只有这一条鉴权分支：token 必须命中
/// registry 的 `access_tokens`（active），不存在环境变量静态 token 的兜底。
pub fn http_router(server: Arc<McpServer>) -> Router {
    Router::new()
        .route("/mcp", post(post_mcp))
        .with_state(server)
}

/// 未认证主体：只用于把"未授权的探测"写进审计链，不代表任何真实 agent。
/// 故意留空 id/role——不能借审计链给匿名调用者伪造一个人格。
fn anonymous_actor() -> Actor {
    Actor {
        tenant: "default".into(),
        role: String::new(),
        actor_type: "agent".into(),
        actor_id: String::new(),
        ..Default::default()
    }
}

fn unauthorized(server: &McpServer, detail: Value) -> Value {
    let actor = anonymous_actor();
    let e = McpError::new("MCP_UNAUTHORIZED", detail);
    let err = server
        .service()
        .policy_decision(&actor, &e, json!({"transport": "http", "surface": "mcp"}))
        .err()
        .unwrap_or(e);
    json!({
        "jsonrpc":"2.0","id":null,
        "error":{"code":-32602,"message":err.message,
                 "data":{"mcp_code": err.code, "detail": err.data}}
    })
}

/// `/mcp` 的 query 回落：只认 `access_token` 一个参数名（大小写敏感），其余参数
///（`token`/`api_key` 之类）一律不看——多一套别名就多一条凭据泄漏面。
///
/// token 是 URL-safe 字符集，因此这里不做百分号解码：编码过的值匹配不上就是未知 token，
/// 走与"没带 token"完全相同的拒绝路径。query 值绝不出现在错误体或日志里。
fn query_access_token(uri: &Uri) -> Option<String> {
    uri.query()?
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|&(key, value)| key == ACCESS_TOKEN_PARAM && !value.is_empty())
        .map(|(_, value)| value.to_string())
}

/// 查询参数名：`access_token`（OAuth 2.0 Bearer Token Usage 的既有名字，客户端不用自造）。
const ACCESS_TOKEN_PARAM: &str = "access_token";

async fn post_mcp(
    State(server): State<Arc<McpServer>>,
    uri: Uri,
    headers: HeaderMap,
    body: String,
) -> Json<Value> {
    // 身份只来自数据库 token：缺 token / token 未知 / 已撤销 / 已过期一律 MCP_UNAUTHORIZED
    //（不回落默认 analyst，也不读环境变量静态 token）。
    //
    // 取 token 的顺序：Authorization 头优先，完全没有该头时才回落到 `?access_token=`。
    // 两者都给时不看 query（不做"不一致就拒绝"的二次判定：那会把一次合法请求拖成 401，
    // 也会把 token 是否有效的差异暴露成两种失败）；缺头回落是给不支持自定义头的 MCP
    // 客户端留的通道（有的 Go 客户端只在 tools/call 上带 Authorization）。
    // 仅 `/mcp` 走这条回落：`/admin/*`、`/internal/mcp/*` 只认头，URL 上的 token 不被接受。
    let token = match crate::admin::bearer(&headers) {
        Some(token) if !token.is_empty() => token.to_string(),
        _ => match query_access_token(&uri) {
            Some(token) => token,
            None => {
                return Json(unauthorized(
                    &server,
                    json!({"hint": "需要 Authorization: Bearer <平台生成的 access token>，或在 URL 上带 ?access_token=<token>"}),
                ));
            }
        },
    };
    let record = match server.service().authenticate_mcp_token(&token) {
        Ok(Some(record)) => record,
        Ok(None) => {
            return Json(unauthorized(
                &server,
                json!({"hint": "Bearer token 未注册、已撤销或已过期"}),
            ));
        }
        Err(e) => {
            return Json(json!({
                "jsonrpc":"2.0","id":null,
                "error":{"code":-32602,"message":e.message,
                          "data":{"mcp_code": e.code, "detail": e.data}}
            }));
        }
    };
    let actor = Actor::access_token(
        &record.tenant,
        &record.caller,
        &record.role,
        &record.id,
        &record.source,
        record.groups.clone(),
    )
    .with_builtin_tools(record.builtin_tools.clone());
    let header_map: HashMap<String, String> = headers
        .iter()
        .filter_map(|(k, v)| v.to_str().ok().map(|s| (k.to_string(), s.to_string())))
        .collect();
    // 通道优先级在这里定一次（Header > _meta 会话 > 角色默认）：判定失败即整体拒绝并写
    // policy_decision，不静默降级；header 解析结果不无条件当作本请求的 scope。
    let scope = match server.http_scope(&actor, &header_map) {
        Ok(s) => s,
        Err(e) => {
            let err = server
                .service()
                .policy_decision(&actor, &e, json!({"transport": "http", "detail": e.data}))
                .err()
                .unwrap_or(e);
            return Json(json!({
                "jsonrpc":"2.0","id":null,
                "error":{"code":-32602,"message":err.message,
                          "data":{"mcp_code": err.code, "detail": err.data}}
            }));
        }
    };
    let req: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            return Json(json!({
                "jsonrpc":"2.0","id":null,
                "error":{"code":-32700,"message":"Parse error","data":{"detail": e.to_string()}}
            }));
        }
    };
    Json(
        server
            .handle_authenticated(&req, &actor, Some(&scope))
            .await,
    )
}

#[cfg(test)]
mod category_tests {
    use crate::catalog::{CATEGORY_ADMIN, CATEGORY_ANALYSIS, CATEGORY_BASIC, CATEGORY_CUSTOM};
    use crate::mcp::builtin_category;

    #[test]
    fn every_builtin_id_has_its_responsibility_category() {
        // basic：基础查询（目录走查 + 模板取数）。
        for id in [
            "search_capabilities",
            "describe_capability",
            "list_groups",
            "db_run_template",
        ] {
            assert_eq!(builtin_category(id), CATEGORY_BASIC, "{id} 应归 basic");
        }
        // analysis：工具列表分析与变更提交审计。
        for id in [
            "admin_review_queue",
            "admin_register_template",
            "admin_set_visibility",
        ] {
            assert_eq!(
                builtin_category(id),
                CATEGORY_ANALYSIS,
                "{id} 应归 analysis"
            );
        }
        // admin：仅审计操作。
        assert_eq!(
            builtin_category("admin_audit_queries"),
            CATEGORY_ADMIN,
            "admin_audit_queries 应归 admin"
        );
    }

    #[test]
    fn unknown_ids_fall_back_to_custom() {
        for id in [
            "",
            "db_query",
            "search",
            "admin_",
            "admin_unknown",
            "upstream__echo",
        ] {
            assert_eq!(builtin_category(id), CATEGORY_CUSTOM, "{id} 应归 custom");
        }
    }
}
