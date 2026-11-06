import type { ReactNode } from 'react'

/**
 * 亚克力卡片：Mica 半透明层 + 1px 描边 + Fluent 双层浅阴影。
 * `flush` 用于表格卡片——内容自己贴边，卡片只保留边框与表头容器。
 * 纯展示容器：不绑定点击、不进入 Tab 顺序，因此不加 role/tabIndex；
 * 卡片内的按钮/链接各自承担交互与焦点。
 */
export function Card({
  title,
  subtitle,
  actions,
  footer,
  flush = false,
  children,
}: {
  title?: string
  subtitle?: string
  actions?: ReactNode
  footer?: ReactNode
  flush?: boolean
  children: ReactNode
}) {
  return (
    <section className="card">
      {(title !== undefined || subtitle !== undefined || actions !== undefined) && (
        <header className="card-head">
          {title !== undefined && <h2>{title}</h2>}
          {subtitle !== undefined && <span className="card-subtitle">{subtitle}</span>}
          {actions !== undefined && <div className="card-head-actions">{actions}</div>}
        </header>
      )}
      <div className={flush ? 'card-body card-body-flush' : 'card-body'}>{children}</div>
      {footer !== undefined && <footer className="card-foot">{footer}</footer>}
    </section>
  )
}
