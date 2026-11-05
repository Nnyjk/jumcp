//! 策略配置：TOML + env，非法配置构造即 Err（启动 fail-closed）。
//! 为什么 env 前缀 HJZL_MCP_：与 Python Settings env_prefix 对齐，部署只改 env 不改代码。

use crate::budget::OverflowPolicy;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

pub const MAX_GROUPS: usize = 20;

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
    #[serde(default)]
    pub allowed_schemas: Vec<String>,
    #[serde(default = "default_statement_timeout")]
    pub statement_timeout_ms: i64,
}
fn default_engine() -> String {
    "postgres".to_string()
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
        if !matches!(self.engine.as_str(), "postgres" | "mysql" | "clickhouse") {
            return Err(format!(
                "数据源 {} 的 engine 非法: {}",
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupDef {
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
    pub name: String,
    #[serde(default)]
    pub default_groups: Vec<String>,
    #[serde(default = "default_role_budget")]
    pub default_budget: i64,
}
fn default_role_budget() -> i64 {
    2000
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    pub fn group_names(&self) -> HashSet<String> {
        self.groups.iter().map(|g| g.name.clone()).collect()
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
        let registered = self.group_names();
        for (role_name, defn) in &self.roles {
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

/// 服务级 Settings：TOML 文件 + HJZL_MCP_* env，构造即校验。
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
    /// 管理面 Bearer token（HJZL_ADMIN_TOKEN）。HTTP 模式未配置即拒绝启动（fail-closed）。
    pub admin_token: String,
    /// 静态客户端平铺阈值（§5.2）：可见 kind=tool 能力数 ≤ 阈值时直接平铺；0 表示禁用平铺。
    pub flatten_threshold: i64,
    pub groups_mode: String,
    /// 预算溢出策略：默认 fail；truncate 必须显式开启（§4.1.4）。
    pub budget_overflow: String,
    pub policy: TenantPolicy,
    pub datasources: Vec<DatasourceRef>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            config_file: "config/policy.toml".to_string(),
            transport: "stdio".to_string(),
            host: "127.0.0.1".to_string(),
            port: 8000,
            audit_path: "data/audit.sqlite3".to_string(),
            registry_path: "data/registry.sqlite3".to_string(),
            templates_file: String::new(),
            admin_token: String::new(),
            flatten_threshold: 40,
            groups_mode: "strict".to_string(),
            budget_overflow: "fail".to_string(),
            policy: TenantPolicy::default(),
            datasources: vec![],
        }
    }
}

impl Settings {
    /// 单一入口：从路径/env 构造并校验（非法 → Err，调用方不得吞掉）。
    pub fn from_env() -> Result<Self, String> {
        let mut s = Settings::default();
        s.config_file = env_or("HJZL_MCP_CONFIG_FILE", &s.config_file);
        s.transport = env_or("HJZL_MCP_TRANSPORT", &s.transport);
        s.host = env_or("HJZL_MCP_HOST", &s.host);
        s.port = env_or("HJZL_MCP_PORT", "8000")
            .parse::<u16>()
            .map_err(|_| "HJZL_MCP_PORT 非法".to_string())?;
        s.audit_path = env_or("HJZL_MCP_AUDIT_PATH", &s.audit_path);
        s.registry_path = env_or("HJZL_MCP_REGISTRY_PATH", &s.registry_path);
        s.templates_file = env_or("HJZL_MCP_TEMPLATES_FILE", &s.templates_file);
        s.admin_token = env_or("HJZL_ADMIN_TOKEN", &s.admin_token);
        s.flatten_threshold = env_or("HJZL_MCP_FLATTEN_THRESHOLD", "40")
            .parse::<i64>()
            .map_err(|_| "HJZL_MCP_FLATTEN_THRESHOLD 非法".to_string())?;
        s.groups_mode = env_or("HJZL_MCP_GROUPS_MODE", &s.groups_mode);
        s.budget_overflow = env_or("HJZL_MCP_BUDGET_OVERFLOW", &s.budget_overflow);
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
        self.policy.validate()?;
        for ds in &self.datasources {
            ds.validate()?;
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
    /// 阶段一唯一数据源：第一个 postgres 且 readonly 的声明。
    pub fn primary_datasource(&self) -> Option<&DatasourceRef> {
        self.datasources
            .iter()
            .find(|d| d.engine == "postgres" && d.readonly)
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
                    name: format!("db.read.g{i}"),
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
                name: "db.read.core".into(),
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
    fn loads_repo_policy_file() {
        let s = Settings::from_env().unwrap();
        assert!(s.validate().is_ok());
    }
}
