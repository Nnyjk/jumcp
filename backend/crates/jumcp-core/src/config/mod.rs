//! 策略配置：TOML + env，非法配置构造即 Err（启动 fail-closed）。
//!
//! env 前缀为什么是 `JUMCP_MCP_`：与 Python Settings 的 env_prefix 对齐，部署只改 env 不改代码。
//! 本模块保留组名/展示名门禁与 `Settings` 入口；租户策略、数据源引用、主体令牌见同目录兄弟模块。

use crate::budget::OverflowPolicy;
use crate::secret::SecretKey;
use std::path::Path;

mod principals;
mod tenant;

pub use principals::{AgentPrincipal, AgentTokens, constant_time_eq, is_principal_id};
pub use tenant::{GroupDef, RoleDef, TenantPolicy};

#[cfg(test)]
mod tests;

pub const MAX_GROUPS: usize = 20;
/// 管理面新增组名的长度上限（字符数；组名只允许 ASCII，等价于字节数）。
pub const MAX_GROUP_NAME_LEN: usize = 64;
/// 保留组名：`basic` 是常驻工具与"逐工具开放的上游工具"的公共基础通道，
/// `builtin` 只是保留名（不能作为任何工具的组值），`admin` 是维护通道的安全边界。
/// 三个通道（配置文件/管理 API）都不得创建、修改或删除它们。
pub const RESERVED_GROUP_NAMES: &[&str] = &["basic", "builtin", "admin"];

/// 代码内置的公共基础组：组本身不是可授权组，成员对所有 token 可见（仍受 allowlist 约束）。
pub const BASIC_GROUP_NAME: &str = "basic";

/// 仅用于拒绝创建的保留名：组值不得为 `builtin`。
pub const LEGACY_BUILTIN_GROUP_NAME: &str = "builtin";

pub fn is_reserved_group_name(name: &str) -> bool {
    RESERVED_GROUP_NAMES.contains(&name)
}

/// 展示名门禁（组/角色通用）：trim 后非空、长度 1..64 字符。
///
/// 展示名只用于界面显示，**绝不参与**权限判定；权限一律看 code。
pub fn validate_display_name(field: &str, name: &str) -> Result<(), String> {
    let trimmed = name.trim();
    let len = trimmed.chars().count();
    if len == 0 {
        return Err(format!("{field} 不能为空"));
    }
    if len > MAX_GROUP_NAME_LEN {
        return Err(format!(
            "{field} {trimmed:?} 超过长度上限 {MAX_GROUP_NAME_LEN}"
        ));
    }
    Ok(())
}

/// 管理面组名门禁：非空、长度受限、仅小写字母数字与 `._-`（因此天然不含空白与逗号）。
pub fn validate_group_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("组名不能为空".to_string());
    }
    if name.len() > MAX_GROUP_NAME_LEN {
        return Err(format!("组名 {name:?} 超过长度上限 {MAX_GROUP_NAME_LEN}"));
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Err(format!(
            "组名 {name:?} 非法：只允许小写字母、数字与 ._-（不含空白与逗号）"
        ));
    }
    Ok(())
}

/// 服务级 Settings：TOML 文件 + JUMCP_MCP_* env，构造即校验。
#[derive(Debug, Clone)]
pub struct Settings {
    pub config_file: String,
    pub transport: String,
    pub host: String,
    /// MCP 面监听端口（`JUMCP_MCP_PORT`，默认 8000）：只挂 `/mcp`、`/internal/mcp/*`、`/health`。
    pub port: u16,
    /// 管理面监听端口（`JUMCP_ADMIN_PORT`，默认 8080）：只挂 `/admin/*`、`/health` 与管理台静态站。
    /// 必须与 `port` 不同，相同即拒绝启动（否则两个面会重新合成一个口）。
    pub admin_port: u16,
    /// 管理台会话 cookie 的存活秒数（`JUMCP_ADMIN_SESSION_TTL_SECS`，默认 43200 = 12h）。
    pub admin_session_ttl_secs: i64,
    /// 会话 cookie 是否带 `Secure`（`JUMCP_ADMIN_COOKIE_SECURE=1`）。
    /// 默认关：纯 HTTP 内网部署下带 Secure 会让浏览器根本不发 cookie，等于登录成功但立刻掉线。
    pub admin_cookie_secure: bool,
    pub audit_path: String,
    /// capability/模板注册表（SQLite）路径：模板与版本历史落这里。
    pub registry_path: String,
    /// 启动时装载的模板定义文件（JSON）；空表示不预置模板。
    pub templates_file: String,
    /// 管理台静态构建产物目录（`JUMCP_MCP_WEB_DIR`）：空表示不托管静态资源。
    /// 非空时启动期必须存在且含 `index.html`（fail-closed，见 [`validate_web_dir`]）。
    pub web_dir: String,
    /// MCP 面 per-request 身份（JUMCP_AGENT_TOKENS="agent_id:role:token,..."）。
    /// HTTP `/mcp` 只认这里派生的身份；为空时 /mcp 一律 `MCP_UNAUTHORIZED`（无兜底角色）。
    pub agent_tokens: AgentTokens,
    /// 静态客户端平铺阈值：可见 kind=tool 能力数 ≤ 阈值时直接平铺；0 表示禁用平铺。
    pub flatten_threshold: i64,
    pub groups_mode: String,
    /// 预算溢出策略：默认 fail；truncate 必须显式开启。
    pub budget_overflow: String,
    /// stdio 上游总开关（`JUMCP_MCP_UPSTREAM_STDIO=1`）：默认关，关闭时拒收 stdio 配置。
    pub upstream_stdio: bool,
    /// 允许注册 `read_only=false` 的上游（`JUMCP_MCP_ALLOW_WRITE_UPSTREAM=1`）。
    pub upstream_write: bool,
    /// 配置字段加密密钥（`JUMCP_SECRET_KEY`，64 位 hex）。HTTP 模式必需：
    /// 内置 MCP 的密码类字段落库前用它加密；缺失/非法即拒绝启动，避免明文密码落盘。
    pub secret_key: Option<SecretKey>,
    /// 传输密钥（`JUMCP_TRANSPORT_KEY`，64 位 hex）。HTTP 模式必需：
    /// 浏览器用它加密 secret 字段，服务端只接受 `enc:v1:` 密文；与 `secret_key`
    /// 用途分离（传输 vs 落库），泄露一把不会连带另一把。
    pub transport_key: Option<SecretKey>,
    pub policy: TenantPolicy,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            config_file: "config/policy.toml".to_string(),
            transport: "stdio".to_string(),
            host: "0.0.0.0".to_string(),
            port: 8000,
            admin_port: 8080,
            admin_session_ttl_secs: 43_200,
            admin_cookie_secure: false,
            audit_path: "data/audit.sqlite3".to_string(),
            registry_path: "data/registry.sqlite3".to_string(),
            templates_file: String::new(),
            web_dir: String::new(),
            agent_tokens: AgentTokens::default(),
            flatten_threshold: 40,
            groups_mode: "strict".to_string(),
            budget_overflow: "fail".to_string(),
            upstream_stdio: false,
            upstream_write: false,
            secret_key: None,
            transport_key: None,
            policy: TenantPolicy::default(),
        }
    }
}

/// 静态托管目录门禁：空串合法（不托管）；非空必须是含 `index.html` 的目录。
///
/// 为什么不静默忽略：`JUMCP_MCP_WEB_DIR` 是部署侧"管理台已同源上线"的声明，
/// 目录缺失或产物没构建时若降级成 404，问题只会在浏览器里暴露；启动期直接拒绝更早、更明确。
pub fn validate_web_dir(dir: &str) -> Result<(), String> {
    if dir.is_empty() {
        return Ok(());
    }
    let path = Path::new(dir);
    if !path.is_dir() {
        return Err(format!("JUMCP_MCP_WEB_DIR 不是目录: {dir}"));
    }
    if !path.join("index.html").is_file() {
        return Err(format!(
            "JUMCP_MCP_WEB_DIR 缺少 index.html（管理台未构建或目录错误）: {dir}"
        ));
    }
    Ok(())
}

impl Settings {
    /// 单一入口：从路径/env 构造并校验（非法 → Err，调用方不得吞掉）。
    pub fn from_env() -> Result<Self, String> {
        let mut s = Settings::default();
        s.config_file = env_or("JUMCP_MCP_CONFIG_FILE", &s.config_file);
        s.transport = env_or("JUMCP_MCP_TRANSPORT", &s.transport);
        s.host = env_or("JUMCP_MCP_HOST", &s.host);
        s.port = env_or("JUMCP_MCP_PORT", "8000")
            .parse::<u16>()
            .map_err(|_| "JUMCP_MCP_PORT 非法".to_string())?;
        s.admin_port = env_or("JUMCP_ADMIN_PORT", "8080")
            .parse::<u16>()
            .map_err(|_| "JUMCP_ADMIN_PORT 非法".to_string())?;
        s.admin_session_ttl_secs = env_or("JUMCP_ADMIN_SESSION_TTL_SECS", "43200")
            .parse::<i64>()
            .map_err(|_| "JUMCP_ADMIN_SESSION_TTL_SECS 非法".to_string())?;
        s.admin_cookie_secure = env_or("JUMCP_ADMIN_COOKIE_SECURE", "0") == "1";
        s.audit_path = env_or("JUMCP_MCP_AUDIT_PATH", &s.audit_path);
        s.registry_path = env_or("JUMCP_MCP_REGISTRY_PATH", &s.registry_path);
        s.templates_file = env_or("JUMCP_MCP_TEMPLATES_FILE", &s.templates_file);
        s.web_dir = env_or("JUMCP_MCP_WEB_DIR", &s.web_dir);
        s.agent_tokens = AgentTokens::parse(&env_or("JUMCP_AGENT_TOKENS", ""))?;
        s.flatten_threshold = env_or("JUMCP_MCP_FLATTEN_THRESHOLD", "40")
            .parse::<i64>()
            .map_err(|_| "JUMCP_MCP_FLATTEN_THRESHOLD 非法".to_string())?;
        s.groups_mode = env_or("JUMCP_MCP_GROUPS_MODE", &s.groups_mode);
        s.budget_overflow = env_or("JUMCP_MCP_BUDGET_OVERFLOW", &s.budget_overflow);
        s.upstream_stdio = env_or("JUMCP_MCP_UPSTREAM_STDIO", "0") == "1";
        s.upstream_write = env_or("JUMCP_MCP_ALLOW_WRITE_UPSTREAM", "0") == "1";
        let secret_raw = env_or("JUMCP_SECRET_KEY", "");
        if !secret_raw.trim().is_empty() {
            s.secret_key = Some(SecretKey::parse(&secret_raw).map_err(|e| e.to_string())?);
        }
        let transport_raw = env_or("JUMCP_TRANSPORT_KEY", "");
        if !transport_raw.trim().is_empty() {
            s.transport_key = Some(SecretKey::parse(&transport_raw).map_err(|e| e.to_string())?);
        }
        if Path::new(&s.config_file).exists() {
            let text = std::fs::read_to_string(&s.config_file).map_err(|e| e.to_string())?;
            s.policy = toml::from_str(&text).map_err(|e| format!("TOML 解析失败: {e}"))?;
        }
        s.validate()?;
        Ok(s)
    }
    pub fn validate(&self) -> Result<(), String> {
        if !matches!(self.transport.as_str(), "stdio" | "streamable-http") {
            return Err(format!("transport 非法: {}", self.transport));
        }
        if self.transport == "streamable-http" && self.secret_key.is_none() {
            return Err(
                "JUMCP_SECRET_KEY 未设置：HTTP 模式必须注入 64 位十六进制密钥（生成方式：openssl rand -hex 32）"
                    .to_string(),
            );
        }
        if self.transport == "streamable-http" && self.transport_key.is_none() {
            return Err(
                "JUMCP_TRANSPORT_KEY 未设置：HTTP 模式必须注入 64 位十六进制密钥（生成方式：openssl rand -hex 32）"
                    .to_string(),
            );
        }
        // 两把密钥用途分离（落库加密 vs 浏览器传输）：相等等于共用一把，泄露一把就同时
        // 失去"落库密文"与"传输密文"两层，配置期直接拒绝。用常量时间比较避免把
        // "前几位相同"这种信息留在启动耗时的侧信道里。
        if let (Some(secret), Some(transport)) = (&self.secret_key, &self.transport_key)
            && constant_time_eq(&secret.to_hex(), &transport.to_hex())
        {
            return Err(
                "JUMCP_SECRET_KEY 与 JUMCP_TRANSPORT_KEY 不能相同：两把密钥必须用途分离（落库加密 vs 浏览器传输加密），泄露一把不得连带另一把"
                    .to_string(),
            );
        }
        if !matches!(self.groups_mode.as_str(), "strict" | "intersect") {
            return Err(format!("groups_mode 非法: {}", self.groups_mode));
        }
        if self.admin_session_ttl_secs <= 0 {
            return Err(format!(
                "JUMCP_ADMIN_SESSION_TTL_SECS 必须为正数: {}",
                self.admin_session_ttl_secs
            ));
        }
        // 两个面必须分开监听：端口相同等于把 /admin 与 /mcp 又合回一个口，
        // 分面隔离（MCP 面看不到管理面、管理面看不到 MCP）会静默失效。
        if self.transport == "streamable-http" && self.admin_port == self.port {
            return Err(format!(
                "JUMCP_ADMIN_PORT 与 JUMCP_MCP_PORT 相同（{}）：管理面与 MCP 面必须使用不同端口",
                self.port
            ));
        }
        if self.flatten_threshold < 0 {
            return Err(format!(
                "flatten_threshold 不能为负: {}",
                self.flatten_threshold
            ));
        }
        if self.registry_path.is_empty() {
            return Err("registry_path 不能为空".to_string());
        }
        if !matches!(self.budget_overflow.as_str(), "fail" | "truncate") {
            return Err(format!("budget_overflow 非法: {}", self.budget_overflow));
        }
        validate_web_dir(&self.web_dir)?;
        self.policy.validate()?;
        // agent 身份的角色必须是已定义角色：否则每次请求才在 resolve_scope 里报错，
        // 部署者会以为服务正常而实际 /mcp 全量失败（启动期一次说清）。
        for principal in self.agent_tokens.principals() {
            if !self.policy.roles.contains_key(&principal.role) {
                return Err(format!(
                    "JUMCP_AGENT_TOKENS 的 agent {} 引用未定义角色 {}",
                    principal.id, principal.role
                ));
            }
        }
        // intersect 须租户开关开启（与 Python validator 一致）。
        if self.groups_mode == "intersect" && !self.policy.allow_intersect {
            return Err("groups_mode=intersect 但租户未开 allow_intersect".to_string());
        }
        Ok(())
    }
    pub fn overflow_policy(&self) -> OverflowPolicy {
        // validate() 已保证取值闭集，此处不会碰到未知值。
        if self.budget_overflow == "truncate" {
            OverflowPolicy::Truncate
        } else {
            OverflowPolicy::Fail
        }
    }
    /// 供测试注入已校验策略。
    pub fn with_policy(mut self, policy: TenantPolicy) -> Result<Self, String> {
        self.policy = policy;
        self.validate()?;
        Ok(self)
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}
