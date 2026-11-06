import type { ReactNode } from 'react'
import type { CapabilityRisk, CapabilityStatus } from '../api/types'
import { RISK_LABEL, STATUS_LABEL } from '../labels'

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

const STATUS_TONE: Record<CapabilityStatus, BadgeTone> = {
  draft: 'neutral',
  review: 'warning',
  published: 'success',
  deprecated: 'neutral',
}

export function StatusBadge({ status }: { status: CapabilityStatus }) {
  return (
    <Badge tone={STATUS_TONE[status]} dot title={status}>
      {STATUS_LABEL[status]}
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
    <Badge tone={RISK_TONE[risk]} title={risk}>
      {RISK_LABEL[risk]}
    </Badge>
  )
}
