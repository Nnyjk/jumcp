//! 注册表：生命周期复用 `CapabilityStore`（kind=template），本层只加编译、版本钉住与结果缓存。

use super::definition::{CompiledTemplate, PreflightReceipt, TemplateDef};
use super::render::compile_with_dialect;
use super::template_error;
use crate::audit::IntoAuditOrigin;
use crate::canonical::sha256_hex;
use crate::errors::McpError;
use crate::registry::{CapabilityRecord, CapabilityStore};
use jumcp_sqlguard::Dialect;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 运行期可见的模板记录（已编译）。
#[derive(Debug, Clone)]
pub struct TemplateRecord {
    pub template_id: String,
    pub version: i64,
    pub group: String,
    pub status: String,
    pub discoverable: bool,
    pub callable: bool,
    pub successor_id: String,
    pub compiled: CompiledTemplate,
}

impl TemplateRecord {
    fn from_capability(rec: &CapabilityRecord, dialect: Dialect) -> Result<Self, McpError> {
        let compiled = CompiledTemplate::from_definition(&rec.definition, dialect)?;
        Ok(Self {
            template_id: rec.capability_id.clone(),
            version: rec.version,
            group: rec.group.clone(),
            status: rec.status.clone(),
            discoverable: rec.discoverable,
            callable: rec.callable,
            successor_id: rec.successor_id.clone(),
            compiled,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    template_id: String,
    version: i64,
    params_hash: String,
}

#[derive(Debug, Clone)]
struct CacheEntry {
    value: Value,
    stored_at: Instant,
    ttl: Duration,
}

/// 模板注册表：生命周期复用 CapabilityStore（kind=template），本层只加编译、版本钉住与结果缓存。
pub struct TemplateRegistry {
    store: Arc<CapabilityStore>,
    dialect: Dialect,
    pins: Mutex<HashMap<(String, String), i64>>,
    cache: Mutex<HashMap<CacheKey, CacheEntry>>,
}

impl TemplateRegistry {
    /// 默认按 PostgreSQL 方言编译（既有测试/调用方）；生产入口用 [`with_dialect`] 指定数据源方言。
    pub fn new(store: Arc<CapabilityStore>) -> Self {
        Self::with_dialect(store, Dialect::Postgres)
    }

    pub fn with_dialect(store: Arc<CapabilityStore>, dialect: Dialect) -> Self {
        Self {
            store,
            dialect,
            pins: Mutex::new(HashMap::new()),
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// 底层存储：发现面（`Service::visible_capabilities`）与模板层必须读同一个 store，
    /// 否则管理面发布会只对其中一侧生效。
    pub fn store(&self) -> Arc<CapabilityStore> {
        self.store.clone()
    }

    /// 注册草案（draft）：门禁不过即拒，不落库。
    pub fn register(&self, value: &Value) -> Result<CompiledTemplate, McpError> {
        let compiled = compile_with_dialect(TemplateDef::parse(value)?, self.dialect)?;
        self.store.register(
            &compiled.def.template_id,
            "template",
            &compiled.def.group,
            &compiled.def.risk,
            compiled.to_definition(),
        )?;
        Ok(compiled)
    }

    /// 草案定义修订：与 `publish_version` 共用同一套模板门禁（parse → 编译 → SQL 校验），
    /// 因此管理面改模板定义不可能绕过 SQL 白名单。
    pub fn revise_draft(
        &self,
        id: &str,
        value: &Value,
        approval_id: &str,
        actor: impl IntoAuditOrigin,
    ) -> Result<CompiledTemplate, McpError> {
        let compiled = compile_with_dialect(TemplateDef::parse(value)?, self.dialect)?;
        if compiled.def.template_id != id {
            return Err(template_error(
                "template_id_mismatch",
                "定义与目标 template_id 不一致",
                json!({"expected": id, "actual": compiled.def.template_id}),
            ));
        }
        self.store
            .revise_draft_definition(id, compiled.to_definition(), approval_id, actor)?;
        Ok(compiled)
    }

    /// 预检回执：编译通过后给出"这份定义的指纹"，发布时必须出示同一指纹。
    pub fn preflight(&self, value: &Value) -> Result<PreflightReceipt, McpError> {
        let compiled = compile_with_dialect(TemplateDef::parse(value)?, self.dialect)?;
        Ok(PreflightReceipt {
            template_id: compiled.def.template_id.clone(),
            base_sql_hash: sha256_hex(&compiled.base_sql),
        })
    }

    /// 发布新版本：必须先预检（回执指纹必须与待发布定义一致），审批复用 registry。
    pub fn publish_version(
        &self,
        id: &str,
        value: &Value,
        approval_id: &str,
        submitted_by: &str,
        approved_by: impl IntoAuditOrigin,
        receipt: &PreflightReceipt,
    ) -> Result<CompiledTemplate, McpError> {
        let compiled = compile_with_dialect(TemplateDef::parse(value)?, self.dialect)?;
        if compiled.def.template_id != id || receipt.template_id != id {
            return Err(template_error(
                "template_id_mismatch",
                "定义与目标 template_id 不一致",
                json!({"expected": id, "actual": compiled.def.template_id}),
            ));
        }
        if sha256_hex(&compiled.base_sql) != receipt.base_sql_hash {
            return Err(McpError::new(
                "MCP_PREFLIGHT_REQUIRED",
                json!({"template_id": id, "hint": "预检回执与待发布定义不一致，必须重新预检"}),
            ));
        }
        self.store.publish_version(
            id,
            compiled.to_definition(),
            approval_id,
            submitted_by,
            approved_by,
        )?;
        Ok(compiled)
    }

    pub fn submit_for_review(&self, id: &str, submitted_by: &str) -> Result<(), McpError> {
        self.store.submit_for_review(id, submitted_by).map(|_| ())
    }

    pub fn publish(&self, id: &str, approval_id: &str, approved_by: &str) -> Result<(), McpError> {
        self.store
            .promote_to_published(id, approval_id, approved_by)
            .map(|_| ())
    }

    pub fn deprecate(&self, id: &str, successor_id: &str) -> Result<(), McpError> {
        self.store.deprecate(id, successor_id).map(|_| ())
    }

    pub fn set_visibility(
        &self,
        id: &str,
        discoverable: Option<bool>,
        callable: Option<bool>,
        actor: impl IntoAuditOrigin,
    ) -> Result<(), McpError> {
        self.store
            .set_visibility(id, discoverable, callable, actor)
            .map(|_| ())
    }

    /// 当前版本；不存在 → NOT_FOUND，deprecated → NOT_FOUND + successor_id。
    pub fn current(&self, id: &str) -> Result<TemplateRecord, McpError> {
        let record = self.store.get(id)?.ok_or_else(|| not_found(id, None))?;
        if record.kind != "template" {
            return Err(not_found(id, None));
        }
        if record.status == "deprecated" {
            return Err(not_found(id, Some(record.successor_id.clone())));
        }
        TemplateRecord::from_capability(&record, self.dialect)
    }

    /// 指定历史版本（会话钉住时用；每个版本都保留可查）。
    pub fn at_version(&self, id: &str, version: i64) -> Result<TemplateRecord, McpError> {
        let record = self
            .store
            .get_version(id, version)?
            .ok_or_else(|| not_found(id, None))?;
        if record.kind != "template" {
            return Err(not_found(id, None));
        }
        TemplateRecord::from_capability(&record, self.dialect)
    }

    /// 会话版本钉住：首调建立绑定；显式换版本即冲突。
    pub fn pin(
        &self,
        session_id: &str,
        template_id: &str,
        requested: Option<i64>,
        current: i64,
    ) -> Result<i64, McpError> {
        let mut pins = self.pins.lock().map_err(|_| McpError::lock())?;
        let key = (session_id.to_string(), template_id.to_string());
        if let Some(pinned) = pins.get(&key) {
            if requested.is_some_and(|v| v != *pinned) {
                return Err(McpError::new(
                    "MCP_VERSION_CONFLICT",
                    json!({"template_id": template_id, "pinned": pinned, "requested": requested}),
                ));
            }
            return Ok(*pinned);
        }
        let chosen = requested.unwrap_or(current);
        pins.insert(key, chosen);
        Ok(chosen)
    }

    pub fn cache_lookup(
        &self,
        template_id: &str,
        version: i64,
        params_hash: &str,
    ) -> Option<Value> {
        let cache = self.cache.lock().ok()?;
        let key = CacheKey {
            template_id: template_id.to_string(),
            version,
            params_hash: params_hash.to_string(),
        };
        let entry = cache.get(&key)?;
        if entry.stored_at.elapsed() >= entry.ttl {
            return None;
        }
        Some(entry.value.clone())
    }

    pub fn cache_store(
        &self,
        template_id: &str,
        version: i64,
        params_hash: &str,
        value: Value,
        ttl_s: i64,
    ) {
        if ttl_s <= 0 {
            return;
        }
        let Ok(mut cache) = self.cache.lock() else {
            return;
        };
        cache.insert(
            CacheKey {
                template_id: template_id.to_string(),
                version,
                params_hash: params_hash.to_string(),
            },
            CacheEntry {
                value,
                stored_at: Instant::now(),
                ttl: Duration::from_secs(ttl_s.min(300) as u64),
            },
        );
    }
}

fn not_found(id: &str, successor: Option<String>) -> McpError {
    let mut data = json!({"template_id": id, "hint": "模板不存在或不可见"});
    if let Some(s) = successor
        && !s.is_empty()
        && let Some(obj) = data.as_object_mut()
    {
        obj.insert("successor_id".into(), json!(s));
    }
    McpError::new("MCP_CAPABILITY_NOT_FOUND", data)
}
