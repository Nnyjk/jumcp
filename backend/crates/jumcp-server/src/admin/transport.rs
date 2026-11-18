//! 传输密钥下发：管理台拿到与进程同一把 `JUMCP_TRANSPORT_KEY` 后，在浏览器里
//! 用 AES-256-GCM 加密 secret 字段再提交 `enc:v1:` 密文。
//!
//! 为什么经鉴权端点下发而不是编译进前端 bundle：静态资源不需要登录就能下载，密钥
//! 一旦进构建产物就等于公开；这里只在管理面 Bearer 鉴权之后返回。`Cache-Control:
//! no-store` 防止密钥被代理/浏览器磁盘缓存留存。
//!
//! 边界：它只让"请求体里的密码是密文"，不替代 TLS——共享密钥随页面下发，能读到
//! 页面与响应的中间人同样能拿到它。

use super::*;

/// `GET /admin/transport-key`：返回 `{"algorithm","key"}`（key 为 64 位小写 hex）。
///
/// 未配置即 503：HTTP 模式启动期已 fail-closed，这条路径只对非 HTTP 构造的状态可达。
pub(super) async fn get_transport_key(State(state): State<AdminState>) -> Response {
    let Some(key) = state.transport_key.as_ref() else {
        return ApiError::from(McpError::with_message(
            "MCP_TRANSPORT_KEY_MISSING",
            "JUMCP_TRANSPORT_KEY 未设置：HTTP 模式必须注入 64 位十六进制密钥（生成方式：openssl rand -hex 32）"
                .to_string(),
            json!({"hint": "openssl rand -hex 32"}),
        ))
        .into_response();
    };
    let mut response = Json(json!({
        "algorithm": "AES-256-GCM",
        "key": key.to_hex(),
    }))
    .into_response();
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}
