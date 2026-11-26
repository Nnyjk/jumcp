//! 会话 scope 解析与工具调用分发：`initialize` 的 `_meta` 通道、`tools/list` 的可见性投影、`tools/call` 的分派、维护通道的授权与审计。

use crate::catalog::{self, Capability};
use crate::service::Service;
use jumcp_core::budget::estimate_tokens;
use jumcp_core::entry::{Actor, EffectiveScope, GroupRequest, resolve_scope};
use jumcp_core::errors::McpError;
use jumcp_core::registry::CapabilityStore;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use super::{admin_tool_definitions, flattened_tool, tool_definitions};

mod admin;
mod template;
mod tools;
mod upstream;

/// 审计载荷骨架：MCP 侧统一从这里起手，字段语义全部由 `jumcp_core::audit::payload` 决定。
fn audit_base(
    object_type: &str,
    object_id: &str,
    action: &str,
    outcome: &str,
) -> serde_json::Map<String, Value> {
    jumcp_core::audit::payload(object_type, object_id, action, outcome)
}

fn audit_data(mut payload: serde_json::Map<String, Value>, data: Value) -> Value {
    payload.insert("data".into(), data);
    Value::Object(payload)
}

/// 给审计载荷补错误字段：失败事件必须能直接读 `error_code` / `error_message`。
fn audit_error(mut payload: Value, e: &McpError) -> Value {
    if let Some(map) = payload.as_object_mut() {
        map.insert("error_code".into(), json!(e.code));
        map.insert("error_message".into(), json!(e.message));
    }
    payload
}

/// 把上下文键并入 `data`（已有 `data` 则合并，没有则新建）。
fn with_audit_context(mut payload: Value, context: &Value) -> Value {
    let Some(source) = context.as_object() else {
        return payload;
    };
    let data = payload.as_object_mut().and_then(|p| {
        p.entry("data")
            .or_insert_with(|| Value::Object(serde_json::Map::new()))
            .as_object_mut()
    });
    if let Some(data) = data {
        for (k, v) in source {
            data.insert(k.clone(), v.clone());
        }
    }
    payload
}

pub struct McpServer {
    service: Arc<Service>,
    /// 前置拦截配置（SQL/文件/接口三族共用）：`None` 表示关闭拦截（排障/对照测试用），
    /// 生产从环境变量构造，见 [`McpServer::with_preflight`]。
    preflight: Option<Arc<crate::preflight::PreflightConfig>>,
    /// `initialize` 的 `_meta` 通道解析出的会话 scope（低于 HTTP 的 `X-MCP-Tool-Groups`）。
    /// 按 actor_id 分桶：多身份共用同一进程时，一个 agent 声明的收窄 scope 不能套到另一个身上。
    session_scope: Mutex<HashMap<String, EffectiveScope>>,
}

impl McpServer {
    pub fn new(service: Service) -> Self {
        Self::from_arc(Arc::new(service))
    }
    pub fn from_arc(service: Arc<Service>) -> Self {
        Self {
            service,
            preflight: Some(Arc::new(crate::preflight::PreflightConfig::from_env())),
            session_scope: Mutex::new(HashMap::new()),
        }
    }
    /// 替换前置拦截配置：测试按用例切换白名单与上限，`None` 关闭拦截。
    pub fn with_preflight(
        mut self,
        preflight: Option<Arc<crate::preflight::PreflightConfig>>,
    ) -> Self {
        self.preflight = preflight;
        self
    }
    pub fn service(&self) -> &Service {
        &self.service
    }

    /// 处理一个 JSON-RPC 请求并返回响应体（null 表示 notification）。
    ///
    /// 这是 **stdio 入口**：没有 HTTP 头，因此只能使用进程默认身份
    /// （`Service::process_actor`）。HTTP 面走 `handle_authenticated`，身份来自 token。
    pub async fn handle(&self, req: &Value) -> Value {
        self.handle_scoped(req, None).await
    }

    /// 同 `handle`，但允许传入请求级 scope（HTTP 头收窄后的结果，通道优先级由调用方保证）。
    /// 身份仍是进程默认身份：只供 stdio 与内部测试使用。
    pub async fn handle_scoped(
        &self,
        req: &Value,
        request_scope: Option<&jumcp_core::entry::EffectiveScope>,
    ) -> Value {
        let actor = self.service.process_actor();
        self.handle_authenticated(req, &actor, request_scope).await
    }

    /// HTTP 面唯一入口：`actor` 必须是 token 派生身份（审计与授权都用它，不读请求自述）。
    pub async fn handle_authenticated(
        &self,
        req: &Value,
        actor: &Actor,
        request_scope: Option<&jumcp_core::entry::EffectiveScope>,
    ) -> Value {
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let params = req.get("params").cloned().unwrap_or(json!({}));
        if method.starts_with("notifications/") {
            return Value::Null;
        }
        match method {
            "initialize" => {
                if let Err(e) = self.apply_meta_scope(&params, actor) {
                    return rpc_mcp_error(&id, &e);
                }
                ok(
                    &id,
                    json!({"protocolVersion":crate::upstream::MCP_PROTOCOL_VERSION,"capabilities":{"tools":{}},"serverInfo":{"name":"jumcp","version":env!("CARGO_PKG_VERSION")}}),
                )
            }
            "tools/list" | "tools/call" => {
                let scope = match request_scope.cloned().or_else(|| self.session_scope(actor)) {
                    Some(s) => s,
                    None => match self.service.require_scope() {
                        Ok(s) => s.clone(),
                        Err(e) => return err_tool(&id, &e),
                    },
                };
                if method == "tools/list" {
                    self.handle_tools_list(&id, &scope, actor).await
                } else {
                    self.handle_tools_call(&id, &params, &scope, actor).await
                }
            }
            "ping" => ok(&id, json!({})),
            _ => rpc_error(&id, -32601, "Method not found", json!({"method": method})),
        }
    }

    /// 该身份在 `initialize` 里声明的会话 scope；没有则 None。
    ///
    /// 键用 token id（有 token 时）：同一 caller 的多个 token 各自收窄，不互相污染。
    pub fn session_scope(&self, actor: &Actor) -> Option<EffectiveScope> {
        let key = session_scope_key(actor);
        self.session_scope
            .lock()
            .ok()
            .and_then(|guard| guard.get(&key).cloned())
    }

    /// HTTP 入口的 scope 选择：`X-MCP-Tool-Groups` > 该身份的 `_meta` 会话 scope > 角色默认。
    ///
    /// 为什么"角色默认"要在这里按 `actor` 解析，而不是交给 `require_scope()`：后者的 scope
    /// 是按进程默认角色算的，多身份下会把 admin 的组当成分析型角色的组（维护通道直接消失）。
    /// 两个通道同时存在时以 Header 为准（文档优先级），不求交集——交集会让 strict 越界判定
    /// 永远看不到被拒绝的组。
    pub fn http_scope(
        &self,
        actor: &Actor,
        headers: &HashMap<String, String>,
    ) -> Result<EffectiveScope, McpError> {
        if crate::service::header_groups(headers).is_some() {
            return self.service.scope_from_headers(actor, headers);
        }
        if let Some(scope) = self.session_scope(actor) {
            return Ok(scope);
        }
        // 角色默认：必须用合并后的策略，否则管理面新增组的授权对"没带头的请求"不生效。
        let policy = self.service.effective_policy()?;
        resolve_scope(
            actor,
            &GroupRequest::default(),
            &policy,
            &self.service.settings.groups_mode,
            None,
        )
    }

    /// `initialize` 的 `_meta` 通道：只在声明了 `mcp/groups` 或 `mcp/toolBudget`
    /// 时生效；越界仍按租户模式判定（strict 直接拒绝，不裁剪）。
    fn apply_meta_scope(&self, params: &Value, actor: &Actor) -> Result<(), McpError> {
        let Some(meta) = params.get("_meta") else {
            return Ok(());
        };
        let declares = meta
            .get("mcp")
            .and_then(|m| m.as_object())
            .is_some_and(|m| m.contains_key("groups") || m.contains_key("toolBudget"));
        if !declares {
            return Ok(());
        }
        let request = GroupRequest {
            meta: meta.clone(),
            ..Default::default()
        };
        let policy = self.service.effective_policy()?;
        let scope = resolve_scope(
            actor,
            &request,
            &policy,
            &self.service.settings.groups_mode,
            None,
        )?;
        let mut guard = self.session_scope.lock().map_err(|_| McpError::lock())?;
        guard.insert(session_scope_key(actor), scope);
        Ok(())
    }

    /// 目录视图（静态客户端路径）：可见 kind=tool 能力数 ≤ 阈值时平铺，否则退回收缩形态。
    ///
    /// 可见集来自 `Service::visible_capabilities_with_upstreams`，因此注册表发布/改可见性/
    /// 插入上游后下一个 `tools/list` 立刻反映（无进程内目录缓存）。
    ///
    /// 上游工具**始终**列出（router 的主能力，不能被平铺阈值藏掉），平铺阈值只作用于
    /// 注册表/内存目录里的 managed 工具。
    async fn catalog_view(&self, scope: &EffectiveScope) -> Result<CatalogView, McpError> {
        let threshold = self.service.settings.flatten_threshold;
        let visible = self
            .service
            .visible_capabilities_with_upstreams(scope)
            .await?;
        let resident = |id: &str| {
            (crate::discovery::basic_enabled(scope)
                && crate::service::RESIDENT_TOOL_NAMES.contains(&id))
                || (crate::discovery::admin_enabled(scope)
                    && crate::service::ADMIN_TOOL_NAMES.contains(&id))
        };
        let mut visible_tools: Vec<&Capability> = visible
            .iter()
            .filter(|c| {
                c.kind == "tool"
                    && c.source != crate::catalog::SOURCE_UPSTREAM
                    && !resident(c.id.as_str())
            })
            .collect();
        visible_tools.sort_by(|a, b| a.id.cmp(&b.id));
        let flatten = threshold > 0 && (visible_tools.len() as i64) <= threshold;
        let mut upstream_tools: Vec<&Capability> = visible
            .iter()
            .filter(|c| c.source == crate::catalog::SOURCE_UPSTREAM)
            .collect();
        upstream_tools.sort_by(|a, b| a.id.cmp(&b.id));
        // 常驻/维护工具按组出现（basic / admin），组不在 scope 内就不进清单；
        // 调用面用同一套判定，因此"看见"与"可调用"不会分叉。
        let mut tools: Vec<Value> = Vec::new();
        if crate::discovery::basic_enabled(scope) {
            tools.extend(tool_definitions());
        }
        if crate::discovery::admin_enabled(scope) {
            tools.extend(admin_tool_definitions());
        }
        tools.extend(upstream_tools.iter().map(|cap| flattened_tool(cap)));
        if flatten {
            tools.extend(visible_tools.iter().map(|cap| flattened_tool(cap)));
        }
        Ok(CatalogView {
            tools,
            flattened: flatten,
            flattened_count: if flatten { visible_tools.len() } else { 0 },
            groups: catalog::group_counts(&visible),
        })
    }

    /// 拒绝路径：写 policy_decision 审计后返回原错误；审计本身失败则把审计错误上抛。
    fn reject(&self, id: &Value, actor: &Actor, e: &McpError, extra: Value) -> Value {
        let mut detail = extra;
        if let (Some(obj), Some(data)) = (detail.as_object_mut(), e.data.as_object()) {
            for (k, v) in data {
                obj.insert(k.clone(), v.clone());
            }
        }
        match self.service.policy_decision(actor, e, detail) {
            Ok(_) => err_tool(id, e),
            Err(audit_error) => err_tool(id, &audit_error),
        }
    }

    /// 工具调用被拒：先写调用台账（拿到 event_id），再写与之关联的 `policy_decision`。
    ///
    /// 台账类型随工具走：普通工具写 `tool_call`，`db_run_template` 写 `template_run`
    /// （`admin/stats.rs` 的模板行只按 `template_run` 聚合，执行前的拒绝也必须落这一类，
    /// 否则模板失败会被静默漏掉）；两类事件不会对同一次失败各计一遍。
    ///
    /// 与 `reject` 的差别只有关联 id：`tools/call` 的拒绝必须让治理面能把"拒绝的调用"
    /// 和"拒绝的判定"对上。模板执行面还要把 object_type 记成 `template`（台账按模板聚合）。
    fn reject_tool_call(
        &self,
        id: &Value,
        actor: &Actor,
        e: &McpError,
        tool: &str,
        context: Value,
    ) -> Value {
        let is_template = tool == "db_run_template";
        let object_id = if is_template {
            context
                .get("template_id")
                .and_then(|v| v.as_str())
                .unwrap_or(tool)
                .to_string()
        } else {
            tool.to_string()
        };
        let object_type = if is_template { "template" } else { "tool" };
        let payload = with_audit_context(
            audit_error(
                Value::Object(audit_base(object_type, &object_id, "invoke", "error")),
                e,
            ),
            &context,
        );
        // 模板调用只有一个台账来源：`template_run`（`admin/stats.rs` 按它算模板行的
        // calls/hit_rate）。执行前的拒绝也写这一条，否则模板行的失败会被静默漏掉；
        // 普通工具仍写 `tool_call`，两类事件不会对同一次失败各计一遍。
        let event_type = if is_template {
            "template_run"
        } else {
            "tool_call"
        };
        let event_id = match self.service.audit_event(event_type, payload, actor) {
            Ok(event_id) => event_id,
            Err(audit_error) => return err_tool(id, &audit_error),
        };
        let mut detail = json!({"tool": tool, "context": context});
        if is_template {
            detail["template_id"] = json!(object_id);
        }
        let correlation = if event_id.is_empty() {
            None
        } else {
            Some(event_id.as_str())
        };
        match self
            .service
            .policy_decision_correlated(actor, e, detail, correlation)
        {
            Ok(_) => err_tool(id, e),
            Err(audit_error) => err_tool(id, &audit_error),
        }
    }
}

/// 目录形态：flat 时包含会话可见的 kind=tool 能力定义，collapsed 时只含元工具 + 分发器。
pub(super) struct CatalogView {
    pub(super) tools: Vec<Value>,
    pub(super) flattened: bool,
    pub(super) flattened_count: usize,
    pub(super) groups: Vec<Value>,
}

/// `_meta` 会话 scope 的分桶键：有 token 时按 token id，否则按 actor_id（stdio/测试）。
fn session_scope_key(actor: &Actor) -> String {
    if actor.token_id.is_empty() {
        actor.actor_id.clone()
    } else {
        format!("token:{}", actor.token_id)
    }
}

/// 上游调用参数的审计脱敏：命中密钥类 key（任意层级）的值一律记 `***`。
///
/// 匹配是"小写后子串包含"，所以 `x-auth-token` / `Client-Secret` 这类带前缀或大小写差异的
/// 头名同样命中；数组元素递归同类处理。
/// 只用于上游调用；内置工具的入参（SQL/模板参数）按既有契约原样落审计。
fn redact_arguments(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (key, inner) in map {
                let lowered = key.to_ascii_lowercase();
                let secret = [
                    "authorization",
                    "token",
                    "password",
                    "passwd",
                    "secret",
                    // `secret` 已覆盖该键；显式保留以免将来改成精确匹配时漏掉。
                    "client_secret",
                    "api_key",
                    "apikey",
                    "cookie",
                    "credential",
                    "private_key",
                    "signature",
                    "bearer",
                    "x-auth",
                ]
                .iter()
                .any(|needle| lowered.contains(needle));
                out.insert(
                    key.clone(),
                    if secret {
                        json!("***")
                    } else {
                        redact_arguments(inner)
                    },
                );
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(redact_arguments).collect()),
        other => other.clone(),
    }
}

fn names(tools: &[Value]) -> Vec<String> {
    tools
        .iter()
        .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(String::from))
        .collect()
}

fn ok(id: &Value, result: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":result})
}

fn rpc_error(id: &Value, code: i64, message: &str, data: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message,"data":data}})
}

/// 协议级错误：`data.mcp_code` 承载稳定错误码，客户端据此分支而不是解析文案。
fn rpc_mcp_error(id: &Value, e: &McpError) -> Value {
    let mut data = e.data.clone();
    if let Some(obj) = data.as_object_mut() {
        obj.insert("mcp_code".into(), json!(e.code));
    }
    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":e.message,"data":data}})
}

/// `tools/call` 结果的规范形状：`content` 永远是 content block 数组，结构化对象另放
/// `structuredContent`，失败再置 `isError`。
///
/// 为什么不再把对象塞进 `content`：MCP 客户端里有的只认 content block 数组（mcp-go 直接
/// 报 `content is not an array`），把对象放 `content` 等于让内置工具对该类客户端不可用；
/// 而纯文本块又丢掉结构，所以文本块承载同一份 JSON、`structuredContent` 承载原对象。
fn tool_result(id: &Value, payload: Value, is_error: bool) -> Value {
    let text = payload.to_string();
    json!({
        "jsonrpc":"2.0","id":id,
        "result":{
            "content":[{"type":"text","text":text}],
            "structuredContent": payload,
            "isError": is_error
        }
    })
}

fn err_tool(id: &Value, e: &McpError) -> Value {
    tool_result(
        id,
        json!({"error":{"code":e.code,"message":e.message,"data":e.data}}),
        true,
    )
}

#[cfg(test)]
mod tests {
    use super::redact_arguments;
    use serde_json::json;

    /// 密钥类键的覆盖面：常见头名/凭证名，任意层级、任意大小写、数组元素内都必须变 `***`。
    #[test]
    fn redact_arguments_masks_common_secret_keys_at_any_depth() {
        let args = json!({
            "authorization": "Bearer abc",
            "X-Auth-Token": "t",
            "x-auth": "x",
            "passwd": "p",
            "private_key": "k",
            "Signature": "sig",
            "Bearer": "b",
            "credential": "c",
            "token": "tk",
            "secret": "s",
            "api_key": "ak",
            "apikey": "ak2",
            "cookie": "c2",
            "password": "pw",
            "headers": {
                "client_secret": "cs",
                "X-AUTH-KEY": "xk",
                "Accept": "application/json"
            },
            "batch": [{"password": "p1"}, {"id": "keep"}],
            "items": ["a", {"apikey": "ak3"}],
            "query": "SELECT 1",
            "limit": 10
        });

        let out = redact_arguments(&args);

        for key in [
            "authorization",
            "X-Auth-Token",
            "x-auth",
            "passwd",
            "private_key",
            "Signature",
            "Bearer",
            "credential",
            "token",
            "secret",
            "api_key",
            "apikey",
            "cookie",
            "password",
        ] {
            assert_eq!(out[key], json!("***"), "{key} 必须脱敏");
        }
        assert_eq!(out["headers"]["client_secret"], json!("***"));
        assert_eq!(out["headers"]["X-AUTH-KEY"], json!("***"));
        assert_eq!(out["batch"][0]["password"], json!("***"));
        assert_eq!(out["items"][1]["apikey"], json!("***"));

        // 普通键与非对象元素原样保留：脱敏不能把审计内容糊成一团。
        assert_eq!(out["headers"]["Accept"], json!("application/json"));
        assert_eq!(out["batch"][1]["id"], json!("keep"));
        assert_eq!(out["items"][0], json!("a"));
        assert_eq!(out["query"], json!("SELECT 1"));
        assert_eq!(out["limit"], json!(10));
    }

    /// 脱敏只产出副本：调用方手里传给上游的参数不能因此被改写。
    #[test]
    fn redact_arguments_does_not_mutate_input() {
        let args = json!({"token": "tk", "nested": {"api_key": "ak"}});
        let out = redact_arguments(&args);
        assert_eq!(out["token"], json!("***"));
        assert_eq!(args["token"], json!("tk"));
        assert_eq!(args["nested"]["api_key"], json!("ak"));
    }
}
