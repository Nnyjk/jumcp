//! 手写 JSON-RPC MCP 服务：tools/list（按 EffectiveScope 过滤 + 常驻预算）+ 元工具三件套
//! + 维护通道工具 + 上游工具转发。数据取数只走模板与上游链路，内置 basic 组不再直连数据源。
//!
//! 为什么手写：rmcp API 随版本漂移，任务允许退回且双传输必须同时支持。

use crate::catalog::{self, Capability};
use crate::service::Service;
use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::post;
use jumcp_core::budget::estimate_tokens;
use jumcp_core::entry::{Actor, EffectiveScope, GroupRequest, resolve_scope};
use jumcp_core::errors::McpError;
use jumcp_core::registry::CapabilityStore;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

pub const READONLY_ANNOTATIONS: &str =
    r#"{"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false}"#;
/// 维护通道写工具（`admin_register_template` / `admin_set_visibility`）的注解：会改本项目
/// 自有数据（非只读、非幂等），但不触碰接入数据源，因此 `destructiveHint=false`。
pub const WRITE_ANNOTATIONS: &str = r#"{"readOnlyHint":false,"destructiveHint":false,"idempotentHint":false,"openWorldHint":false}"#;
/// 未声明注解且非只读的投影（非只读上游 / sensitive 能力）：按协议最保守缺省值，
/// 让客户端要求人工确认；绝不冒充只读。
const DESTRUCTIVE_ANNOTATIONS: &str =
    r#"{"readOnlyHint":false,"destructiveHint":true,"idempotentHint":false,"openWorldHint":true}"#;

/// 每份常量各自解析一次：共用同一个 `OnceLock` 会让后调用的注解拿到先解析的那份。
fn annotations() -> &'static Value {
    static CACHE: OnceLock<Value> = OnceLock::new();
    CACHE.get_or_init(|| {
        serde_json::from_str(READONLY_ANNOTATIONS)
            .expect("READONLY_ANNOTATIONS 必须是合法 JSON 对象")
    })
}

fn write_annotations() -> &'static Value {
    static CACHE: OnceLock<Value> = OnceLock::new();
    CACHE.get_or_init(|| {
        serde_json::from_str(WRITE_ANNOTATIONS).expect("WRITE_ANNOTATIONS 必须是合法 JSON 对象")
    })
}

fn destructive_annotations() -> &'static Value {
    static CACHE: OnceLock<Value> = OnceLock::new();
    CACHE.get_or_init(|| {
        serde_json::from_str(DESTRUCTIVE_ANNOTATIONS)
            .expect("DESTRUCTIVE_ANNOTATIONS 必须是合法 JSON 对象")
    })
}

pub fn tool_definitions() -> Vec<Value> {
    vec![
        tool(
            "search_capabilities",
            "统一检索入口：返回候选列表，不含完整 schema",
            json!({"type":"object","properties":{
                "query":{"type":"string"},
                "kind":{"type":"string"},
                "group":{"type":"string"},
                "category":{"type":"string","description":"分类过滤：basic|analysis|admin|custom"},
                "limit":{"type":"integer"}
            },"required":["query"]}),
        ),
        tool(
            "describe_capability",
            "按 id 取完整 schema（只说明怎么调用，不构成调用通道）",
            json!({"type":"object","properties":{"capability_id":{"type":"string"}},"required":["capability_id"]}),
        ),
        tool(
            "list_groups",
            "组索引：只含组名与计数，不含定义",
            json!({"type":"object","properties":{}}),
        ),
    ]
}

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({"name": name, "description": description, "inputSchema": input_schema, "annotations": annotations(), "category": builtin_category(name)})
}

/// 维护通道里的**写**工具：注解必须与行为一致（readOnlyHint=false），不能复用只读注解。
fn write_tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({"name": name, "description": description, "inputSchema": input_schema, "annotations": write_annotations(), "category": builtin_category(name)})
}

/// 内置工具的分类标签（唯一映射），按**职责**三组：
///
/// - `basic`（基础查询、计算）：走查目录的工具——`search_capabilities` /
///   `describe_capability` / `list_groups` 回答"有什么、怎么调"。取数本身不在内置工具里：
///   模板与上游工具各自按 `group_code` 归组，不占 `basic`。
/// - `analysis`（工具列表分析、变更提交审计）：面向工具目录与变更流的观察/提交动作——
///   `admin_review_queue` 看队列、`admin_register_template` 提交草案、
///   `admin_set_visibility` 提交可见性变更；它们只读或写本项目自有元数据。
/// - `admin`（仅审计操作）：`admin_audit_queries`，查询调用审计流水。
///
/// 分类与授权解耦，只是展示/检索标签：它**不是** `scope.groups` 的授权组，Header 收窄不会
/// 改变常驻工具的可见性（常驻性由 `RESIDENT_TOOL_NAMES` 判定）；`admin_*` 的可见性与调用
/// 权限仍由 `discovery::admin_enabled`（组 `admin` 在 scope 内）与 token allowlist 双重判定。
/// 未知 id 落到 `custom`：新增内置工具忘记归类时不会冒充 `admin`。
pub fn builtin_category(id: &str) -> &'static str {
    use crate::catalog::{CATEGORY_ADMIN, CATEGORY_ANALYSIS, CATEGORY_BASIC, CATEGORY_CUSTOM};
    match id {
        "search_capabilities" | "describe_capability" | "list_groups" => CATEGORY_BASIC,
        "admin_review_queue" | "admin_register_template" | "admin_set_visibility" => {
            CATEGORY_ANALYSIS
        }
        "admin_audit_queries" => CATEGORY_ADMIN,
        _ => CATEGORY_CUSTOM,
    }
}

/// 统一审计载荷骨架（单一来源在 `jumcp_core::audit::payload`）。
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

/// 维护通道的内置工具（`group="admin"`）：只在 `scope.groups` 含 admin 时出现在
/// `tools/list` 与发现面，调用前还要再判一次（不依赖客户端是否看过清单）。
///
/// 边界（§4.4.6）：只能改能力定义（只产 draft）/ 可见性，**不提供发布或审批工具**——
/// `review → published` 只能走带人工审批的 HTTP 管理面，agent 无法自我发布。
pub fn admin_tool_definitions() -> Vec<Value> {
    vec![
        write_tool(
            "admin_register_template",
            "登记模板草案（status=draft；发布必须走人工审批的管理面）",
            json!({"type":"object","properties":{
                "definition":{"type":"object","description":"模板定义原文（template_id/group/sql/params...）"}
            },"required":["definition"]}),
        ),
        tool(
            "admin_review_queue",
            "审核队列：draft/review 两类能力（只读）",
            json!({"type":"object","properties":{}}),
        ),
        tool(
            "admin_audit_queries",
            "审计查询（只读，limit 上限 200）",
            json!({"type":"object","properties":{
                "filter":{"type":"object","description":"tenant/event_type/event_id/object_type/object_id/actor_id/actor_role/token_id/ts_after/ts_before"},
                "order":{"type":"string","description":"asc|desc（默认 desc）"},
                "limit":{"type":"integer"},
                "offset":{"type":"integer"}
            }}),
        ),
        write_tool(
            "admin_set_visibility",
            "改 discoverable/callable（写 admin_change；不改定义、不触发重新审核）",
            json!({"type":"object","properties":{
                "capability_id":{"type":"string"},
                "discoverable":{"type":"boolean"},
                "callable":{"type":"boolean"}
            },"required":["capability_id"]}),
        ),
    ]
}

/// 平铺用的工具定义：与 `describe_capability` 完全同源（同一个 params_schema / one_line），
/// 不允许两套拼装逻辑，否则同一能力会出现两个 schema。
///
/// 上游条目额外带 `source`/`server_id`/`upstream_tool` 与上游自己的 annotations（原样透传）。
///
/// 上游没声明注解时按 `risk` 保守生成：只读投影给只读注解，非只读投影给协议最保守缺省值
///（`destructiveHint=true`），不能让可写上游冒充 `readOnlyHint=true`。
fn flattened_tool(cap: &Capability) -> Value {
    let fallback = if cap.risk == "read" {
        annotations()
    } else {
        destructive_annotations()
    };
    let mut tool = json!({
        "name": cap.id,
        "description": cap.one_line,
        "inputSchema": cap.params_schema,
        "annotations": cap.annotations.clone().unwrap_or_else(|| fallback.clone()),
        "group": cap.group,
        "category": cap.category
    });
    if let Some(source) = &cap.upstream {
        tool["source"] = json!(crate::catalog::SOURCE_UPSTREAM);
        tool["server_id"] = json!(source.server_id);
        tool["upstream_tool"] = json!(source.tool);
    }
    tool
}

pub struct McpServer {
    service: Arc<Service>,
    /// `initialize` 的 `_meta` 通道解析出的会话 scope（§4.1.6 优先级 2，低于 HTTP Header）。
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
            session_scope: Mutex::new(HashMap::new()),
        }
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

    /// 同 `handle`，但允许传入请求级 scope（HTTP 头收窄后的结果，§4.1.6 通道优先级由调用方保证）。
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

    /// 该身份在 `initialize` 里声明的会话 scope（§4.1.6 优先级 2）；没有则 None。
    ///
    /// 键用 token id（有 token 时）：同一 caller 的多个 token 各自收窄，不互相污染。
    pub fn session_scope(&self, actor: &Actor) -> Option<EffectiveScope> {
        let key = session_scope_key(actor);
        self.session_scope
            .lock()
            .ok()
            .and_then(|guard| guard.get(&key).cloned())
    }

    /// HTTP 入口的 scope 选择（§4.1.6 通道优先级）：
    /// `X-MCP-Tool-Groups`（1）> 该身份的 `_meta` 会话 scope（2）> 角色默认（4）。
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

    /// `initialize` 的 `_meta` 通道（§4.1.6 优先级 2）：只在声明了 `mcp/groups` 或 `mcp/toolBudget`
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

    /// 目录视图（§5.2 静态客户端路径）：可见 kind=tool 能力数 ≤ 阈值时平铺，否则退回收缩形态。
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
            crate::service::RESIDENT_TOOL_NAMES.contains(&id)
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
        // 常驻/维护工具都受 token 的内置工具 allowlist 约束；维护工具另需 admin 组
        // （双重判定，客户端看过清单不构成授权）。
        let mut tools: Vec<Value> = tool_definitions()
            .into_iter()
            .filter(|def| {
                def.get("name")
                    .and_then(|n| n.as_str())
                    .is_some_and(|id| crate::discovery::tool_allowed(scope, id))
            })
            .collect();
        if crate::discovery::admin_enabled(scope) {
            tools.extend(admin_tool_definitions().into_iter().filter(|def| {
                def.get("name")
                    .and_then(|n| n.as_str())
                    .is_some_and(|id| crate::discovery::tool_allowed(scope, id))
            }));
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

    async fn handle_tools_list(&self, id: &Value, scope: &EffectiveScope, actor: &Actor) -> Value {
        let view = match self.catalog_view(scope).await {
            Ok(view) => view,
            Err(e) => return err_tool(id, &e),
        };
        // 常驻 = allowlist 内的 builtin + 本次 scope 授权的 admin builtin + 全部上游工具
        //（truncate 时保留它们；上游工具本身就是 router 的主能力，不能被预算裁掉）。
        let is_resident = |tool: &Value| {
            if tool.get("source").and_then(|s| s.as_str()) == Some(crate::catalog::SOURCE_UPSTREAM)
            {
                return true;
            }
            let Some(name) = tool.get("name").and_then(|n| n.as_str()) else {
                return false;
            };
            crate::discovery::tool_allowed(scope, name)
                && (crate::service::RESIDENT_TOOL_NAMES.contains(&name)
                    || (crate::discovery::admin_enabled(scope)
                        && crate::service::ADMIN_TOOL_NAMES.contains(&name)))
        };
        // catalog_view 已按可见集拼好常驻 + 上游 + 平铺定义，无需二次过滤（二次过滤会再查一遍 store）。
        let kept = &view.tools;
        // 组索引本身也计入常驻预算（§5.1「组索引 ≤20 组」那一行）。
        let index_tokens = estimate_tokens(&json!(view.groups));
        let resident = self.service.resident_tokens(kept) + index_tokens;
        match self.service.check_resident(resident) {
            Ok(result) if result.detail.get("truncated").is_some() => {
                // truncate（显式开启）：只保留常驻工具，丢弃的能力组必须回显（§4.1.4）。
                let resident_only: Vec<Value> =
                    kept.iter().filter(|t| is_resident(t)).cloned().collect();
                let dropped: Vec<String> = kept
                    .iter()
                    .filter(|t| !is_resident(t))
                    .filter_map(|t| t.get("group").and_then(|g| g.as_str()).map(String::from))
                    .collect();
                let spent = self.service.resident_tokens(&resident_only) + index_tokens;
                if let Err(e) = self.service.audit_event(
                    "tools_list_served",
                    audit_data(
                        audit_base("catalog", "tools", "list_tools", "summary"),
                        json!({
                            "tools": names(&resident_only),
                            "resident_tokens": spent,
                            "truncated": true,
                            "dropped_groups": dropped,
                            "flattened": view.flattened
                        }),
                    ),
                    actor,
                ) {
                    return err_tool(id, &e);
                }
                ok(
                    id,
                    json!({
                        "tools": resident_only,
                        "groups": view.groups,
                        "truncated": true,
                        "dropped_groups": dropped,
                        "budget_spent": spent,
                        "token_estimate": spent,
                        "flattened": false
                    }),
                )
            }
            Ok(_) => {
                if let Err(e) = self.service.audit_event(
                    "tools_list_served",
                    audit_data(
                        audit_base("catalog", "tools", "list_tools", "ok"),
                        json!({
                            "tools": names(kept),
                            "resident_tokens": resident,
                            "flattened": view.flattened,
                            "flattened_count": view.flattened_count
                        }),
                    ),
                    actor,
                ) {
                    return err_tool(id, &e);
                }
                ok(
                    id,
                    json!({
                        "tools": kept,
                        "groups": view.groups,
                        "token_estimate": resident,
                        "truncated": false,
                        "flattened": view.flattened,
                        "flattened_count": view.flattened_count
                    }),
                )
            }
            Err(mut e) => {
                // 平铺是主动选择，超预算时必须告诉调用方"关掉平铺"而不是只报预算不足。
                if view.flattened
                    && let Some(obj) = e.data.as_object_mut()
                {
                    obj.insert("flattened_count".into(), json!(view.flattened_count));
                    obj.insert(
                        "flatten_threshold".into(),
                        json!(self.service.settings.flatten_threshold),
                    );
                    obj.insert(
                        "suggestions".into(),
                        json!([
                            "设置 JUMCP_MCP_FLATTEN_THRESHOLD=0 关闭平铺，改用 search_capabilities 按需发现",
                            "缩小 X-MCP-Tool-Groups 以减少可见能力数"
                        ]),
                    );
                }
                self.reject(
                    id,
                    actor,
                    &e,
                    json!({
                        "tool": "tools/list",
                        "resident_tokens": resident,
                        "flattened": view.flattened
                    }),
                )
            }
        }
    }

    async fn handle_tools_call(
        &self,
        id: &Value,
        params: &Value,
        scope: &jumcp_core::entry::EffectiveScope,
        actor: &Actor,
    ) -> Value {
        let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
        let args = params.get("arguments").cloned().unwrap_or(json!({}));
        // 内置工具的统一调用门禁：token 的 allowlist 是精确白名单，不能"列表里没有但调用放行"。
        // admin_* 在这里只过第一道门，第二道仍由 `authorize_admin` 判 groups 是否含 admin。
        if jumcp_core::registry::is_builtin_id(name) && !crate::discovery::tool_allowed(scope, name)
        {
            let e = McpError::new("MCP_CAPABILITY_NOT_FOUND", json!({"tool": name}));
            return self.reject(
                id,
                actor,
                &e,
                json!({"tool": name, "surface": "mcp/builtin-allowlist"}),
            );
        }
        match name {
            "search_capabilities" => {
                let query = args.get("query").and_then(|q| q.as_str()).unwrap_or("");
                let kind = args.get("kind").and_then(|k| k.as_str());
                let group = args.get("group").and_then(|g| g.as_str());
                let category = args.get("category").and_then(|c| c.as_str());
                let limit = args
                    .get("limit")
                    .and_then(|l| l.as_u64())
                    .unwrap_or(5)
                    .min(50) as usize;
                let hits = {
                    let visible = match self
                        .service
                        .visible_capabilities_with_upstreams(scope)
                        .await
                    {
                        Ok(v) => v,
                        Err(e) => return err_tool(id, &e),
                    };
                    catalog::search(&visible, query, kind, group, category, limit)
                };
                let ids: Vec<String> = hits.iter().map(|h| h.id.clone()).collect();
                if let Err(e) = self.service.audit_event(
                    "capability_search",
                    audit_data(
                        audit_base("catalog", "catalog", "search", "ok"),
                        json!({
                            "query": query,
                            "kind": kind,
                            "group": group,
                            "category": category,
                            "limit": limit,
                            "returned_ids": ids
                        }),
                    ),
                    actor,
                ) {
                    return err_tool(id, &e);
                }
                let content: Vec<Value> = hits.iter().map(|h| json!({"id": h.id, "kind": h.kind, "group": h.group, "category": h.category, "one_line": h.one_line, "param_summary": h.param_summary, "token_cost": h.token_cost})).collect();
                ok(id, json!({"content": content, "isError": false}))
            }
            "describe_capability" => {
                let cap_id = args
                    .get("capability_id")
                    .and_then(|c| c.as_str())
                    .unwrap_or("");
                let visible = match self
                    .service
                    .visible_capabilities_with_upstreams(scope)
                    .await
                {
                    Ok(v) => v,
                    Err(e) => return err_tool(id, &e),
                };
                let Some(cap) = catalog::find(&visible, cap_id) else {
                    // 不可见与不存在同构（§4.2.3）。
                    return err_tool(
                        id,
                        &McpError::new(
                            "MCP_CAPABILITY_NOT_FOUND",
                            json!({"capability_id": cap_id}),
                        ),
                    );
                };
                let cost = estimate_tokens(&cap.params_schema);
                let key = crate::service::session_key(actor);
                match self.service.check_schema(&key, cost) {
                    Ok(result) if result.allowed => {
                        if let Err(e) = self.service.record_schema(&key, cost) {
                            return err_tool(id, &e);
                        }
                        if let Err(e) = self.service.audit_event(
                            "capability_describe",
                            audit_data(
                                audit_base("capability", cap_id, "describe", "ok"),
                                json!({"schema_tokens": cost}),
                            ),
                            actor,
                        ) {
                            return err_tool(id, &e);
                        }
                        ok(
                            id,
                            json!({
                                "content": json!({
                                    "id": cap.id,
                                    "kind": cap.kind,
                                    "group": cap.group,
                                    "risk": cap.risk,
                                    "one_line": cap.one_line,
                                    "params_schema": cap.params_schema,
                                    "source": cap.source,
                                    "category": cap.category
                                }),
                                "isError": false,
                                "schema_tokens": cost
                            }),
                        )
                    }
                    Ok(_) | Err(_) => {
                        let e = McpError::new(
                            "MCP_CONTEXT_BUDGET_EXHAUSTED",
                            json!({"capability_id": cap_id, "incoming_tokens": cost}),
                        );
                        self.reject(id, actor, &e, json!({"tool": "describe_capability"}))
                    }
                }
            }
            "list_groups" => {
                let visible = match self
                    .service
                    .visible_capabilities_with_upstreams(scope)
                    .await
                {
                    Ok(v) => v,
                    Err(e) => return err_tool(id, &e),
                };
                let counts = catalog::group_counts(&visible);
                ok(id, json!({"content": counts, "isError": false}))
            }
            // 维护通道：可见性与调用权都由服务端 scope 判定（客户端是否看过清单不影响结果）。
            "admin_register_template"
            | "admin_review_queue"
            | "admin_audit_queries"
            | "admin_set_visibility" => {
                // 拒绝路径（未授权）也走同一个收尾，管理台才能看到维护工具的失败计数。
                let result = if let Some(denied) = self.authorize_admin(id, actor, scope, name) {
                    denied
                } else {
                    match name {
                        "admin_register_template" => self.admin_register_template(id, &args, actor),
                        "admin_review_queue" => self.admin_review_queue(id, actor),
                        "admin_audit_queries" => self.admin_audit_queries(id, &args, actor),
                        // 分支已由上方 match 收窄到四个 id，这里只可能是 set_visibility。
                        _ => self.admin_set_visibility(id, &args, actor),
                    }
                };
                self.finish_admin_call(id, name, &args, result, actor)
            }
            _ => self
                .tool_upstream(id, name, &args, scope, actor)
                .await
                .unwrap_or_else(|| rpc_error(id, -32602, "Unknown tool", json!({"tool": name}))),
        }
    }

    /// 上游工具调用：名字解析（当前投影精确匹配 → `server_id__` 前缀回退）→ 转发 → 审计。
    ///
    /// 返回 `None` 表示名字不属于任何启用上游（调用方回 `-32602`）；上游自己的失败不外抛，
    /// 一律整形为 MCP `isError` 结果，且**不**把上游 JSON-RPC error 当成本服务的协议错误。
    async fn tool_upstream(
        &self,
        id: &Value,
        name: &str,
        args: &Value,
        scope: &EffectiveScope,
        actor: &Actor,
    ) -> Option<Value> {
        let manager = self.service.upstreams()?;
        // allowlist 必须先判：被拒绝的名字不得触发任何上游连接/工具拉取（只查 SQLite 前缀）。
        // 未命中任何上游前缀的名字仍交给 `resolve` 返回 None → -32602（与"不存在"同构）。
        if !crate::discovery::tool_allowed(scope, name) && manager.matches_upstream(scope, name) {
            let e = McpError::new("MCP_CAPABILITY_NOT_FOUND", json!({"tool": name}));
            return Some(self.reject(
                id,
                actor,
                &e,
                json!({"tool": name, "surface": "mcp/upstream-allowlist"}),
            ));
        }
        let call = manager.resolve(scope, name).await?;
        let started = Instant::now();
        let outcome = manager.call(&call, args).await;
        let duration_ms = started.elapsed().as_millis() as i64;
        // 参数按 key 递归脱敏（token/password/authorization...）：上游入参可能是凭证。
        let mut data = json!({
            "arguments": redact_arguments(args),
            "upstream_id": call.upstream_id,
            "upstream_tool": call.tool,
            "exposed_tool": name,
            "duration_ms": duration_ms
        });
        match outcome {
            Ok((result, definition_hash)) => {
                let is_error = result
                    .get("isError")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                data["outcome"] = json!(if is_error { "error" } else { "ok" });
                data["definition_hash"] = json!(definition_hash);
                let mut payload =
                    audit_base("tool", name, "call", if is_error { "error" } else { "ok" });
                if is_error {
                    payload.insert("error_code".into(), json!("MCP_UPSTREAM_ERROR"));
                    payload.insert("error_message".into(), json!("上游工具返回 isError=true"));
                }
                payload.insert("data".into(), data);
                if let Err(e) = self
                    .service
                    .audit_event("tool_call", Value::Object(payload), actor)
                {
                    return Some(err_tool(id, &e));
                }
                let content = result
                    .get("content")
                    .cloned()
                    .unwrap_or_else(|| json!(result));
                Some(ok(id, json!({"content": content, "isError": is_error})))
            }
            Err(failure) => {
                let outcome_label = failure.outcome;
                data["outcome"] = json!(outcome_label);
                data["definition_hash"] = json!(failure.definition_hash);
                let e = McpError::with_message(
                    &failure.code,
                    failure.message.clone(),
                    json!({"upstream_id": failure.upstream_id, "tool": call.tool}),
                );
                let mut payload = audit_base("tool", name, "call", outcome_label);
                payload.insert("error_code".into(), json!(e.code));
                payload.insert("error_message".into(), json!(e.message));
                payload.insert("data".into(), data);
                if let Err(audit_error) =
                    self.service
                        .audit_event("tool_call", Value::Object(payload), actor)
                {
                    return Some(err_tool(id, &audit_error));
                }
                Some(err_tool(id, &e))
            }
        }
    }

    /// admin 工具的统一授权：只看服务端 scope，不看请求参数与客户端清单。
    ///
    /// 非 admin 组直接与"不存在"同构（`MCP_CAPABILITY_NOT_FOUND`），并写 policy_decision——
    /// 探测维护通道本身就是一个需要被审计的事件。
    fn authorize_admin(
        &self,
        id: &Value,
        actor: &Actor,
        scope: &EffectiveScope,
        tool: &str,
    ) -> Option<Value> {
        if crate::discovery::admin_enabled(scope) {
            return None;
        }
        let e = McpError::new("MCP_CAPABILITY_NOT_FOUND", json!({"tool": tool}));
        Some(self.reject(
            id,
            actor,
            &e,
            json!({
                "tool": tool,
                "required_group": crate::discovery::ADMIN_GROUP,
                "surface": "mcp/admin"
            }),
        ))
    }

    fn require_store(&self) -> Result<&CapabilityStore, McpError> {
        self.service.store.as_deref().ok_or_else(|| {
            McpError::new(
                "MCP_STORE_ERROR",
                json!({"hint": "能力注册表未注入（CapabilityStore 未就绪）"}),
            )
        })
    }

    /// 维护通道写工具的 fail-closed 前置：**审计未启用就不写**。
    ///
    /// `Service::audit_event` 在 `audit: None` 时是 no-op，写工具若照常落库就成了
    /// "写成功但无痕迹"——治理面不可静默失效，因此任何落库动作之前先在这里拒绝。
    /// 拒绝本身也走 `reject`（写 policy_decision）；无审计时那条审计同样是 no-op，
    /// 这是可接受的：首要目标是不写。
    fn require_audit(&self, id: &Value, actor: &Actor, tool: &str) -> Option<Value> {
        if self.service.audit.is_some() {
            return None;
        }
        let e = McpError::new(
            "MCP_AUDIT_ERROR",
            json!({"hint": "审计未启用，维护通道拒绝写入"}),
        );
        Some(self.reject(id, actor, &e, json!({"tool": tool})))
    }

    /// 维护工具调用的收尾：写一条 `tool_call` 审计后原样返回结果。
    ///
    /// 为什么在这里统一写：4 个 admin 工具走同一分支，逐处写必有一处遗漏——而管理台的
    /// 工具台账只认 `tool_call`，漏写就等于该工具永远 calls=0/health=unknown。
    /// 拒绝路径（未授权、审计缺失、builtin 不可写、参数非法）由调用方一并交进来，记
    /// `outcome=error` + `mcp_code`；成功则用 `estimate_tokens(结果)` 记 tokens。
    fn finish_admin_call(
        &self,
        id: &Value,
        tool: &str,
        args: &Value,
        result: Value,
        actor: &Actor,
    ) -> Value {
        let content = result
            .get("result")
            .and_then(|r| r.get("content"))
            .unwrap_or(&result);
        let payload = match content.get("error") {
            Some(error) => {
                let mut base = audit_base("tool", tool, "invoke", "error");
                base.insert(
                    "error_code".into(),
                    error
                        .get("code")
                        .and_then(|c| c.as_str())
                        .map_or(Value::Null, |c| json!(c)),
                );
                base.insert(
                    "error_message".into(),
                    error
                        .get("message")
                        .and_then(|m| m.as_str())
                        .map_or(Value::Null, |m| json!(m)),
                );
                audit_data(base, json!({"arguments": args}))
            }
            None => audit_data(
                audit_base("tool", tool, "invoke", "ok"),
                json!({"arguments": args, "tokens": estimate_tokens(content)}),
            ),
        };
        // 审计写失败必须上抛（与其它入口同一约定），不吞成"调用成功"。
        if let Err(e) = self.service.audit_event("tool_call", payload, actor) {
            return err_tool(id, &e);
        }
        result
    }

    /// `admin_register_template`：定义原文 → `TemplateDef::parse` 门禁 + 编译 → 落 draft。
    ///
    /// 只产草案：`review → published` 必须走 HTTP 管理面的人工审批，agent 无法自我发布。
    fn admin_register_template(&self, id: &Value, args: &Value, actor: &Actor) -> Value {
        const TOOL: &str = "admin_register_template";
        // 审计未启用 → 任何编译/落库之前就拒绝（见 `require_audit`）。
        if let Some(denied) = self.require_audit(id, actor, TOOL) {
            return denied;
        }
        let Some(definition) = args.get("definition").filter(|v| v.is_object()).cloned() else {
            return self.reject(
                id,
                actor,
                &McpError::validation("definition 必须是模板定义对象".to_string()),
                json!({"tool": TOOL}),
            );
        };
        let Some(registry) = self.service.templates.as_ref() else {
            return err_tool(
                id,
                &McpError::new(
                    "MCP_STORE_ERROR",
                    json!({"hint": "模板注册表未注入（TemplateRegistry 未就绪）"}),
                ),
            );
        };
        // 组必须先存在（配置 policy 组或管理组），否则不落库。
        let group = definition
            .get("group")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if let Err(e) = self.service.ensure_group_exists(group) {
            return self.reject(id, actor, &e, json!({"tool": TOOL, "group": group}));
        }
        // registry.register = TemplateDef::parse（校验门禁）+ 编译 + store 落 draft。
        let compiled = match registry.register(&definition) {
            Ok(c) => c,
            Err(e) => return self.reject(id, actor, &e, json!({"tool": TOOL})),
        };
        let record = match registry.store().get(&compiled.def.template_id) {
            Ok(Some(r)) => r,
            Ok(None) => {
                return err_tool(
                    id,
                    &McpError::store(format!("register 后记录缺失: {}", compiled.def.template_id)),
                );
            }
            Err(e) => return err_tool(id, &e),
        };
        // 审计失败必须上抛：draft 已落库而审计缺行，属"不可见但无痕迹"的残留风险
        // （draft 既不 discoverable 也不 callable，不会泄露给任何 agent，故不需回滚）。
        let mut payload = audit_base("capability", &record.capability_id, "create", "ok");
        payload.insert("to_status".into(), json!("draft"));
        payload.insert(
            "changes".into(),
            json!(jumcp_core::registry::definition_changes(
                &json!({}),
                &record.definition
            )),
        );
        payload.insert(
            "data".into(),
            json!({
                "definition_hash_before": Value::Null,
                "definition_hash_after": record.definition_hash,
                "version": record.version
            }),
        );
        if let Err(e) = self
            .service
            .audit_event("admin_change", Value::Object(payload), actor)
        {
            return err_tool(id, &e);
        }
        ok(
            id,
            json!({
                "content": {
                    "id": record.capability_id,
                    "status": record.status,
                    "version": record.version,
                    "definition_hash": record.definition_hash
                },
                "isError": false
            }),
        )
    }

    /// `admin_review_queue`：只读审核队列（draft/review）。
    fn admin_review_queue(&self, id: &Value, actor: &Actor) -> Value {
        const TOOL: &str = "admin_review_queue";
        let store = match self.require_store() {
            Ok(s) => s,
            Err(e) => return self.reject(id, actor, &e, json!({"tool": TOOL})),
        };
        let records = match store.review_queue() {
            Ok(r) => r,
            Err(e) => return self.reject(id, actor, &e, json!({"tool": TOOL})),
        };
        let items: Vec<Value> = records.iter().map(crate::admin::queue_item).collect();
        ok(
            id,
            json!({
                "content": {"items": items, "total": items.len()},
                "isError": false
            }),
        )
    }

    /// `admin_audit_queries`：只读审计查询，limit 夹紧在 1..=200（与 HTTP 管理面同一上限）。
    fn admin_audit_queries(&self, id: &Value, args: &Value, actor: &Actor) -> Value {
        const TOOL: &str = "admin_audit_queries";
        let filter = match args.get("filter") {
            None | Some(Value::Null) => None,
            Some(v) if v.is_object() => Some(v.clone()),
            Some(_) => {
                return self.reject(
                    id,
                    actor,
                    &McpError::validation("filter 必须是对象".to_string()),
                    json!({"tool": TOOL}),
                );
            }
        };
        let limit = args
            .get("limit")
            .and_then(|v| v.as_i64())
            .unwrap_or(50)
            .clamp(1, crate::admin::MAX_LIMIT);
        let offset = args
            .get("offset")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            .max(0);
        let order = match args.get("order").and_then(|v| v.as_str()) {
            None => jumcp_core::audit::Order::Desc,
            Some(raw) => match jumcp_core::audit::Order::parse(raw) {
                Ok(o) => o,
                Err(e) => return self.reject(id, actor, &e, json!({"tool": TOOL})),
            },
        };
        let Some(log) = self.service.audit.as_ref() else {
            return err_tool(
                id,
                &McpError::new("MCP_AUDIT_ERROR", json!({"hint": "审计未启用"})),
            );
        };
        let (events, total) = match log.query_page(filter.as_ref(), limit, offset, order) {
            Ok(r) => r,
            // 非法过滤键属于"请求被拒"，按契约写 policy_decision。
            Err(e) => return self.reject(id, actor, &e, json!({"tool": TOOL})),
        };
        ok(
            id,
            json!({
                "content": {"events": events, "total": total, "limit": limit, "offset": offset, "order": if order == jumcp_core::audit::Order::Asc { "asc" } else { "desc" }},
                "isError": false
            }),
        )
    }

    /// `admin_set_visibility`：只改双布尔，不改定义；`admin_change` 由 store 写入
    /// （before/after 同为当前 definition_hash，diff 记字段变更）。
    fn admin_set_visibility(&self, id: &Value, args: &Value, actor: &Actor) -> Value {
        const TOOL: &str = "admin_set_visibility";
        // 审计未启用 → 任何落库之前就拒绝（见 `require_audit`）。
        if let Some(denied) = self.require_audit(id, actor, TOOL) {
            return denied;
        }
        let Some(capability_id) = args
            .get("capability_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        else {
            return self.reject(
                id,
                actor,
                &McpError::validation("capability_id 必填".to_string()),
                json!({"tool": TOOL}),
            );
        };
        let discoverable = args.get("discoverable").and_then(|v| v.as_bool());
        let callable = args.get("callable").and_then(|v| v.as_bool());
        if discoverable.is_none() && callable.is_none() {
            return self.reject(
                id,
                actor,
                &McpError::validation(
                    "discoverable/callable 至少要指定一个（无变更的可见性写入不产生审计价值）"
                        .to_string(),
                ),
                json!({"tool": TOOL, "capability_id": capability_id}),
            );
        }
        let store = match self.require_store() {
            Ok(s) => s,
            Err(e) => return self.reject(id, actor, &e, json!({"tool": TOOL})),
        };
        let record = match store.set_visibility(capability_id, discoverable, callable, actor) {
            Ok(r) => r,
            Err(e) => {
                return self.reject(
                    id,
                    actor,
                    &e,
                    json!({"tool": TOOL, "capability_id": capability_id}),
                );
            }
        };
        ok(
            id,
            json!({
                "content": {
                    "id": record.capability_id,
                    "discoverable": record.discoverable,
                    "callable": record.callable,
                    "version": record.version,
                    "last_changed_at": record.updated_at
                },
                "isError": false
            }),
        )
    }
}

/// 目录形态：flat 时包含会话可见的 kind=tool 能力定义，collapsed 时只含元工具 + 分发器。
struct CatalogView {
    tools: Vec<Value>,
    flattened: bool,
    flattened_count: usize,
    groups: Vec<Value>,
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
                    "secret",
                    "api_key",
                    "apikey",
                    "cookie",
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

/// 协议级错误（§6.4）：`data.mcp_code` 承载稳定错误码，客户端据此分支而不是解析文案。
fn rpc_mcp_error(id: &Value, e: &McpError) -> Value {
    let mut data = e.data.clone();
    if let Some(obj) = data.as_object_mut() {
        obj.insert("mcp_code".into(), json!(e.code));
    }
    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":e.message,"data":data}})
}

fn err_tool(id: &Value, e: &McpError) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":{"content":{"error":{"code":e.code,"message":e.message,"data":e.data}},"isError":true}})
}

// ---------------------------- HTTP 入口 ----------------------------

/// `/mcp` 路由：per-request 身份来自 `Authorization: Bearer <平台生成的 access token>`。
///
/// 放在库内（而不是 main 里）是为了让集成测试能走真实的 axum 链路验证身份派生，
/// 而不是在测试里复制一份鉴权逻辑。生产环境只有这一条鉴权分支：token 必须命中
/// registry 的 `access_tokens`（active），不存在环境变量静态 token 的兜底。
pub fn http_router(server: Arc<McpServer>) -> Router {
    Router::new()
        .route("/mcp", post(post_mcp))
        .with_state(server)
}

/// 未认证主体：只用于把"未授权的探测"写进审计链，不代表任何真实 agent。
/// 故意留空 id/role——不能借审计链给匿名调用者伪造一个人格。
fn anonymous_actor() -> Actor {
    Actor {
        tenant: "default".into(),
        role: String::new(),
        actor_type: "agent".into(),
        actor_id: String::new(),
        ..Default::default()
    }
}

fn unauthorized(server: &McpServer, detail: Value) -> Value {
    let actor = anonymous_actor();
    let e = McpError::new("MCP_UNAUTHORIZED", detail);
    let err = server
        .service()
        .policy_decision(&actor, &e, json!({"transport": "http", "surface": "mcp"}))
        .err()
        .unwrap_or(e);
    json!({
        "jsonrpc":"2.0","id":null,
        "error":{"code":-32602,"message":err.message,
                 "data":{"mcp_code": err.code, "detail": err.data}}
    })
}

async fn post_mcp(
    State(server): State<Arc<McpServer>>,
    headers: HeaderMap,
    body: String,
) -> Json<Value> {
    // 身份只来自数据库 token：缺 token / token 未知 / 已撤销 / 已过期一律 MCP_UNAUTHORIZED
    //（不回落默认 analyst，也不读环境变量静态 token）。
    let token = match crate::admin::bearer(&headers) {
        Some(token) if !token.is_empty() => token,
        _ => {
            return Json(unauthorized(
                &server,
                json!({"hint": "需要 Authorization: Bearer <平台生成的 access token>"}),
            ));
        }
    };
    let record = match server.service().authenticate_mcp_token(token) {
        Ok(Some(record)) => record,
        Ok(None) => {
            return Json(unauthorized(
                &server,
                json!({"hint": "Bearer token 未注册、已撤销或已过期"}),
            ));
        }
        Err(e) => {
            return Json(json!({
                "jsonrpc":"2.0","id":null,
                "error":{"code":-32602,"message":e.message,
                          "data":{"mcp_code": e.code, "detail": e.data}}
            }));
        }
    };
    let actor = Actor::access_token(
        &record.tenant,
        &record.caller,
        &record.role,
        &record.id,
        &record.source,
        record.groups.clone(),
    )
    .with_builtin_tools(record.builtin_tools.clone());
    let header_map: HashMap<String, String> = headers
        .iter()
        .filter_map(|(k, v)| v.to_str().ok().map(|s| (k.to_string(), s.to_string())))
        .collect();
    // 通道优先级在这里定一次（Header > _meta 会话 > 角色默认）：判定失败即整体拒绝并写
    // policy_decision，不静默降级；不再把 header 解析结果无条件当作本请求的 scope。
    let scope = match server.http_scope(&actor, &header_map) {
        Ok(s) => s,
        Err(e) => {
            let err = server
                .service()
                .policy_decision(&actor, &e, json!({"transport": "http", "detail": e.data}))
                .err()
                .unwrap_or(e);
            return Json(json!({
                "jsonrpc":"2.0","id":null,
                "error":{"code":-32602,"message":err.message,
                          "data":{"mcp_code": err.code, "detail": err.data}}
            }));
        }
    };
    let req: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            return Json(json!({
                "jsonrpc":"2.0","id":null,
                "error":{"code":-32700,"message":"Parse error","data":{"detail": e.to_string()}}
            }));
        }
    };
    Json(
        server
            .handle_authenticated(&req, &actor, Some(&scope))
            .await,
    )
}

#[cfg(test)]
mod category_tests {
    use super::builtin_category;
    use crate::catalog::{CATEGORY_ADMIN, CATEGORY_ANALYSIS, CATEGORY_BASIC, CATEGORY_CUSTOM};

    #[test]
    fn every_builtin_id_has_its_responsibility_category() {
        // basic：基础查询（目录走查）。
        for id in ["search_capabilities", "describe_capability", "list_groups"] {
            assert_eq!(builtin_category(id), CATEGORY_BASIC, "{id} 应归 basic");
        }
        // analysis：工具列表分析与变更提交审计。
        for id in [
            "admin_review_queue",
            "admin_register_template",
            "admin_set_visibility",
        ] {
            assert_eq!(
                builtin_category(id),
                CATEGORY_ANALYSIS,
                "{id} 应归 analysis"
            );
        }
        // admin：仅审计操作。
        assert_eq!(
            builtin_category("admin_audit_queries"),
            CATEGORY_ADMIN,
            "admin_audit_queries 应归 admin"
        );
    }

    #[test]
    fn unknown_ids_fall_back_to_custom() {
        for id in [
            "",
            "db_query",
            "search",
            "admin_",
            "admin_unknown",
            "upstream__echo",
        ] {
            assert_eq!(builtin_category(id), CATEGORY_CUSTOM, "{id} 应归 custom");
        }
    }
}
