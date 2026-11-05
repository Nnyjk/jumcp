/**
 * admin API 契约层（§4.4.4 admin_* 函数）：把 §4.4.4 的治理动作映射成 REST 调用。
 * 后端 admin HTTP 面尚未落地（W4.1），调用失败一律以 ApiError 呈现，页面据此显示错误态。
 */
import { request, requestText } from './client'
import type { AuditEnvelope, AuditQuery, CapabilityRecord } from './types'

/** 审核队列条目：draft / review 态的能力记录。 */
export interface ReviewQueueItem extends CapabilityRecord {
  submitted_by: string
  submitted_at: string
  approval_id: string
}

/** 拉取待审核队列（draft / review）。只读，写操作在 UI 上尚未接通。 */
export async function fetchReviewQueue(signal?: AbortSignal): Promise<ReviewQueueItem[]> {
  return request<ReviewQueueItem[]>('/admin/review-queue', { signal })
}

/** 按 §4.4.4 admin_audit_queries 的过滤条件查询审计。 */
export async function fetchAuditEvents(
  query: AuditQuery,
  signal?: AbortSignal,
): Promise<AuditEnvelope[]> {
  return request<AuditEnvelope[]>('/admin/audit-events', {
    params: {
      tenant: query.tenant,
      event_type: query.event_type,
      session_id: query.session_id,
      actor_role: query.actor_role,
    },
    signal,
  })
}

/** 拉取能力清单（例外处理视图的只读来源）。 */
export async function fetchCapabilities(signal?: AbortSignal): Promise<CapabilityRecord[]> {
  return request<CapabilityRecord[]>('/admin/capabilities', { signal })
}

/** 健康检查：后端当前返回纯文本 "ok"。 */
export async function fetchHealth(signal?: AbortSignal): Promise<string> {
  return requestText('/health', { signal })
}
