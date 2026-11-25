//! 租户策略：组/角色定义与预算上限；反序列化时把只写 `name` 的组补成 code。

use super::{BASIC_GROUP_NAME, LEGACY_BUILTIN_GROUP_NAME, MAX_GROUPS, validate_display_name};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// 策略里的组定义：`code` 是唯一权限标识，`name` 只是中文展示名。
///
/// `[[groups]]` 只写 `name` 时按 code 处理，展示名等于 code
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
    /// 中文展示名（不是 code 的重复）；允许与 code 相同。
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
                    // 只写了 `name`：它就是 code。
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
        // 角色展示名缺失时回退 code，不因此启动失败。
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

/// 代码内置 `basic` 组的定义：scope 解析只读它的数值字段，展示名固定。
///
/// 配置文件不得定义 `basic`（见 `validate`），因此这里是它唯一的来源；
/// 与普通组同形是为了让 `resolve_scope` 的预算/优先级计算不必为它开分支。
fn basic_group_def() -> &'static GroupDef {
    static BASIC: std::sync::LazyLock<GroupDef> = std::sync::LazyLock::new(|| GroupDef {
        code: BASIC_GROUP_NAME.to_string(),
        name: "基础工具".to_string(),
        priority: 0,
        min_budget: 800,
    });
    &BASIC
}

impl TenantPolicy {
    /// 已注册组 code 集合：scope 解析/角色默认组/管理组合并一律按 code。
    ///
    /// 恒含代码内置的 `basic`：它是真实授权组（令牌可勾选），只是定义不来自配置文件。
    pub fn group_codes(&self) -> HashSet<String> {
        let mut out: HashSet<String> = self.groups.iter().map(|g| g.code.clone()).collect();
        out.insert(BASIC_GROUP_NAME.to_string());
        out
    }
    /// 按 code 查组定义（权限判定只看 code，展示名不参与）。
    ///
    /// `basic` 返回内置定义：scope 解析要读它的 `min_budget`/`priority`，配置文件不允许定义它。
    pub fn group(&self, code: &str) -> Option<&GroupDef> {
        if code == BASIC_GROUP_NAME {
            return Some(basic_group_def());
        }
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
        // `basic` 是代码内置的注册组：角色默认组/令牌都可以直接引用它。
        registered.insert(BASIC_GROUP_NAME.to_string());
        for g in &self.groups {
            // 配置层只保证非空与唯一：旧部署的组名可能不符合新管理面正则，
            // 加载期不因组名非法打死进程（管理面新建仍走 validate_group_name）。
            if g.code.is_empty() {
                return Err("组 code 不能为空".to_string());
            }
            // basic 由代码内置、builtin 是保留名：配置文件定义它们会让
            // "组索引里有一个不可治理的组"，启动即失败（admin 仍允许由策略定义）。
            if g.code == BASIC_GROUP_NAME || g.code == LEGACY_BUILTIN_GROUP_NAME {
                return Err(format!(
                    "组 code {code} 是代码内置组名，配置文件不得定义",
                    code = g.code
                ));
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
