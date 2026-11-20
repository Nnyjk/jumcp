//! 静态托管的启动期契约：`JUMCP_MCP_WEB_DIR` 走真实 `Settings::from_env` 路径。
//!
//! 为什么用子进程而不是 `std::env::set_var`：env 是进程级全局状态，
//! 测试并行时改它会让同进程其他用例读到别的值（Rust 2024 起还需 unsafe）。
//! 直接拉起真实二进制，既无竞态，也真的验证了"配置非法 → 非零退出"。

use std::process::Command;

/// 与 `jumcp_core::registry::admins::BOOTSTRAP_ADMIN_REQUIRED` 逐字一致。
const BOOTSTRAP_MESSAGE: &str = "JUMCP_ADMIN_USERNAME/JUMCP_ADMIN_PASSWORD 未设置：首次启动必须提供初始管理员账号（密码至少 12 位）";

fn bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_jumcp-server"));
    // 显式清掉宿主可能带进来的配置，避免用例依赖运行环境。
    cmd.env_remove("JUMCP_MCP_CONFIG_FILE")
        .env_remove("JUMCP_MCP_WEB_DIR")
        .env_remove("JUMCP_ADMIN_USERNAME")
        .env_remove("JUMCP_ADMIN_PASSWORD")
        .env_remove("JUMCP_AGENT_TOKENS")
        .env_remove("JUMCP_MCP_UPSTREAM_STDIO")
        .env_remove("JUMCP_MCP_ALLOW_WRITE_UPSTREAM")
        .env_remove("JUMCP_TRANSPORT_KEY")
        .env("JUMCP_MCP_TRANSPORT", "streamable-http")
        // HTTP 模式 fail-closed：没有密钥连静态目录校验都不会走到。
        .env(
            "JUMCP_SECRET_KEY",
            "2222222222222222222222222222222222222222222222222222222222222222",
        )
        .env(
            "JUMCP_TRANSPORT_KEY",
            "3333333333333333333333333333333333333333333333333333333333333333",
        )
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
        .env("JUMCP_ADMIN_USERNAME", "alice")
        .env("JUMCP_ADMIN_PASSWORD", "alice-correct-horse")
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
        .env("JUMCP_ADMIN_USERNAME", "alice")
        .env("JUMCP_ADMIN_PASSWORD", "alice-correct-horse")
        .output()
        .expect("启动 jumcp-server");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "目录不存在必须非零退出: {stderr}");
    assert!(stderr.contains("不是目录"), "{stderr}");
}

/// 反向证明：合法目录能通过启动校验，进程继续走到下一条 fail-closed 检查
/// （首次启动缺初始管理员凭据即拒绝启动），而不是卡在 web_dir 上。
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
        "缺初始管理员凭据仍须拒绝启动: {stderr}"
    );
    assert!(stderr.contains(BOOTSTRAP_MESSAGE), "{stderr}");
    assert!(
        !stderr.contains("index.html"),
        "web_dir 校验不应失败: {stderr}"
    );
    assert!(!stderr.contains("不是目录"), "{stderr}");
}

/// 空库 + 缺凭据：必须按文案拒绝启动（运维照这句就知道该配哪两个变量）。
#[test]
fn startup_requires_bootstrap_admin_credentials() {
    let work = tempfile::tempdir().unwrap();
    let out = bin()
        .env("JUMCP_MCP_AUDIT_PATH", work.path().join("audit.sqlite3"))
        .env(
            "JUMCP_MCP_REGISTRY_PATH",
            work.path().join("registry.sqlite3"),
        )
        .output()
        .expect("启动 jumcp-server");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(stderr.contains(BOOTSTRAP_MESSAGE), "{stderr}");
}

/// 口令不足 12 位同样拒绝启动：弱口令不能因为"首次启动"被放行。
#[test]
fn startup_rejects_short_bootstrap_password() {
    let work = tempfile::tempdir().unwrap();
    let out = bin()
        .env("JUMCP_MCP_AUDIT_PATH", work.path().join("audit.sqlite3"))
        .env(
            "JUMCP_MCP_REGISTRY_PATH",
            work.path().join("registry.sqlite3"),
        )
        .env("JUMCP_ADMIN_USERNAME", "alice")
        .env("JUMCP_ADMIN_PASSWORD", "short")
        .output()
        .expect("启动 jumcp-server");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(stderr.contains(BOOTSTRAP_MESSAGE), "{stderr}");
}
