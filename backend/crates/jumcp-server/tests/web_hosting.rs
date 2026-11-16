//! 静态托管的启动期契约：`JUMCP_MCP_WEB_DIR` 走真实 `Settings::from_env` 路径。
//!
//! 为什么用子进程而不是 `std::env::set_var`：env 是进程级全局状态，
//! 测试并行时改它会让同进程其他用例读到别的值（Rust 2024 起还需 unsafe）。
//! 直接拉起真实二进制，既无竞态，也真的验证了"配置非法 → 非零退出"。

use std::process::Command;

fn bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_jumcp-server"));
    // 显式清掉宿主可能带进来的配置，避免用例依赖运行环境。
    cmd.env_remove("JUMCP_MCP_CONFIG_FILE")
        .env_remove("JUMCP_MCP_WEB_DIR")
        .env_remove("JUMCP_ADMIN_TOKENS")
        .env_remove("JUMCP_AGENT_TOKENS")
        .env_remove("JUMCP_MCP_UPSTREAM_STDIO")
        .env_remove("JUMCP_MCP_ALLOW_WRITE_UPSTREAM")
        .env("JUMCP_MCP_TRANSPORT", "streamable-http")
        .env("JUMCP_MCP_CONFIG_FILE", "nonexistent/policy.toml");
    cmd
}

/// 目录存在但没构建产物：必须在监听端口前失败，且错误说得清是 `index.html` 的问题。
#[test]
fn startup_rejects_web_dir_without_index_html() {
    let dir = tempfile::tempdir().unwrap();
    let out = bin()
        .env("JUMCP_MCP_WEB_DIR", dir.path())
        .env("JUMCP_MCP_AUDIT_PATH", dir.path().join("audit.sqlite3"))
        .env(
            "JUMCP_MCP_REGISTRY_PATH",
            dir.path().join("registry.sqlite3"),
        )
        .env("JUMCP_ADMIN_TOKENS", "alice:tok_alice_000000000")
        .output()
        .expect("启动 jumcp-server");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "缺 index.html 必须非零退出: {stderr}"
    );
    assert!(stderr.contains("index.html"), "{stderr}");
    assert!(stderr.contains("JUMCP_MCP_WEB_DIR"), "{stderr}");
}

#[test]
fn startup_rejects_web_dir_that_is_not_a_directory() {
    let dir = tempfile::tempdir().unwrap();
    let out = bin()
        .env("JUMCP_MCP_WEB_DIR", dir.path().join("dist"))
        .env("JUMCP_MCP_AUDIT_PATH", dir.path().join("audit.sqlite3"))
        .env(
            "JUMCP_MCP_REGISTRY_PATH",
            dir.path().join("registry.sqlite3"),
        )
        .env("JUMCP_ADMIN_TOKENS", "alice:tok_alice_000000000")
        .output()
        .expect("启动 jumcp-server");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "目录不存在必须非零退出: {stderr}");
    assert!(stderr.contains("不是目录"), "{stderr}");
}

/// 反向证明：合法目录能通过启动校验，进程继续走到下一条 fail-closed 检查
/// （HTTP 模式没有 admin token 即拒绝启动），而不是卡在 web_dir 上。
#[test]
fn valid_web_dir_passes_startup_validation() {
    let web = tempfile::tempdir().unwrap();
    std::fs::write(web.path().join("index.html"), "<!doctype html>").unwrap();
    let work = tempfile::tempdir().unwrap();
    let out = bin()
        .env("JUMCP_MCP_WEB_DIR", web.path())
        .env("JUMCP_MCP_AUDIT_PATH", work.path().join("audit.sqlite3"))
        .env(
            "JUMCP_MCP_REGISTRY_PATH",
            work.path().join("registry.sqlite3"),
        )
        .output()
        .expect("启动 jumcp-server");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "无 admin token 仍须拒绝启动: {stderr}"
    );
    assert!(stderr.contains("JUMCP_ADMIN_TOKENS"), "{stderr}");
    assert!(
        !stderr.contains("index.html"),
        "web_dir 校验不应失败: {stderr}"
    );
    assert!(!stderr.contains("不是目录"), "{stderr}");
}
