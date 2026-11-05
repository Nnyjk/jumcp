//! 手写 JSON-RPC MCP 服务：tools/list（按 EffectiveScope 过滤 + 常驻预算）+ 元工具三件套 +
//! db_query_readonly 完整链（sqlguard → 预算预检 → 执行 → 整形 → 审计 → 记账）。
//! 为什么手写：rmcp API 随版本漂移，任务允许退回且双传输必须同时支持。

use crate::catalog::Capability;
use crate::executor::{QueryParams, check_placeholders, validate_sql};
use crate::service::Service;
use hjzl_core::budget::estimate_tokens;
use hjzl_core::canonical::sha256_hex;
use hjzl_core::entry::{Actor, EffectiveScope, GroupRequest, resolve_scope};
use hjzl_core::errors::McpError;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

pub const READONLY_ANNOTATIONS: &str =
    r#"{"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false}"#;

/// 常量解析只做一次；常量写错时在首次使用即 panic，而不是静默返回空注解。
fn annotations() -> &'static Value {
    static CACHE: OnceLock<Value> = OnceLock::new();
    CACHE.get_or_init(|| {
        serde_json::from_str(READONLY_ANNOTATIONS)
            .expect("READONLY_ANNOTATIONS 必须是合法 JSON 对象")
    })
}

pub fn tool_definitions() -> Vec<Value> {
    vec![
        tool(
            "search_capabilities",
            "统一检索入口：返回候选列表，不含完整 schema",
            json!({"type":"object","properties":{"query":{"type":"string"},"kind":{"type":"string"},"group":{"type":"string"},"limit":{"type":"integer"}},"required":["query"]}),
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
        tool(
            "db_run_template",
            "模板入口：注册期已校验的固定查询路径（版本钉住 + 结果缓存）",
            json!({"type":"object","properties":{
                "template_id":{"type":"string"},
                "params":{"type":"object"},
                "version":{"type":"integer"}
            },"required":["template_id"]}),
        ),
        tool(
            "db_query_readonly",
            "通用只读入口：sqlguard 校验 → 预算 → 只读事务执行 → 整形 → 审计",
            json!({"type":"object","properties":{
                "sql":{"type":"string"},
                "params":{"type":"array","description":"位置参数（按 $1..$n 顺序绑定）"},
                "max_rows":{"type":"integer","description":"默认 100，上限 1000"},
                "max_tokens":{"type":"integer"},
                "cell_char_limit":{"type":"integer","description":"默认 200，上限 4000"},
                "cell_char_offset":{"type":"integer"},
                "allowed_schemas":{"type":"array","description":"只能是连接声明的子集"}
            },"required":["sql"]}),
        ),
    ]
}

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({"name": name, "description": description, "inputSchema": input_schema, "annotations": annotations()})
}

/// 平铺用的工具定义：与 `describe_capability` 完全同源（同一个 params_schema / one_line），
/// 不允许两套拼装逻辑，否则同一能力会出现两个 schema。
fn flattened_tool(cap: &Capability) -> Value {
    json!({
        "name": cap.id,
        "description": cap.one_line,
        "inputSchema": cap.params_schema,
        "annotations": annotations(),
        "group": cap.group
    })
}

pub struct McpServer {
    service: Arc<Service>,
    /// `initialize` 的 `_meta` 通道解析出的会话 scope（§4.1.6 优先级 2，低于 HTTP Header）。
    session_scope: Mutex<Option<EffectiveScope>>,
}

impl McpServer {
    pub fn new(service: Service) -> Self {
        Self::from_arc(Arc::new(service))
    }
    pub fn from_arc(service: Arc<Service>) -> Self {
        Self {
            service,
            session_scope: Mutex::new(None),
        }
    }
    pub fn service(&self) -> &Service {
        &self.service
    }

    /// 处理一个 JSON-RPC 请求并返回响应体（null 表示 notification）。
    pub async fn handle(&self, req: &Value) -> Value {
        self.handle_scoped(req, None).await
    }

    /// 同 `handle`，但允许传入请求级 scope（HTTP 头收窄后的结果，§4.1.6 通道优先级由调用方保证）。
    pub async fn handle_scoped(
        &self,
        req: &Value,
        request_scope: Option<&hjzl_core::entry::EffectiveScope>,
    ) -> Value {
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let params = req.get("params").cloned().unwrap_or(json!({}));
        if method.starts_with("notifications/") {
            return Value::Null;
        }
        match method {
            "initialize" => {
                if let Err(e) = self.apply_meta_scope(&params) {
                    return rpc_mcp_error(&id, &e);
                }
                ok(
                    &id,
                    json!({"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"hjzl-mcp-server","version":"0.1.0"}}),
                )
            }
            "tools/list" | "tools/call" => {
                let scope = match request_scope.cloned().or_else(|| self.session_scope()) {
                    Some(s) => s,
                    None => match self.service.require_scope() {
                        Ok(s) => s.clone(),
                        Err(e) => return err_tool(&id, &e),
                    },
                };
                if method == "tools/list" {
                    self.handle_tools_list(&id, &scope).await
                } else {
                    self.handle_tools_call(&id, &params, &scope).await
                }
            }
            "ping" => ok(&id, json!({})),
            _ => rpc_error(&id, -32601, "Method not found", json!({"method": method})),
        }
    }

    fn session_scope(&self) -> Option<EffectiveScope> {
        self.session_scope
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
    }

    /// `initialize` 的 `_meta` 通道（§4.1.6 优先级 2）：只在声明了 `mcp/groups` 或 `mcp/toolBudget`
    /// 时生效；越界仍按租户模式判定（strict 直接拒绝，不裁剪）。
    fn apply_meta_scope(&self, params: &Value) -> Result<(), McpError> {
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
        let settings = &self.service.settings;
        let request = GroupRequest {
            meta: meta.clone(),
            ..Default::default()
        };
        let scope = resolve_scope(
            &self.actor(),
            &request,
            &settings.policy,
            &settings.groups_mode,
            None,
        )?;
        let mut guard = self.session_scope.lock().map_err(|_| McpError::lock())?;
        *guard = Some(scope);
        Ok(())
    }

    /// 目录视图（§5.2 静态客户端路径）：可见 kind=tool 能力数 ≤ 阈值时平铺，否则退回收缩形态。
    fn catalog_view(&self, scope: &EffectiveScope) -> CatalogView {
        let threshold = self.service.settings.flatten_threshold;
        let mut visible_tools: Vec<&Capability> = self
            .service
            .catalog
            .visible(scope)
            .into_iter()
            .filter(|c| {
                c.kind == "tool" && !crate::service::RESIDENT_TOOL_NAMES.contains(&c.id.as_str())
            })
            .collect();
        visible_tools.sort_by(|a, b| a.id.cmp(&b.id));
        let flatten = threshold > 0 && (visible_tools.len() as i64) <= threshold;
        let mut tools = tool_definitions();
        if flatten {
            tools.extend(visible_tools.iter().map(|cap| flattened_tool(cap)));
        }
        CatalogView {
            tools,
            flattened: flatten,
            flattened_count: if flatten { visible_tools.len() } else { 0 },
            groups: self.service.catalog.group_counts(scope),
        }
    }

    fn actor(&self) -> Actor {
        Actor::new("default", "analyst")
    }

    /// 拒绝路径：写 policy_decision 审计后返回原错误；审计本身失败则把审计错误上抛。
    fn reject(&self, id: &Value, actor: &Actor, e: &McpError, extra: Value) -> Value {
        let mut detail = extra;
        if let (Some(obj), Some(data)) = (detail.as_object_mut(), e.data.as_object()) {
            for (k, v) in data {
                obj.insert(k.clone(), v.clone());
            }
        }
        match self.service.policy_decision(actor, &e.code, detail) {
            Ok(()) => err_tool(id, e),
            Err(audit_error) => err_tool(id, &audit_error),
        }
    }

    async fn handle_tools_list(&self, id: &Value, scope: &EffectiveScope) -> Value {
        let actor = self.actor();
        let view = self.catalog_view(scope);
        let kept = self.service.tools_for_scope_with(scope, &view.tools);
        // 组索引本身也计入常驻预算（§5.1「组索引 ≤20 组」那一行）。
        let index_tokens = estimate_tokens(&json!(view.groups));
        let resident = self.service.resident_tokens(&kept) + index_tokens;
        match self.service.check_resident(resident) {
            Ok(result) if result.detail.get("truncated").is_some() => {
                // truncate（显式开启）：只保留常驻工具，丢弃的能力组必须回显（§4.1.4）。
                let resident_only: Vec<Value> = kept
                    .iter()
                    .filter(|t| {
                        let name = t.get("name").and_then(|n| n.as_str()).unwrap_or("");
                        crate::service::RESIDENT_TOOL_NAMES.contains(&name)
                    })
                    .cloned()
                    .collect();
                let dropped: Vec<String> = kept
                    .iter()
                    .filter(|t| {
                        let name = t.get("name").and_then(|n| n.as_str()).unwrap_or("");
                        !crate::service::RESIDENT_TOOL_NAMES.contains(&name)
                    })
                    .filter_map(|t| t.get("group").and_then(|g| g.as_str()).map(String::from))
                    .collect();
                let spent = self.service.resident_tokens(&resident_only) + index_tokens;
                if let Err(e) = self.service.audit_event(
                    "tools_list_served",
                    json!({
                        "tools": names(&resident_only),
                        "resident_tokens": spent,
                        "truncated": true,
                        "dropped_groups": dropped,
                        "flattened": view.flattened
                    }),
                    &actor,
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
                    json!({
                        "tools": names(&kept),
                        "resident_tokens": resident,
                        "flattened": view.flattened,
                        "flattened_count": view.flattened_count
                    }),
                    &actor,
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
                            "设置 HJZL_MCP_FLATTEN_THRESHOLD=0 关闭平铺，改用 search_capabilities 按需发现",
                            "缩小 X-MCP-Tool-Groups 以减少可见能力数"
                        ]),
                    );
                }
                self.reject(
                    id,
                    &actor,
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
        scope: &hjzl_core::entry::EffectiveScope,
    ) -> Value {
        let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
        let args = params.get("arguments").cloned().unwrap_or(json!({}));
        let actor = self.actor();
        match name {
            "search_capabilities" => {
                let query = args.get("query").and_then(|q| q.as_str()).unwrap_or("");
                let kind = args.get("kind").and_then(|k| k.as_str());
                let group = args.get("group").and_then(|g| g.as_str());
                let limit = args
                    .get("limit")
                    .and_then(|l| l.as_u64())
                    .unwrap_or(5)
                    .min(50) as usize;
                let hits = self
                    .service
                    .catalog
                    .search(scope, query, kind, group, limit);
                let ids: Vec<String> = hits.iter().map(|h| h.id.clone()).collect();
                if let Err(e) = self.service.audit_event(
                    "capability_search",
                    json!({"query": query, "returned_ids": ids}),
                    &actor,
                ) {
                    return err_tool(id, &e);
                }
                let content: Vec<Value> = hits.iter().map(|h| json!({"id": h.id, "kind": h.kind, "group": h.group, "one_line": h.one_line, "param_summary": h.param_summary, "token_cost": h.token_cost})).collect();
                ok(id, json!({"content": content, "isError": false}))
            }
            "describe_capability" => {
                let cap_id = args
                    .get("capability_id")
                    .and_then(|c| c.as_str())
                    .unwrap_or("");
                let Some(cap) = self.service.catalog.describe(scope, cap_id).cloned() else {
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
                match self.service.check_schema(cost) {
                    Ok(result) if result.allowed => {
                        if let Err(e) = self.service.record_schema(cost) {
                            return err_tool(id, &e);
                        }
                        if let Err(e) = self.service.audit_event(
                            "capability_describe",
                            json!({"capability_id": cap_id, "schema_tokens": cost}),
                            &actor,
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
                                    "params_schema": cap.params_schema
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
                        self.reject(id, &actor, &e, json!({"tool": "describe_capability"}))
                    }
                }
            }
            "list_groups" => {
                let counts = self.service.catalog.group_counts(scope);
                ok(id, json!({"content": counts, "isError": false}))
            }
            "db_query_readonly" => self.tool_db_query_readonly(id, &args, scope, &actor).await,
            "db_run_template" => self.tool_db_run_template(id, &args, scope, &actor).await,
            _ => rpc_error(id, -32602, "Unknown tool", json!({"tool": name})),
        }
    }

    /// 固定链：schema 收窄 → sqlguard → 占位符 → 预算预检 → 执行 → 审计 → 记账。
    async fn tool_db_query_readonly(
        &self,
        id: &Value,
        args: &Value,
        scope: &hjzl_core::entry::EffectiveScope,
        actor: &Actor,
    ) -> Value {
        let sql = args.get("sql").and_then(|s| s.as_str()).unwrap_or("");
        let q = match QueryParams::new(
            sql.to_string(),
            &self.service.connection_schemas(),
            self.service.statement_timeout_ms(),
            args,
        ) {
            Ok(q) => q,
            Err(e) => return self.reject(id, actor, &e, json!({"tool": "db_query_readonly"})),
        };
        if let Err(e) = validate_sql(&q) {
            return self.reject(id, actor, &e, json!({"tool": "db_query_readonly"}));
        }
        if let Err(e) = check_placeholders(&q.sql, q.params.len()) {
            return self.reject(id, actor, &e, json!({"tool": "db_query_readonly"}));
        }
        // 预算预检：会话已进摘要模式时本次结果只回聚合值。
        let pre = match self.service.check_result(0) {
            Ok(p) => p,
            Err(e) => return self.reject(id, actor, &e, json!({"tool": "db_query_readonly"})),
        };

        let started = Instant::now();
        let outcome = self.service.executor.execute(scope, &q).await;
        let elapsed_ms = started.elapsed().as_millis() as i64;
        let sql_hash = sha256_hex(&q.sql);
        match outcome {
            Ok(result) => {
                let tokens = result
                    .get("tokens")
                    .and_then(|t| t.as_i64())
                    .unwrap_or_else(|| estimate_tokens(&result));
                let row_count = result
                    .get("row_count")
                    .and_then(|r| r.as_i64())
                    .unwrap_or_default();
                let truncated = result
                    .get("truncated")
                    .and_then(|t| t.as_bool())
                    .unwrap_or(false);
                // max_tokens 是调用方显式声明的结果预算：超限默认 fail（不静默截断）。
                if let Some(cap) = q.max_tokens
                    && tokens > cap
                {
                    let e = McpError::new(
                        "MCP_BUDGET_EXCEEDED",
                        json!({"required": tokens, "available": cap, "scope": "max_tokens"}),
                    );
                    if let Err(audit_error) = self.service.audit_event(
                        "tool_call",
                        json!({
                            "tool": "db_query_readonly",
                            "sql_hash": sql_hash,
                            "tokens": tokens,
                            "mcp_code": e.code,
                            "outcome": "budget_rejected"
                        }),
                        actor,
                    ) {
                        return err_tool(id, &audit_error);
                    }
                    return err_tool(id, &e);
                }
                let budget = match self.service.record_result(tokens) {
                    Ok(b) => b,
                    Err(e) => return err_tool(id, &e),
                };
                let summary = budget.summary || pre.summary;
                if let Err(e) = self.service.audit_event(
                    "tool_call",
                    json!({
                        "tool": "db_query_readonly",
                        "sql_hash": sql_hash,
                        "row_count": row_count,
                        "truncated": truncated,
                        "elapsed_ms": elapsed_ms,
                        "tokens": tokens,
                        "summary": summary,
                        "outcome": "ok"
                    }),
                    actor,
                ) {
                    return err_tool(id, &e);
                }
                if summary {
                    let summarized = json!({
                        "row_count": row_count,
                        "summary": true,
                        "truncated": truncated,
                        "elapsed_ms": elapsed_ms,
                        "note": "会话结果预算已达上限：仅返回聚合值，明细请缩小结果集或用模板分页"
                    });
                    return ok(id, json!({"content": summarized, "isError": false}));
                }
                ok(id, json!({"content": result, "isError": false}))
            }
            Err(e) => {
                // outcome=unknown 表示结果未知：不重试、不重放，只写审计并回错（§6.3）。
                let outcome_label =
                    if e.data.get("outcome").and_then(|v| v.as_str()) == Some("unknown") {
                        "unknown"
                    } else {
                        "error"
                    };
                if let Err(audit_error) = self.service.audit_event(
                    "tool_call",
                    json!({
                        "tool": "db_query_readonly",
                        "sql_hash": sql_hash,
                        "elapsed_ms": elapsed_ms,
                        "mcp_code": e.code,
                        "outcome": outcome_label
                    }),
                    actor,
                ) {
                    return err_tool(id, &audit_error);
                }
                err_tool(id, &e)
            }
        }
    }
    /// db_run_template：版本钉住 → 参数校验 → 缓存 → 只读事务执行 → 结果预算 → 审计。
    async fn tool_db_run_template(
        &self,
        id: &Value,
        args: &Value,
        scope: &hjzl_core::entry::EffectiveScope,
        actor: &Actor,
    ) -> Value {
        let Some(registry) = self.service.templates.as_ref() else {
            return err_tool(
                id,
                &McpError::new(
                    "MCP_NOT_IMPLEMENTED",
                    json!({"hint": "模板注册表未注入（TemplateRegistry 未就绪）"}),
                ),
            );
        };
        let Some(template_id) = args.get("template_id").and_then(|v| v.as_str()) else {
            return self.reject(
                id,
                actor,
                &McpError::validation("template_id 必填".to_string()),
                json!({"tool": "db_run_template"}),
            );
        };
        let params = args.get("params").cloned().unwrap_or(json!({}));
        let requested_version = args.get("version").and_then(|v| v.as_i64());

        let current = match registry.current(template_id) {
            Ok(r) => r,
            Err(e) => return err_tool(id, &e),
        };
        // 组不可见 / 未开放：与不存在同构（§2.6 能力探测面关闭）。
        if !scope.groups.contains(&current.group) || !current.discoverable || !current.callable {
            return err_tool(
                id,
                &McpError::new(
                    "MCP_CAPABILITY_NOT_FOUND",
                    json!({"template_id": template_id}),
                ),
            );
        }
        let version = match registry.pin(
            &self.service.session_id,
            template_id,
            requested_version,
            current.version,
        ) {
            Ok(v) => v,
            Err(e) => return err_tool(id, &e),
        };
        let record = if version == current.version {
            current
        } else {
            match registry.at_version(template_id, version) {
                Ok(r) => r,
                Err(e) => return err_tool(id, &e),
            }
        };
        let prepared = match record.compiled.render(&params) {
            Ok(p) => p,
            Err(e) => {
                return self.reject(
                    id,
                    actor,
                    &e,
                    json!({"tool": "db_run_template", "template_id": template_id}),
                );
            }
        };
        let cache_state = if registry
            .cache_lookup(template_id, version, &prepared.params_hash)
            .is_some()
        {
            "hit"
        } else {
            "miss"
        };
        if let Some(cached) = registry.cache_lookup(template_id, version, &prepared.params_hash) {
            if let Err(e) = self.service.audit_event(
                "template_run",
                json!({
                    "template_id": template_id,
                    "version": version,
                    "params_hash": prepared.params_hash,
                    "outcome": "ok",
                    "cache": "hit",
                    "elapsed_ms": 0
                }),
                actor,
            ) {
                return err_tool(id, &e);
            }
            return ok(
                id,
                json!({"content": cached, "isError": false, "cache": "hit", "version": version}),
            );
        }

        let constraints = &record.compiled.def.constraints;
        let schemas = match effective_schemas(
            &self.service.connection_schemas(),
            &constraints.allowed_schemas,
        ) {
            Ok(s) => s,
            Err(e) => return err_tool(id, &e),
        };
        let q = QueryParams::from_parts(
            prepared.sql.clone(),
            prepared.values.clone(),
            constraints.auto_limit,
            200,
            0,
            schemas,
            constraints.statement_timeout_ms,
        );
        let started = Instant::now();
        let outcome = self.service.executor.execute(scope, &q).await;
        let elapsed_ms = started.elapsed().as_millis() as i64;
        match outcome {
            Ok(result) => {
                let tokens = result
                    .get("tokens")
                    .and_then(|t| t.as_i64())
                    .unwrap_or_else(|| estimate_tokens(&result));
                let row_count = result
                    .get("row_count")
                    .and_then(|r| r.as_i64())
                    .unwrap_or_default();
                if tokens > constraints.max_result_tokens {
                    let over = McpError::new(
                        "MCP_BUDGET_EXCEEDED",
                        json!({
                            "template_id": template_id,
                            "required": tokens,
                            "available": constraints.max_result_tokens,
                            "scope": "max_result_tokens"
                        }),
                    );
                    if self.service.overflow_policy() == hjzl_core::budget::OverflowPolicy::Fail {
                        if let Err(e) = self.service.audit_event(
                            "template_run",
                            json!({
                                "template_id": template_id, "version": version,
                                "params_hash": prepared.params_hash, "outcome": "budget_rejected",
                                "cache": cache_state, "elapsed_ms": elapsed_ms
                            }),
                            actor,
                        ) {
                            return err_tool(id, &e);
                        }
                        return err_tool(id, &over);
                    }
                    let summarized = json!({
                        "row_count": row_count,
                        "summary": true,
                        "truncated": true,
                        "template_id": template_id,
                        "version": version
                    });
                    if let Err(e) = self.service.audit_event(
                        "template_run",
                        json!({
                            "template_id": template_id, "version": version,
                            "params_hash": prepared.params_hash, "outcome": "summary",
                            "cache": cache_state, "elapsed_ms": elapsed_ms
                        }),
                        actor,
                    ) {
                        return err_tool(id, &e);
                    }
                    return ok(
                        id,
                        json!({"content": summarized, "isError": false, "cache": cache_state, "version": version}),
                    );
                }
                let budget = match self.service.record_result(tokens) {
                    Ok(b) => b,
                    Err(e) => return err_tool(id, &e),
                };
                if let Err(e) = self.service.audit_event(
                    "template_run",
                    json!({
                        "template_id": template_id,
                        "version": version,
                        "params_hash": prepared.params_hash,
                        "outcome": "ok",
                        "cache": cache_state,
                        "elapsed_ms": elapsed_ms
                    }),
                    actor,
                ) {
                    return err_tool(id, &e);
                }
                let mut content = result;
                if let Some(obj) = content.as_object_mut() {
                    obj.insert("template_id".into(), json!(template_id));
                    obj.insert("version".into(), json!(version));
                    obj.insert("cache".into(), json!(cache_state));
                    if let Some(variant) = &prepared.variant_id {
                        obj.insert("variant".into(), json!(variant));
                    }
                    obj.insert("summary".into(), json!(budget.summary));
                }
                if budget.summary {
                    return ok(
                        id,
                        json!({"content": json!({"row_count": row_count, "summary": true, "template_id": template_id, "version": version}), "isError": false, "cache": cache_state, "version": version}),
                    );
                }
                registry.cache_store(
                    template_id,
                    version,
                    &prepared.params_hash,
                    content.clone(),
                    record.compiled.def.result_cache_ttl_s,
                );
                ok(
                    id,
                    json!({"content": content, "isError": false, "cache": cache_state, "version": version}),
                )
            }
            Err(e) => {
                let outcome_label =
                    if e.data.get("outcome").and_then(|v| v.as_str()) == Some("unknown") {
                        "unknown"
                    } else {
                        "error"
                    };
                if let Err(audit_error) = self.service.audit_event(
                    "template_run",
                    json!({
                        "template_id": template_id,
                        "version": version,
                        "params_hash": prepared.params_hash,
                        "outcome": outcome_label,
                        "cache": cache_state,
                        "elapsed_ms": elapsed_ms,
                        "mcp_code": e.code
                    }),
                    actor,
                ) {
                    return err_tool(id, &audit_error);
                }
                err_tool(id, &e)
            }
        }
    }
}

/// 目录形态：flat 时包含会话可见的 kind=tool 能力定义，collapsed 时只含元工具 + 分发器。
struct CatalogView {
    tools: Vec<Value>,
    flattened: bool,
    flattened_count: usize,
    groups: Vec<Value>,
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

/// 模板声明的 schema 只能比连接声明更窄：越界即拒绝，不做隐式降级。
fn effective_schemas(connection: &[String], template: &[String]) -> Result<Vec<String>, McpError> {
    if template.is_empty() {
        return Ok(connection.to_vec());
    }
    if !connection.is_empty() {
        let offending: Vec<&String> = template
            .iter()
            .filter(|s| !connection.contains(*s))
            .collect();
        if !offending.is_empty() {
            return Err(McpError::new(
                "MCP_SCHEMA_OUT_OF_SCOPE",
                json!({"offending_schemas": offending, "allowed": connection}),
            ));
        }
    }
    Ok(template.to_vec())
}
