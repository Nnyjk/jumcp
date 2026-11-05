/**
 * admin API 契约层（§4.4.4 admin_* 函数）：把后端 admin HTTP 面的真实响应结构映射成类型化结果。
 * 写操作（submit / publish / visibility）本层不提供：管理台当前只读，操作按钮保持禁用。
 */
import { request } from './client'
import type {
  AuditPage,
  AuditQuery,
  CapabilityItem,
  CapabilityQuery,
  HealthPayload,
  Page,
  ReviewQueueItem,
} from './types'

/** 分页固定条数（后端默认 limit=50，上限 200）。 */
export const PAGE_LIMIT = 50

/** 拉取待审核队列（draft / review）。 */
export async function fetchReviewQueue(signal?: AbortSignal): Promise<Page<ReviewQueueItem>> {
  return request<Page<ReviewQueueItem>>('/admin/review-queue', { signal })
}

/** 按 §4.4.4 admin_audit_queries 的过滤条件分页查询审计。 */
export async function fetchAuditEvents(
  query: AuditQuery,
  offset: number,
  signal?: AbortSignal,
): Promise<AuditPage> {
  return request<AuditPage>('/admin/audit-events', {
    params: {
      tenant: query.tenant,
      event_type: query.event_type,
      session_id: query.session_id,
      actor_role: query.actor_role,
      ts_after: query.ts_after,
      ts_before: query.ts_before,
      limit: PAGE_LIMIT,
      offset,
    },
    signal,
  })
}

/** 能力清单（例外处理视图的只读来源）。 */
export async function fetchCapabilities(
  query: CapabilityQuery,
  offset: number,
  signal?: AbortSignal,
): Promise<Page<CapabilityItem>> {
  return request<Page<CapabilityItem>>('/admin/capabilities', {
    params: {
      status: query.status,
      group: query.group,
      q: query.q,
      limit: PAGE_LIMIT,
      offset,
    },
    signal,
  })
}

/** 健康检查：后端返回结构化 JSON（含审计链校验结论）。 */
export async function fetchHealth(signal?: AbortSignal): Promise<HealthPayload> {
  return request<HealthPayload>('/health', { signal })
}
