import { ApiError } from '../../api/client'
import type { GroupItem, GroupQuery, GroupSource } from '../../api/types'
import { GROUP_SOURCE_LABEL } from '../../labels'
import { noticeMessage } from '../../notice'

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

/**
 * 数值边界约束：priority 与 min_budget 均为整型数值，
 * 业务规则要求 priority ≥ 0、min_budget > 0；空字符串必须显式解析为 undefined，不得隐式转换为 0，防止绕过必填校验。
 */
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

/**
 * 约束与引用保护：后端在组仍被受管能力或访问令牌引用时返回 409 并携带引用计数，禁止级联破坏鉴权拓扑；
 * 提示信息完整呈现受管能力与访问令牌的依赖数量，并保留后端原始错误原因，引导管理员先解绑依赖。
 */
export function groupDeleteMessage(error: unknown): string {
  if (error instanceof ApiError && error.status === 409) {
    const data = error.data as Record<string, unknown> | undefined
    const raw = error.message.trim()
    return `删除被拒绝：该组仍被 ${countText(data?.capability_count)} 个受管能力、${countText(
      data?.token_count,
    )} 个访问令牌引用。${raw === '' ? '请先解除或迁移相关引用后再删除。' : `后端提示：${raw}`}（${
      error.code
    } · HTTP 409）`
  }
  return noticeMessage(error, '删除组失败')
}
