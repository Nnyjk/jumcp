//! 内置 MCP 插件契约：目录（package/字段/工具）与调用入口的唯一来源。
//!
//! core 不认识任何具体数据源：插件 crate 实现 [`BuiltinMcp`]，server 侧显式注册后
//! 通过 `POST /internal/mcp/{package_id}/{upstream_id}` 分发。这里只依赖 serde_json 与 std，
//! 使插件不被迫引入路由/存储/HTTP 依赖。

use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;

/// 配置字段的取值形态：决定表单控件；`Password` 只是渲染提示，落库与否看 `secret`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    Password,
}

impl FieldKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FieldKind::Text => "text",
            FieldKind::Password => "password",
        }
    }
}

/// 一个可配置字段：`key` 是 API/UI 的稳定标识，`env` 是落库到上游 env 的键名。
#[derive(Debug, Clone, Copy)]
pub struct BuiltinField {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: FieldKind,
    pub required: bool,
    pub secret: bool,
    pub default: Option<&'static str>,
    pub placeholder: Option<&'static str>,
    pub help: Option<&'static str>,
    pub env: &'static str,
}

/// 目录展示用的工具摘要（完整定义见 [`BuiltinToolDef`]）。
#[derive(Debug, Clone, Copy)]
pub struct BuiltinTool {
    pub name: &'static str,
    pub summary: &'static str,
}

/// 内置包元数据。`id` 同时是 install URL 与 catalog_id 的一部分，发布后不应改动。
#[derive(Debug, Clone, Copy)]
pub struct BuiltinPackage {
    pub id: &'static str,
    pub name: &'static str,
    pub summary: &'static str,
    pub version: &'static str,
    pub tags: &'static [&'static str],
    pub default_upstream_id: &'static str,
    pub fields: &'static [BuiltinField],
    pub tools: &'static [BuiltinTool],
}

/// 一次调用可读取的配置：按字段 key 取值，值已解密。
///
/// 为什么按 key 而不是按 env：插件不应该知道落库字段名之外的东西；
/// Debug 只打印 key 集合，避免明文密码随错误分支/日志外泄。
#[derive(Clone)]
pub struct BuiltinConfig {
    values: BTreeMap<String, String>,
}

impl BuiltinConfig {
    pub fn new(values: BTreeMap<String, String>) -> Self {
        Self { values }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }
}

impl fmt::Debug for BuiltinConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let keys: Vec<&str> = self.values.keys().map(String::as_str).collect();
        f.debug_tuple("BuiltinConfig").field(&keys).finish()
    }
}

/// MCP `tools/list` 的单项定义（`input_schema` 原样下发给客户端）。
#[derive(Debug, Clone)]
pub struct BuiltinToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// 插件返回给 MCP 客户端的错误；`code` 是稳定的 `MCP_*` 码。
#[derive(Debug, Clone)]
pub struct BuiltinError {
    pub code: String,
    pub message: String,
    pub data: Value,
}

impl BuiltinError {
    pub fn new(code: impl Into<String>, message: impl Into<String>, data: Value) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            data,
        }
    }
}

impl fmt::Display for BuiltinError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for BuiltinError {}

/// 插件返回类型：`Pin<Box<dyn Future>>` 让 trait 保持 dyn 兼容且不必引入 async-trait。
pub type BuiltinFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// 内置 MCP 插件：目录元数据、工具清单与调用逻辑。
///
/// `call` 返回的是 MCP `tools/call` 的 `result` 对象（成功含 content+structuredContent，
/// 失败含 isError:true 且 content[0].text 为稳定错误码），服务器不再二次包装。
pub trait BuiltinMcp: Send + Sync {
    fn package(&self) -> &'static BuiltinPackage;

    fn tools(&self) -> Vec<BuiltinToolDef>;

    fn call<'a>(
        &'a self,
        config: &'a BuiltinConfig,
        tool: &'a str,
        arguments: &'a Value,
    ) -> BuiltinFuture<'a, Result<Value, BuiltinError>>;
}
