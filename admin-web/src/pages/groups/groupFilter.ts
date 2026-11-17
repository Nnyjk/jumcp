import { ApiError } from '../../api/client'
import type { GroupItem, GroupQuery, GroupSource } from '../../api/types'
import { GROUP_SOURCE_LABEL } from '../../labels'
import { noticeFromError, type Notice } from '../../notice'

export type EditorTarget = { mode: 'create' } | { mode: 'edit'; group: GroupItem }

export interface GroupFilter {
  q: string
  source: '' | GroupSource
  role: string
}

export const EMPTY_FILTER: GroupFilter = { q: '', source: '', role: '' }

export const SOURCE_OPTIONS: Array<{ value: '' | GroupSource; label: string }> = [
  { value: '', label: '全部来源' },
  { value: 'policy', label: GROUP_SOURCE_LABEL.policy },
  { value: 'managed', label: GROUP_SOURCE_LABEL.managed },
  { value: 'builtin', label: GROUP_SOURCE_LABEL.builtin },
]

/** priority / min_budget 都是整数；后端要求 priority ≥ 0、min_budget > 0，空串不当作 0。 */
export function parseInteger(raw: string, minimum: number): number | undefined {
  if (raw.trim() === '') return undefined
  const value = Number(raw)
  return Number.isInteger(value) && value >= minimum ? value : undefined
}

export function filterToQuery(filter: GroupFilter): GroupQuery {
  return {
    q: filter.q.trim() || undefined,
    source: filter.source === '' ? undefined : filter.source,
    role: filter.role || undefined,
  }
}

function countText(value: unknown): string {
  return typeof value === 'number' || typeof value === 'string' ? String(value) : '—'
}

/** 409 表示组仍被 capability / token 引用：两个计数都展示，并保留后端原始消息。 */
export function groupDeleteError(error: unknown): Notice {
  if (error instanceof ApiError && error.status === 409) {
    const data = error.data as Record<string, unknown> | undefined
    const raw = error.message.trim()
    return {
      severity: 'danger',
      title: '组仍被引用',
      description: `删除被拒绝：能力 ${countText(data?.capability_count)} 个、令牌 ${countText(data?.token_count)} 个仍引用该组。${
        raw === '' ? '请先迁移或删除这些引用后重试。' : `后端提示：${raw}`
      }`,
      meta: `${error.code} · HTTP 409`,
    }
  }
  return noticeFromError(error, '删除组失败')
}
