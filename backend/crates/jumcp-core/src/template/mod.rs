//! 模板能力：定义、注册期门禁、运行期渲染与注册表。
//!
//! 边界：
//! - `definition`：定义结构与注册期字段校验；
//! - `render`：结构变体白名单、命名占位符改写、sqlguard 门禁、参数绑定与渲染；
//! - `registry`：生命周期（复用 `CapabilityStore`）、版本钉住与结果缓存。
//!
//! 只允许三类结构变体：可选过滤、`{{col:enum}}` 白名单标识符、显式 variants 矩阵；
//! 其它 `{{ }}` 与 Jinja 表达式一律注册即拒——模板不是模板引擎。命名占位符在注册时
//! 一次性改写为位置占位符（ClickHouse `?` / PostgreSQL `$n`），运行期只做绑定；改写映射
//! 随定义入库，审计与错误定位据此还原参数名。校验门禁与注销共用同一个 AST 校验器
//! （jumcp-sqlguard）：渲染后必须仍是单条 SELECT/WITH。

mod definition;
mod registry;
mod render;

pub use definition::{
    CompiledTemplate, CompiledVariant, Constraints, PARAM_TYPES, ParamSpec, Preflight,
    PreflightReceipt, TemplateDef, Variant,
};
pub use registry::{TemplateRecord, TemplateRegistry};
pub use render::{PreparedQuery, compile, compile_with_dialect};

use crate::errors::McpError;
use serde_json::{Value, json};

/// 统一错误构造：`MCP_TEMPLATE_INVALID` + 规则名 + 详情（可并入调用方给出的结构化上下文）。
fn template_error(rule: &str, message: &str, detail: Value) -> McpError {
    let mut data = json!({"rule": rule, "message": message});
    if let (Some(obj), Some(extra)) = (data.as_object_mut(), detail.as_object()) {
        for (k, v) in extra {
            obj.insert(k.clone(), v.clone());
        }
    }
    McpError::with_message("MCP_TEMPLATE_INVALID", message.to_string(), data)
}

#[cfg(test)]
mod tests;
