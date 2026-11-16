//! 策略配置：TOML + env，非法配置构造即 Err（启动 fail-closed）。
//! 为什么 env 前缀 JUMCP_MCP_：与 Python Settings env_prefix 对齐，部署只改 env 不改代码。

use crate::budget::OverflowPolicy;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

pub const MAX_GROUPS: usize = 20;
/// 管理面新增组名的长度上限（字符数；组名只允许 ASCII，等价于字节数）。
pub const MAX_GROUP_NAME_LEN: usize = 64;
/// 保留组名：`builtin` 是常驻工具的保留组，`admin` 是维护通道的安全边界。
/// 三个通道（配置/管理 API）都不得创建、修改或删除它们。
pub const RESERVED_GROUP_NAMES: &[&str] = &["builtin", "admin"];

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasourceRef {
    pub name: String,
    /// 存放 DSN 的环境变量名；配置里永不出现明文连接串（W1.8）。
    pub dsn_env: String,
    #[serde(default = "default_engine")]
    pub engine: String,
    #[serde(default = "default_true")]
    pub readonly: bool,
    /// 连接级 schema 白名单：请求的 allowed_schemas 只能是它的子集（只做收敛）。
    /// ClickHouse 下语义为允许的 database 名列表；为空表示不限制。
    #[serde(default)]
    pub allowed_schemas: Vec<String>,
    #[serde(default = "default_statement_timeout")]
    pub statement_timeout_ms: i64,
}
fn default_engine() -> String {
    "clickhouse".to_string()
}
fn default_true() -> bool {
    true
}
fn default_statement_timeout() -> i64 {
    15_000
}

impl DatasourceRef {
    /// 边界校验：明文连接串含 `=`/`://`/`/` 一律拒（W1.8 凭证不落配置）。
    pub fn validate(&self) -> Result<(), String> {
        if self.name.is_empty() || self.dsn_env.is_empty() {
            return Err(format!("数据源 {} 的 name/dsn_env 不能为空", self.name));
        }
        if self.dsn_env.contains('=') || self.dsn_env.contains("://") || self.dsn_env.contains('/')
        {
            return Err(format!(
                "数据源 {} 的 dsn_env 只能是环境变量名，禁止保存明文连接串",
                self.name
            ));
        }
        if self.engine != "clickhouse" {
            return Err(format!(
                "数据源 {} 的 engine 不支持: {}（当前只接受 clickhouse connector agent）",
                self.name, self.engine
            ));
        }
        if self.statement_timeout_ms <= 0 {
            return Err(format!(
                "数据源 {} 的 statement_timeout_ms 必须为正",
                self.name
            ));
        }
        Ok(())
    }
    /// 读进程 env；缺失即显式 Err，不返回占位值。
    pub fn dsn(&self) -> Result<String, String> {
        std::env::var(&self.dsn_env).map_err(|_| {
            format!(
                "数据源 {} 引用的环境变量 {} 未设置",
                self.name, self.dsn_env
            )
        })
    }
}

/// 策略里的组定义：`code` 是唯一权限标识，`name` 只是中文展示名。
///
/// 旧配置兼容：`[[groups]]` 只写 `name` 时，该 `name` 被当成 code，展示名等于 code
/// （规范化在 [`TenantPolicy`] 的反序列化里完成，见 `TenantPolicyWire`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupDef {
    /// 组 code：capability.group / token.groups / role.default_groups / scope / 审计一律用它。
    pub code: String,
    /// 中文展示名；不参与任何权限判定。
    pub name: String,
    #[serde(default)]
    pub priority: i64,
    #[serde(default = "default_min_budget")]
    pub min_budget: i64,
}
fn default_min_budget() -> i64 {
    800
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleDef {
    /// 中文展示名（不是 code 的重复）；旧配置里等于 code 也能加载。
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub default_groups: Vec<String>,
    #[serde(default = "default_role_budget")]
    pub default_budget: i64,
}
fn default_role_budget() -> i64 {
    2000
}

/// 租户策略。反序列化统一走 [`TenantPolicyWire`]：组/角色的旧字段（`name` 当 code）
/// 在这里一次性规范化，运行期代码看到的永远是 code + 展示名。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(from = "TenantPolicyWire")]
pub struct TenantPolicy {
    #[serde(default)]
    pub groups: Vec<GroupDef>,
    #[serde(default)]
    pub roles: std::collections::HashMap<String, RoleDef>,
    #[serde(default = "default_cap")]
    pub tenant_budget_cap: i64,
    #[serde(default)]
    pub allow_intersect: bool,
}

/// TOML 载体：`code` 与 `name` 都可省略，载入后按兼容规则归一。
#[derive(Debug, Deserialize)]
struct GroupDefWire {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    priority: i64,
    #[serde(default = "default_min_budget")]
    min_budget: i64,
}

#[derive(Debug, Deserialize)]
struct TenantPolicyWire {
    #[serde(default)]
    groups: Vec<GroupDefWire>,
    #[serde(default)]
    roles: std::collections::HashMap<String, RoleDef>,
    #[serde(default = "default_cap")]
    tenant_budget_cap: i64,
    #[serde(default)]
    allow_intersect: bool,
}

impl From<TenantPolicyWire> for TenantPolicy {
    fn from(wire: TenantPolicyWire) -> Self {
        let groups = wire
            .groups
            .into_iter()
            .map(|g| {
                let raw_code = g
                    .code
                    .as_deref()
                    .map(str::trim)
                    .filter(|c| !c.is_empty())
                    // 旧配置只有 `name`：那时它存的就是 code。
                    .or_else(|| g.name.as_deref().map(str::trim).filter(|n| !n.is_empty()))
                    .unwrap_or_default()
                    .to_string();
                let raw_name = g
                    .name
                    .as_deref()
                    .map(str::trim)
                    .filter(|n| !n.is_empty())
                    .unwrap_or(&raw_code)
                    .to_string();
                GroupDef {
                    name: raw_name,
                    code: raw_code,
                    priority: g.priority,
                    min_budget: g.min_budget,
                }
            })
            .collect();
        // 角色展示名缺失时回退 code（旧配置不会因此启动失败）。
        let roles = wire
            .roles
            .into_iter()
            .map(|(code, mut def)| {
                let trimmed = def.name.trim();
                def.name = if trimmed.is_empty() {
                    code.clone()
                } else {
                    trimmed.to_string()
                };
                (code, def)
            })
            .collect();
        TenantPolicy {
            groups,
            roles,
            tenant_budget_cap: wire.tenant_budget_cap,
            allow_intersect: wire.allow_intersect,
        }
    }
}
impl Default for TenantPolicy {
    fn default() -> Self {
        Self {
            groups: vec![],
            roles: std::collections::HashMap::new(),
            tenant_budget_cap: 2000,
            allow_intersect: false,
        }
    }
}
fn default_cap() -> i64 {
    2000
}

impl TenantPolicy {
    /// 已注册组 code 集合：scope 解析/角色默认组/管理组合并一律按 code。
    pub fn group_codes(&self) -> HashSet<String> {
        self.groups.iter().map(|g| g.code.clone()).collect()
    }
    /// 按 code 查组定义（权限判定只看 code，展示名不参与）。
    pub fn group(&self, code: &str) -> Option<&GroupDef> {
        self.groups.iter().find(|g| g.code == code)
    }
    /// 未定义角色即显式 Err，不静默当无权限角色。
    pub fn role(&self, role: &str) -> Result<&RoleDef, String> {
        self.roles
            .get(role)
            .ok_or_else(|| format!("租户策略未定义角色 {role}"))
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.groups.len() > MAX_GROUPS {
            return Err(format!("组数 {} 超过上限 {MAX_GROUPS}", self.groups.len()));
        }
        let mut registered = HashSet::new();
        for g in &self.groups {
            // 配置层只保证非空与唯一：旧部署的组名可能不符合新管理面正则，
            // 不能因为加载旧配置就把进程打死（管理面新建仍走 validate_group_name）。
            if g.code.is_empty() {
                return Err("组 code 不能为空".to_string());
            }
            validate_display_name("组展示名", &g.name)?;
            if !registered.insert(g.code.clone()) {
                return Err(format!("组 code 重复: {}", g.code));
            }
        }
        for (role_name, defn) in &self.roles {
            validate_display_name("角色展示名", &defn.name)?;
            for g in &defn.default_groups {
                if !registered.contains(g) {
                    return Err(format!("角色 {role_name} 引用未注册组: {g}"));
                }
            }
            if defn.default_budget <= 0 {
                return Err(format!("角色 {role_name} default_budget 非法"));
            }
        }
        Ok(())
    }
}

/// datasources 与策略同文件时的顶层容器（与 `[[groups]]` 等策略键并存）。
#[derive(Debug, Clone, Default, Deserialize)]
struct DatasourceFile {
    #[serde(default)]
    datasources: Vec<DatasourceRef>,
}

/// 管理面主体：token 派生身份，`X-Admin-Actor` 只是自述（不可信）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminPrincipal {
    pub id: String,
    pub token: String,
}

#[derive(Debug, Clone, Default)]
pub struct AdminTokens {
    principals: Vec<AdminPrincipal>,
}

impl AdminTokens {
    /// 解析 `id:token,id:token`。格式错误、id 重复、token 重复、token 过短一律拒绝（启动 fail-closed）。
    pub fn parse(raw: &str) -> Result<Self, String> {
        let mut principals: Vec<AdminPrincipal> = Vec::new();
        for item in raw.split(',') {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }
            let (id, token) = item
                .split_once(':')
                .ok_or_else(|| format!("JUMCP_ADMIN_TOKENS 项 {item:?} 必须是 id:token 形式"))?;
            let id = id.trim();
            let token = token.trim();
            if !is_principal_id(id) {
                return Err(format!(
                    "JUMCP_ADMIN_TOKENS 的 id {id:?} 非法（[a-z0-9._-]{{2,32}}）"
                ));
            }
            if token.len() < 16 {
                return Err(format!(
                    "JUMCP_ADMIN_TOKENS 的 token（{id}）必须至少 16 字符"
                ));
            }
            if principals.iter().any(|p| p.id == id) {
                return Err(format!("JUMCP_ADMIN_TOKENS 存在重复 id: {id}"));
            }
            if principals.iter().any(|p| p.token == token) {
                return Err(format!("JUMCP_ADMIN_TOKENS 存在重复 token（{id}）"));
            }
            principals.push(AdminPrincipal {
                id: id.to_string(),
                token: token.to_string(),
            });
        }
        Ok(Self { principals })
    }

    pub fn is_empty(&self) -> bool {
        self.principals.is_empty()
    }

    pub fn len(&self) -> usize {
        self.principals.len()
    }

    pub fn ids(&self) -> Vec<&str> {
        self.principals.iter().map(|p| p.id.as_str()).collect()
    }

    /// 常量时间鉴权：遍历全部候选、等长填充逐字节比并累积，不做 `==` 早退。
    pub fn authenticate(&self, presented: &str) -> Option<&str> {
        let mut matched: Option<&str> = None;
        for principal in &self.principals {
            if constant_time_eq(&principal.token, presented) {
                matched = Some(principal.id.as_str());
            }
        }
        matched
    }
}

pub fn is_principal_id(id: &str) -> bool {
    (2..=32).contains(&id.len())
        && id.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

/// MCP 面的 agent 主体：`JUMCP_AGENT_TOKENS="agent_id:role:token,..."` 的解析结果。
///
/// 为什么单独一组 token：HTTP `/mcp` 必须按请求派生身份（`Actor.actor_id` 即 agent_id），
/// 否则所有 agent 共用进程默认角色，审计无法追责，admin 组也无从区分。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPrincipal {
    pub id: String,
    pub role: String,
    pub token: String,
}

#[derive(Debug, Clone, Default)]
pub struct AgentTokens {
    principals: Vec<AgentPrincipal>,
}

impl AgentTokens {
    /// 解析 `agent_id:role:token,...`：格式错误、id 非法/重复、token 为空/重复一律拒绝。
    ///
    /// role 是否在 `policy.roles` 里由 `Settings::validate` 判定——解析发生在读策略文件之前，
    /// 这里看不到策略；启动时两处都会跑，因此非法角色仍然在启动期暴露（fail-closed）。
    pub fn parse(raw: &str) -> Result<Self, String> {
        let mut principals: Vec<AgentPrincipal> = Vec::new();
        for item in raw.split(',') {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }
            let mut parts = item.splitn(3, ':');
            let (Some(id), Some(role), Some(token)) = (parts.next(), parts.next(), parts.next())
            else {
                return Err(format!(
                    "JUMCP_AGENT_TOKENS 项 {item:?} 必须是 agent_id:role:token 形式"
                ));
            };
            let id = id.trim();
            let role = role.trim();
            let token = token.trim();
            if !is_principal_id(id) {
                return Err(format!(
                    "JUMCP_AGENT_TOKENS 的 agent_id {id:?} 非法（[a-z0-9._-]{{2,32}}）"
                ));
            }
            if role.is_empty() {
                return Err(format!("JUMCP_AGENT_TOKENS 的 agent {id} 缺少 role"));
            }
            if token.is_empty() {
                return Err(format!("JUMCP_AGENT_TOKENS 的 token（{id}）不能为空"));
            }
            if principals.iter().any(|p| p.id == id) {
                return Err(format!("JUMCP_AGENT_TOKENS 存在重复 agent_id: {id}"));
            }
            if principals.iter().any(|p| p.token == token) {
                return Err(format!("JUMCP_AGENT_TOKENS 存在重复 token（{id}）"));
            }
            principals.push(AgentPrincipal {
                id: id.to_string(),
                role: role.to_string(),
                token: token.to_string(),
            });
        }
        Ok(Self { principals })
    }

    pub fn is_empty(&self) -> bool {
        self.principals.is_empty()
    }

    pub fn len(&self) -> usize {
        self.principals.len()
    }

    pub fn principals(&self) -> &[AgentPrincipal] {
        &self.principals
    }

    /// 常量时间鉴权：遍历全部候选、等长填充逐字节比并累积，不做 `==` 早退。
    pub fn authenticate(&self, presented: &str) -> Option<&AgentPrincipal> {
        let mut matched: Option<&AgentPrincipal> = None;
        for principal in &self.principals {
            if constant_time_eq(&principal.token, presented) {
                matched = Some(principal);
            }
        }
        matched
    }
}

/// 等长填充比较：长度差与逐字节异或结果一起按位或，全程无早退。
pub fn constant_time_eq(expected: &str, presented: &str) -> bool {
    let a = expected.as_bytes();
    let b = presented.as_bytes();
    let len = a.len().max(b.len());
    let mut diff: u8 = (a.len() ^ b.len()).min(1) as u8;
    for i in 0..len {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= x ^ y;
    }
    diff == 0
}

/// 服务级 Settings：TOML 文件 + JUMCP_MCP_* env，构造即校验。
#[derive(Debug, Clone)]
pub struct Settings {
    pub config_file: String,
    pub transport: String,
    pub host: String,
    pub port: u16,
    pub audit_path: String,
    /// capability/模板注册表（SQLite）路径：模板与版本历史落这里。
    pub registry_path: String,
    /// 启动时装载的模板定义文件（JSON）；空表示不预置模板。
    pub templates_file: String,
    /// 管理台静态构建产物目录（`JUMCP_MCP_WEB_DIR`）：空表示不托管静态资源。
    /// 非空时启动期必须存在且含 `index.html`（fail-closed，见 [`validate_web_dir`]）。
    pub web_dir: String,
    /// 管理面 token→actor 映射（JUMCP_ADMIN_TOKENS="alice:tok,bob:tok"）。
    /// HTTP 模式为空即拒绝启动（fail-closed）；不接受任何“匿名管理员”降级。
    pub admin_tokens: AdminTokens,
    /// MCP 面 per-request 身份（JUMCP_AGENT_TOKENS="agent_id:role:token,..."）。
    /// HTTP `/mcp` 只认这里派生的身份；为空时 /mcp 一律 `MCP_UNAUTHORIZED`（无兜底角色）。
    pub agent_tokens: AgentTokens,
    /// 静态客户端平铺阈值（§5.2）：可见 kind=tool 能力数 ≤ 阈值时直接平铺；0 表示禁用平铺。
    pub flatten_threshold: i64,
    pub groups_mode: String,
    /// 预算溢出策略：默认 fail；truncate 必须显式开启（§4.1.4）。
    pub budget_overflow: String,
    /// stdio 上游总开关（`JUMCP_MCP_UPSTREAM_STDIO=1`）：默认关，关闭时拒收 stdio 配置。
    pub upstream_stdio: bool,
    /// 允许注册 `read_only=false` 的上游（`JUMCP_MCP_ALLOW_WRITE_UPSTREAM=1`）。
    pub upstream_write: bool,
    pub policy: TenantPolicy,
    pub datasources: Vec<DatasourceRef>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            config_file: "config/policy.toml".to_string(),
            transport: "stdio".to_string(),
            host: "0.0.0.0".to_string(),
            port: 8000,
            audit_path: "data/audit.sqlite3".to_string(),
            registry_path: "data/registry.sqlite3".to_string(),
            templates_file: String::new(),
            web_dir: String::new(),
            admin_tokens: AdminTokens::default(),
            agent_tokens: AgentTokens::default(),
            flatten_threshold: 40,
            groups_mode: "strict".to_string(),
            budget_overflow: "fail".to_string(),
            upstream_stdio: false,
            upstream_write: false,
            policy: TenantPolicy::default(),
            datasources: vec![],
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
        s.audit_path = env_or("JUMCP_MCP_AUDIT_PATH", &s.audit_path);
        s.registry_path = env_or("JUMCP_MCP_REGISTRY_PATH", &s.registry_path);
        s.templates_file = env_or("JUMCP_MCP_TEMPLATES_FILE", &s.templates_file);
        s.web_dir = env_or("JUMCP_MCP_WEB_DIR", &s.web_dir);
        s.admin_tokens = AdminTokens::parse(&env_or("JUMCP_ADMIN_TOKENS", ""))?;
        s.agent_tokens = AgentTokens::parse(&env_or("JUMCP_AGENT_TOKENS", ""))?;
        s.flatten_threshold = env_or("JUMCP_MCP_FLATTEN_THRESHOLD", "40")
            .parse::<i64>()
            .map_err(|_| "JUMCP_MCP_FLATTEN_THRESHOLD 非法".to_string())?;
        s.groups_mode = env_or("JUMCP_MCP_GROUPS_MODE", &s.groups_mode);
        s.budget_overflow = env_or("JUMCP_MCP_BUDGET_OVERFLOW", &s.budget_overflow);
        s.upstream_stdio = env_or("JUMCP_MCP_UPSTREAM_STDIO", "0") == "1";
        s.upstream_write = env_or("JUMCP_MCP_ALLOW_WRITE_UPSTREAM", "0") == "1";
        if Path::new(&s.config_file).exists() {
            let text = std::fs::read_to_string(&s.config_file).map_err(|e| e.to_string())?;
            s.policy = toml::from_str(&text).map_err(|e| format!("TOML 解析失败: {e}"))?;
            let extra: DatasourceFile =
                toml::from_str(&text).map_err(|e| format!("TOML datasources 解析失败: {e}"))?;
            s.datasources = extra.datasources;
        }
        s.validate()?;
        Ok(s)
    }
    pub fn validate(&self) -> Result<(), String> {
        if !matches!(self.transport.as_str(), "stdio" | "streamable-http") {
            return Err(format!("transport 非法: {}", self.transport));
        }
        if !matches!(self.groups_mode.as_str(), "strict" | "intersect") {
            return Err(format!("groups_mode 非法: {}", self.groups_mode));
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
        for ds in &self.datasources {
            ds.validate()?;
        }
        // fail-closed：声明了数据源就必须恰好一个 readonly；非 readonly 数据源不允许静默忽略。
        if !self.datasources.is_empty() {
            let mutable: Vec<&str> = self
                .datasources
                .iter()
                .filter(|ds| !ds.readonly)
                .map(|ds| ds.name.as_str())
                .collect();
            if !mutable.is_empty() {
                return Err(format!(
                    "数据源必须全部 readonly，当前非只读声明: {}",
                    mutable.join(",")
                ));
            }
            if self.datasources.len() != 1 {
                let names: Vec<&str> = self.datasources.iter().map(|ds| ds.name.as_str()).collect();
                return Err(format!(
                    "必须恰好一个 readonly 数据源，当前声明了 {} 个: {}",
                    self.datasources.len(),
                    names.join(",")
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
    /// 唯一主数据源：第一个 readonly 声明。
    ///
    /// 执行面只支持一个主数据源，多个 readonly 声明由启动入口显式拒绝；
    /// 引擎是否已实现同样在启动入口按 `engine` 分发时判定（fail-closed）。
    pub fn primary_datasource(&self) -> Option<&DatasourceRef> {
        self.datasources.iter().find(|d| d.readonly)
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_over_limit_groups() {
        let p = TenantPolicy {
            groups: (0..21)
                .map(|i| GroupDef {
                    code: format!("db.read.g{i}"),
                    name: format!("组{i}"),
                    priority: 1,
                    min_budget: 800,
                })
                .collect(),
            ..Default::default()
        };
        assert!(p.validate().is_err());
    }
    #[test]
    fn rejects_unknown_role_group() {
        let mut roles = std::collections::HashMap::new();
        roles.insert(
            "analyst".into(),
            RoleDef {
                name: "analyst".into(),
                default_groups: vec!["db.read.nope".into()],
                default_budget: 2000,
            },
        );
        let p = TenantPolicy {
            groups: vec![GroupDef {
                code: "db.read.core".into(),
                name: "核心数据只读".into(),
                priority: 1,
                min_budget: 800,
            }],
            roles,
            ..Default::default()
        };
        assert!(p.validate().is_err());
    }
    #[test]
    fn intersect_gate() {
        let s = Settings {
            groups_mode: "intersect".to_string(),
            ..Default::default()
        };
        assert!(s.validate().is_err());
        let s2 = Settings {
            groups_mode: "intersect".to_string(),
            policy: TenantPolicy {
                allow_intersect: true,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(s2.validate().is_ok());
    }
    #[test]
    fn budget_overflow_closed_set() {
        let s = Settings {
            budget_overflow: "sometimes".to_string(),
            ..Default::default()
        };
        assert!(s.validate().is_err());
        let ok = Settings {
            budget_overflow: "truncate".to_string(),
            ..Default::default()
        };
        assert!(ok.validate().is_ok());
        assert_eq!(
            ok.overflow_policy(),
            crate::budget::OverflowPolicy::Truncate
        );
    }
    #[test]
    fn datasource_dsn_must_be_env_ref() {
        let ds = DatasourceRef {
            name: "main".into(),
            dsn_env: "postgres://u:p@h/db".into(),
            engine: "postgres".into(),
            readonly: true,
            allowed_schemas: vec![],
            statement_timeout_ms: 1000,
        };
        assert!(ds.validate().is_err());
    }
    #[test]
    fn datasources_must_be_exactly_one_readonly() {
        let base = DatasourceRef {
            name: "ch".into(),
            dsn_env: "JUMCP_TEST_CLICKHOUSE_DSN".into(),
            engine: "clickhouse".into(),
            readonly: true,
            allowed_schemas: vec![],
            statement_timeout_ms: 1000,
        };
        let ok = Settings {
            datasources: vec![base.clone()],
            ..Default::default()
        };
        assert!(ok.validate().is_ok());

        let mutable = Settings {
            datasources: vec![DatasourceRef {
                readonly: false,
                ..base.clone()
            }],
            ..Default::default()
        };
        let e = mutable.validate().unwrap_err();
        assert!(e.contains("readonly"), "{e}");
        assert!(e.contains("ch"), "{e}");

        let duplicate = Settings {
            datasources: vec![
                base.clone(),
                DatasourceRef {
                    name: "ch2".into(),
                    ..base
                },
            ],
            ..Default::default()
        };
        let e = duplicate.validate().unwrap_err();
        assert!(e.contains("恰好一个"), "{e}");
        assert!(e.contains("ch,ch2"), "{e}");
    }

    #[test]
    fn clickhouse_engine_is_accepted_and_primary_is_first_readonly() {
        let clickhouse = DatasourceRef {
            name: "ch".into(),
            dsn_env: "JUMCP_TEST_CLICKHOUSE_DSN".into(),
            engine: "clickhouse".into(),
            readonly: true,
            allowed_schemas: vec!["env_air".into()],
            statement_timeout_ms: 1000,
        };
        assert!(clickhouse.validate().is_ok());

        let mutable = DatasourceRef {
            engine: "mysql".into(),
            readonly: false,
            ..clickhouse.clone()
        };
        // 非 clickhouse 引擎即使只是声明也拒绝：启动入口不保留假实现。
        assert!(mutable.validate().is_err());

        let settings = Settings {
            datasources: vec![mutable, clickhouse],
            ..Default::default()
        };
        assert_eq!(
            settings.primary_datasource().map(|ds| ds.name.as_str()),
            Some("ch")
        );
    }
    #[test]
    fn admin_tokens_parse_and_authenticate() {
        let tokens =
            AdminTokens::parse("alice:tok_alice_000000000,bob:tok_bob_00000000000").unwrap();
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens.authenticate("tok_bob_00000000000"), Some("bob"));
        assert_eq!(tokens.authenticate("tok_bob_00000000001"), None);
        assert_eq!(tokens.authenticate(""), None);
        assert!(AdminTokens::parse("").unwrap().is_empty());
    }

    #[test]
    fn admin_tokens_reject_bad_inputs() {
        assert!(AdminTokens::parse("alice").is_err(), "缺少冒号");
        assert!(
            AdminTokens::parse("ALICE:tok_alice_000000000").is_err(),
            "id 大写"
        );
        assert!(
            AdminTokens::parse("a:tok_alice_000000000").is_err(),
            "id 过短"
        );
        assert!(
            AdminTokens::parse("alice:short,bob:tok_bob_00000000000").is_err(),
            "token 过短"
        );
        assert!(
            AdminTokens::parse("alice:tok_x_0000000000,alice:tok_y_0000000000").is_err(),
            "id 重复"
        );
        assert!(
            AdminTokens::parse("alice:tok_same_00000000,bob:tok_same_00000000").is_err(),
            "token 重复"
        );
    }

    #[test]
    fn agent_tokens_parse_and_authenticate() {
        let tokens = AgentTokens::parse(
            "analyst-1:analyst:tok_a_000000000000, bot-admin:admin:tok_b_000000000000",
        )
        .unwrap();
        assert_eq!(tokens.len(), 2);
        assert_eq!(
            tokens
                .authenticate("tok_b_000000000000")
                .map(|p| (p.id.as_str(), p.role.as_str())),
            Some(("bot-admin", "admin")),
            "合法 token 必须能查到 agent_id 与 role"
        );
        assert!(tokens.authenticate("tok_unknown_000000").is_none());
        assert!(tokens.authenticate("").is_none());
        assert!(AgentTokens::parse("").unwrap().is_empty());
    }

    #[test]
    fn agent_tokens_reject_bad_inputs() {
        assert!(
            AgentTokens::parse("analyst-1:analyst").is_err(),
            "缺少 token"
        );
        assert!(
            AgentTokens::parse("analyst-1::tok_000000000000").is_err(),
            "role 为空"
        );
        assert!(
            AgentTokens::parse("analyst-1:analyst:").is_err(),
            "token 为空"
        );
        assert!(
            AgentTokens::parse("A1:analyst:tok_000000000000").is_err(),
            "agent_id 非法"
        );
        assert!(
            AgentTokens::parse("a1:analyst:tok_1_00000000000,a1:analyst:tok_2_00000000000")
                .is_err(),
            "agent_id 重复"
        );
        assert!(
            AgentTokens::parse("a1:analyst:tok_same_0000,a2:admin:tok_same_0000").is_err(),
            "token 重复"
        );
    }

    /// 角色必须在 policy.roles 里存在；否则启动期 Err，而不是每个请求才报未知角色。
    #[test]
    fn agent_token_role_must_exist_in_policy() {
        let mut roles = std::collections::HashMap::new();
        roles.insert(
            "analyst".into(),
            RoleDef {
                name: "analyst".into(),
                default_groups: vec!["db.read.core".into()],
                default_budget: 2000,
            },
        );
        let base = Settings {
            policy: TenantPolicy {
                groups: vec![GroupDef {
                    code: "db.read.core".into(),
                    name: "核心数据只读".into(),
                    priority: 1,
                    min_budget: 800,
                }],
                roles,
                ..Default::default()
            },
            agent_tokens: AgentTokens::parse("analyst-1:analyst:tok_000000000000").unwrap(),
            ..Default::default()
        };
        assert!(base.validate().is_ok());

        let bad = Settings {
            agent_tokens: AgentTokens::parse("ghost-1:ghost:tok_000000000000").unwrap(),
            ..base.clone()
        };
        let e = bad.validate().unwrap_err();
        assert!(e.contains("未定义角色"), "{e}");
    }

    #[test]
    fn constant_time_eq_matches_semantics() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "abcd"));
        assert!(!constant_time_eq("abcd", "abc"));
        assert!(constant_time_eq("", ""));
    }

    #[test]
    fn loads_repo_policy_file() {
        let s = Settings::from_env().unwrap();
        assert!(s.validate().is_ok());
    }

    /// 新形状：组 code + 中文展示名；角色的 name 是展示名而不是 code。
    #[test]
    fn parses_group_code_with_chinese_names() {
        let text = r#"
tenant_budget_cap = 2000

[roles.analyst]
name = "数据分析"
default_groups = ["db.read.core"]
default_budget = 2000

[[groups]]
code = "db.read.core"
name = "核心数据只读"
priority = 10
min_budget = 800
"#;
        let p: TenantPolicy = toml::from_str(text).unwrap();
        assert_eq!(p.groups[0].code, "db.read.core");
        assert_eq!(p.groups[0].name, "核心数据只读");
        assert_eq!(p.groups[0].min_budget, 800);
        assert_eq!(p.roles["analyst"].name, "数据分析");
        assert_eq!(
            p.group_codes(),
            HashSet::from(["db.read.core".to_string()]),
            "内部组集合只含 code"
        );
        p.validate().unwrap();
    }

    /// 旧配置兼容：`[[groups]]` 只有 name 时，name 就是 code，展示名回退 code。
    #[test]
    fn legacy_name_only_group_keeps_code_as_display_name() {
        let text = r#"
[[groups]]
name = "db.read.core"
priority = 10
"#;
        let p: TenantPolicy = toml::from_str(text).unwrap();
        assert_eq!(p.groups[0].code, "db.read.core");
        assert_eq!(p.groups[0].name, "db.read.core");
        assert_eq!(p.groups[0].min_budget, 800);
    }

    /// 角色缺 name（旧配置）时回退 code，不因展示名缺失启动失败。
    #[test]
    fn legacy_role_without_name_falls_back_to_code() {
        let text = r#"
[roles.executor]
default_groups = []
default_budget = 1600
"#;
        let p: TenantPolicy = toml::from_str(text).unwrap();
        assert_eq!(p.roles["executor"].name, "executor");
    }

    /// 展示名重复 code 是允许的（旧配置），但空展示名必须拒绝。
    #[test]
    fn empty_display_name_is_rejected() {
        let p = TenantPolicy {
            groups: vec![GroupDef {
                code: "db.read.core".into(),
                name: "  ".into(),
                priority: 1,
                min_budget: 800,
            }],
            ..Default::default()
        };
        assert!(p.validate().is_err());
    }

    /// 默认不托管：空串是合法值，行为与未引入静态托管前完全一致。
    #[test]
    fn empty_web_dir_is_valid_and_means_no_hosting() {
        assert!(validate_web_dir("").is_ok());
        let s = Settings::default();
        assert_eq!(s.web_dir, "");
        assert!(s.validate().is_ok());
    }

    /// 目录存在但产物没构建（缺 index.html）：启动期报错，而不是运行期 404。
    #[test]
    fn web_dir_without_index_html_is_rejected_at_startup() {
        let dir = tempfile::tempdir().unwrap();
        let e = validate_web_dir(dir.path().to_str().unwrap()).unwrap_err();
        assert!(e.contains("缺少 index.html"), "{e}");

        let s = Settings {
            web_dir: dir.path().to_string_lossy().into_owned(),
            ..Default::default()
        };
        assert!(
            s.validate().is_err(),
            "缺 index.html 不得通过 Settings::validate"
        );
    }

    #[test]
    fn web_dir_pointing_at_missing_path_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("dist");
        let e = validate_web_dir(missing.to_str().unwrap()).unwrap_err();
        assert!(e.contains("不是目录"), "{e}");
    }

    #[test]
    fn web_dir_with_index_html_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), "<!doctype html>").unwrap();
        validate_web_dir(dir.path().to_str().unwrap()).unwrap();
        let s = Settings {
            web_dir: dir.path().to_string_lossy().into_owned(),
            ..Default::default()
        };
        s.validate().unwrap();
    }
}
