import type { ReactNode } from 'react'
import type { CapabilityRisk, ToolHealth, ToolSource, UpstreamStatus } from '../api/types'
import {
  LIFECYCLE_LABEL,
  RISK_LABEL,
  STATUS_TONE,
  TOOL_CATEGORY_LABEL,
  TOOL_HEALTH_LABEL,
  TOOL_OUTCOME_LABEL,
  TOOL_SOURCE_LABEL,
  UPSTREAM_STATUS_LABEL,
} from '../labels'

export type BadgeTone = 'neutral' | 'accent' | 'success' | 'warning' | 'danger'

/**
 * Fluent 药丸徽标：语义色只表达状态，不承担唯一信息载体（文案同时给出）。
 * 未知取值回退中性色并原样显示后端值，不隐藏、不猜中文名。
 * `title` 用于保留后端原值（如 status 的 draft/published）；`dot` 只是视觉点缀，aria-hidden。
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

/**
 * 生命周期徽标：取值可能是流程状态（草稿/待审/已发布/已下线），也可能是 builtin/unknown
 * ——内置能力没有流程状态，显示「内置」并不代表已发布。与来源徽标并排展示，避免二者混读。
 */
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

/** 风险等级徽标：read/write/sensitive/admin 是能力自报的风险等级，与工具分类（基础/分析/管理/自定义）不是一回事。 */
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

/** 上游探测状态：上游清单与基础工具面板共用，title 保留后端原值。 */
export function UpstreamStatusBadge({ status }: { status: UpstreamStatus }) {
  return (
    <Badge tone={STATUS_TONE[status] ?? 'neutral'} dot title={status}>
      {UPSTREAM_STATUS_LABEL[status] ?? status}
    </Badge>
  )
}
