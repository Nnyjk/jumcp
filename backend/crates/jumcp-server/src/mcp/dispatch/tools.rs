//! `tools/list` 的可见性投影与 `tools/call` 的分派路由。

use super::*;

impl McpServer {
    pub(super) async fn handle_tools_list(
        &self,
        id: &Value,
        scope: &EffectiveScope,
        actor: &Actor,
    ) -> Value {
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
        // 组索引本身也计入常驻预算。
        let index_tokens = estimate_tokens(&json!(view.groups));
        let resident = self.service.resident_tokens(kept) + index_tokens;
        match self.service.check_resident(resident) {
            Ok(result) if result.detail.get("truncated").is_some() => {
                // truncate（显式开启）：只保留常驻工具，丢弃的能力组必须回显。
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

    pub(super) async fn handle_tools_call(
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
                let hits: Vec<Value> = hits.iter().map(|h| json!({"id": h.id, "kind": h.kind, "group": h.group, "category": h.category, "one_line": h.one_line, "param_summary": h.param_summary, "token_cost": h.token_cost})).collect();
                // 结构化结果必须是对象（`structuredContent` 的规范形状），命中列表挂在 `results`。
                tool_result(id, json!({"results": hits}), false)
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
                    // 不可见与不存在同构。
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
                // 预算查询本身失败是存储/锁错误，必须原样上抛：把它当"预算耗尽"
                // 会把治理面的坏状态伪装成用量问题，调用方永远查不到根因。
                let budget = match self.service.check_schema(&key, cost) {
                    Ok(result) => result,
                    Err(e) => return err_tool(id, &e),
                };
                if !budget.allowed {
                    let e = McpError::new(
                        "MCP_CONTEXT_BUDGET_EXHAUSTED",
                        json!({"capability_id": cap_id, "incoming_tokens": cost}),
                    );
                    return self.reject(id, actor, &e, json!({"tool": "describe_capability"}));
                }
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
                tool_result(
                    id,
                    json!({
                        "id": cap.id,
                        "kind": cap.kind,
                        "group": cap.group,
                        "risk": cap.risk,
                        "one_line": cap.one_line,
                        "params_schema": cap.params_schema,
                        "source": cap.source,
                        "category": cap.category,
                        // schema 成本仍随结果回显：它是本次调用计入会话预算的量。
                        "schema_tokens": cost
                    }),
                    false,
                )
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
                // 结构化结果必须是对象：组索引挂在 `groups` 下，条目字段不变。
                tool_result(id, json!({"groups": counts}), false)
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
}
