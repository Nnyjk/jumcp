//! 服务聚合：配置、catalog、执行委托、审计与按会话预算。
//! 为什么预算放在 Service 而不是每个 handler：schema/result 累计必须跨调用共享（§5.1 按会话计）。

use crate::catalog::{Capability, Catalog};
use crate::executor::{DefaultQueryExecutor, QueryExecutor};
use hjzl_core::audit::{ActorRef, AuditEvent, AuditLog};
use hjzl_core::budget::{
    BudgetCaps, BudgetCheckResult, BudgetLedger, OverflowPolicy, estimate_tokens,
};
use hjzl_core::config::Settings;
use hjzl_core::entry::{Actor, EffectiveScope, GroupRequest, resolve_scope};
use hjzl_core::errors::McpError;
use hjzl_core::template::TemplateRegistry;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

pub const RESIDENT_TOOL_NAMES: &[&str] = &[
    "search_capabilities",
    "describe_capability",
    "list_groups",
    "db_query_readonly",
    "db_run_template",
];

pub struct Service {
    pub settings: Settings,
    pub catalog: Catalog,
    pub executor: Arc<dyn QueryExecutor>,
    pub audit: Option<Arc<AuditLog>>,
    pub scope: Option<EffectiveScope>,
    pub budget: Mutex<BudgetLedger>,
    pub templates: Option<Arc<TemplateRegistry>>,
    /// 单进程骨架的会话标识；接入真实会话表后由调用方传入。
    pub session_id: String,
}

impl Service {
    pub fn new(settings: Settings) -> Self {
        let scope = default_scope(&settings);
        let mut catalog = Catalog::default();
        catalog.register(Capability {
            id: "db_query_readonly".into(),
            kind: "tool".into(),
            group: "db.read.core".into(),
            one_line: "通用只读 SQL 入口（L1–L4 全链）".into(),
            ..Capability::new("db_query_readonly", "tool", "db.read.core")
        });
        Self {
            settings,
            catalog,
            executor: Arc::new(DefaultQueryExecutor),
            audit: None,
            scope,
            budget: Mutex::new(BudgetLedger::new(None)),
            templates: None,
            session_id: "default".to_string(),
        }
    }
    pub fn with_executor(mut self, executor: Arc<dyn QueryExecutor>) -> Self {
        self.executor = executor;
        self
    }
    pub fn with_catalog(mut self, catalog: Catalog) -> Self {
        self.catalog = catalog;
        self
    }
    pub fn with_audit(mut self, audit: Arc<AuditLog>) -> Self {
        self.audit = Some(audit);
        self
    }
    pub fn with_scope(mut self, scope: EffectiveScope) -> Self {
        self.scope = Some(scope);
        self
    }
    pub fn with_templates(mut self, templates: Arc<TemplateRegistry>) -> Self {
        self.templates = Some(templates);
        self
    }
    pub fn with_budget_caps(mut self, caps: BudgetCaps) -> Self {
        self.budget = Mutex::new(BudgetLedger::new(Some(caps)));
        self
    }

    pub fn require_scope(&self) -> Result<&EffectiveScope, McpError> {
        self.scope.as_ref().ok_or_else(|| {
            McpError::new(
                "MCP_STORE_ERROR",
                json!({"error": "EffectiveScope 未初始化"}),
            )
        })
    }

    pub fn overflow_policy(&self) -> OverflowPolicy {
        self.settings.overflow_policy()
    }

    /// 连接声明的 schema 白名单（请求只能收窄，不能扩大）。
    pub fn connection_schemas(&self) -> Vec<String> {
        self.settings
            .primary_datasource()
            .map(|d| d.allowed_schemas.clone())
            .unwrap_or_default()
    }

    pub fn statement_timeout_ms(&self) -> i64 {
        self.settings
            .primary_datasource()
            .map_or(15_000, |d| d.statement_timeout_ms)
    }

    /// tools/list 投影：常驻工具恒在，kind=tool 能力按可见组投影（§4.2.1 索引态）。
    pub fn tools_for_scope(&self, tools: &[Value]) -> Vec<Value> {
        let Ok(scope) = self.require_scope() else {
            return vec![];
        };
        self.tools_for_scope_with(scope, tools)
    }

    /// 同上，但用请求级 scope（HTTP 头可收窄可见集，§4.1.6）。
    pub fn tools_for_scope_with(&self, scope: &EffectiveScope, tools: &[Value]) -> Vec<Value> {
        let visible: std::collections::HashSet<String> = self
            .catalog
            .visible(scope)
            .iter()
            .map(|c| c.id.clone())
            .collect();
        tools
            .iter()
            .filter(|t| {
                let name = t.get("name").and_then(|n| n.as_str()).unwrap_or("");
                RESIDENT_TOOL_NAMES.contains(&name) || visible.contains(name)
            })
            .cloned()
            .collect()
    }

    /// 常驻 token 计量（§5.1 口径：每个工具 canonical JSON 字节数 / 4，逐个累加）。
    pub fn resident_tokens(&self, tools: &[Value]) -> i64 {
        tools.iter().map(estimate_tokens).sum()
    }

    /// 常驻预算判定：默认 fail；truncate 显式开启才返回带标记的结果。
    pub fn check_resident(&self, resident_tokens: i64) -> Result<BudgetCheckResult, McpError> {
        let ledger = self.budget.lock().map_err(|_| McpError::lock())?;
        ledger.check_resident(resident_tokens, self.overflow_policy())
    }

    pub fn check_schema(&self, incoming_tokens: i64) -> Result<BudgetCheckResult, McpError> {
        let mut ledger = self.budget.lock().map_err(|_| McpError::lock())?;
        Ok(ledger.check_schema(&self.session_id, incoming_tokens, self.overflow_policy()))
    }

    pub fn record_schema(&self, token_cost: i64) -> Result<(), McpError> {
        let mut ledger = self.budget.lock().map_err(|_| McpError::lock())?;
        ledger.record_schema(&self.session_id, token_cost);
        Ok(())
    }

    pub fn check_result(&self, incoming_tokens: i64) -> Result<BudgetCheckResult, McpError> {
        let mut ledger = self.budget.lock().map_err(|_| McpError::lock())?;
        Ok(ledger.check_result(&self.session_id, incoming_tokens))
    }

    pub fn record_result(&self, token_cost: i64) -> Result<BudgetCheckResult, McpError> {
        let mut ledger = self.budget.lock().map_err(|_| McpError::lock())?;
        Ok(ledger.check_result(&self.session_id, token_cost))
    }

    /// 追加审计：审计写失败必须上抛（治理面不可静默失效）。
    pub fn audit_event(
        &self,
        event_type: &str,
        payload: Value,
        actor: &Actor,
    ) -> Result<(), McpError> {
        let Some(log) = &self.audit else {
            return Ok(());
        };
        log.append(AuditEvent {
            tenant: actor.tenant.clone(),
            actor: ActorRef {
                actor_type: actor.actor_type.clone(),
                id: actor.actor_id.clone(),
                role: actor.role.clone(),
            },
            event_type: event_type.into(),
            payload,
            session_id: self.session_id.clone(),
            ts: String::new(),
            event_id: String::new(),
        })
        .map(|_| ())
    }

    /// 拒绝路径统一审计（§4.4.3 policy_decision）：契约是 reason_code + 请求上下文。
    pub fn policy_decision(
        &self,
        actor: &Actor,
        reason_code: &str,
        detail: Value,
    ) -> Result<(), McpError> {
        self.audit_event(
            "policy_decision",
            json!({"reason_code": reason_code, "detail": detail}),
            actor,
        )
    }
}

pub fn health(settings: &Settings) -> Value {
    json!({"status": "ok", "groups": settings.policy.group_names().len()})
}

fn default_scope(settings: &Settings) -> Option<EffectiveScope> {
    // 骨架占位：首个角色跑通链路；真实鉴权接入后由调用方用 with_scope 覆盖。
    let role = settings.policy.roles.keys().next()?.clone();
    let actor = Actor::new("default", &role);
    let req = GroupRequest::default();
    resolve_scope(&actor, &req, &settings.policy, &settings.groups_mode, None).ok()
}

/// 从 HTTP 头解析 EffectiveScope（鉴权占位 + 三通道 + 预算，细节在 hjzl-core::entry）。
pub fn scope_from_headers(
    settings: &Settings,
    headers: &std::collections::HashMap<String, String>,
) -> Result<EffectiveScope, McpError> {
    let role = settings
        .policy
        .roles
        .keys()
        .next()
        .cloned()
        .unwrap_or_else(|| "analyst".to_string());
    let actor = Actor::new("default", &role);
    let header_groups = headers
        .iter()
        .find(|(k, _)| k.to_lowercase() == "x-mcp-tool-groups")
        .map(|(_, v)| v.clone());
    let req = GroupRequest {
        header_groups,
        ..Default::default()
    };
    resolve_scope(&actor, &req, &settings.policy, &settings.groups_mode, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn settings() -> Settings {
        Settings {
            policy: hjzl_core::config::TenantPolicy {
                groups: vec![hjzl_core::config::GroupDef {
                    name: "db.read.core".into(),
                    priority: 10,
                    min_budget: 800,
                }],
                roles: {
                    let mut m = std::collections::HashMap::new();
                    m.insert(
                        "analyst".into(),
                        hjzl_core::config::RoleDef {
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
}
