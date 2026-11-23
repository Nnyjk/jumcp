//! 维护通道（`admin_*`）的授权、前置检查与六个写/读工具的 dispatch 实现。

use super::*;

impl McpServer {
    /// admin 工具的统一授权：只看服务端 scope，不看请求参数与客户端清单。
    ///
    /// 非 admin 组直接与"不存在"同构（`MCP_CAPABILITY_NOT_FOUND`），并写 policy_decision——
    /// 探测维护通道本身就是一个需要被审计的事件。
    pub(super) fn authorize_admin(
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
    /// 为什么在这里统一写：6 个维护工具走同一分支，逐处写必有一处遗漏——而管理台的
    /// 工具台账只认 `tool_call`，漏写就等于该工具永远 calls=0/health=unknown。
    /// 拒绝路径（未授权、审计缺失、builtin 不可写、参数非法）由调用方一并交进来，记
    /// `outcome=error` + `mcp_code`；成功则用 `estimate_tokens(结果)` 记 tokens。
    pub(super) fn finish_admin_call(
        &self,
        id: &Value,
        tool: &str,
        args: &Value,
        result: Value,
        actor: &Actor,
    ) -> Value {
        // 结构化结果在 `structuredContent`：`content` 已是 content block 数组，不能再当
        // 对象取 `error`/估算 tokens。错误路径也带 `structuredContent`，所以这里两种
        // outcome 读同一处，台账口径不变。
        let structured = result
            .get("result")
            .and_then(|r| r.get("structuredContent"))
            .unwrap_or(&result);
        let payload = match structured.get("error") {
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
                json!({"arguments": args, "tokens": estimate_tokens(structured)}),
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
    /// 只产草案：定义一旦落库就不再由工具改动；放行（`review → published`）走
    /// `admin_publish_capability`（agent 自审）或 HTTP 管理面（外部审批），两条路都不碰定义。
    pub(super) fn admin_register_template(&self, id: &Value, args: &Value, actor: &Actor) -> Value {
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
        tool_result(
            id,
            json!({
                "id": record.capability_id,
                "status": record.status,
                "version": record.version,
                "definition_hash": record.definition_hash
            }),
            false,
        )
    }

    /// `admin_review_queue`：只读审核队列（draft/review）。
    pub(super) fn admin_review_queue(&self, id: &Value, actor: &Actor) -> Value {
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
        tool_result(id, json!({"items": items, "total": items.len()}), false)
    }

    /// `admin_audit_queries`：只读审计查询，limit 夹紧在 1..=200（与 HTTP 管理面同一上限）。
    pub(super) fn admin_audit_queries(&self, id: &Value, args: &Value, actor: &Actor) -> Value {
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
        tool_result(
            id,
            json!({
                "events": events,
                "total": total,
                "limit": limit,
                "offset": offset,
                "order": if order == jumcp_core::audit::Order::Asc { "asc" } else { "desc" }
            }),
            false,
        )
    }

    /// `admin_tool_candidates`：定制化 tool 候选加权评分（只读）。
    ///
    /// 与 HTTP `GET /admin/tools/candidates` 共用 `crate::admin::tool_candidates_report`：
    /// 审计 agent 在会话里看到的排名必须与看板逐字一致，否则"为什么推荐这个"无从对照。
    /// `window_days` 缺省 7（与统计台账同口径），窗口与条数的夹紧在报告层完成。
    pub(super) fn admin_tool_candidates(&self, id: &Value, args: &Value, actor: &Actor) -> Value {
        const TOOL: &str = "admin_tool_candidates";
        let Some(log) = self.service.audit.as_ref() else {
            return err_tool(
                id,
                &McpError::new("MCP_AUDIT_ERROR", json!({"hint": "审计未启用"})),
            );
        };
        let window_days = args
            .get("window_days")
            .and_then(Value::as_i64)
            .unwrap_or(crate::admin::STATS_DEFAULT_WINDOW_DAYS);
        // 默认 50 而不是 HTTP 的 200：候选带着理由文本，整表塞进会话没有价值。
        let limit = args.get("limit").and_then(Value::as_i64).unwrap_or(50);
        match crate::admin::tool_candidates_report(log, window_days, limit) {
            Ok(report) => tool_result(id, report, false),
            Err(e) => self.reject(id, actor, &e, json!({"tool": TOOL})),
        }
    }

    /// `admin_set_visibility`：只改双布尔，不改定义；`admin_change` 由 store 写入
    /// （before/after 同为当前 definition_hash，diff 记字段变更）。
    pub(super) fn admin_set_visibility(&self, id: &Value, args: &Value, actor: &Actor) -> Value {
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
        let record =
            match self.write_visibility(id, actor, TOOL, capability_id, discoverable, callable) {
                Ok(r) => r,
                Err(denied) => return denied,
            };
        tool_result(
            id,
            json!({
                "id": record.capability_id,
                "discoverable": record.discoverable,
                "callable": record.callable,
                "version": record.version,
                "last_changed_at": record.updated_at
            }),
            false,
        )
    }

    /// `discoverable`/`callable` 写入的唯一实现：`admin_set_visibility` 与
    /// `admin_publish_capability` 共用，避免两处各写一套校验。
    ///
    /// 两个参数都缺省时拒绝（无变更的可见性写入不产生审计价值）；`store.set_visibility`
    /// 只落真实变化，无变化不写 `admin_change`。返回拒绝载荷而不是 `McpError`：
    /// 调用点直接 `return`，错误上下文里的 `tool` 由调用方决定。
    fn write_visibility(
        &self,
        id: &Value,
        actor: &Actor,
        tool: &str,
        capability_id: &str,
        discoverable: Option<bool>,
        callable: Option<bool>,
    ) -> Result<jumcp_core::registry::CapabilityRecord, Value> {
        if discoverable.is_none() && callable.is_none() {
            return Err(self.reject(
                id,
                actor,
                &McpError::validation(
                    "discoverable/callable 至少要指定一个（无变更的可见性写入不产生审计价值）"
                        .to_string(),
                ),
                json!({"tool": tool, "capability_id": capability_id}),
            ));
        }
        let store = match self.require_store() {
            Ok(s) => s,
            Err(e) => return Err(self.reject(id, actor, &e, json!({"tool": tool}))),
        };
        store
            .set_visibility(capability_id, discoverable, callable, actor)
            .map_err(|e| {
                self.reject(
                    id,
                    actor,
                    &e,
                    json!({"tool": tool, "capability_id": capability_id}),
                )
            })
    }

    /// 最近一次 `publish` 审计里的 `approval` 载荷：幂等返回时如实回放"是谁批的"，
    /// 而不是把人工发布也标成 agent 自审。只投影契约里的三个字段，与首次发布的响应同形
    /// （`approved_at`/`submitted_by` 等留在审计原文里）。
    fn last_publish_approval(
        &self,
        id: &Value,
        actor: &Actor,
        tool: &str,
        capability_id: &str,
    ) -> Result<Value, Value> {
        let Some(audit) = self.service.audit.as_deref() else {
            return Ok(Value::Null);
        };
        let events = audit
            .query(Some(
                &json!({"event_type": "admin_change", "object_id": capability_id}),
            ))
            .map_err(|e| {
                self.reject(
                    id,
                    actor,
                    &e,
                    json!({"tool": tool, "capability_id": capability_id}),
                )
            })?;
        let found = events
            .iter()
            .rev()
            .find(|e| e.payload.get("action").and_then(Value::as_str) == Some("publish"))
            .and_then(|e| e.payload.get("approval"))
            .and_then(Value::as_object);
        Ok(match found {
            Some(approval) => json!({
                "id": approval.get("id").cloned().unwrap_or(Value::Null),
                "source": approval.get("source").cloned().unwrap_or(Value::Null),
                "verified": approval.get("verified").cloned().unwrap_or(Value::Null),
            }),
            None => Value::Null,
        })
    }

    /// `admin_publish_capability`：agent 自审放行（`draft → review → published`）。
    ///
    /// 与 HTTP 管理面 `POST /admin/capabilities/{id}/publish` 的分工：人发布必须带外部单号
    /// （留痕 `source="external"`、`verified=false`），agent 发布的批准人来自已鉴权 token
    /// （`source="agent"`、`verified=true`），单号缺省由服务端生成
    /// `agent:<actor_id>:<rfc3339>`。两条路都只推进状态、不碰定义——改定义仍然只有
    /// `admin_register_template` 产新草案这一条路。
    ///
    /// 状态语义：`draft` 先按人工流程 `submit_for_review` 提审（提审人 = 当前 agent）再发布；
    /// `review` 直接发布；`published` 幂等返回（不重复盖章、不重复写 publish 审计，
    /// 但允许本次给可见性）；`deprecated` 拒绝。
    pub(super) fn admin_publish_capability(
        &self,
        id: &Value,
        args: &Value,
        actor: &Actor,
    ) -> Value {
        const TOOL: &str = "admin_publish_capability";
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
        let store = match self.require_store() {
            Ok(s) => s,
            Err(e) => return self.reject(id, actor, &e, json!({"tool": TOOL})),
        };
        let before = match store.get(capability_id) {
            Ok(Some(record)) => record,
            Ok(None) => {
                return self.reject(
                    id,
                    actor,
                    &McpError::new(
                        "MCP_CAPABILITY_NOT_FOUND",
                        json!({"capability_id": capability_id}),
                    ),
                    json!({"tool": TOOL, "capability_id": capability_id}),
                );
            }
            Err(e) => {
                return self.reject(
                    id,
                    actor,
                    &e,
                    json!({"tool": TOOL, "capability_id": capability_id}),
                );
            }
        };
        let status_before = before.status.clone();
        if status_before == "deprecated" {
            return self.reject(
                id,
                actor,
                &McpError::new(
                    "MCP_STATE_TRANSITION_INVALID",
                    json!({"capability_id": capability_id, "from": status_before, "to": "published"}),
                ),
                json!({"tool": TOOL, "capability_id": capability_id}),
            );
        }
        let (published, approval) = if status_before == "published" {
            let approval = match self.last_publish_approval(id, actor, TOOL, capability_id) {
                Ok(v) => v,
                Err(denied) => return denied,
            };
            (before, approval)
        } else {
            if status_before == "draft" {
                let submitted = match store.submit_for_review(capability_id, &actor.actor_id) {
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
                // 提审也是治理变更：与 HTTP submit 同款补记，否则审计里会凭空出现一个
                // review 状态（registry 本身不写 submit 审计）。
                let mut payload = audit_base("capability", capability_id, "submit", "ok");
                payload.insert("from_status".into(), json!(status_before));
                payload.insert("to_status".into(), json!(submitted.status));
                payload.insert(
                    "changes".into(),
                    json!([jumcp_core::registry::change(
                        "status",
                        json!(status_before),
                        json!(submitted.status)
                    )]),
                );
                payload.insert(
                    "data".into(),
                    json!({
                        "submitted_by": actor.actor_id,
                        "version": submitted.version,
                        "definition_hash_before": submitted.definition_hash,
                        "definition_hash_after": submitted.definition_hash
                    }),
                );
                if let Err(e) =
                    self.service
                        .audit_event("admin_change", Value::Object(payload), actor)
                {
                    return self.reject(
                        id,
                        actor,
                        &e,
                        json!({"tool": TOOL, "capability_id": capability_id}),
                    );
                }
            }
            // 单号只在这里生成：核心层不做兜底，避免两个地方都能造 id。
            let approval_id = args
                .get("approval_id")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    format!(
                        "agent:{}:{}",
                        actor.actor_id,
                        chrono::Utc::now().to_rfc3339()
                    )
                });
            match store.publish_by_agent(capability_id, &approval_id, actor) {
                Ok(record) => (
                    record,
                    json!({"id": approval_id, "source": "agent", "verified": true}),
                ),
                Err(e) => {
                    return self.reject(
                        id,
                        actor,
                        &e,
                        json!({"tool": TOOL, "capability_id": capability_id}),
                    );
                }
            }
        };
        // 发布与可见性同一次调用完成：可见性走既有写路径（真实变化才写 admin_change）。
        let record = if discoverable.is_some() || callable.is_some() {
            match self.write_visibility(id, actor, TOOL, capability_id, discoverable, callable) {
                Ok(r) => r,
                Err(denied) => return denied,
            }
        } else {
            published
        };
        tool_result(
            id,
            json!({
                "id": record.capability_id,
                "status": record.status,
                "version": record.version,
                "definition_hash": record.definition_hash,
                "discoverable": record.discoverable,
                "callable": record.callable,
                "approval": approval
            }),
            false,
        )
    }
}
