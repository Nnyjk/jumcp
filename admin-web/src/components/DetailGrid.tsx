import type { ReactNode } from 'react'

/**
 * 详情 label/value 网格：每个字段自成一格，靠 CSS 在 1–4 列间自适应。
 * 用 dl > div > dt/dd 的分组写法，既保留语义又让每个字段成为栅格项。
 */
export function DetailGrid({ children }: { children: ReactNode }) {
  return <dl className="detail-grid">{children}</dl>
}

/**
 * 单个详情字段。`mono` 用于 ID/哈希/时间（等宽并允许 break-all），
 * `wide` 让代码块、JSON 这类长内容独占整行。
 */
export function DetailField({
  label,
  title,
  mono = false,
  wide = false,
  children,
}: {
  label: string
  /** 后端原始 key/原值：中文 label 之外仍可悬停看到契约字段名。 */
  title?: string | undefined
  mono?: boolean
  wide?: boolean
  children: ReactNode
}) {
  const className = ['detail-field', wide ? 'detail-field-wide' : ''].filter(Boolean).join(' ')
  return (
    <div className={className}>
      <dt>{label}</dt>
      <dd className={mono ? 'mono' : undefined} title={title}>
        {children}
      </dd>
    </div>
  )
}
