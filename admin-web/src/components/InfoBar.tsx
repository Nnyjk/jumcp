import type { ReactNode } from 'react'
import { Icon, type IconName } from './Icon'

export type InfoBarSeverity = 'info' | 'success' | 'warning' | 'danger'

const SEVERITY_ICON: Record<InfoBarSeverity, IconName> = {
  info: 'info',
  success: 'success',
  warning: 'alert',
  danger: 'alert',
}

/** Fluent InfoBar：图标 + 标题 + 描述 + 可选元信息（如审计 event_id）。 */
export function InfoBar({
  severity,
  title,
  description,
  meta,
  children,
}: {
  severity: InfoBarSeverity
  title: string
  description?: string
  meta?: string | undefined
  children?: ReactNode
}) {
  return (
    <div
      className={`infobar infobar-${severity}`}
      role={severity === 'danger' ? 'alert' : 'status'}
    >
      <span className="infobar-icon">
        <Icon name={SEVERITY_ICON[severity]} size={20} />
      </span>
      <div className="infobar-body">
        <p className="infobar-title">{title}</p>
        {description !== undefined && <p className="infobar-desc">{description}</p>}
        {meta !== undefined && <p className="infobar-meta">{meta}</p>}
        {children}
      </div>
    </div>
  )
}
