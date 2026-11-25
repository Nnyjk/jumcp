//! 上游 MCP router：配置加载、连接池、工具缓存、失败隔离与调用转发。
//!
//! 边界：
//! - 只做聚合：一个 `/mcp` 暴露内置工具 + 所有启用上游的工具，不提供逐上游透传端点。
//! - 命名 `<server_id>__<tool>`，两侧非 `[A-Za-z0-9_-]` → `-`，整名 ≤64；冲突拒绝，不加后缀。
//! - 传输只实现 MCP Streamable HTTP（POST + JSON/SSE 响应 + `Mcp-Session-Id`）；stdio 上游
//!   由部署开关控制，`Command` 直接 spawn 且显式剔除管理/agent token 环境变量。
//! - 惰性连接、空闲回收，不做后台轮询；管理面 `refresh` 是唯一"主动探测"入口。
//!
//! 失败隔离：单个上游不可达只影响它自己——缓存里的旧工具继续可见（状态 `degraded`），
//! 其他上游与内置工具照常；调用失败整形为 MCP `isError` 结果。
//!
//! 连接池、工具缓存与失败隔离留在本文件；两种传输实现在 `http` / `stdio`，共用件在 `client`。

mod client;
mod http;
mod stdio;

use self::client::UpstreamClient;

use crate::builtin::BuiltinEndpoint;
use crate::catalog::{Capability, SOURCE_UPSTREAM};
use crate::discovery;
use jumcp_core::entry::EffectiveScope;
use jumcp_core::errors::McpError;
use jumcp_core::registry::{
    CapabilityStore, EXPOSED_TOOL_NAME_MAX, GroupExposure, McpUpstream, UPSTREAM_NAME_SEP,
    UpstreamTool, is_builtin_id, is_upstream_tool_name,
};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinSet;

/// `tools/list` 缓存有效期：过期后下一次投影重新拉取；管理面写配置会立即失效整条缓存。
const TOOLS_TTL: Duration = Duration::from_secs(30);

/// MCP 协议版本：对外回执与向上游 `initialize` 使用同一常量（当前只支持这一个版本）。
pub const MCP_PROTOCOL_VERSION: &str = "2025-03-26";

/// 一次已解析的上游调用：名字、原始工具与配置指纹。
#[derive(Debug, Clone)]
pub struct UpstreamCall {
    pub upstream_id: String,
    pub definition_hash: String,
    pub tool: String,
    pub exposed: String,
}

/// 上游调用的失败形态：不含 body/凭据，只带可对外展示的原因。
#[derive(Debug, Clone)]
pub struct UpstreamFailure {
    pub upstream_id: String,
    pub code: String,
    pub message: String,
    /// 审计 outcome：超时/结果未知用 `unknown`，其余 `error`。
    pub outcome: &'static str,
    pub definition_hash: String,
}

impl UpstreamFailure {
    fn error(upstream_id: &str, code: &str, message: impl Into<String>) -> Self {
        Self {
            upstream_id: upstream_id.into(),
            code: code.into(),
            message: message.into(),
            outcome: "error",
            definition_hash: String::new(),
        }
    }
    fn unknown(upstream_id: &str, code: &str, message: impl Into<String>) -> Self {
        Self {
            outcome: "unknown",
            ..Self::error(upstream_id, code, message)
        }
    }
    fn with_hash(mut self, hash: &str) -> Self {
        self.definition_hash = hash.to_string();
        self
    }
}

/// 上游运行态（管理面 GET 的 status 字段来源）。
#[derive(Debug, Clone)]
pub struct UpstreamRuntime {
    pub status: &'static str,
    pub status_message: String,
    pub tool_count: i64,
    pub last_checked_at: Option<String>,
}

/// 暴露名规则：`<server_id>__<tool>`，两侧非法字符转 `-`。
///
/// **不截断**：超长名在拉取工具时整台拒绝（见 `conflict_message`），否则旧名回退会拿
/// 截断后的名字去调上游。server id 不含 `__`，因此第一次出现的 `__` 一定是分隔符。
pub fn exposed_tool_name(server_id: &str, tool: &str) -> String {
    let sanitize = |raw: &str| -> String {
        raw.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '-'
                }
            })
            .collect()
    };
    format!(
        "{}{UPSTREAM_NAME_SEP}{}",
        sanitize(server_id),
        sanitize(tool)
    )
}

/// 维护通道专属：admin 类别上游、或工具名以 `admin_` 开头。
///
/// 这类工具只能进 `admin` 组，否则任何业务组都会变成绕过维护通道的旁路。
pub fn admin_only(def: &McpUpstream, tool: Option<&str>) -> bool {
    def.category == crate::catalog::CATEGORY_ADMIN
        || tool.is_some_and(|name| name.starts_with("admin_"))
}

/// 该工具能否出现在指定组里：维护通道专属工具只允许 `admin` 组。
///
/// `tool = None` 表示整台上游（第一层授予），此时只看上游类别。
pub fn group_allows(def: &McpUpstream, tool: Option<&str>, group: &str) -> bool {
    !admin_only(def, tool) || group == discovery::ADMIN_GROUP
}

/// 工具对本次 scope 是否可见：任一 scope 组同时满足"能放该工具"与两层的暴露判定。
///
/// 授予/规则是数据，写入时的校验拦得住常规路径；读取时再叠一次 [`group_allows`]，
/// 库里的历史行（迁移、手工 SQL）也不会把维护通道工具漏进业务组。
fn scope_visible(
    scope: &EffectiveScope,
    exposure: &GroupExposure,
    def: &McpUpstream,
    tool: &str,
) -> bool {
    scope
        .groups
        .iter()
        .any(|group| group_allows(def, Some(tool), group) && exposure.visible(group, &def.id, tool))
}

struct Pooled {
    generation: String,
    client: Arc<UpstreamClient>,
}

struct CacheEntry {
    generation: String,
    fetched_at: Instant,
    last_checked_at: String,
    tools: Vec<UpstreamTool>,
    /// 最近一次拉取/调用的失败；`tools` 非空时为 `degraded`。
    failure: Option<(String, &'static str)>,
}

#[derive(Default)]
struct Inner {
    pool: HashMap<String, Pooled>,
    tools: HashMap<String, CacheEntry>,
}

/// 上游管理器：配置现查 SQLite，连接池与工具缓存按配置指纹失效。
pub struct UpstreamManager {
    store: Arc<CapabilityStore>,
    http: reqwest::Client,
    inner: Mutex<Inner>,
    /// 每个上游一把建连锁：并发请求不会为同一上游拉起两个 stdio 进程/两条初始化链。
    connect_locks: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    /// 内置 MCP 端点：URL 命中其 base 时注入进程内 Bearer token（见 builtin 模块）。
    builtin: Option<BuiltinEndpoint>,
}

impl UpstreamManager {
    pub fn new(store: Arc<CapabilityStore>) -> Self {
        // 重定向只允许同 host（跨 host 一律停止）：上游地址由管理面填写，必须防被重定向到内网。
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 3 {
                    return attempt.stop();
                }
                match attempt.previous().last() {
                    Some(previous) if previous.host_str() != attempt.url().host_str() => {
                        attempt.stop()
                    }
                    _ => attempt.follow(),
                }
            }))
            .build()
            .expect("reqwest client 初始化失败");
        Self {
            store,
            http,
            inner: Mutex::new(Inner::default()),
            connect_locks: Mutex::new(HashMap::new()),
            builtin: None,
        }
    }

    /// 挂上内置 MCP 端点（HTTP 模式启动时由 main 注入）。
    pub fn with_builtin(mut self, endpoint: BuiltinEndpoint) -> Self {
        self.builtin = Some(endpoint);
        self
    }

    /// 内置端点信息；`None` 表示本次启动没有内嵌端点（stdio 模式或测试未注入）。
    pub fn builtin_endpoint(&self) -> Option<&BuiltinEndpoint> {
        self.builtin.as_ref()
    }

    pub fn store(&self) -> &Arc<CapabilityStore> {
        &self.store
    }

    fn lock_for(&self, id: &str) -> Arc<AsyncMutex<()>> {
        let mut locks = self.connect_locks.lock().unwrap_or_else(|e| e.into_inner());
        locks.entry(id.to_string()).or_default().clone()
    }

    fn pooled(&self, id: &str, generation: &str) -> Option<Arc<UpstreamClient>> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .pool
            .get(id)
            .filter(|p| p.generation == generation)
            .map(|p| p.client.clone())
    }

    fn drop_pooled(&self, id: &str) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.pool.remove(id);
    }

    /// 配置写后失效：连接与工具缓存一起丢，下一次请求重新建连/拉取。
    pub fn invalidate(&self, id: &str) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.pool.remove(id);
        inner.tools.remove(id);
    }

    /// 删除上游后回收连接与缓存。
    pub fn remove(&self, id: &str) {
        self.invalidate(id);
        let mut locks = self.connect_locks.lock().unwrap_or_else(|e| e.into_inner());
        locks.remove(id);
    }

    /// 名字是否可能被路由到某个启用上游：纯 SQLite 判定，**不建连、不拉取**。
    ///
    /// 调用面在 `resolve`（会触网、会拉起 stdio 进程）之前用它做 fail-closed 预检：
    /// 组内没有任何授予、或该工具被逐工具规则显式排除时，直接拒绝而不产生上游交互。
    /// 这里拿暴露名后缀比对存储里的**原始**工具名，看不到上游真实清单——需要清洗的名字
    /// 可能被多拒一次，`resolve` 仍是最终判定，因此只收紧不放宽。
    pub fn matches_upstream(&self, scope: &EffectiveScope, name: &str) -> bool {
        let Ok(exposure) = self.store.group_exposure() else {
            return false;
        };
        let touched = exposure.touched_upstreams(&scope.groups);
        let Ok(defs) = self.store.upstreams() else {
            return false;
        };
        defs.iter().any(|def| {
            if !def.enabled || !touched.contains(&def.id) {
                return false;
            }
            let Some(suffix) = name.strip_prefix(&format!("{}{UPSTREAM_NAME_SEP}", def.id)) else {
                return false;
            };
            !suffix.is_empty() && scope_visible(scope, &exposure, def, suffix)
        })
    }

    /// 暴露名所属的上游 id：只按 `<id>__` 前缀判定，纯 SQLite 查询，**不建连、不拉工具清单**。
    ///
    /// 供 SQL 前置拦截在 `resolve`（会触网）之前定位审计坐标。为什么不用 `resolve` 的结果：
    /// 拦截必须发生在任何上游交互之前，而被拒的调用恰恰不该产生交互。
    /// 准入过滤与 [`Self::matches_upstream`] 一致（只看 enabled）；名字最终由 `resolve`
    /// 判为不可路由，这里的拦截只是更早的 fail-closed。
    pub fn owning_upstream(&self, name: &str) -> Option<String> {
        let defs = self.store.upstreams().ok()?;
        defs.into_iter()
            .find(|def| def.enabled && name.starts_with(&format!("{}{UPSTREAM_NAME_SEP}", def.id)))
            .map(|def| def.id)
    }

    /// 缓存里的上游工具（管理面读视图用）：**不触网**，从未拉取过时返回空。
    pub fn cached_tools(&self, id: &str) -> Vec<UpstreamTool> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .tools
            .get(id)
            .map(|entry| entry.tools.clone())
            .unwrap_or_default()
    }

    async fn client_for(
        &self,
        def: &McpUpstream,
    ) -> Result<(Arc<UpstreamClient>, String), UpstreamFailure> {
        let generation = def.definition_hash();
        if let Some(client) = self.pooled(&def.id, &generation) {
            return Ok((client, generation));
        }
        let lock = self.lock_for(&def.id);
        let _guard = lock.lock().await;
        if let Some(client) = self.pooled(&def.id, &generation) {
            return Ok((client, generation));
        }
        let client = UpstreamClient::connect(self.http.clone(), def, self.builtin.as_ref())
            .await
            .map_err(|e| e.with_hash(&generation))?;
        let client = Arc::new(client);
        {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.pool.insert(
                def.id.clone(),
                Pooled {
                    generation: generation.clone(),
                    client: client.clone(),
                },
            );
        }
        Ok((client, generation))
    }

    /// 拉一次 `tools/list`（只做网络与握手，命名/冲突校验在写缓存前统一做）。
    async fn fetch_tools(
        &self,
        def: &McpUpstream,
    ) -> Result<(Vec<UpstreamTool>, String), UpstreamFailure> {
        let (client, generation) = self.client_for(def).await?;
        let tools = client
            .list_tools(&def.id)
            .await
            .map_err(|e| e.with_hash(&generation))?;
        Ok((tools, generation))
    }

    /// 暴露名合法性 + 冲突检测：返回 `Some(message)` 表示整台上游拒绝（不静默改名/丢工具）。
    ///
    /// 在缓存写锁内调用，因此"检查"与"插入"之间没有并发窗口：两个上游同时首拉时，
    /// 后进入临界区的那台会看到已插入的名字并整台置 error。
    ///
    /// 冲突面三处：内置 id、注册表里已存在的 capability_id（managed 能力，含 draft）、
    /// 其他上游已暴露的名字。注册表读取失败按存储错误上抛，不当作"没有冲突"。
    fn conflict_message(
        &self,
        def: &McpUpstream,
        tools: &[UpstreamTool],
        inner: &Inner,
    ) -> Result<Option<String>, McpError> {
        let managed: HashSet<String> = self
            .store
            .all()?
            .into_iter()
            .map(|rec| rec.capability_id)
            .collect();
        let mut seen: HashSet<String> = HashSet::new();
        for tool in tools {
            let exposed = exposed_tool_name(&def.id, &tool.name);
            if exposed.len() > EXPOSED_TOOL_NAME_MAX || !is_upstream_tool_name(&exposed) {
                return Ok(Some(format!(
                    "工具 {:?} 的暴露名 {exposed} 不合规（需 <server_id>__<tool> 且整名 ≤{EXPOSED_TOOL_NAME_MAX}），拒绝接入",
                    tool.name
                )));
            }
            if is_builtin_id(&exposed) {
                return Ok(Some(format!("工具 {exposed} 与内置工具重名，拒绝接入")));
            }
            if managed.contains(&exposed) {
                return Ok(Some(format!(
                    "工具 {exposed} 与注册表能力重名（managed capability），拒绝接入"
                )));
            }
            if !seen.insert(exposed.clone()) {
                return Ok(Some(format!("工具 {exposed} 规范化后重名，拒绝接入")));
            }
            for (other_id, entry) in &inner.tools {
                if other_id == &def.id {
                    continue;
                }
                if entry
                    .tools
                    .iter()
                    .any(|other| exposed_tool_name(other_id, &other.name) == exposed)
                {
                    return Ok(Some(format!(
                        "工具 {exposed} 与其他上游（{other_id}）已暴露的工具重名，拒绝接入"
                    )));
                }
            }
        }
        Ok(None)
    }

    /// 单个上游的工具（缓存命中直接返回；过期/缺失则重新拉取，失败保留旧工具）。
    ///
    /// 失败也刷新 `fetched_at`：失败的上游在 TTL 内不重复打点（避免每个 `tools/list` 都
    /// 等一次超时），但保留旧工具并把状态标成 `degraded`，不影响其他上游。
    async fn tools_for(&self, def: &McpUpstream) -> Vec<UpstreamTool> {
        let generation = def.definition_hash();
        let now = Instant::now();
        if let Ok(inner) = self.inner.lock()
            && let Some(entry) = inner.tools.get(&def.id)
            && entry.generation == generation
            && now.duration_since(entry.fetched_at) < TOOLS_TTL
        {
            return entry.tools.clone();
        }
        let outcome = self.fetch_tools(def).await;
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match outcome {
            Ok((tools, generation)) => {
                let conflict = match self.conflict_message(def, &tools, &inner) {
                    Ok(conflict) => conflict,
                    Err(e) => Some(e.message),
                };
                if let Some(message) = conflict {
                    inner.tools.insert(
                        def.id.clone(),
                        CacheEntry {
                            generation,
                            fetched_at: Instant::now(),
                            last_checked_at: now_iso(),
                            tools: Vec::new(),
                            failure: Some((message, "error")),
                        },
                    );
                    return Vec::new();
                }
                inner.tools.insert(
                    def.id.clone(),
                    CacheEntry {
                        generation,
                        fetched_at: Instant::now(),
                        last_checked_at: now_iso(),
                        tools: tools.clone(),
                        failure: None,
                    },
                );
                tools
            }
            Err(failure) => {
                let entry = inner
                    .tools
                    .entry(def.id.clone())
                    .or_insert_with(|| CacheEntry {
                        generation: generation.clone(),
                        fetched_at: Instant::now(),
                        last_checked_at: now_iso(),
                        tools: Vec::new(),
                        failure: None,
                    });
                entry.generation = generation;
                entry.fetched_at = Instant::now();
                entry.last_checked_at = now_iso();
                entry.failure = Some((failure.message, failure.outcome));
                entry.tools.clone()
            }
        }
    }

    /// 让该上游的工具缓存立即过期（保留旧工具）：refresh 用它强制真实重拉。
    fn expire_tools(&self, id: &str) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = inner.tools.get_mut(id) {
            entry.fetched_at = Instant::now() - TOOLS_TTL - Duration::from_secs(1);
        }
    }

    /// 一次请求内的上游视图：只含"本次 scope 至少有一个组授予"的启用上游，
    /// **并行**拉取、逐台上游独立超时。
    ///
    /// 可见性是**逐工具**判定的：工具对 scope 可见 ⇔ 至少一个 scope 组判定它可见
    /// （该组授予这台上游，且没有该组的 `exclude` 规则）。第一层授予因此不能整台短路，
    /// 否则组内的逐工具排除永远无效。
    async fn projection(
        self: &Arc<Self>,
        scope: &EffectiveScope,
        exposure: &GroupExposure,
    ) -> Vec<(McpUpstream, Vec<UpstreamTool>)> {
        let touched = exposure.touched_upstreams(&scope.groups);
        let defs: Vec<McpUpstream> = match self.store.upstreams() {
            Ok(rows) => rows
                .into_iter()
                .filter(|d| d.enabled && touched.contains(&d.id))
                .collect(),
            Err(_) => return Vec::new(),
        };
        let mut set = JoinSet::new();
        for def in defs {
            let me = self.clone();
            set.spawn(async move {
                let tools = me.tools_for(&def).await;
                (def, tools)
            });
        }
        let mut out = Vec::new();
        while let Some(joined) = set.join_next().await {
            let Ok((def, tools)) = joined else {
                continue;
            };
            let tools: Vec<UpstreamTool> = tools
                .into_iter()
                .filter(|tool| scope_visible(scope, exposure, &def, &tool.name))
                .collect();
            // 授予了但当前不可见工具的上游也要留在投影里：`resolve` 的 `<id>__` 前缀回退
            // 靠它判断"名字属于已接入的上游"，上游拉取失败（无工具）时同样要能走到转发/报错。
            out.push((def, tools));
        }
        out.sort_by(|a, b| a.0.id.cmp(&b.0.id));
        out
    }

    /// 本次 scope 可见的上游工具投影（失败的上游返回空工具，不阻塞其他上游）。
    pub async fn capabilities(self: &Arc<Self>, scope: &EffectiveScope) -> Vec<Capability> {
        let Ok(exposure) = self.store.group_exposure() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (def, tools) in self.projection(scope, &exposure).await {
            for tool in &tools {
                let exposed = exposed_tool_name(&def.id, &tool.name);
                // 一个工具可能同时属于多个 scope 组：上报字典序最小者，保证同一 scope 内稳定。
                let Some(group) = exposure.visible_group(&scope.groups, &def.id, &tool.name) else {
                    continue;
                };
                out.push(discovery::project_upstream(&def, tool, &exposed, group));
            }
        }
        out
    }

    /// 解析一次调用：当前投影精确匹配 → `server_id__` 前缀回退 → `None`。
    ///
    /// 前缀回退是"客户端缓存了旧 `tools/list`"的兜底：不要求该工具在本次缓存里，
    /// 但要求它按组规则仍然可见（授权不因缓存陈旧而放宽）。
    pub async fn resolve(
        self: &Arc<Self>,
        scope: &EffectiveScope,
        name: &str,
    ) -> Option<UpstreamCall> {
        let Ok(exposure) = self.store.group_exposure() else {
            return None;
        };
        let projection = self.projection(scope, &exposure).await;
        for (def, tools) in &projection {
            for tool in tools {
                if exposed_tool_name(&def.id, &tool.name) == name {
                    return Some(UpstreamCall {
                        upstream_id: def.id.clone(),
                        definition_hash: def.definition_hash(),
                        tool: tool.name.clone(),
                        exposed: name.to_string(),
                    });
                }
            }
        }
        for (def, tools) in &projection {
            let prefix = format!("{}{UPSTREAM_NAME_SEP}", def.id);
            let Some(suffix) = name.strip_prefix(&prefix) else {
                continue;
            };
            if suffix.is_empty() {
                continue;
            }
            let known = tools
                .iter()
                .find(|t| exposed_tool_name(&def.id, &t.name) == name);
            if known.is_none() && !scope_visible(scope, &exposure, def, suffix) {
                continue;
            }
            let tool = known
                .map(|t| t.name.clone())
                .unwrap_or_else(|| suffix.to_string());
            return Some(UpstreamCall {
                upstream_id: def.id.clone(),
                definition_hash: def.definition_hash(),
                tool,
                exposed: name.to_string(),
            });
        }
        None
    }

    /// 转发一次调用，返回（上游结果, 实际使用的配置指纹）。
    pub async fn call(
        &self,
        call: &UpstreamCall,
        arguments: &Value,
    ) -> Result<(Value, String), UpstreamFailure> {
        let def = self
            .store
            .upstream(&call.upstream_id)
            .map_err(|e| UpstreamFailure::error(&call.upstream_id, &e.code, e.message))?
            .filter(|d| d.enabled)
            .ok_or_else(|| {
                UpstreamFailure::error(
                    &call.upstream_id,
                    "MCP_UPSTREAM_NOT_FOUND",
                    "上游不存在或已停用",
                )
            })?;
        let (client, generation) = self.client_for(&def).await?;
        match client.call(&def.id, &call.tool, arguments).await {
            Ok(result) => Ok((result, generation)),
            Err(failure) => {
                // 传输层失败后不复用可能已坏掉的连接：下一次调用重新建连/拉起进程。
                self.drop_pooled(&def.id);
                Err(failure.with_hash(&generation))
            }
        }
    }

    /// 管理面 refresh：重新建连 + 拉取工具；连接失败不返回 Err（由 status 表达）。
    pub async fn refresh(self: &Arc<Self>, id: &str) -> Result<UpstreamRuntime, McpError> {
        let def = self
            .store
            .upstream(id)?
            .ok_or_else(|| McpError::new("MCP_UPSTREAM_NOT_FOUND", json!({"id": id})))?;
        // 连接与工具缓存都必须失效：只丢连接会被 30s TTL 的缓存挡住，refresh 变成假探测。
        self.drop_pooled(id);
        self.expire_tools(id);
        let _ = self.tools_for(&def).await;
        Ok(self.runtime(&def))
    }

    /// 运行态：状态取缓存（`ok|degraded|error|unknown`），不触发任何网络请求。
    pub fn runtime(&self, def: &McpUpstream) -> UpstreamRuntime {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(entry) = inner.tools.get(&def.id) else {
            return UpstreamRuntime {
                status: "unknown",
                status_message: String::new(),
                tool_count: 0,
                last_checked_at: None,
            };
        };
        let tool_count = entry.tools.len() as i64;
        let (status, message) = match &entry.failure {
            Some((message, _)) if tool_count == 0 => ("error", message.clone()),
            Some((message, _)) => ("degraded", message.clone()),
            None => ("ok", String::new()),
        };
        UpstreamRuntime {
            status,
            status_message: message,
            tool_count,
            last_checked_at: Some(entry.last_checked_at.clone()),
        }
    }

    /// 全部上游 + 运行态（管理面列表；配置现查 store，状态取缓存）。
    pub fn views(&self) -> Result<Vec<(McpUpstream, UpstreamRuntime)>, McpError> {
        Ok(self
            .store
            .upstreams()?
            .into_iter()
            .map(|def| {
                let runtime = self.runtime(&def);
                (def, runtime)
            })
            .collect())
    }
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// 上游工具是否由本服务暴露（供 `tools/list` 判定常驻集合）。
pub fn is_upstream_capability(cap: &Capability) -> bool {
    cap.source == SOURCE_UPSTREAM
}
