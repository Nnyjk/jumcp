//! 启动期模板预置：从部署配置装载模板定义，走同一条"注册 → 预检 → 提审 → 发布 → 开放"链。
//!
//! 为什么由部署文件携带 approval 字段：审批主体属于治理记录，配置里显式写明提交人/批准人，
//! 服务端只做四眼校验（同一人提交+批准即拒），不允许"启动即自动发布"的隐式路径。

use hjzl_core::errors::McpError;
use hjzl_core::template::TemplateRegistry;
use serde_json::{Value, json};

/// 一条预置记录：定义 + 治理字段（approval_id/submitted_by/approved_by）。
#[derive(Debug, Clone)]
pub struct TemplateSeed {
    pub definition: Value,
    pub approval_id: String,
    pub submitted_by: String,
    pub approved_by: String,
    pub visibility_approval_id: String,
}

/// 读取模板文件（JSON：`{"templates":[{...}]}`）；文件缺失即报错，不静默当空。
pub fn load_template_seeds(path: &str) -> Result<Vec<TemplateSeed>, McpError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| McpError::config(format!("读取模板文件 {path} 失败: {e}")))?;
    let root: Value = serde_json::from_str(&text)
        .map_err(|e| McpError::config(format!("模板文件 {path} 不是合法 JSON: {e}")))?;
    let items = root
        .get("templates")
        .and_then(|t| t.as_array())
        .ok_or_else(|| McpError::config(format!("模板文件 {path} 缺少 templates 数组")))?;
    let mut seeds = Vec::with_capacity(items.len());
    for (idx, item) in items.iter().enumerate() {
        let definition = item.get("definition").cloned().ok_or_else(|| {
            McpError::config(format!("模板文件 {path} 第 {idx} 项缺少 definition"))
        })?;
        let field = |name: &str| -> Result<String, McpError> {
            item.get(name)
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from)
                .ok_or_else(|| McpError::config(format!("模板文件 {path} 第 {idx} 项缺少 {name}")))
        };
        seeds.push(TemplateSeed {
            definition,
            approval_id: field("approval_id")?,
            submitted_by: field("submitted_by")?,
            approved_by: field("approved_by")?,
            visibility_approval_id: item
                .get("visibility_approval_id")
                .and_then(|v| v.as_str())
                .unwrap_or("ap-visibility")
                .to_string(),
        });
    }
    Ok(seeds)
}

/// 预置发布：注册草案 → （调用方先做 EXPLAIN 预检）→ 提审 → 发布 → 开放。
///
/// `preflight` 由调用方在执行 EXPLAIN 前拿到编译产物；这里把它作为参数传入，避免二次编译。
pub fn publish_seed(
    registry: &TemplateRegistry,
    seed: &TemplateSeed,
    compiled: &hjzl_core::template::CompiledTemplate,
) -> Result<(), McpError> {
    let id = compiled.def.template_id.clone();
    // 只有"已发布"才算预置完成：draft/review 必须继续走完审批，否则启动就把半成品当成品。
    if let Ok(current) = registry.current(&id)
        && current.status == "published"
    {
        return Ok(());
    }
    registry.submit_for_review(&id)?;
    registry.publish(
        &id,
        &seed.approval_id,
        &seed.submitted_by,
        &seed.approved_by,
    )?;
    registry.set_visibility(
        &id,
        Some(compiled.def.discoverable),
        Some(compiled.def.callable),
        &seed.visibility_approval_id,
    )?;
    Ok(())
}

/// 预置流程的共享入口：注册（编译门禁）→ 返回编译产物供调用方跑 EXPLAIN。
pub fn register_seed(
    registry: &TemplateRegistry,
    seed: &TemplateSeed,
) -> Result<hjzl_core::template::CompiledTemplate, McpError> {
    registry.register(&seed.definition).map_err(|e| {
        McpError::with_message(
            &e.code,
            format!("模板 {} 未通过注册门禁: {}", seed.approval_id, e.message),
            json!({"approval_id": seed.approval_id, "detail": e.data}),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use hjzl_core::registry::CapabilityStore;
    use std::sync::Arc;

    fn seed_value(sql: &str) -> Value {
        json!({
            "template_id": "order.daily_summary",
            "group": "db.read.analytics",
            "risk": "read",
            "description": "按日汇总",
            "sql": sql,
            "params": [{"name":"day","type":"date","required":true,"nullable":false}],
            "constraints": {"allowed_schemas": ["public"]},
            "discoverable": true,
            "callable": true
        })
    }

    fn write_file(name: &str, content: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn seeds_load_and_publish() {
        let path = write_file(
            "hjzl-seed-ok.json",
            &json!({"templates":[{
                "definition": seed_value("SELECT 1 FROM public.orders WHERE day = :day"),
                "approval_id": "ap-1",
                "submitted_by": "alice",
                "approved_by": "bob"
            }]})
            .to_string(),
        );
        let seeds = load_template_seeds(path.to_str().unwrap()).unwrap();
        let store = Arc::new(CapabilityStore::open(":memory:", None).unwrap());
        let registry = TemplateRegistry::new(store);
        let compiled = register_seed(&registry, &seeds[0]).unwrap();
        publish_seed(&registry, &seeds[0], &compiled).unwrap();
        let current = registry.current("order.daily_summary").unwrap();
        assert!(current.callable && current.discoverable);
        assert_eq!(current.version, 1);
        // 幂等：再次预置不报错、不产生新版本。
        publish_seed(&registry, &seeds[0], &compiled).unwrap();
        assert_eq!(registry.current("order.daily_summary").unwrap().version, 1);
    }

    #[test]
    fn missing_file_is_explicit_error() {
        let e = load_template_seeds("definitely-not-here.json").unwrap_err();
        assert_eq!(e.code, "MCP_CONFIG_ERROR");
    }

    #[test]
    fn same_person_approval_rejected() {
        let path = write_file(
            "hjzl-seed-same.json",
            &json!({"templates":[{
                "definition": seed_value("SELECT 1 FROM public.orders WHERE day = :day"),
                "approval_id": "ap-1",
                "submitted_by": "alice",
                "approved_by": "alice"
            }]})
            .to_string(),
        );
        let seeds = load_template_seeds(path.to_str().unwrap()).unwrap();
        let store = Arc::new(CapabilityStore::open(":memory:", None).unwrap());
        let registry = TemplateRegistry::new(store);
        let compiled = register_seed(&registry, &seeds[0]).unwrap();
        let e = publish_seed(&registry, &seeds[0], &compiled).unwrap_err();
        assert_eq!(e.code, "MCP_APPROVAL_REQUIRED");
    }
}
