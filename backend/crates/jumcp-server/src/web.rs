//! 管理台静态托管：HTTP 模式下把 `admin-web/dist` 当成无头 Web 服务直接提供。
//!
//! 边界：
//! - 只在 streamable-http 模式挂载，且 `JUMCP_MCP_WEB_DIR` 为空时**完全不挂**（行为与未引入前一致）。
//! - 路径空间隔离：`/admin`、`/mcp`、`/health` 永远归后端，未知的这类路径返回 JSON 404，
//!   **不**回退成 `index.html`——否则管理 API 拼错路径会被前端当成页面，调用方拿到 200 HTML。
//! - 静态文件的路径解析交给 `tower-http` 的 `ServeDir`/`ServeFile`：不手写路径拼接，穿越由上游拒绝。
//! - 不注入任何 `Authorization`：管理台前端不持有凭证，登录靠 `POST /admin/login` 下发的 HttpOnly 会话 cookie。

use axum::body::Body;
use axum::extract::State;
use axum::http::{Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tower::ServiceExt;
use tower_http::services::{ServeDir, ServeFile};

/// 后端路径空间：这两个面上的 `/admin`、`/mcp`、`/health`、`/internal` 都**归后端**，
/// 未命中也只回 JSON 404，绝不回退 SPA。
///
/// 管理面的清单**故意包含** `/mcp` 与 `/internal`：那不是它提供的路由，而是它必须拒绝的
/// 另一个面的命名空间。少写这条，浏览器访问 `/mcp/typo` 会拿到一张管理台 HTML（200），
/// 排查时看到的是"页面正常"，而真正的问题是端点在另一个端口。
const API_PREFIXES: &[&str] = &["/admin", "/mcp", "/health", "/internal"];

/// 带内容哈希的构建产物（Vite 写进 `assets/`）：内容变了文件名就变，可以长期缓存。
const IMMUTABLE_CACHE: &str = "public, max-age=31536000, immutable";
/// 其余静态文件（含 `index.html`）：每次都回源确认，避免部署新版本后浏览器还在跑旧入口。
const REVALIDATE_CACHE: &str = "no-cache";

/// 已校验的静态资源集合：目录与 `index.html` 的句柄都提前建好，请求期只做 clone（不新建路径）。
struct WebAssets {
    dir: ServeDir,
    index: ServeFile,
    path: PathBuf,
}

impl WebAssets {
    fn new(dir: &Path) -> Self {
        let index = dir.join("index.html");
        Self {
            dir: ServeDir::new(dir),
            index: ServeFile::new(&index),
            path: index,
        }
    }

    async fn respond(&self, req: axum::extract::Request) -> Response {
        let path = req.uri().path().to_string();
        let method = req.method().clone();
        // 后端路径空间优先：未命中的管理/MCP 路径必须是 404，而不是 SPA 页面。
        // 顺序也很重要：分面之后 `/mcp`、`/admin/*` 在"另一面"上属于"这个面没有该端点"，
        // 先判 405 会让 `POST /mcp` 在管理面回 405（"方法不对"），误导客户端以为换个方法就能用。
        if is_api_path(&path) {
            return not_found_response(&path);
        }
        if !matches!(method, Method::GET | Method::HEAD) {
            return (
                StatusCode::METHOD_NOT_ALLOWED,
                [(header::ALLOW, "GET, HEAD")],
                "method not allowed\n",
            )
                .into_response();
        }
        if path == "/" || path == "/index.html" {
            return self.index_response(&method).await;
        }
        // `ServeDir` 的响应体是它自己的类型，统一成 axum `Body` 便于加缓存头/回退。
        let served = self
            .dir
            .clone()
            .oneshot(req)
            .await
            .map(|res| res.map(Body::new));
        match served {
            Ok(res) if res.status() != StatusCode::NOT_FOUND => with_cache(res, &path),
            Ok(_) if is_spa_deep_link(&path) => self.index_response(&method).await,
            Ok(res) => res,
            Err(err) => {
                // ServeDir 的错误类型是 Infallible；保留分支只为不 panic。
                tracing::error!(%err, "静态资源请求失败");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error\n").into_response()
            }
        }
    }

    async fn index_response(&self, method: &Method) -> Response {
        let req = axum::http::Request::builder()
            .method(method.clone())
            .uri("/index.html")
            .body(Body::empty());
        match req {
            Ok(req) => match self.index.clone().oneshot(req).await {
                // 入口固定回源校验：缓存判定按 URL 规则来，不拿文件系统路径碰运气。
                Ok(res) => with_cache(res.map(Body::new), "/index.html"),
                Err(err) => {
                    tracing::error!(%err, path = %self.path.display(), "index.html 读取失败");
                    (StatusCode::INTERNAL_SERVER_ERROR, "internal error\n").into_response()
                }
            },
            Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal error\n").into_response(),
        }
    }
}

/// 统一的"路径不存在"响应：JSON 而不是 HTML，并带上路径便于对着日志分辨落在哪个面。
pub(crate) fn not_found_response(path: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"error": {
            "code": "MCP_NOT_FOUND",
            "message": "未找到该路径",
            "data": {"path": path}
        }})),
    )
        .into_response()
}

/// 不托管静态站的面（MCP 面）的兜底：未知路径一律 JSON 404。
///
/// MCP 面上没有页面可言，`/`、`/admin/*`、拼错的路径都必须 404；用兜底而不是逐条 deny，
/// 是因为新增路由时忘记补 deny 的代价是"悄悄多暴露一条路径"，兜底则是"悄悄少暴露"，方向相反。
pub fn strict_fallback(app: Router) -> Router {
    app.fallback(|req: axum::extract::Request| async move { not_found_response(req.uri().path()) })
}

/// 在 HTTP 应用上挂载静态托管：`web_dir` 为空时原样返回（不挂静态，行为不变）。
///
/// 目录门禁在这里再校验一次：即使调用方绕过了 `Settings::validate`，
/// 也不允许"声明托管但目录不可用"的服务起监听。
pub fn attach(app: Router, web_dir: &str) -> Result<Router, String> {
    if web_dir.is_empty() {
        return Ok(app);
    }
    jumcp_core::config::validate_web_dir(web_dir)?;
    let assets = Arc::new(WebAssets::new(Path::new(web_dir)));
    tracing::info!(web_dir, "管理台静态托管已启用");
    // 与 Vite dev 代理同语义：浏览器导航（`Accept: text/html`）的 `/health` 落管理台页面，
    // 健康检查/XHR 仍走后端 JSON。只在托管开启时生效——没有静态产物就没有页面可回。
    let app = app.layer(axum::middleware::from_fn_with_state(
        assets.clone(),
        health_browser_navigation,
    ));
    Ok(app.fallback(move |req: axum::extract::Request| {
        let assets = assets.clone();
        async move { assets.respond(req).await }
    }))
}

/// `GET|HEAD /health` + `Accept: text/html` → 管理台 `index.html`；其余一律交给后端。
///
/// 为什么按 `Accept` 分流而不是按路径：管理台没有独立的 `/health` 路由，浏览器导航
/// 拿到 `index.html` 后由客户端路由落到工作台；而探针/监控/管理台 XHR 请求的是后端健康数据。
/// 这与开发期 Vite 代理的 `bypass` 规则一致，保证 dev 与生产同源行为相同。
async fn health_browser_navigation(
    State(assets): State<Arc<WebAssets>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let is_health_page = req.uri().path() == "/health"
        && matches!(*req.method(), Method::GET | Method::HEAD)
        && accepts_html(req.headers());
    if is_health_page {
        return assets.index_response(req.method()).await;
    }
    next.run(req).await
}

/// `Accept` 里是否声明了 `text/html`（媒体类型大小写不敏感，可能有多条 `Accept` 头）。
fn accepts_html(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get_all(header::ACCEPT)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .any(|value| value.to_ascii_lowercase().contains("text/html"))
}

/// 后端路由前缀（含自身）判断：`/admin`、`/admin/...`、`/health` 都算。
fn is_api_path(path: &str) -> bool {
    API_PREFIXES.iter().any(|prefix| {
        path == *prefix
            || (path.len() > prefix.len()
                && path.starts_with(prefix)
                && path.as_bytes()[prefix.len()] == b'/')
    })
}

/// SPA 深链：`/review`、`/capabilities/{id}` 这类不带扩展名的路径回退到 `index.html`。
///
/// 带扩展名的路径视为静态资源：真的不存在就返回 404，不回退成 HTML，
/// 否则缺失的 JS 会以 `200 text/html` 返回，浏览器报的错会离真正原因很远。
fn is_spa_deep_link(path: &str) -> bool {
    let last = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    !last.is_empty() && !last.contains('.')
}

/// 静态响应缓存头：`assets/` 下的哈希产物长缓存，其余回源校验。
fn with_cache(mut res: Response, path: &str) -> Response {
    if res.status().is_success() {
        let value = if path.starts_with("/assets/") {
            IMMUTABLE_CACHE
        } else {
            REVALIDATE_CACHE
        };
        res.headers_mut().insert(
            header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static(value),
        );
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;
    use axum::routing::{get, post};
    use tower::ServiceExt;

    const INDEX_MARKER: &str = "<div id=\"root\">jumcp-admin</div>";

    struct Fixture {
        _tmp: tempfile::TempDir,
        /// `<tmp>/dist`：被托管的构建产物目录。
        dist: PathBuf,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dist = tmp.path().join("dist");
        std::fs::create_dir_all(dist.join("assets")).expect("assets dir");
        std::fs::write(
            dist.join("index.html"),
            format!("<!doctype html>{INDEX_MARKER}"),
        )
        .expect("index");
        std::fs::write(dist.join("assets/app-abc123.js"), "console.log(1)").expect("asset");
        // 目录外的秘密文件：路径穿越若能读到它就说明实现有洞。
        std::fs::write(tmp.path().join("secret.txt"), "TOP-SECRET").expect("secret");
        Fixture { _tmp: tmp, dist }
    }

    /// 与 `run_http` 同形的后端路由：`/health`、`/admin/*`、`/mcp`。
    fn backend_app() -> Router {
        Router::new()
            .route("/health", get(|| async { "{\"status\":\"ok\"}" }))
            .route("/admin/me", get(|| async { "{\"id\":\"alice\"}" }))
            .route("/mcp", post(|| async { "{}" }))
    }

    fn hosted(dist: &Path) -> Router {
        attach(backend_app(), dist.to_str().unwrap()).expect("attach")
    }

    async fn send(app: &Router, method: Method, uri: &str) -> (StatusCode, String, String) {
        send_accept(app, method, uri, None).await
    }

    async fn send_accept(
        app: &Router,
        method: Method,
        uri: &str,
        accept: Option<&str>,
    ) -> (StatusCode, String, String) {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(accept) = accept {
            builder = builder.header(header::ACCEPT, accept);
        }
        let res = app
            .clone()
            .oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let cache = res
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&bytes).into_owned(), cache)
    }

    #[tokio::test]
    async fn root_serves_index_with_no_cache() {
        let f = fixture();
        let app = hosted(&f.dist);
        let (status, body, cache) = send(&app, Method::GET, "/").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains(INDEX_MARKER), "{body}");
        assert_eq!(cache, REVALIDATE_CACHE);
    }

    #[tokio::test]
    async fn spa_deep_links_fall_back_to_index() {
        let f = fixture();
        let app = hosted(&f.dist);
        for uri in ["/review", "/audit", "/capabilities/cap-42", "/review/"] {
            let (status, body, cache) = send(&app, Method::GET, uri).await;
            assert_eq!(status, StatusCode::OK, "{uri}");
            assert!(body.contains(INDEX_MARKER), "{uri}: {body}");
            assert_eq!(cache, REVALIDATE_CACHE, "{uri}");
        }
    }

    #[tokio::test]
    async fn backend_routes_win_over_static_fallback() {
        let f = fixture();
        let app = hosted(&f.dist);
        let (status, body, _) = send(&app, Method::GET, "/health").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("\"status\":\"ok\""), "{body}");

        let (status, body, _) = send(&app, Method::GET, "/admin/me").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("alice"), "{body}");

        // GET /mcp 只注册了 POST：保持 405，不能被静态回退成页面。
        let (status, body, _) = send(&app, Method::GET, "/mcp").await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
        assert!(!body.contains(INDEX_MARKER), "{body}");
    }

    /// `/health` 按 `Accept` 分流（同 Vite dev 代理的 bypass 规则）：
    /// 浏览器导航拿到管理台页面，探针/XHR/curl 拿到后端 JSON。
    #[tokio::test]
    async fn health_splits_on_accept_header() {
        let f = fixture();
        let app = hosted(&f.dist);

        // 1) 无 Accept（curl / 探针）：后端 JSON，不被拦截。
        let (status, body, _) = send_accept(&app, Method::GET, "/health", None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("\"status\":\"ok\""), "{body}");

        // 2) XHR 风格（*/*）：仍是后端 JSON。
        let (status, body, _) = send_accept(&app, Method::GET, "/health", Some("*/*")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("\"status\":\"ok\""), "{body}");

        // 3) 浏览器导航：管理台 index，且入口不缓存。
        let (status, body, cache) = send_accept(
            &app,
            Method::GET,
            "/health",
            Some("text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains(INDEX_MARKER), "{body}");
        assert_eq!(cache, REVALIDATE_CACHE);

        // 4) 媒体类型大小写不敏感。
        let (status, body, _) = send_accept(&app, Method::HEAD, "/health", Some("TEXT/HTML")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.is_empty(), "HEAD 不应带响应体: {body}");
    }

    /// 拦截只覆盖 `GET|HEAD /health`：其它方法与非精确路径仍走后端。
    #[tokio::test]
    async fn health_guard_does_not_leak_to_other_methods_or_subpaths() {
        let f = fixture();
        let app = hosted(&f.dist);

        // POST 即使声明 text/html 也不拦截：/health 只注册了 GET，保持 405。
        let (status, body, _) = send_accept(
            &app,
            Method::POST,
            "/health",
            Some("text/html,application/xhtml+xml"),
        )
        .await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
        assert!(!body.contains(INDEX_MARKER), "{body}");

        // /health/* 仍交给后端路径空间：JSON 404，不回退页面。
        let (status, body, _) =
            send_accept(&app, Method::GET, "/health/deep", Some("text/html")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.starts_with('{'), "{body}");
        assert!(!body.contains(INDEX_MARKER), "{body}");
    }

    /// 未挂载静态产物时没有页面可回：`/health` 无论 Accept 都是后端 JSON。
    #[tokio::test]
    async fn health_guard_inactive_without_web_dir() {
        let app = attach(backend_app(), "").expect("空目录合法");
        let (status, body, _) = send_accept(&app, Method::GET, "/health", Some("text/html")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("\"status\":\"ok\""), "{body}");
    }

    /// 未知 API 路径不得变成 HTML：调用方必须能看出"路径不存在"。
    #[tokio::test]
    async fn unknown_api_paths_stay_json_404() {
        let f = fixture();
        let app = hosted(&f.dist);
        for uri in ["/admin/unknown", "/health/deep", "/mcp/tools", "/admin/"] {
            let (status, body, _) = send(&app, Method::GET, uri).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
            assert!(!body.contains(INDEX_MARKER), "{uri}: {body}");
            assert!(body.starts_with('{'), "{uri}: {body}");
        }
    }

    #[tokio::test]
    async fn hashed_assets_are_cached_long_but_missing_assets_stay_404() {
        let f = fixture();
        let app = hosted(&f.dist);
        let (status, body, cache) = send(&app, Method::GET, "/assets/app-abc123.js").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "console.log(1)");
        assert_eq!(cache, IMMUTABLE_CACHE);

        let (status, body, _) = send(&app, Method::GET, "/assets/missing-deadbeef.js").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(!body.contains(INDEX_MARKER), "{body}");
    }

    /// 未配置 `web_dir` 时 `attach` 是恒等函数：`/` 不会被静态资源接管。
    #[tokio::test]
    async fn empty_web_dir_mounts_nothing() {
        let app = attach(backend_app(), "").expect("attach 空目录合法");
        let (status, _, _) = send(&app, Method::GET, "/").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _, _) = send(&app, Method::GET, "/review").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    /// 未构建产物（缺 index.html）时拒绝挂载：不能等到浏览器里才暴露。
    #[tokio::test]
    async fn attach_rejects_dir_without_index() {
        let tmp = tempfile::tempdir().unwrap();
        let err = attach(backend_app(), tmp.path().to_str().unwrap()).unwrap_err();
        assert!(err.contains("index.html"), "{err}");
    }

    /// 路径穿越：`..` 与编码后的 `%2e%2e` 都不得读出目录外文件。
    #[tokio::test]
    async fn path_traversal_cannot_escape_web_dir() {
        let f = fixture();
        let app = hosted(&f.dist);
        for uri in [
            "/../secret.txt",
            "/%2e%2e/secret.txt",
            "/assets/../../secret.txt",
            "/..%2fsecret.txt",
            "/assets/%2e%2e/%2e%2e/secret.txt",
        ] {
            let (status, body, _) = send(&app, Method::GET, uri).await;
            assert!(
                !body.contains("TOP-SECRET"),
                "{uri} 读到了目录外文件: {status} {body}"
            );
            assert!(!status.is_success(), "{uri} 不应成功: {body}");
        }
    }

    #[tokio::test]
    async fn non_get_methods_are_rejected_for_static_paths() {
        let f = fixture();
        let app = hosted(&f.dist);
        let (status, _, _) = send(&app, Method::POST, "/review").await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    }
}
