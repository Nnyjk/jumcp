//! 手写 JSON-RPC MCP 服务：tools/list（按 EffectiveScope 过滤 + 常驻预算）+ 元工具三件套
//! + 维护通道工具 + 上游工具转发。数据取数只走模板与上游链路，内置 basic 组不直连数据源。
//!
//! 为什么手写：rmcp 的 API 随版本漂移，手写 JSON-RPC 让 stdio 与 HTTP 共用同一套判定与审计。
//!
//! 本文件放清单与投影（工具定义、注解常量、职责分类）；scope 与分发在 `dispatch`，传输在 `http`。

use crate::catalog::Capability;
use serde_json::{Value, json};
use std::sync::OnceLock;

mod dispatch;
mod http;

pub use dispatch::McpServer;
pub use http::http_router;

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

/// 维护通道的内置工具（`group="admin"`）：只在 `scope.groups` 含 admin 时出现在
/// `tools/list` 与发现面，调用前还要再判一次（不依赖客户端是否看过清单）。
///
/// 边界：只能改能力定义（只产 draft）/ 可见性，**不提供发布或审批工具**——
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
