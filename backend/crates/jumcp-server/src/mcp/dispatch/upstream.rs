//! 上游工具调用臂：名字解析（精确投影 → `server_id__` 前缀回退）、转发与审计。

use super::*;

impl McpServer {
    /// 上游工具调用：名字解析（当前投影精确匹配 → `server_id__` 前缀回退）→ 转发 → 审计。
    ///
    /// 返回 `None` 表示名字不属于任何启用上游（调用方回 `-32602`）；上游自己的失败不外抛，
    /// 一律整形为 MCP `isError` 结果，且**不**把上游 JSON-RPC error 当成本服务的协议错误。
    pub(super) async fn tool_upstream(
        &self,
        id: &Value,
        name: &str,
        args: &Value,
        scope: &EffectiveScope,
        actor: &Actor,
    ) -> Option<Value> {
        let manager = self.service.upstreams()?;
        let owner = manager.owning_upstream(name);
        // 组授权预检必须先判：组内没有授予、或该工具被逐工具规则排除时，直接拒绝而不产生
        // 任何上游连接/工具拉取（只查 SQLite）。未命中任何上游前缀的名字仍交给 `resolve`
        // 返回 None → -32602（与"不存在"同构）。
        if let Some(upstream_id) = owner.as_deref()
            && !manager.matches_upstream(scope, name)
        {
            // 拒绝照记审计，但回包必须与"名字不存在"逐字节同构：否则调用方能用错误形状
            // 探出某台上游是否存在、是否已接入。
            let e = McpError::new("MCP_CAPABILITY_NOT_FOUND", json!({"tool": name}));
            let _ = self.service.policy_decision(
                actor,
                &e,
                json!({"tool": name, "surface": "mcp/upstream-groups", "upstream_id": upstream_id}),
            );
            return None;
        }
        // 前置拦截（SQL/文件/接口三族）：必须在 `resolve` 之前——它要建连并拉 tools/list，
        // 被拒的调用连"上游存在哪些工具"都不该去问。上游是第三方，本服务是唯一收口点。
        if let Some(guard) = self.preflight.as_deref()
            && let Some(upstream_id) = owner.as_deref()
            && let Some(crate::preflight::Rejection {
                family,
                rule,
                message,
                hint,
            }) = crate::preflight::check_call(args, guard)
        {
            let e = McpError::with_message(
                family.error_code(),
                message,
                json!({
                    "rule": rule,
                    "hint": hint,
                    "upstream_id": upstream_id,
                    "tool": name,
                }),
            );
            let mut detail = json!({
                "surface": family.surface(),
                "rule": rule,
                "tool": name,
                "upstream_id": upstream_id,
            });
            // sql 只留前 200 字符：审计要能复盘意图，但不能把整条大查询塞进台账。
            // 文件/接口调用的入参里没有查询文本，这一项只对 SQL 族有意义。
            if family == crate::preflight::Family::Sql
                && let Some(sql) = crate::preflight::sql_argument(args)
            {
                detail["sql"] = json!(sql.chars().take(200).collect::<String>());
            }
            return Some(self.reject(id, actor, &e, detail));
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
}
