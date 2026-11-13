//! 上游 MCP router：配置加载、连接池、工具缓存、失败隔离与调用转发。
//!
//! 边界（与 `research_magpie_router.md` 的取舍一致）：
//! - 只做聚合：一个 `/mcp` 暴露内置工具 + 所有启用上游的工具，不提供逐上游透传端点。
//! - 命名 `<server_id>__<tool>`，两侧非 `[A-Za-z0-9_-]` → `-`，整名 ≤64；冲突拒绝，不加后缀。
//! - 传输只实现 MCP Streamable HTTP（POST + JSON/SSE 响应 + `Mcp-Session-Id`）；stdio 上游
//!   由部署开关控制，`Command` 直接 spawn 且显式剔除管理/agent token 环境变量。
//! - 惰性连接、空闲回收，不做后台轮询；管理面 `refresh` 是唯一"主动探测"入口。
//!
//! 失败隔离：单个上游不可达只影响它自己——缓存里的旧工具继续可见（状态 `degraded`），
//! 其他上游与内置工具照常；调用失败整形为 MCP `isError` 结果。

use crate::catalog::{Capability, SOURCE_UPSTREAM};
use crate::discovery;
use jumcp_core::entry::EffectiveScope;
use jumcp_core::errors::McpError;
use jumcp_core::registry::{
    CapabilityStore, EXPOSED_TOOL_NAME_MAX, McpUpstream, UPSTREAM_NAME_SEP, UpstreamTool,
    is_builtin_id, is_upstream_tool_name,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinSet;

/// 未显式声明时，`tools/list` 缓存的有效期；管理面写操作会立即失效。
const TOOLS_TTL: Duration = Duration::from_secs(30);
/// `tools/list` 翻页上限（防御上游返回自引用 cursor）。
const MAX_TOOL_PAGES: usize = 20;

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
    /// 是否允许 `read_only=false` 的上游进入投影/调用面（部署开关的运行时副本）。
    write_allowed: bool,
    http: reqwest::Client,
    inner: Mutex<Inner>,
    /// 每个上游一把建连锁：并发请求不会为同一上游拉起两个 stdio 进程/两条初始化链。
    connect_locks: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
}

impl UpstreamManager {
    pub fn new(store: Arc<CapabilityStore>, write_allowed: bool) -> Self {
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
            write_allowed,
            http,
            inner: Mutex::new(Inner::default()),
            connect_locks: Mutex::new(HashMap::new()),
        }
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

    /// 名字是否落在某个"会参与路由"的上游前缀上：纯 SQLite 判定，**不建连、不拉取**。
    ///
    /// 供调用面的 allowlist 预检使用——被拒绝的名字不应产生任何上游连接/工具拉取。
    pub fn matches_upstream(&self, scope: &EffectiveScope, name: &str) -> bool {
        let Ok(defs) = self.store.upstreams() else {
            return false;
        };
        defs.iter().any(|def| {
            def.enabled
                && scope.groups.contains(&def.group)
                && (def.read_only || self.write_allowed)
                && name.starts_with(&format!("{}{UPSTREAM_NAME_SEP}", def.id))
        })
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
        let client = UpstreamClient::connect(self.http.clone(), def)
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

    /// 一次请求内的上游视图：只含启用且组在 scope 内的上游，**并行**拉取、逐台上游独立超时。
    async fn projection(
        self: &Arc<Self>,
        scope: &EffectiveScope,
    ) -> Vec<(McpUpstream, Vec<UpstreamTool>)> {
        let defs: Vec<McpUpstream> = match self.store.upstreams() {
            Ok(rows) => rows
                .into_iter()
                .filter(|d| {
                    d.enabled
                        && scope.groups.contains(&d.group)
                        // 写上游的准入是部署开关：DB 里残留的 read_only=false 在开关关闭时
                        // 既不投影也不可调（不能只靠写入期校验）。
                        && (d.read_only || self.write_allowed)
                })
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
            if let Ok(pair) = joined {
                out.push(pair);
            }
        }
        out.sort_by(|a, b| a.0.id.cmp(&b.0.id));
        out
    }

    /// 本次 scope 可见的上游工具投影（失败的上游返回空工具，不阻塞其他上游）。
    pub async fn capabilities(self: &Arc<Self>, scope: &EffectiveScope) -> Vec<Capability> {
        let mut out = Vec::new();
        for (def, tools) in self.projection(scope).await {
            for tool in &tools {
                let exposed = exposed_tool_name(&def.id, &tool.name);
                // token 的 allowlist 同时约束内置与上游：不在白名单里的暴露名既不可发现。
                if !discovery::tool_allowed(scope, &exposed) {
                    continue;
                }
                out.push(discovery::project_upstream(&def, tool, &exposed));
            }
        }
        out
    }

    /// 解析一次调用：当前投影精确匹配 → `server_id__` 前缀回退 → `None`。
    ///
    /// 前缀回退是"客户端缓存了旧 `tools/list`"的兜底：不要求该工具在本次缓存里，
    /// 但要求上游本身启用且组在 scope 内（授权不因缓存陈旧而放宽）。
    pub async fn resolve(
        self: &Arc<Self>,
        scope: &EffectiveScope,
        name: &str,
    ) -> Option<UpstreamCall> {
        let projection = self.projection(scope).await;
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
            let tool = tools
                .iter()
                .find(|t| exposed_tool_name(&def.id, &t.name) == name)
                .map_or_else(|| suffix.to_string(), |t| t.name.clone());
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
        if !def.read_only && !self.write_allowed {
            return Err(UpstreamFailure::error(
                &def.id,
                "MCP_UPSTREAM_WRITE_FORBIDDEN",
                "上游声明可写，但本部署未开启 JUMCP_MCP_ALLOW_WRITE_UPSTREAM",
            ));
        }
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

// ---------------------------- 上游客户端（http / stdio） ----------------------------

struct UpstreamClient {
    transport: Transport,
}

enum Transport {
    // stdio 侧持有的进程句柄/管道明显更大，装箱避免每次 match 都搬动整个枚举。
    Http(Box<HttpTransport>),
    Stdio(Box<StdioTransport>),
}

impl UpstreamClient {
    async fn connect(http: reqwest::Client, def: &McpUpstream) -> Result<Self, UpstreamFailure> {
        // env 的 `env:NAME` 引用对两种传输都在建连时解析：http 不使用 env，但写错的引用
        // 仍然要显式失败（静默忽略会让配置错误在切换传输后才暴露）。
        let env = resolve_refs(&def.id, "env", &def.env)?;
        let mut transport = match def.transport.as_str() {
            "http" => Transport::Http(Box::new(HttpTransport::new(http, def)?)),
            _ => Transport::Stdio(Box::new(StdioTransport::spawn(def, env).await?)),
        };
        transport.initialize(def).await?;
        Ok(Self { transport })
    }

    async fn list_tools(&self, id: &str) -> Result<Vec<UpstreamTool>, UpstreamFailure> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_TOOL_PAGES {
            let params = cursor
                .as_ref()
                .map_or_else(|| json!({}), |c| json!({"cursor": c}));
            let result = self.request("tools/list", params).await?;
            for item in result
                .get("tools")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(name) = item.get("name").and_then(Value::as_str) else {
                    continue;
                };
                out.push(UpstreamTool {
                    name: name.to_string(),
                    description: item
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    input_schema: item
                        .get("inputSchema")
                        .filter(|v| v.is_object())
                        .cloned()
                        .unwrap_or_else(|| json!({"type":"object","properties":{}})),
                    annotations: item.get("annotations").filter(|v| v.is_object()).cloned(),
                });
            }
            cursor = result
                .get("nextCursor")
                .and_then(Value::as_str)
                .filter(|c| !c.is_empty())
                .map(str::to_string);
            if cursor.is_none() {
                return Ok(out);
            }
        }
        Err(UpstreamFailure::error(
            id,
            "MCP_UPSTREAM_ERROR",
            "tools/list 翻页超过上限（疑似自引用 cursor）",
        ))
    }

    async fn call(
        &self,
        id: &str,
        tool: &str,
        arguments: &Value,
    ) -> Result<Value, UpstreamFailure> {
        self.request("tools/call", json!({"name": tool, "arguments": arguments}))
            .await
            .map_err(|mut e| {
                e.upstream_id = id.to_string();
                e
            })
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, UpstreamFailure> {
        self.transport.request(method, params).await
    }
}

impl Transport {
    async fn initialize(&mut self, def: &McpUpstream) -> Result<(), UpstreamFailure> {
        self.request(
            "initialize",
            json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "jumcp-mcp-router", "version": env!("CARGO_PKG_VERSION")}
            }),
        )
        .await?;
        // `notifications/initialized` 没有 id，也没有响应体；失败按建连失败处理。
        self.notify("notifications/initialized", json!({}), def)
            .await
    }

    async fn notify(
        &self,
        method: &str,
        params: Value,
        def: &McpUpstream,
    ) -> Result<(), UpstreamFailure> {
        match self {
            Transport::Http(http) => http.notify(method, params).await,
            Transport::Stdio(stdio) => stdio.notify(method, params).await.map_err(|mut e| {
                e.upstream_id = def.id.clone();
                e
            }),
        }
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, UpstreamFailure> {
        match self {
            Transport::Http(http) => http.request(method, params).await,
            Transport::Stdio(stdio) => stdio.request(method, params).await,
        }
    }
}

/// `env:NAME` / 字面量解析：建连时一次性完成，缺失的环境变量显式失败（不静默空串）。
fn resolve_refs(
    upstream_id: &str,
    field: &str,
    map: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, UpstreamFailure> {
    let mut out = BTreeMap::new();
    for (key, value) in map {
        let resolved = match value.strip_prefix("env:") {
            Some(name) => std::env::var(name).map_err(|_| {
                UpstreamFailure::error(
                    upstream_id,
                    "MCP_UPSTREAM_INVALID",
                    format!("{field}.{key} 引用的环境变量 {name} 未设置"),
                )
            })?,
            None => value.clone(),
        };
        out.insert(key.clone(), resolved);
    }
    Ok(out)
}

fn http_failure(upstream_id: &str, error: &reqwest::Error) -> UpstreamFailure {
    if error.is_timeout() {
        return UpstreamFailure::unknown(
            upstream_id,
            "MCP_EXEC_TIMEOUT",
            format!("上游请求超时: {error}"),
        );
    }
    UpstreamFailure::error(
        upstream_id,
        "MCP_UPSTREAM_UNREACHABLE",
        format!("上游不可达: {error}"),
    )
}

struct HttpTransport {
    client: reqwest::Client,
    upstream_id: String,
    url: reqwest::Url,
    headers: BTreeMap<String, String>,
    session: AsyncMutex<Option<String>>,
    timeout: Duration,
}

impl HttpTransport {
    fn new(client: reqwest::Client, def: &McpUpstream) -> Result<Self, UpstreamFailure> {
        let url = reqwest::Url::parse(&def.url).map_err(|_| {
            UpstreamFailure::error(&def.id, "MCP_UPSTREAM_INVALID", "url 不是合法的绝对地址")
        })?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(UpstreamFailure::error(
                &def.id,
                "MCP_UPSTREAM_INVALID",
                "url 只允许 http/https",
            ));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(UpstreamFailure::error(
                &def.id,
                "MCP_UPSTREAM_INVALID",
                "url 不允许携带 userinfo",
            ));
        }
        Ok(Self {
            client,
            upstream_id: def.id.clone(),
            url,
            headers: resolve_refs(&def.id, "headers", &def.headers)?,
            session: AsyncMutex::new(None),
            timeout: Duration::from_millis(def.timeout_ms as u64),
        })
    }

    fn build(&self, body: &Value) -> reqwest::RequestBuilder {
        let mut req = self
            .client
            .post(self.url.clone())
            .timeout(self.timeout)
            .header(
                reqwest::header::ACCEPT,
                "application/json, text/event-stream",
            )
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(body);
        for (key, value) in &self.headers {
            req = req.header(key.as_str(), value.as_str());
        }
        req
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), UpstreamFailure> {
        let body = json!({"jsonrpc":"2.0","method":method,"params":params});
        let mut req = self.build(&body);
        if let Some(session) = self.session.lock().await.clone() {
            req = req.header("mcp-session-id", session);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| http_failure(&self.upstream_id, &e))?;
        if !resp.status().is_success() {
            return Err(UpstreamFailure::error(
                &self.upstream_id,
                "MCP_UPSTREAM_ERROR",
                format!("{method} 返回 HTTP {}", resp.status()),
            ));
        }
        Ok(())
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, UpstreamFailure> {
        let id = next_request_id();
        let body = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        let mut req = self.build(&body);
        if let Some(session) = self.session.lock().await.clone() {
            req = req.header("mcp-session-id", session);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| http_failure(&self.upstream_id, &e))?;
        if let Some(session) = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
            .filter(|v| !v.is_empty())
        {
            *self.session.lock().await = Some(session.to_string());
        }
        let status = resp.status();
        if !status.is_success() {
            return Err(UpstreamFailure::error(
                &self.upstream_id,
                "MCP_UPSTREAM_UNREACHABLE",
                format!("{method} 返回 HTTP {status}"),
            ));
        }
        let sse = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("text/event-stream"));
        let envelope = if sse {
            read_sse_response(resp, id).await.map_err(|message| {
                UpstreamFailure::error(
                    &self.upstream_id,
                    "MCP_UPSTREAM_ERROR",
                    format!("{method}: {message}"),
                )
            })?
        } else {
            resp.json::<Value>()
                .await
                .map_err(|e| http_failure(&self.upstream_id, &e))?
        };
        rpc_result(&self.upstream_id, method, envelope)
    }
}

/// 从 SSE 流里取出与 `id` 匹配的 JSON-RPC 响应（忽略进度/日志等通知事件）。
async fn read_sse_response(mut resp: reqwest::Response, id: i64) -> Result<Value, String> {
    let mut buffer = String::new();
    loop {
        let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| format!("SSE 读取失败: {e}"))?
        else {
            break;
        };
        buffer.push_str(&String::from_utf8_lossy(&chunk).replace("\r\n", "\n"));
        while let Some(position) = buffer.find("\n\n") {
            let event: String = buffer.drain(..position + 2).collect();
            let data = event
                .lines()
                .filter_map(|line| line.strip_prefix("data:").map(str::trim))
                .collect::<Vec<_>>()
                .join("\n");
            if data.is_empty() {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(&data) else {
                continue;
            };
            let matches_id = value
                .get("id")
                .and_then(Value::as_i64)
                .is_some_and(|got| got == id);
            if matches_id && (value.get("result").is_some() || value.get("error").is_some()) {
                return Ok(value);
            }
        }
    }
    Err("SSE 流已结束但未收到匹配响应".to_string())
}

fn rpc_result(upstream_id: &str, method: &str, envelope: Value) -> Result<Value, UpstreamFailure> {
    if let Some(error) = envelope.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("upstream error");
        let code = error
            .get("code")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        return Err(UpstreamFailure::error(
            upstream_id,
            "MCP_UPSTREAM_ERROR",
            format!("{method} 失败: [{code}] {message}"),
        ));
    }
    match envelope.get("result") {
        Some(result) => Ok(result.clone()),
        None => Err(UpstreamFailure::error(
            upstream_id,
            "MCP_UPSTREAM_ERROR",
            format!("{method} 响应缺少 result"),
        )),
    }
}

struct StdioTransport {
    io: AsyncMutex<StdioIo>,
    upstream_id: String,
    /// 进程句柄只用于生命周期：`kill_on_drop(true)` 保证随连接池回收一起退出。
    _child: Child,
    timeout: Duration,
}

struct StdioIo {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl StdioTransport {
    async fn spawn(
        def: &McpUpstream,
        env: BTreeMap<String, String>,
    ) -> Result<Self, UpstreamFailure> {
        let mut command = Command::new(&def.command);
        command
            .args(&def.args)
            .envs(&env)
            // 上游进程不得继承本服务的治理凭据：谁拉起进程，谁负责收窄环境。
            .env_remove("JUMCP_ADMIN_TOKENS")
            .env_remove("JUMCP_AGENT_TOKENS")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            // stderr 丢弃：上游可能把 env 里的凭据打到 stderr，status_message 会进 GET 响应。
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|e| {
            UpstreamFailure::error(
                &def.id,
                "MCP_UPSTREAM_UNREACHABLE",
                format!("stdio 上游启动失败: {e}"),
            )
        })?;
        let stdin = child.stdin.take().ok_or_else(|| {
            UpstreamFailure::error(&def.id, "MCP_UPSTREAM_ERROR", "stdio 上游缺少 stdin")
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            UpstreamFailure::error(&def.id, "MCP_UPSTREAM_ERROR", "stdio 上游缺少 stdout")
        })?;
        Ok(Self {
            io: AsyncMutex::new(StdioIo {
                stdin,
                stdout: BufReader::new(stdout),
            }),
            upstream_id: def.id.clone(),
            _child: child,
            timeout: Duration::from_millis(def.timeout_ms as u64),
        })
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), UpstreamFailure> {
        let line = serde_json::to_string(&json!({"jsonrpc":"2.0","method":method,"params":params}))
            .map_err(|e| {
                UpstreamFailure::error(
                    &self.upstream_id,
                    "MCP_UPSTREAM_ERROR",
                    format!("请求序列化失败: {e}"),
                )
            })?;
        let mut io = self.io.lock().await;
        io.stdin
            .write_all(format!("{line}\n").as_bytes())
            .await
            .map_err(|e| self.io_failure(&e))?;
        io.stdin.flush().await.map_err(|e| self.io_failure(&e))
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, UpstreamFailure> {
        let id = next_request_id();
        let line = serde_json::to_string(
            &json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
        )
        .map_err(|e| {
            UpstreamFailure::error(
                &self.upstream_id,
                "MCP_UPSTREAM_ERROR",
                format!("请求序列化失败: {e}"),
            )
        })?;
        let mut io = self.io.lock().await;
        io.stdin
            .write_all(format!("{line}\n").as_bytes())
            .await
            .map_err(|e| self.io_failure(&e))?;
        io.stdin.flush().await.map_err(|e| self.io_failure(&e))?;
        let mut line = String::new();
        loop {
            line.clear();
            let read = tokio::time::timeout(self.timeout, io.stdout.read_line(&mut line)).await;
            let read = match read {
                Ok(result) => result.map_err(|e| self.io_failure(&e))?,
                Err(_) => {
                    return Err(UpstreamFailure::unknown(
                        &self.upstream_id,
                        "MCP_EXEC_TIMEOUT",
                        format!("stdio 上游 {method} 超时"),
                    ));
                }
            };
            if read == 0 {
                return Err(UpstreamFailure::error(
                    &self.upstream_id,
                    "MCP_UPSTREAM_UNREACHABLE",
                    "stdio 上游已退出（stdout EOF）",
                ));
            }
            let Ok(envelope) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            if envelope.get("id").and_then(Value::as_i64) != Some(id) {
                // 上游主动推送的通知/其他响应：router 不消费通知。
                continue;
            }
            return rpc_result(&self.upstream_id, method, envelope);
        }
    }

    fn io_failure(&self, error: &std::io::Error) -> UpstreamFailure {
        UpstreamFailure::error(
            &self.upstream_id,
            "MCP_UPSTREAM_UNREACHABLE",
            format!("stdio 上游 IO 失败: {error}"),
        )
    }
}

fn next_request_id() -> i64 {
    use std::sync::atomic::{AtomicI64, Ordering};
    static NEXT: AtomicI64 = AtomicI64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// 上游工具是否由本服务暴露（供 `tools/list` 判定常驻集合）。
pub fn is_upstream_capability(cap: &Capability) -> bool {
    cap.source == SOURCE_UPSTREAM
}
