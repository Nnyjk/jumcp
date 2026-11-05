/** 领域类型：与 §4.4.3 审计 envelope、§4.4.1 能力记录一一对应。 */

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

/** §4.4.4 admin_audit_queries 支持的过滤条件。 */
export interface AuditQuery {
  tenant?: string
  event_type?: string
  session_id?: string
  actor_role?: string
}

/** §4.4.1 生命周期状态（与双布尔正交）。 */
export type CapabilityStatus = 'draft' | 'review' | 'published' | 'deprecated'

export type CapabilityKind = 'tool' | 'template' | 'endpoint'

export type CapabilityRisk = 'read' | 'write' | 'sensitive' | 'admin'

/** §4.4.1 能力记录。 */
export interface CapabilityRecord {
  capability_id: string
  kind: CapabilityKind
  group: string
  risk: CapabilityRisk
  status: CapabilityStatus
  discoverable: boolean
  callable: boolean
  version: number
  definition_hash: string
  successor_id: string
}
