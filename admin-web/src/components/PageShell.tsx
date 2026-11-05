import type { ReactNode } from 'react'
import { ApiError } from '../api/client'

/** 页面骨架头：标题 + 只读/未实现标识（§4.4.5 禁止假装可用）。 */
export function PageShell({
  title,
  mode,
  description,
  children,
}: {
  title: string
  mode: '只读' | '未实现'
  description: string
  children: ReactNode
}) {
  return (
    <section className="page">
      <div className="page-head">
        <h2>{title}</h2>
        <span className={mode === '只读' ? 'badge badge-readonly' : 'badge badge-skeleton'}>
          {mode === '只读' ? '只读视图' : '未实现'}
        </span>
      </div>
      <p className="page-desc">{description}</p>
      {children}
    </section>
  )
}

/** 错误态：把 ApiError 的状态码/错误码原样呈现。 */
export function ErrorState({ error }: { error: unknown }) {
  if (error instanceof ApiError) {
    return (
      <div className="state state-error">
        <strong>请求失败</strong>
        <span>
          HTTP {error.status} · {error.code}
        </span>
        <span>{error.message}</span>
      </div>
    )
  }
  return (
    <div className="state state-error">
      <strong>请求失败</strong>
      <span>{error instanceof Error ? error.message : String(error)}</span>
    </div>
  )
}

/** 空态。 */
export function EmptyState({ hint }: { hint: string }) {
  return (
    <div className="state state-empty">
      <strong>暂无数据</strong>
      <span>{hint}</span>
    </div>
  )
}

/** 加载态。 */
export function LoadingState() {
  return <div className="state state-loading">加载中…</div>
}

/** 占位操作按钮：显式标注未接通，避免被误认为可用。 */
export function DisabledAction({ label }: { label: string }) {
  return (
    <button type="button" className="btn btn-disabled" disabled title="尚未接通后端 admin API">
      {label}（未接通）
    </button>
  )
}
