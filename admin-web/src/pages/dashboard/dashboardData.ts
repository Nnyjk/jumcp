import type { AuditChainState, AuditEnvelope } from '../../api/types'

/** 趋势只取一页样本：后端 audit limit 上限 200，避免把「最近 N 条」当成全量。 */
export const DASHBOARD_EVENT_LIMIT = 200

/** 治理动态合并两类事件后展示的条数。 */
export const GOVERNANCE_LIMIT = 8

/** 看板 KPI 里的审计链短文案；健康页用更长的解释性文案，色板两页共用。 */
export const CHAIN_LABEL: Record<AuditChainState, string> = {
  valid: '完整',
  tampered: '被篡改',
  error: '校验失败',
}

const COUNT_FORMAT = new Intl.NumberFormat('zh-CN')

export function formatCount(value: number): string {
  return COUNT_FORMAT.format(value)
}

export function windowStartIso(days: number): string {
  return new Date(Date.now() - days * 24 * 60 * 60 * 1000).toISOString()
}

function localDateKey(ts: string): string | undefined {
  const date = new Date(ts)
  if (Number.isNaN(date.getTime())) return undefined
  const month = String(date.getMonth() + 1).padStart(2, '0')
  const day = String(date.getDate()).padStart(2, '0')
  return `${date.getFullYear()}-${month}-${day}`
}

export interface TrendPoint {
  date: string
  count: number
}

/** 按浏览器本地日期把两类事件合并计数；没有事件的日期不补零（样本不代表全量）。 */
export function aggregateDaily(events: AuditEnvelope[]): TrendPoint[] {
  const counts = new Map<string, number>()
  for (const event of events) {
    const key = localDateKey(event.ts)
    if (key === undefined) continue
    counts.set(key, (counts.get(key) ?? 0) + 1)
  }
  return [...counts.entries()]
    .sort(([left], [right]) => left.localeCompare(right))
    .map(([date, count]) => ({ date, count }))
}
