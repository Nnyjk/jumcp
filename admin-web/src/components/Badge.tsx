import type { ReactNode } from 'react'
import type { CapabilityRisk, CapabilityStatus } from '../api/types'

export type BadgeTone = 'neutral' | 'accent' | 'success' | 'warning' | 'danger'

/** Fluent 药丸徽标：语义色只表达状态，不承担唯一信息载体（文案同时给出）。 */
export function Badge({
  tone = 'neutral',
  dot = false,
  children,
}: {
  tone?: BadgeTone
  dot?: boolean
  children: ReactNode
}) {
  return (
    <span className={`badge badge-${tone}`}>
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
    <Badge tone={STATUS_TONE[status]} dot>
      {status}
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
  return <Badge tone={RISK_TONE[risk]}>{risk}</Badge>
}
