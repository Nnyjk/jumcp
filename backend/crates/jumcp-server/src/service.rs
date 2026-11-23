//! 服务聚合：配置、catalog、审计与按会话预算。
//! 为什么预算放在 Service 而不是每个 handler：schema/result 累计必须跨调用共享。

use crate::catalog::{Capability, Catalog};
use crate::discovery;
use crate::upstream::UpstreamManager;
use jumcp_core::audit::{ActorRef, AuditEvent, AuditLog, ChainVerdict};
use jumcp_core::budget::{
    BudgetCaps, BudgetCheckResult, BudgetLedger, OverflowPolicy, estimate_tokens,
};
use jumcp_core::config::{GroupDef, Settings, TenantPolicy};
use jumcp_core::entry::{Actor, EffectiveScope, GroupRequest, resolve_scope};
use jumcp_core::errors::McpError;
use jumcp_core::registry::CapabilityStore;
use jumcp_core::template::TemplateRegistry;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

/// 常驻工具名：与 `jumcp_core::registry::RESIDENT_CAPABILITY_IDS` 同一份列表
/// （"哪些 id 是内置"只能有一个来源，否则发现面与存储层会漂移）。
pub const RESIDENT_TOOL_NAMES: &[&str] = jumcp_core::registry::RESIDENT_CAPABILITY_IDS;

/// 维护工具名：`group="admin"` 的内置工具（可见性由 `discovery::admin_enabled` 判定）。
pub const ADMIN_TOOL_NAMES: &[&str] = jumcp_core::registry::ADMIN_CAPABILITY_IDS;

/// 预算与模板版本钉住的会话键：由请求身份派生，`"<tenant>:<actor_id>"`。
///
/// 为什么不用进程级 `session_id`：多 agent 共用一个进程时，一个 agent 的 schema/结果 token
/// 会吃掉另一个 agent 的预算，模板版本钉住也会互相顶掉。键只依赖本次请求的身份，
/// 因此并发请求之间没有共享可写状态（账本内部仍按 key 分桶）。
/// stdio 没有 per-request 身份（`actor_id` 为空），沿用 `"default"`。
pub fn session_key(actor: &Actor) -> String {
    if actor.actor_id.is_empty() {
        "default".to_string()
    } else {
        format!("{}:{}", actor.tenant, actor.actor_id)
    }
}

/// `X-MCP-Tool-Groups` 的原始值（头名大小写不敏感）；`Some("")` 表示显式空集。
///
/// 取"是否存在"与"取值"共用这一处，避免 HTTP 入口的通道优先级判定与 scope 解析各写一份。
pub fn header_groups(headers: &std::collections::HashMap<String, String>) -> Option<String> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("x-mcp-tool-groups"))
        .map(|(_, v)| v.clone())
}

pub struct Service {
    pub settings: Settings,
    pub catalog: Catalog,
    /// 能力注册表：发现面每次请求从这里按 scope 现查，因此管理面发布即时可见。
    pub store: Option<Arc<CapabilityStore>>,
    pub audit: Option<Arc<AuditLog>>,
    pub scope: Option<EffectiveScope>,
    pub budget: Mutex<BudgetLedger>,
    pub templates: Option<Arc<TemplateRegistry>>,
    /// 动态插入的上游 MCP server（`None` = 本进程未启用 router 上游）。
    pub upstreams: Option<Arc<UpstreamManager>>,
    /// 审计事件的 `session_id` 列（审计检索用）。
    /// 预算账本与模板版本钉住**不**用它：那两处按请求身份派生（见 `session_key`）。
    pub session_id: String,
    /// 后台静态审计快照（已渲染的候选报告）：`None` = 调度未启用、尚未刷新或审计未启用。
    ///
    /// 只由 `crate::static_audit` 写；MCP 工具读它来跳过"调用时扫描审计库"这一步。
    static_audit: Mutex<Option<Arc<Value>>>,
}

impl Service {
    pub fn new(settings: Settings) -> Self {
        let scope = default_scope(&settings);
        Self {
            settings,
            catalog: Catalog::default(),
            store: None,
            audit: None,
            scope,
            budget: Mutex::new(BudgetLedger::new(None)),
            templates: None,
            upstreams: None,
            session_id: "default".to_string(),
            static_audit: Mutex::new(None),
        }
    }
    pub fn with_catalog(mut self, catalog: Catalog) -> Self {
        self.catalog = catalog;
        self
    }
    pub fn with_store(mut self, store: Arc<CapabilityStore>) -> Self {
        self.store = Some(store);
        self
    }
    pub fn with_audit(mut self, audit: Arc<AuditLog>) -> Self {
        self.audit = Some(audit);
        self
    }
    /// 覆盖后台静态审计快照：调度器按固定窗口重算后整体替换（不合并、不追加）。
    pub fn set_static_audit(&self, report: Value) -> Result<(), McpError> {
        let mut slot = self.static_audit.lock().map_err(|_| McpError::lock())?;
        *slot = Some(Arc::new(report));
        Ok(())
    }
    /// 读取最近一次静态审计快照；`None` 时调用方必须回退到实时计算。
    pub fn static_audit(&self) -> Option<Arc<Value>> {
        self.static_audit.lock().ok().and_then(|slot| slot.clone())
    }
    pub fn with_scope(mut self, scope: EffectiveScope) -> Self {
        self.scope = Some(scope);
        self
    }
    /// 注入模板注册表时一并接上它的 store：发现面与模板层必须读同一份能力表。
    pub fn with_templates(mut self, templates: Arc<TemplateRegistry>) -> Self {
        self.store = Some(templates.store());
        self.templates = Some(templates);
        self
    }
    pub fn with_budget_caps(mut self, caps: BudgetCaps) -> Self {
        self.budget = Mutex::new(BudgetLedger::new(Some(caps)));
        self
    }
    /// 挂上 router 上游管理器：此后发现面/`tools/call` 会把启用上游的工具一并投影。
    pub fn with_upstreams(mut self, upstreams: Arc<UpstreamManager>) -> Self {
        self.upstreams = Some(upstreams);
        self
    }
    pub fn upstreams(&self) -> Option<&Arc<UpstreamManager>> {
        self.upstreams.as_ref()
    }

    pub fn require_scope(&self) -> Result<&EffectiveScope, McpError> {
        self.scope.as_ref().ok_or_else(|| {
            McpError::new(
                "MCP_STORE_ERROR",
                json!({"error": "EffectiveScope 未初始化"}),
            )
        })
    }

    /// 进程默认身份：**只**用于 stdio（逐行 JSON-RPC，没有 HTTP 头可派生 per-request 身份）。
    /// HTTP 面的身份一律来自数据库 access token（`access_tokens` 为空时由
    /// `JUMCP_AGENT_TOKENS` 一次性引导导入），不存在回落到这里的路径。
    pub fn process_actor(&self) -> Actor {
        Actor::new("default", &default_role(&self.settings).unwrap_or_default())
    }

    pub fn overflow_policy(&self) -> OverflowPolicy {
        self.settings.overflow_policy()
    }

    /// HTTP `/mcp` 的唯一鉴权：`Authorization: Bearer <数据库 access token>`。
    ///
    /// 只接受 active（未撤销/未过期）令牌并刷新 `last_used_at`；store 未注入时一律拒绝
    /// （不存在"没有注册表就放行"的降级路径）。
    pub fn authenticate_mcp_token(
        &self,
        presented: &str,
    ) -> Result<Option<jumcp_core::access::AccessTokenRecord>, McpError> {
        match self.store.as_deref() {
            Some(store) => store.authenticate_access_token(presented),
            None => Ok(None),
        }
    }

    /// 发现面唯一可见集：builtin 常驻工具 ∪ 注册表可见行 ∪ 内存目录可见条目。
    /// search / describe / list_groups / tools-list 平铺 全部走这里。
    pub fn visible_capabilities(
        &self,
        scope: &EffectiveScope,
    ) -> Result<Vec<Capability>, McpError> {
        discovery::visible_capabilities(&self.catalog, self.store.as_deref(), scope)
    }

    /// 调用面集合：`published ∧ callable ∧ group ∈ scope`（可见性不决定可调用性）。
    /// 系统直调（连接器/上游）用它与发现面共享 predicate 实现，
    /// 但不要求 discoverable。
    pub fn callable_capabilities(
        &self,
        scope: &EffectiveScope,
    ) -> Result<Vec<Capability>, McpError> {
        discovery::callable_capabilities(&self.catalog, self.store.as_deref(), scope)
    }

    /// 发现面可见集 + 本次 scope 可见的上游工具投影。
    ///
    /// 上游按"启用 ∧ 组在 scope 内"过滤，失败的上游只贡献空集（失败隔离），
    /// 因此单个上游不可达不会影响内置工具与其他上游。
    pub async fn visible_capabilities_with_upstreams(
        &self,
        scope: &EffectiveScope,
    ) -> Result<Vec<Capability>, McpError> {
        let base = self.visible_capabilities(scope)?;
        let Some(upstreams) = self.upstreams() else {
            return Ok(base);
        };
        let extra = upstreams.capabilities(scope).await;
        Ok(discovery::merge_exposed(base, extra))
    }

    /// 常驻 token 计量（口径：每个工具 canonical JSON 字节数 / 4，逐个累加）。
    pub fn resident_tokens(&self, tools: &[Value]) -> i64 {
        tools.iter().map(estimate_tokens).sum()
    }

    /// 常驻预算判定：默认 fail；truncate 显式开启才返回带标记的结果。
    pub fn check_resident(&self, resident_tokens: i64) -> Result<BudgetCheckResult, McpError> {
        let ledger = self.budget.lock().map_err(|_| McpError::lock())?;
        ledger.check_resident(resident_tokens, self.overflow_policy())
    }

    /// schema 预算预检：`session_key` 是请求身份派生出的会话键（见 `session_key`），
    /// 只有同一 agent 的跨请求才共享同一本账。
    pub fn check_schema(
        &self,
        session_key: &str,
        incoming_tokens: i64,
    ) -> Result<BudgetCheckResult, McpError> {
        let mut ledger = self.budget.lock().map_err(|_| McpError::lock())?;
        Ok(ledger.check_schema(session_key, incoming_tokens, self.overflow_policy()))
    }

    pub fn record_schema(&self, session_key: &str, token_cost: i64) -> Result<(), McpError> {
        let mut ledger = self.budget.lock().map_err(|_| McpError::lock())?;
        ledger.record_schema(session_key, token_cost);
        Ok(())
    }

    /// 结果记账：返回记账后的预算状态（`summary` 决定是否只回摘要）。
    pub fn record_result(
        &self,
        session_key: &str,
        token_cost: i64,
    ) -> Result<BudgetCheckResult, McpError> {
        let mut ledger = self.budget.lock().map_err(|_| McpError::lock())?;
        ledger.record_result(session_key, token_cost);
        Ok(ledger.summary_state(session_key))
    }

    /// 追加审计：审计写失败必须上抛（治理面不可静默失效）。
    ///
    /// 返回 `event_id`：拒绝路径要把 `policy_decision.correlation_id` 指向同一次失败的
    /// `tool_call`，所以写入点必须能把 id 交回调用方（未启用审计时返回空串）。
    pub fn audit_event(
        &self,
        event_type: &str,
        payload: Value,
        actor: &Actor,
    ) -> Result<String, McpError> {
        let Some(log) = &self.audit else {
            return Ok(String::new());
        };
        log.append(AuditEvent {
            tenant: actor.tenant.clone(),
            actor: ActorRef {
                actor_type: actor.actor_type.clone(),
                id: actor.actor_id.clone(),
                role: actor.role.clone(),
                token_id: (!actor.token_id.is_empty()).then(|| actor.token_id.clone()),
                source: (!actor.source.is_empty()).then(|| actor.source.clone()),
            },
            event_type: event_type.into(),
            payload,
            session_id: self.session_id.clone(),
            ts: String::new(),
            event_id: String::new(),
        })
        .map(|env| env.event_id)
    }

    /// 拒绝路径统一审计（policy_decision）：统一载荷 + 人类可读错误信息。
    pub fn policy_decision(
        &self,
        actor: &Actor,
        e: &McpError,
        detail: Value,
    ) -> Result<String, McpError> {
        self.policy_decision_correlated(actor, e, detail, None)
    }

    /// 带关联的拒绝审计：`correlation_id` 指向同一次失败先写入的 `tool_call` event_id。
    pub fn policy_decision_correlated(
        &self,
        actor: &Actor,
        e: &McpError,
        detail: Value,
        correlation_id: Option<&str>,
    ) -> Result<String, McpError> {
        let object_id = detail
            .get("object_id")
            .and_then(Value::as_str)
            .or_else(|| detail.get("tool").and_then(Value::as_str))
            .or_else(|| detail.get("capability_id").and_then(Value::as_str))
            .or_else(|| detail.get("template_id").and_then(Value::as_str))
            .or_else(|| detail.get("filter").and_then(Value::as_str))
            .unwrap_or("request");
        let mut payload = jumcp_core::audit::payload("request", object_id, "deny", "denied");
        payload.insert("error_code".into(), json!(e.code));
        payload.insert("error_message".into(), json!(e.message));
        payload.insert(
            "correlation_id".into(),
            correlation_id.map_or(Value::Null, |id| json!(id)),
        );
        payload.insert("data".into(), json!({"detail": detail}));
        self.audit_event("policy_decision", Value::Object(payload), actor)
    }

    /// 有效租户策略 = 配置策略 ∪ 管理面新增组。
    ///
    /// 规则（唯一实现，所有 scope 解析调用点共用）：
    /// - `policy.groups` 原样保留（配置管理，API 不可改/删）；同名管理组不重复追加；
    /// - 管理组追加进 `groups`；
    /// - 管理组 `roles` 里的角色，把组名追加进该角色 `default_groups`（去重，保持已有顺序）。
    ///
    /// 每次现查 SQLite：管理面新增/删除组后**下一次**请求的 scope 解析立即生效。
    /// `resolve_scope` 仍是纯函数——这里只是给它一份合并后的 policy。
    pub fn effective_policy(&self) -> Result<TenantPolicy, McpError> {
        let mut policy = self.settings.policy.clone();
        let Some(store) = self.store.as_deref() else {
            return Ok(policy);
        };
        for group in store.managed_groups()? {
            if policy.groups.iter().any(|g| g.code == group.code) {
                continue;
            }
            // 授权/scope 一律用 code；展示名只随定义带过去。
            for role in &group.roles {
                if let Some(def) = policy.roles.get_mut(role)
                    && !def.default_groups.iter().any(|g| g == &group.code)
                {
                    def.default_groups.push(group.code.clone());
                }
            }
            policy.groups.push(GroupDef {
                code: group.code,
                name: group.name,
                priority: group.priority,
                min_budget: group.min_budget,
            });
        }
        Ok(policy)
    }

    /// 创建能力前的组存在性校验：组必须来自配置 policy 或管理面新增组。
    ///
    /// 两条入口（HTTP create / MCP admin_register_template）共用本方法；不存在时返回
    /// `MCP_GROUP_NOT_FOUND`（HTTP 404），调用方必须在落库之前调用。
    pub fn ensure_group_exists(&self, group: &str) -> Result<(), McpError> {
        if self.settings.policy.groups.iter().any(|g| g.code == group) {
            return Ok(());
        }
        if let Some(store) = self.store.as_deref()
            && store.managed_group(group)?.is_some()
        {
            return Ok(());
        }
        Err(McpError::new(
            "MCP_GROUP_NOT_FOUND",
            json!({"group": group}),
        ))
    }

    /// 从 HTTP 头解析 EffectiveScope（身份由调用方派生 + 三通道 + 预算，细节在 jumcp-core::entry）。
    ///
    /// 身份不从请求头/请求体自述取值：`actor` 必须是 token 派生结果（HTTP：`JUMCP_AGENT_TOKENS`）。
    /// 策略用 `effective_policy()`：管理面新增组必须对下一次请求立即生效。
    pub fn scope_from_headers(
        &self,
        actor: &Actor,
        headers: &std::collections::HashMap<String, String>,
    ) -> Result<EffectiveScope, McpError> {
        let req = GroupRequest {
            header_groups: header_groups(headers),
            ..Default::default()
        };
        let policy = self.effective_policy()?;
        resolve_scope(actor, &req, &policy, &self.settings.groups_mode, None)
    }
}

pub fn health(settings: &Settings) -> Value {
    json!({
        "status": "ok",
        "groups": settings.policy.group_codes().len()
    })
}

/// `/health` 的报告体：两个监听面共用同一份口径（各自挂一次，但内容一致）。
///
/// 为什么两面都要有：healthcheck 可能打管理口也可能打 MCP 口，只在一个面应答会让
/// 另一半的探针永远失败。报告里只有版本与审计链判定，不含任何凭据。
pub fn health_report(service: &Service) -> Value {
    // 链被篡改时 status 仍是 200（服务确实在跑），但 audit_chain 显式报警，供看板直接着色。
    let (chain, detail) = match &service.audit {
        Some(log) => match log.verify_chain() {
            Ok(ChainVerdict::Valid) => ("valid", Value::Null),
            Ok(ChainVerdict::Tampered { event_id }) => ("tampered", json!(event_id)),
            Ok(ChainVerdict::Broken { reason }) => ("error", json!(reason)),
            Err(e) => ("error", json!(e.message)),
        },
        None => ("error", json!("audit log not configured")),
    };
    let mut body = json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "audit_chain": chain
    });
    if !detail.is_null() {
        body["audit_chain_detail"] = detail;
    }
    // 静态审计快照的新鲜度：调度停摆时这里会停在旧时间，运维不必翻日志才能发现。
    if let Some(report) = service.static_audit() {
        body["static_audit"] = json!({
            "generated_at": report["generated_at"],
            "candidates": report["candidates"].as_array().map_or(0, Vec::len)
        });
    }
    body
}

/// 进程默认角色（**仅 stdio** 使用）：只在 `default_groups` 不含 `"admin"` 的角色里选。
///
/// 这是**安全判定**，不是排序偏好：stdio 没有鉴权，进程默认身份就是连上来的人的身份。
/// 多角色部署里角色名往往 `admin` 排最前，若按字典序取第一个，stdio 进程会直接拿到维护通道
/// （`admin` 组）——默认身份不得持有维护通道。
///
/// 取法：优先 `analyst`（默认角色、最小权限），否则按名字排序取第一个非 admin 角色。
/// 全部角色都持 admin 组时返回 `None`：`process_actor` 得到空角色，后续 `require_scope()`
/// 因策略里查不到该角色而失败，即 fail-closed——绝不回退到特权角色。
fn default_role(settings: &Settings) -> Option<String> {
    let privileged =
        |role: &jumcp_core::config::RoleDef| role.default_groups.iter().any(|g| g == "admin");
    if settings
        .policy
        .roles
        .get("analyst")
        .is_some_and(|r| !privileged(r))
    {
        return Some("analyst".to_string());
    }
    let mut names: Vec<&String> = settings
        .policy
        .roles
        .iter()
        .filter(|(_, r)| !privileged(r))
        .map(|(name, _)| name)
        .collect();
    names.sort();
    names.first().map(|s| (*s).clone())
}

fn default_scope(settings: &Settings) -> Option<EffectiveScope> {
    // 骨架占位：首个角色跑通链路；真实鉴权接入后由调用方用 with_scope 覆盖。
    let role = default_role(settings)?;
    let actor = Actor::new("default", &role);
    let req = GroupRequest::default();
    resolve_scope(&actor, &req, &settings.policy, &settings.groups_mode, None).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn settings() -> Settings {
        Settings {
            policy: jumcp_core::config::TenantPolicy {
                groups: vec![jumcp_core::config::GroupDef {
                    code: "db.read.core".into(),
                    name: "核心数据只读".into(),
                    priority: 10,
                    min_budget: 800,
                }],
                roles: {
                    let mut m = std::collections::HashMap::new();
                    m.insert(
                        "analyst".into(),
                        jumcp_core::config::RoleDef {
                            name: "analyst".into(),
                            default_groups: vec!["db.read.core".into()],
                            default_budget: 2000,
                        },
                    );
                    m
                },
                tenant_budget_cap: 2000,
                allow_intersect: false,
            },
            ..Default::default()
        }
    }
    /// stdio 默认身份必须是最小权限的 analyst，而不是字典序最前的 admin。
    #[test]
    fn process_actor_prefers_analyst_role() {
        let mut s = settings();
        s.policy.roles.insert(
            "admin".into(),
            jumcp_core::config::RoleDef {
                name: "admin".into(),
                default_groups: vec!["db.read.core".into()],
                default_budget: 2500,
            },
        );
        let service = Service::new(s);
        assert_eq!(service.process_actor().role, "analyst");
        assert_eq!(service.process_actor().actor_type, "agent");
    }

    /// 安全判定：只有 admin 角色时，stdio 默认身份不得回退到特权角色（空角色 → require_scope 失败）。
    #[test]
    fn process_actor_never_defaults_to_admin_role() {
        let mut s = settings();
        s.policy.roles.clear();
        s.policy.roles.insert(
            "admin".into(),
            jumcp_core::config::RoleDef {
                name: "admin".into(),
                default_groups: vec!["db.read.core".into(), "admin".into()],
                default_budget: 2500,
            },
        );
        let service = Service::new(s);
        assert_eq!(
            service.process_actor().role,
            "",
            "全部角色都持 admin 组时默认角色必须为空（fail-closed）"
        );
        assert!(
            service.require_scope().is_err(),
            "空默认角色不得解析出 scope，stdio 必须 fail-closed"
        );
    }

    /// 无 analyst 时按名字取第一个**非 admin** 角色：admin 与 reader → reader。
    #[test]
    fn process_actor_skips_admin_when_no_analyst() {
        let mut s = settings();
        s.policy.roles.clear();
        for role in ["admin", "reader"] {
            s.policy.roles.insert(
                role.into(),
                jumcp_core::config::RoleDef {
                    name: role.into(),
                    default_groups: if role == "admin" {
                        vec!["db.read.core".into(), "admin".into()]
                    } else {
                        vec!["db.read.core".into()]
                    },
                    default_budget: 2000,
                },
            );
        }
        let service = Service::new(s);
        assert_eq!(
            service.process_actor().role,
            "reader",
            "字典序最前的 admin 必须被跳过"
        );
    }

    #[test]
    fn health_ok() {
        assert_eq!(health(&settings())["groups"], 1);
    }
    #[test]
    fn resident_tokens_sum_per_tool() {
        let service = Service::new(settings());
        let tools = vec![json!({"name":"a","description":"x"}), json!({"name":"b"})];
        let expected: i64 = tools.iter().map(estimate_tokens).sum();
        assert_eq!(service.resident_tokens(&tools), expected);
        assert!(expected > 0);
    }

    /// 管理面新增组必须立刻进入有效策略：组本身可解析，且角色拿到该组的默认授权。
    #[test]
    fn effective_policy_merges_managed_groups_into_roles() {
        use jumcp_core::registry::{CapabilityStore, ManagedGroup};

        let store = std::sync::Arc::new(CapabilityStore::open(":memory:", None).unwrap());
        store
            .insert_managed_group(
                &ManagedGroup {
                    code: "db.read.new".into(),
                    name: "新只读组".into(),
                    priority: 7,
                    min_budget: 900,
                    roles: vec!["analyst".into()],
                    updated_at: String::new(),
                },
                "alice",
            )
            .unwrap();
        store
            .insert_managed_group(
                &ManagedGroup {
                    code: "db.read.temp".into(),
                    name: "临时组".into(),
                    priority: 3,
                    min_budget: 800,
                    roles: vec!["analyst".into()],
                    updated_at: String::new(),
                },
                "alice",
            )
            .unwrap();
        store.remove_managed_group("db.read.temp", "alice").unwrap();

        let service = Service::new(settings()).with_store(store);
        let policy = service.effective_policy().unwrap();
        assert!(
            policy.groups.iter().any(|g| g.code == "db.read.new"),
            "管理组必须进入 groups"
        );
        let merged = policy
            .groups
            .iter()
            .find(|g| g.code == "db.read.new")
            .expect("managed group merged");
        assert_eq!(merged.name, "新只读组", "合并时保留展示名");
        assert!(
            !policy.groups.iter().any(|g| g.code == "db.read.temp"),
            "已删除的管理组不得再出现"
        );
        assert!(
            policy.roles["analyst"]
                .default_groups
                .contains(&"db.read.new".to_string()),
            "管理组 roles 必须追加进该角色的 default_groups"
        );

        // scope 解析用合并后的策略：新组可以直接通过请求头声明。
        let actor = Actor::new("default", "analyst");
        let mut headers = std::collections::HashMap::new();
        headers.insert("x-mcp-tool-groups".to_string(), "db.read.new".to_string());
        let scope = service.scope_from_headers(&actor, &headers).unwrap();
        assert_eq!(scope.groups, vec!["db.read.new".to_string()]);
        assert_eq!(scope.source, "header");
    }
}
