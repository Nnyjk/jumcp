/**
 * 领域类型：与后端 admin API 的真实响应结构一一对应。
 * 审计事件按新契约建模：payload 只认结构化字段，旧事件不做过字段回退。
 */

/** §4.4.3 event_type 闭集。 */
export type AuditEventType =
  | 'tools_list_served'
  | 'capability_search'
  | 'capability_describe'
  | 'tool_call'
  | 'template_run'
  | 'policy_decision'
  | 'admin_change'

/** §4.4.3 actor{type,id,role}；MCP token 事件额外带来源与 token_id，旧事件没有这两个字段。 */
export interface AuditActor {
  type: string
  id: string
  role: string
  source?: string | undefined
  token_id?: string | undefined
}

/** 审计对象类型：治理对象与运行期对象共用一套枚举。 */
export type AuditObjectType =
  | 'capability'
  | 'group'
  | 'tool'
  | 'template'
  | 'catalog'
  | 'request'
  | 'token'

/** 审计动作：治理动作与运行期动作分开取值，但共用同一字段。 */
export type AuditAction =
  | 'create'
  | 'update_definition'
  | 'submit'
  | 'publish'
  | 'reject'
  | 'visibility'
  | 'deprecate'
  | 'group_create'
  | 'group_update'
  | 'group_delete'
  | 'token_create'
  | 'token_revoke'
  | 'token_reissue'
  | 'token_delete'
  | 'invoke'
  | 'search'
  | 'describe'
  | 'list_tools'
  | 'deny'

export type AuditOutcome = 'ok' | 'error' | 'denied' | 'summary'

/** 单字段变更：before/after 是原始值，可能是任意 JSON。 */
export interface AuditChange {
  field: string
  before: unknown
  after: unknown
}

/** 审批信息：source=external 表示外部审批系统单号，系统不校验真伪。 */
export interface AuditApproval {
  id: string
  source: 'external' | 'none'
  verified: boolean
  submitted_by: string | null
  approved_by: string | null
  approved_at: string | null
}

/**
 * 结构化审计载荷。旧事件可能整块缺失这些字段：`structuredPayload()` 会判定为旧格式，
 * 页面只展示原始载荷，不尝试从旧字段（capability_id/diff/approval_id）还原语义。
 */
export interface AuditPayload {
  object_type?: AuditObjectType | undefined
  object_id?: string | undefined
  action?: AuditAction | undefined
  outcome?: AuditOutcome | undefined
  error_code?: string | null | undefined
  error_message?: string | null | undefined
  from_status?: string | null | undefined
  to_status?: string | null | undefined
  changes?: AuditChange[] | undefined
  approval?: AuditApproval | null | undefined
  reason?: string | null | undefined
  /** 载荷结构版本（固定为 1）；能力/模板版本一律读 data.version。 */
  schema_version?: number | null | undefined
  correlation_id?: string | null | undefined
  data?: Record<string, unknown> | undefined
}

/** §4.4.3 审计 envelope。hash/prev_hash 仅保留原始值，不再作为一等展示字段。 */
export interface AuditEnvelope {
  event_id: string
  ts: string
  tenant: string
  actor: AuditActor
  event_type: AuditEventType
  payload: AuditPayload & Record<string, unknown>
  prev_hash?: string | undefined
  hash?: string | undefined
}

/** 分页响应：列表类接口统一 {items,total,limit,offset}。 */
export interface Page<T> {
  items: T[]
  total: number
  limit: number
  offset: number
}

/** 不分页的清单响应：审核队列只返回 {items,total}。 */
export interface ItemPage<T> {
  items: T[]
  total: number
}

export type AuditOrder = 'asc' | 'desc'

/** 审计查询响应：事件数组键名是 events，不是 items。 */
export interface AuditPage {
  events: AuditEnvelope[]
  total: number
  limit: number
  offset: number
  order?: AuditOrder
}

/** §4.4.4 admin_audit_queries 支持的过滤条件（后端 FILTER_KEYS 白名单，已删除 session_id）。 */
export interface AuditQuery {
  tenant?: string | undefined
  event_type?: string | undefined
  object_type?: string | undefined
  object_id?: string | undefined
  actor_id?: string | undefined
  event_id?: string | undefined
  actor_role?: string | undefined
  token_id?: string | undefined
  ts_after?: string | undefined
  ts_before?: string | undefined
  order?: AuditOrder | undefined
}

/** GET /admin/me：后端按 token 派生的当前主体。 */
export interface AdminIdentity {
  id: string
  role: string
  type: string
}

/** §4.4.1 生命周期状态（与双布尔正交）。 */
export type CapabilityStatus = 'draft' | 'review' | 'published' | 'deprecated'

export type CapabilityKind = 'tool' | 'template' | 'endpoint'

export type CapabilityRisk = 'read' | 'write' | 'sensitive' | 'admin'

/** 来源：builtin 由进程内注册，managed 由管理台维护。 */
export type CapabilitySource = 'builtin' | 'managed'

/** 能力记录（后端以 id 为能力标识，不是 capability_id）。 */
export interface CapabilityItem {
  id: string
  kind: CapabilityKind
  group: string
  risk: CapabilityRisk
  status: CapabilityStatus
  discoverable: boolean
  callable: boolean
  version: number
  definition_hash: string
  successor_id: string
  /** 最后变更（含可见性）；定义本身的更新时间见 definition_updated_at。 */
  last_changed_at: string | null
  submitted_by: string | null
  submitted_at: string | null
  definition_updated_at: string | null
  source?: CapabilitySource | undefined
}

/** 审核队列条目：draft / review 的摘要视图。 */
export interface ReviewQueueItem {
  id: string
  kind: CapabilityKind
  group: string
  risk: CapabilityRisk
  status: CapabilityStatus
  version: number
  submitted_by: string | null
  submitted_at: string | null
  last_changed_at: string | null
  definition_hash: string
}

/** POST /admin/capabilities/{id}/reject 的 200 响应：退回后回到 draft。 */
export interface RejectedCapability {
  id: string
  status: 'draft'
  version: number
  last_changed_at: string | null
  reason: string
}

/** 能力列表查询参数（后端 capabilities 接口白名单）。 */
export interface CapabilityQuery {
  status?: string
  group?: string
  q?: string
}

/** 健康检查：链校验结论。 */
export type AuditChainState = 'valid' | 'tampered' | 'error'

export interface HealthDatasource {
  name: string
  engine: string
  readonly: boolean
}

export interface HealthPayload {
  status: string
  version: string
  datasources: HealthDatasource[]
  audit_chain: AuditChainState
  /** 篡改事件 id 或校验失败原因：可能是字符串或对象，按原样转文本展示。 */
  audit_chain_detail?: unknown
}

/** 工具台账：统计视图的 kind 可能来自未纳管定义，故保留 unknown。 */
export type ToolKind = 'template' | 'tool' | 'endpoint' | 'unknown'

/** 生命周期在能力状态之外还可能出现 builtin（内置）与 unknown。 */
export type ToolLifecycle = CapabilityStatus | 'builtin' | 'unknown'

export type ToolSource = 'builtin' | 'managed'

export type ToolHealth = 'ok' | 'degraded' | 'failing' | 'unknown'

export type ToolCallOutcome = 'ok' | 'summary' | 'error' | 'denied' | 'unknown' | 'budget_rejected'

export interface ToolStatsWindow {
  days: number
  from: string
  to: string
}

/** GET /admin/tools/stats 的单行工具统计。比率是 0–1 小数，token 是该时间窗内的聚合值。 */
export interface ToolStatsRow {
  id: string
  kind: ToolKind
  /**
   * 上游分类（basic / analysis / admin / custom）。旧统计行没有该字段：
   * 台账显示破折号，不按 kind 或来源推断。
   */
  category?: string
  group: string
  lifecycle_status: ToolLifecycle
  risk: CapabilityRisk | null
  discoverable: boolean | null
  callable: boolean | null
  version: number | null
  last_changed_at: string | null
  source: ToolSource
  health: ToolHealth
  calls: number
  success: number
  failure: number
  success_rate: number | null
  cache_hits: number
  hit_rate: number | null
  retries: number
  avg_response_tokens: number | null
  p95_response_tokens: number | null
  max_response_tokens: number | null
  last_call_at: string | null
  last_outcome: ToolCallOutcome | null
  recent_tokens: number[]
}

export interface ToolStatsPayload {
  window: ToolStatsWindow
  truncated: boolean
  tools: ToolStatsRow[]
}

/** GET /admin/tools/{id}/calls 的单条调用记录。 */
export interface ToolCallRecord {
  event_id: string
  ts: string
  event_type: AuditEventType
  object_type: AuditObjectType
  object_id: string
  actor: {
    id: string | null
    role: string | null
    source?: string | null
    token_id?: string | null
  }
  outcome: string
  tokens: number | null
  cache: string | null
  /** 缓存命中或拒绝路径没有执行 SQL，后端以 null/0 表示「未执行」。 */
  elapsed_ms: number | null
  version: number | null
  error_code: string | null
  error_message: string | null
  /** 原始入参对象，可能为 null。 */
  arguments: unknown
  /** 实际执行的 SQL/脚本，无则为 null。 */
  executed_script: string | null
  /** SQL 占位符 ? 的绑定值，按顺序排列。 */
  bound_params: unknown[] | null
}

export interface ToolCallsPage {
  id: string
  total: number
  limit: number
  offset: number
  calls: ToolCallRecord[]
}

/** 新建草案的 201 响应。 */
export interface CreatedCapability {
  id: string
  status: string
  version: number
  definition_hash: string
  last_changed_at?: string | null
}

/** GET /admin/capabilities/{id}：能力完整记录 + 定义正文。 */
export interface CapabilityDetail extends CapabilityItem {
  definition: Record<string, unknown>
}

/** GET /admin/capabilities/{id}/versions 的单条版本记录。 */
export interface CapabilityVersion {
  version: number
  definition: Record<string, unknown>
  definition_hash: string
  status: string | null
  created_at: string | null
  submitted_by: string | null
  submitted_at: string | null
  approved_by: string | null
  approved_at: string | null
  approval_id: string | null
}

export interface CapabilityVersionsPage {
  items: CapabilityVersion[]
  total: number
}

/** PUT /admin/capabilities/{id}/definition 的 200 响应。 */
export interface UpdatedCapabilityDefinition {
  id: string
  status: CapabilityStatus
  version: number
  definition_hash: string
  last_changed_at: string | null
}

/** 组的来源：policy 来自 config/policy.toml（只读），managed 由管理台维护。 */
export type GroupSource = 'policy' | 'managed'

/** 组：`code` 是契约值（能力、令牌、审计都用它），`name` 只用于展示。 */
export interface GroupItem {
  code: string
  name: string
  source: GroupSource
  priority: number
  min_budget: number
  roles: string[]
  capability_count: number
  token_count: number
}

export interface GroupPage {
  items: GroupItem[]
  total: number
}

/** GET /admin/groups 的过滤条件：q 同时匹配 code/name。 */
export interface GroupQuery {
  q?: string | undefined
  source?: GroupSource | undefined
  role?: string | undefined
  /** 下拉选项一次取全：上游接入的授权组下拉用 200，避免默认分页截断。 */
  limit?: number | undefined
}

/** 新建组的 body（POST /admin/groups）。 */
export interface GroupCreateInput {
  code: string
  name: string
  priority: number
  min_budget: number
  roles: string[]
}

/** 更新组的 body（PUT /admin/groups/{code}）：code 不可改。 */
export interface GroupUpdateInput {
  name: string
  priority: number
  min_budget: number
  roles: string[]
}

/** 角色：`code` 是契约值（令牌、组、审计都用它），`name` 只用于展示。 */
export interface RoleItem {
  code: string
  name: string
  default_groups: string[]
  default_budget: number
}

/** GET /admin/roles：角色清单不分页。 */
export interface RolePage {
  items: RoleItem[]
}

/** MCP 接入令牌状态：expired 由后端按 expires_at 派生，revoked 为手工撤销。 */
export type AccessTokenStatus = 'active' | 'revoked' | 'expired'

/** GET /admin/tokens 的单条令牌记录；明文 token 只出现在创建响应里。 */
export interface AccessTokenItem {
  id: string
  name: string
  token_prefix: string
  tenant: string
  role: string
  groups: string[]
  builtin_tools: string[]
  source: string
  caller: string
  status: AccessTokenStatus
  created_at: string
  created_by: string
  expires_at: string | null
  last_used_at: string | null
  revoked_at: string | null
}

export interface AccessTokenPage {
  items: AccessTokenItem[]
  total: number
}

/** POST /admin/tokens 的 body；expires_at 为 RFC3339，留空表示永不过期。 */
export interface AccessTokenCreateInput {
  name: string
  role: string
  groups: string[]
  builtin_tools: string[]
  source: string
  caller: string
  expires_at?: string | null
}

/**
 * POST /admin/tokens 的 201 响应：token 只在这一次返回。
 * POST /admin/tokens/{id}/reissue 也复用该形状（200），token 同样只出现一次。
 */
export interface AccessTokenCreated {
  token: string
  item: AccessTokenItem
}

/** DELETE /admin/tokens/{id} 的 200 响应：记录被硬删除，审计事件另行保留。 */
export interface DeletedAccessToken {
  deleted: boolean
  id: string
}

/** options.roles 与 GET /admin/roles 同形：展示名由 name 提供，令牌里存 code。 */
export type TokenRoleOption = RoleItem

export interface TokenGroupOption {
  code: string
  name: string
  source: GroupSource
  priority: number
  min_budget: number
  token_count: number
}

/** options 里的内置工具；group=admin 的工具必须同时勾选 admin 组才能授权。 */
export interface TokenBuiltinToolOption {
  name: string
  label: string
  group: 'builtin' | 'admin'
}

/** GET /admin/tokens/options：角色默认组、组来源与可选内置工具。 */
export interface TokenOptions {
  roles: TokenRoleOption[]
  /** 组的权威来源是 GET /admin/groups；这里保留后端字段但不作为展示来源。 */
  groups: TokenGroupOption[]
  builtin_tools: TokenBuiltinToolOption[]
}

/* ----------------------------- MCP 上游接入 ------------------------------ */

/** 上游传输方式：http 走 URL，stdio 走 command + args。 */
export type UpstreamTransport = 'http' | 'stdio'

/** 上游分类：三组内置工具（basic/analysis/admin）与自定义接入。 */
export type UpstreamCategory = 'basic' | 'analysis' | 'admin' | 'custom'

/** 上游探测状态：unknown 表示尚未探测或后端无法判定。 */
export type UpstreamStatus = 'ok' | 'degraded' | 'error' | 'unknown'

/** GET /admin/upstreams 的单条上游记录。 */
export interface UpstreamItem {
  id: string
  name: string
  transport: UpstreamTransport
  url: string | null
  command: string | null
  args: string[]
  env: Record<string, string>
  headers: Record<string, string>
  group: string
  category: UpstreamCategory
  read_only: boolean
  enabled: boolean
  timeout_ms: number
  status: UpstreamStatus
  /** 探测结论的人话说明；正常时可能为空串。 */
  status_message: string
  tool_count: number
  last_checked_at: string | null
  updated_at: string
}

/**
 * POST/PUT /admin/upstreams 的 body。
 * `id` 仅创建时有意义；编辑时后端以路径参数为准。env/headers 的值支持 `env:NAME` 引用，
 * GET 会把 Authorization/Cookie 等敏感值显示为 `***`，原样回传即保持原值。
 */
export interface UpstreamInput {
  id: string
  name: string
  transport: UpstreamTransport
  url?: string | null
  command?: string | null
  args: string[]
  env: Record<string, string>
  headers: Record<string, string>
  group: string
  category: UpstreamCategory
  read_only: boolean
  enabled: boolean
  timeout_ms: number
}
