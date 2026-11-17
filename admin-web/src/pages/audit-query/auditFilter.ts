import type { AuditOrder, AuditQuery } from '../../api/types'
import { localInputToIso, minutesAgoIso } from '../../format'

export type TimePreset = 'all' | '15m' | '1h' | '24h' | '7d' | 'custom'

export const TIME_PRESETS: Array<{ value: TimePreset; label: string }> = [
  { value: '15m', label: '最近 15 分钟' },
  { value: '1h', label: '最近 1 小时' },
  { value: '24h', label: '最近 24 小时' },
  { value: '7d', label: '最近 7 天' },
  { value: 'custom', label: '自定义范围' },
  { value: 'all', label: '不限时间' },
]

export const PRESET_MINUTES: Record<Exclude<TimePreset, 'all' | 'custom'>, number> = {
  '15m': 15,
  '1h': 60,
  '24h': 24 * 60,
  '7d': 7 * 24 * 60,
}

export interface FilterState {
  tenant: string
  event_type: string
  actor_role: string
  actor_id: string
  token_id: string
  object_type: string
  object_id: string
  event_id: string
  order: AuditOrder
  preset: TimePreset
  customFrom: string
  customTo: string
}

export const EMPTY_FILTER: FilterState = {
  tenant: '',
  event_type: '',
  actor_role: '',
  actor_id: '',
  token_id: '',
  object_type: '',
  object_id: '',
  event_id: '',
  order: 'desc',
  preset: 'all',
  customFrom: '',
  customTo: '',
}

/** 视图栈入口（访问令牌 / 能力档案）带入的预设筛选。 */
export interface AuditInitialFilter {
  tokenId?: string | undefined
  objectId?: string | undefined
  eventId?: string | undefined
}

/** URL query（event_id / object_id / token_id）用于从调用详情、令牌与能力档案深链进入时预填筛选。 */
export function filterFromUrl(params: URLSearchParams): FilterState {
  return {
    ...EMPTY_FILTER,
    event_id: params.get('event_id') ?? '',
    object_id: params.get('object_id') ?? '',
    object_type: params.get('object_type') ?? '',
    token_id: params.get('token_id') ?? '',
  }
}

/** 视图栈入口带来的预设筛选优先；直接访问 /audit?event_id=… 时退回 URL 参数。 */
export function initialFilter(
  initial: AuditInitialFilter | undefined,
  params: URLSearchParams,
): FilterState {
  if (initial === undefined) return filterFromUrl(params)
  return {
    ...EMPTY_FILTER,
    token_id: initial.tokenId ?? '',
    object_id: initial.objectId ?? '',
    event_id: initial.eventId ?? '',
  }
}

/** 预设/自定义时间范围 → RFC3339；「不限」不下发时间参数。 */
export function queryFromFilter(filter: FilterState): AuditQuery {
  const query: AuditQuery = {
    tenant: filter.tenant.trim() || undefined,
    event_type: filter.event_type || undefined,
    actor_role: filter.actor_role.trim() || undefined,
    actor_id: filter.actor_id.trim() || undefined,
    token_id: filter.token_id.trim() || undefined,
    object_type: filter.object_type || undefined,
    object_id: filter.object_id.trim() || undefined,
    event_id: filter.event_id.trim() || undefined,
    order: filter.order,
  }
  if (filter.preset === 'custom') {
    const after = localInputToIso(filter.customFrom)
    const before = localInputToIso(filter.customTo)
    if (after !== undefined) query.ts_after = after
    if (before !== undefined) query.ts_before = before
  } else if (filter.preset !== 'all') {
    query.ts_after = minutesAgoIso(PRESET_MINUTES[filter.preset])
  }
  return query
}
