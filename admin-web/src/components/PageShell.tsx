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

/** 极简分页：上一页 / 下一页 + 总条数（后端 total 是过滤后的全量条数）。 */
export function Pagination({
  total,
  offset,
  limit,
  onChange,
}: {
  total: number
  offset: number
  limit: number
  onChange: (nextOffset: number) => void
}) {
  const from = total === 0 ? 0 : offset + 1
  const to = Math.min(offset + limit, total)
  const canPrev = offset > 0
  const canNext = offset + limit < total

  return (
    <div className="pagination">
      <button
        type="button"
        className="btn btn-secondary"
        disabled={!canPrev}
        onClick={() => onChange(Math.max(0, offset - limit))}
      >
        上一页
      </button>
      <button
        type="button"
        className="btn btn-secondary"
        disabled={!canNext}
        onClick={() => onChange(offset + limit)}
      >
        下一页
      </button>
      <span className="hint">
        第 {from}-{to} 条 / 共 {total} 条（每页 {limit}）
      </span>
    </div>
  )
}
