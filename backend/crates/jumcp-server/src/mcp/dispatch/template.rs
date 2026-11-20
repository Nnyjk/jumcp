//! `db_run_template` 执行臂：版本钉住 → 参数渲染 → 结果缓存 → 执行 → 结果预算 → 审计。
//!
//! 这是"模板"唯一的内置执行入口：agent 不能自由拼 SQL，只能调用注册期已过 sqlguard 的
//! 固定查询路径。可见性只决定"谁能看见"，调用面判定是 `published ∧ callable ∧ group ∈ scope`。

use super::*;
use crate::service::session_key;
use jumcp_core::budget::OverflowPolicy;

impl McpServer {
    /// 模板执行：一次调用最多执行一条注册期校验过的固定 SQL。
    pub(super) async fn tool_db_run_template(
        &self,
        id: &Value,
        args: &Value,
        scope: &EffectiveScope,
        actor: &Actor,
    ) -> Value {
        let context = template_audit_context(args);
        let Some(registry) = self.service.templates.as_ref() else {
            let e = McpError::new(
                "MCP_STORE_ERROR",
                json!({"hint": "模板注册表未注入（TemplateRegistry 未就绪）"}),
            );
            return self.reject_tool_call(id, actor, &e, "db_run_template", context);
        };
        let Some(template_id) = args.get("template_id").and_then(|v| v.as_str()) else {
            let e = McpError::validation("template_id 必填".to_string());
            return self.reject_tool_call(id, actor, &e, "db_run_template", context);
        };
        let params = args.get("params").cloned().unwrap_or(json!({}));
        let requested_version = args.get("version").and_then(|v| v.as_i64());

        // 1. 注册表当前版本（不存在 / 非模板 / 已下架同构为 not_found）。
        let current = match registry.current(template_id) {
            Ok(r) => r,
            Err(e) => return self.reject_tool_call(id, actor, &e, "db_run_template", context),
        };
        // 2. 调用面判定与发现面共用同一处谓词实现。
        let callable = match self.service.callable_capabilities(scope) {
            Ok(v) => v,
            Err(e) => return self.reject_tool_call(id, actor, &e, "db_run_template", context),
        };
        if catalog::find(&callable, template_id).is_none() {
            let e = McpError::new(
                "MCP_CAPABILITY_NOT_FOUND",
                json!({"template_id": template_id}),
            );
            return self.reject_tool_call(id, actor, &e, "db_run_template", context);
        }
        // 3. 会话版本钉住：键按身份派生，不同 agent 可以各自钉住同一模板的不同版本。
        let key = session_key(actor);
        let version = match registry.pin(&key, template_id, requested_version, current.version) {
            Ok(v) => v,
            Err(e) => return self.reject_tool_call(id, actor, &e, "db_run_template", context),
        };
        let record = if version == current.version {
            current
        } else {
            match registry.at_version(template_id, version) {
                Ok(r) => r,
                Err(e) => return self.reject_tool_call(id, actor, &e, "db_run_template", context),
            }
        };
        // 4. 渲染成可直接发送的 SQL：上游 `query` 工具只收文本，没有参数数组。
        let rendered = match record.compiled.render_inline(&params) {
            Ok(r) => r,
            Err(e) => return self.reject_tool_call(id, actor, &e, "db_run_template", context),
        };
        let target = &record.compiled.def.target;
        // 5. 预检：注册期校验的是带占位符的形态，字面量落地后必须再过一遍运行时门禁。
        if let Some(guard) = self.preflight.as_deref()
            && let Err(rejection) = crate::preflight::sql::check(&rendered.sql, guard)
        {
            let e = McpError::with_message(
                "MCP_SQL_BLOCKED",
                rejection.message,
                json!({
                    "rule": rejection.rule,
                    "hint": rejection.hint,
                    "template_id": template_id,
                    "upstream": target.upstream,
                    "tool": target.tool
                }),
            );
            return self.reject_tool_call(id, actor, &e, "db_run_template", context);
        }
        // 6. 结果缓存：命中即不再执行（同 session 同参数连调两次只有第一次真跑）。
        if let Some(cached) = registry.cache_lookup(template_id, version, &rendered.params_hash) {
            // 命中分支没有执行结果：token 取缓存里记的值，缺失时才按同一口径估算。
            let cached_tokens = cached
                .get("tokens")
                .and_then(|t| t.as_i64())
                .unwrap_or_else(|| estimate_tokens(&cached));
            if let Err(e) = self.service.audit_event(
                "template_run",
                audit_data(
                    audit_base("template", template_id, "invoke", "ok"),
                    json!({
                        "arguments": args,
                        "executed_script": rendered.sql,
                        "bound_params": params,
                        "cache": "hit",
                        // 口径是"执行查询耗时"：命中路径没有执行，所以恒为 0，不是漏测。
                        "elapsed_ms": 0,
                        "tokens": cached_tokens,
                        "version": version,
                        "upstream_id": target.upstream,
                        "upstream_tool": target.tool
                    }),
                ),
                actor,
            ) {
                return err_tool(id, &e);
            }
            return tool_result(
                id,
                json!({"content": cached, "cache": "hit", "version": version}),
                false,
            );
        }

        // 7. 执行：交给已接入的上游 MCP 工具，本进程不持有数据库连接。
        let Some(manager) = self.service.upstreams() else {
            let e = McpError::new(
                "MCP_STORE_ERROR",
                json!({"hint": "上游管理器未注入（UpstreamManager 未就绪）"}),
            );
            return self.reject_tool_call(id, actor, &e, "db_run_template", context);
        };
        let exposed = crate::upstream::exposed_tool_name(&target.upstream, &target.tool);
        let Some(call) = manager.resolve(scope, &exposed).await else {
            let e = McpError::new(
                "MCP_UPSTREAM_NOT_FOUND",
                json!({
                    "upstream": target.upstream,
                    "tool": target.tool,
                    "hint": "模板 target 指向的上游工具不在本次调用可见的上游范围内"
                }),
            );
            return self.reject_tool_call(id, actor, &e, "db_run_template", context);
        };
        // `database` 是上游工具的可选参数：缺省时不发该键，显式 null 会被严格 schema 拒绝。
        let mut upstream_args = json!({"sql": rendered.sql});
        if let Some(database) = &target.database {
            upstream_args["database"] = json!(database);
        }
        let started = Instant::now();
        let outcome = manager.call(&call, &upstream_args).await;
        let local_elapsed_ms = started.elapsed().as_millis() as i64;
        let result = match outcome {
            Ok((value, _hash)) => match upstream_payload(&value) {
                Ok(payload) => payload,
                Err(e) => {
                    let audit = audit_error(
                        audit_data(
                            audit_base("template", template_id, "invoke", "error"),
                            json!({
                                "arguments": args,
                                "executed_script": rendered.sql,
                                "bound_params": params,
                                "cache": "miss",
                                "elapsed_ms": local_elapsed_ms,
                                "version": version,
                                "upstream_id": call.upstream_id,
                                "upstream_tool": call.tool
                            }),
                        ),
                        &e,
                    );
                    if let Err(audit_err) = self.service.audit_event("template_run", audit, actor) {
                        return err_tool(id, &audit_err);
                    }
                    return err_tool(id, &e);
                }
            },
            Err(failure) => {
                let e = McpError::with_message(
                    &failure.code,
                    failure.message,
                    json!({"upstream_id": failure.upstream_id, "tool": call.tool, "outcome": failure.outcome}),
                );
                // 失败口径沿用上游给的 outcome（超时/传输中断可能是 unknown，不能当确定失败重试）。
                let audit = audit_error(
                    audit_data(
                        audit_base("template", template_id, "invoke", failure.outcome),
                        json!({
                            "arguments": args,
                            "executed_script": rendered.sql,
                            "bound_params": params,
                            "cache": "miss",
                            "elapsed_ms": local_elapsed_ms,
                            "version": version,
                            "upstream_id": call.upstream_id,
                            "upstream_tool": call.tool
                        }),
                    ),
                    &e,
                );
                if let Err(audit_err) = self.service.audit_event("template_run", audit, actor) {
                    return err_tool(id, &audit_err);
                }
                return err_tool(id, &e);
            }
        };
        // 上游自己的 elapsed_ms 更接近真实执行耗时；缺失时退回本地调用耗时。
        let elapsed_ms = result
            .get("elapsed_ms")
            .and_then(Value::as_i64)
            .unwrap_or(local_elapsed_ms);

        // 8. 结果预算：单次结果超 `max_result_tokens` 按溢出策略失败或降级为摘要。
        let constraints = &record.compiled.def.constraints;
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
            if self.service.overflow_policy() == OverflowPolicy::Fail {
                let payload = audit_data(
                    audit_base("template", template_id, "invoke", "error"),
                    json!({
                        "arguments": args,
                        "upstream_id": call.upstream_id,
                        "upstream_tool": call.tool,
                        "executed_script": rendered.sql,
                        "bound_params": params,
                        "cache": "miss",
                        "elapsed_ms": elapsed_ms,
                        "tokens": tokens,
                        "version": version
                    }),
                );
                if let Err(e) =
                    self.service
                        .audit_event("template_run", audit_error(payload, &over), actor)
                {
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
                audit_data(
                    audit_base("template", template_id, "invoke", "summary"),
                    json!({
                        "arguments": args,
                        "upstream_id": call.upstream_id,
                        "upstream_tool": call.tool,
                        "executed_script": rendered.sql,
                        "bound_params": params,
                        "cache": "miss",
                        "elapsed_ms": elapsed_ms,
                        "tokens": tokens,
                        "row_count": row_count,
                        "truncated": true,
                        "version": version
                    }),
                ),
                actor,
            ) {
                return err_tool(id, &e);
            }
            return tool_result(
                id,
                json!({"content": summarized, "cache": "miss", "version": version}),
                false,
            );
        }

        // 9. 记账 → 审计 → 回填模板元信息 → 写结果缓存。
        let budget = match self.service.record_result(&key, tokens) {
            Ok(b) => b,
            Err(e) => return err_tool(id, &e),
        };
        if let Err(e) = self.service.audit_event(
            "template_run",
            audit_data(
                audit_base("template", template_id, "invoke", "ok"),
                json!({
                    "arguments": args,
                    "upstream_id": call.upstream_id,
                    "upstream_tool": call.tool,
                    "executed_script": rendered.sql,
                    "bound_params": params,
                    "cache": "miss",
                    "elapsed_ms": elapsed_ms,
                    "tokens": tokens,
                    "row_count": row_count,
                    "truncated": truncated,
                    "version": version
                }),
            ),
            actor,
        ) {
            return err_tool(id, &e);
        }
        if budget.summary {
            return tool_result(
                id,
                json!({
                    "content": json!({"row_count": row_count, "summary": true, "template_id": template_id, "version": version}),
                    "cache": "miss",
                    "version": version
                }),
                false,
            );
        }
        let mut content = result;
        if let Some(obj) = content.as_object_mut() {
            obj.insert("template_id".into(), json!(template_id));
            obj.insert("version".into(), json!(version));
            obj.insert("cache".into(), json!("miss"));
            if let Some(variant) = &rendered.variant_id {
                obj.insert("variant".into(), json!(variant));
            }
            obj.insert("summary".into(), json!(budget.summary));
            // 命中路径没有执行结果可比对：把本次记账的 token 数一起缓存，
            // 否则同一份结果在"miss 时估算、hit 时重新估算"下会给出两个数。
            obj.insert("tokens".into(), json!(tokens));
        }
        registry.cache_store(
            template_id,
            version,
            &rendered.params_hash,
            content.clone(),
            record.compiled.def.result_cache_ttl_s,
        );
        tool_result(
            id,
            json!({"content": content, "cache": "miss", "version": version}),
            false,
        )
    }
}

/// `db_run_template` 早期错误的审计数据：模板还没 render，没有脚本，只有原始入参。
fn template_audit_context(args: &Value) -> Value {
    let mut context = json!({
        "arguments": args,
        "bound_params": args.get("params").cloned().unwrap_or_else(|| json!({})),
    });
    if let Some(template_id) = args.get("template_id").and_then(|v| v.as_str()) {
        context["template_id"] = json!(template_id);
    }
    if let Some(version) = args.get("version") {
        context["version"] = version.clone();
    }
    context
}

/// 上游查询结果的正文：`structuredContent` 优先，其次解析首个 content 文本里的 JSON。
///
/// 上游按 MCP 规范可以只用文本承载结果（内置 clickhouse 包两种都给），模板契约要求
/// `row_count`/`rows` 这类结构化字段，拿不到就无法兑现响应形状，只能明确失败。
fn upstream_payload(result: &Value) -> Result<Value, McpError> {
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        return Err(McpError::new(
            "MCP_UPSTREAM_ERROR",
            json!({"hint": "上游工具以 isError 标记了失败", "result": result}),
        ));
    }
    if let Some(structured) = result
        .get("structuredContent")
        .filter(|v| v.is_object())
        .cloned()
    {
        return Ok(structured);
    }
    let text = result
        .get("content")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|item| item.get("text"))
        .and_then(Value::as_str);
    match text.and_then(|t| serde_json::from_str::<Value>(t).ok()) {
        Some(parsed) if parsed.is_object() => Ok(parsed),
        _ => Err(McpError::new(
            "MCP_UPSTREAM_ERROR",
            json!({"hint": "上游未返回结构化查询结果（缺 structuredContent 且首个 content 文本不是 JSON 对象）"}),
        )),
    }
}
