import type { ReactNode } from 'react'
import type { CapabilityRisk, ToolHealth, ToolSource } from '../api/types'
import {
  LIFECYCLE_LABEL,
  RISK_LABEL,
  TOOL_CATEGORY_LABEL,
  TOOL_HEALTH_LABEL,
  TOOL_OUTCOME_LABEL,
  TOOL_SOURCE_LABEL,
} from '../labels'

export type BadgeTone = 'neutral' | 'accent' | 'success' | 'warning' | 'danger'

/**
 * Fluent 药丸徽标：语义色只表达状态，不承担唯一信息载体（文案同时给出）。
 * `title` 用于保留后端原值（如 status 的 draft/published）。
 */
export function Badge({
  tone = 'neutral',
  dot = false,
  title,
  children,
}: {
  tone?: BadgeTone
  dot?: boolean
  title?: string
  children: ReactNode
}) {
  return (
    <span className={`badge badge-${tone}`} title={title}>
      {dot && <span className="badge-dot" aria-hidden="true" />}
      {children}
    </span>
  )
}

const LIFECYCLE_TONE: Record<string, BadgeTone> = {
  draft: 'neutral',
  review: 'warning',
  published: 'success',
  deprecated: 'neutral',
  builtin: 'accent',
  unknown: 'neutral',
}

/** 生命周期徽标：与「来源」分列，避免把内置误读成已发布。 */
export function LifecycleBadge({ status }: { status: string }) {
  return (
    <Badge tone={LIFECYCLE_TONE[status] ?? 'neutral'} dot title={status}>
      {LIFECYCLE_LABEL[status] ?? status}
    </Badge>
  )
}

const RISK_TONE: Record<CapabilityRisk, BadgeTone> = {
  read: 'accent',
  write: 'warning',
  sensitive: 'danger',
  admin: 'danger',
}

export function RiskBadge({ risk }: { risk: CapabilityRisk }) {
  return (
    <Badge tone={RISK_TONE[risk] ?? 'neutral'} title={risk}>
      {RISK_LABEL[risk] ?? risk}
    </Badge>
  )
}

const HEALTH_TONE: Record<ToolHealth, BadgeTone> = {
  ok: 'success',
  degraded: 'warning',
  failing: 'danger',
  unknown: 'neutral',
}

export function HealthBadge({ health }: { health: ToolHealth }) {
  return (
    <Badge tone={HEALTH_TONE[health] ?? 'neutral'} dot title={health}>
      {TOOL_HEALTH_LABEL[health] ?? health}
    </Badge>
  )
}

const OUTCOME_TONE: Record<string, BadgeTone> = {
  ok: 'success',
  summary: 'accent',
  error: 'danger',
  denied: 'warning',
  budget_rejected: 'warning',
  unknown: 'neutral',
}

export function OutcomeBadge({ outcome }: { outcome: string | null | undefined }) {
  if (outcome === null || outcome === undefined || outcome === '') {
    return <span className="muted">—</span>
  }
  return (
    <Badge tone={OUTCOME_TONE[outcome] ?? 'neutral'} title={outcome}>
      {TOOL_OUTCOME_LABEL[outcome] ?? outcome}
    </Badge>
  )
}

/** 来源：builtin 内置（进程内注册）/ managed 受管（管理台维护）。 */
export function SourceBadge({ source }: { source: ToolSource }) {
  return (
    <Badge tone={source === 'builtin' ? 'accent' : 'neutral'} title={source}>
      {TOOL_SOURCE_LABEL[source] ?? source}
    </Badge>
  )
}

const CATEGORY_TONE: Record<string, BadgeTone> = {
  basic: 'accent',
  analysis: 'success',
  admin: 'warning',
  custom: 'neutral',
}

/** 上游分类：三组内置分类与自定义接入都有中文名，徽标与 title 只给中文；未知值才原样回退。 */
export function CategoryBadge({ category }: { category: string }) {
  const label = TOOL_CATEGORY_LABEL[category]
  return (
    <Badge tone={CATEGORY_TONE[category] ?? 'neutral'} title={label ?? category}>
      {label ?? category}
    </Badge>
  )
}
