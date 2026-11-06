import { useId, type ReactNode } from 'react'
import { Icon, type IconName } from './Icon'

export type InfoBarSeverity = 'info' | 'success' | 'warning' | 'danger'

const SEVERITY_ICON: Record<InfoBarSeverity, IconName> = {
  info: 'info',
  success: 'success',
  warning: 'alert',
  danger: 'alert',
}

/**
 * Fluent InfoBar：图标 + 标题 + 描述 + 可选元信息（如审计 event_id）。
 * 即时性完全由 role 承担：danger=alert（assertive），其余=status（polite），不再重复 aria-live，
 * 否则同一消息会被读屏播报两次。标题/描述用内部生成 id 关联，调用方无需参与。
 */
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
  const titleId = useId()
  const detailId = useId()
  const hasDetail = description !== undefined || meta !== undefined || children !== undefined

  return (
    <div
      className={`infobar infobar-${severity}`}
      role={severity === 'danger' ? 'alert' : 'status'}
      aria-labelledby={titleId}
      aria-describedby={hasDetail ? detailId : undefined}
    >
      <span className="infobar-icon">
        <Icon name={SEVERITY_ICON[severity]} size={20} />
      </span>
      <div className="infobar-body">
        <p className="infobar-title" id={titleId}>
          {title}
        </p>
        {hasDetail && (
          <div id={detailId}>
            {description !== undefined && <p className="infobar-desc">{description}</p>}
            {meta !== undefined && <p className="infobar-meta">{meta}</p>}
            {children}
          </div>
        )}
      </div>
    </div>
  )
}
