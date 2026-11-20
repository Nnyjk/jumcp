//! `db_run_template` 执行臂：版本钉住 → 参数渲染 → 结果缓存 → 执行 → 结果预算 → 审计。
//!
//! 这是"模板"唯一的内置执行入口：agent 不能自由拼 SQL，只能调用注册期已过 sqlguard 的
//! 固定查询路径。可见性只决定"谁能看见"，调用面判定是 `published ∧ callable ∧ group ∈ scope`。

use super::*;
use crate::executor::QueryParams;
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
        // 4. 参数渲染（缺失/类型/枚举/column_ref 白名单在注册期已校验，这里只做取值）。
        let prepared = match record.compiled.render(&params) {
            Ok(p) => p,
            Err(e) => return self.reject_tool_call(id, actor, &e, "db_run_template", context),
        };
        // 5. 结果缓存：命中即不再执行（同 session 同参数连调两次只有第一次真跑）。
        if let Some(cached) = registry.cache_lookup(template_id, version, &prepared.params_hash) {
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
                        "executed_script": prepared.sql,
                        "bound_params": prepared.values,
                        "cache": "hit",
                        // 口径是"执行查询耗时"：命中路径没有执行，所以恒为 0，不是漏测。
                        "elapsed_ms": 0,
                        "tokens": cached_tokens,
                        "version": version
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

        // 6. 执行：模板 schema 只能收窄连接白名单，不能扩大（判定与自由查询共用一份）。
        let constraints = &record.compiled.def.constraints;
        let schemas = match jumcp_core::schemas::narrow_schemas(
            &self.service.connection_schemas(),
            &constraints.allowed_schemas,
        ) {
            Ok(s) => s,
            Err(e) => {
                return self.reject_tool_call(id, actor, &e, "db_run_template", context);
            }
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
        if let Err(e) = self.service.executor.validate(&q) {
            return self.reject_tool_call(id, actor, &e, "db_run_template", context);
        }
        let started = Instant::now();
        let outcome = self.service.executor.execute(scope, &q).await;
        let elapsed_ms = started.elapsed().as_millis() as i64;
        let result = match outcome {
            Ok(result) => result,
            Err(e) => {
                let outcome_label = match e.data.get("outcome").and_then(|v| v.as_str()) {
                    Some("unknown") => "unknown",
                    _ => "error",
                };
                let payload = audit_data(
                    audit_base("template", template_id, "invoke", outcome_label),
                    json!({
                        "arguments": args,
                        "executed_script": prepared.sql,
                        "bound_params": prepared.values,
                        "cache": "miss",
                        "elapsed_ms": elapsed_ms,
                        "version": version
                    }),
                );
                if let Err(audit) =
                    self.service
                        .audit_event("template_run", audit_error(payload, &e), actor)
                {
                    return err_tool(id, &audit);
                }
                return err_tool(id, &e);
            }
        };

        // 7. 结果预算：单次结果超 `max_result_tokens` 按溢出策略失败或降级为摘要。
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
                        "executed_script": prepared.sql,
                        "bound_params": prepared.values,
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
                        "executed_script": prepared.sql,
                        "bound_params": prepared.values,
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

        // 8. 记账 → 审计 → 回填模板元信息 → 写结果缓存。
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
                    "executed_script": prepared.sql,
                    "bound_params": prepared.values,
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
            if let Some(variant) = &prepared.variant_id {
                obj.insert("variant".into(), json!(variant));
            }
            obj.insert("summary".into(), json!(budget.summary));
        }
        registry.cache_store(
            template_id,
            version,
            &prepared.params_hash,
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
