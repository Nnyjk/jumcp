/** 领域类型：与后端 admin API 的真实响应结构一一对应。 */

/** §4.4.3 event_type 闭集。 */
export type AuditEventType =
  | 'session_init'
  | 'tools_list_served'
  | 'capability_search'
  | 'capability_describe'
  | 'tool_call'
  | 'template_run'
  | 'policy_decision'
  | 'admin_change'

/** §4.4.3 actor{type,id,role}。 */
export interface AuditActor {
  type: string
  id: string
  role: string
}

/** §4.4.3 审计 envelope（追加表单条记录的完整形态）。 */
export interface AuditEnvelope {
  event_id: string
  ts: string
  tenant: string
  actor: AuditActor
  session_id: string
  event_type: AuditEventType
  payload: Record<string, unknown>
  prev_hash: string
  hash: string
}

/** 分页响应：列表类接口统一 {items,total,limit,offset}。 */
export interface Page<T> {
  items: T[]
  total: number
  limit: number
  offset: number
}

/** 审计查询响应：事件数组键名是 events，不是 items。 */
export interface AuditPage {
  events: AuditEnvelope[]
  total: number
  limit: number
  offset: number
}

/** §4.4.4 admin_audit_queries 支持的过滤条件（后端 FILTER_KEYS 白名单）。 */
export interface AuditQuery {
  tenant?: string
  event_type?: string
  session_id?: string
  actor_role?: string
  ts_after?: string
  ts_before?: string
}

/** §4.4.1 生命周期状态（与双布尔正交）。 */
export type CapabilityStatus = 'draft' | 'review' | 'published' | 'deprecated'

export type CapabilityKind = 'tool' | 'template' | 'endpoint'

export type CapabilityRisk = 'read' | 'write' | 'sensitive' | 'admin'

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
  updated: string
}

/** 审核队列条目：draft / review 的摘要视图。 */
export interface ReviewQueueItem {
  id: string
  kind: CapabilityKind
  group: string
  risk: CapabilityRisk
  status: CapabilityStatus
  version: number
  updated: string
  definition_hash: string
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
  audit_chain_detail?: string
}
