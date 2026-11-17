import type { ReactNode } from 'react'
import { gsap } from 'gsap'
import { ApiError } from '../api/client'
import { Icon } from './Icon'
import { MOTION_DURATION, MOTION_EASE, useGsapScope } from '../motion/useGsap'

/** 页面内容容器：16px 栅格；进入时对直接子块做一次性透明度/位移 stagger。 */
export function PageShell({ children }: { children: ReactNode }) {
  const pageRef = useGsapScope<HTMLDivElement>((scope) => {
    gsap.from(scope.children, {
      opacity: 0,
      y: 10,
      duration: MOTION_DURATION.enter,
      ease: MOTION_EASE,
      stagger: 0.04,
      clearProps: 'opacity,transform',
    })
  })

  return (
    <div className="page" ref={pageRef}>
      {children}
    </div>
  )
}

const STATUS_HINT: Record<number, string> = {
  401: '管理面按 Bearer token 判定身份。前端不持有凭证：请确认网关/BFF 已注入 Authorization，本地开发用 JUMCP_DEV_ADMIN_TOKEN 启动。',
  403: '当前主体没有该 admin 接口的角色授权；拒绝已写入 policy_decision 审计。',
}

/**
 * 错误态：原样呈现 ApiError 的 HTTP 状态与业务错误码，并针对 401/403/网络失败给出处置线索。
 * 不吞异常、不伪造默认数据。
 */
export function ErrorState({ error }: { error: unknown }) {
  const apiError = error instanceof ApiError ? error : undefined
  const network = apiError?.status === 0
  const title = apiError?.status === 401
    ? '未授权访问管理面'
    : network
      ? '无法连接后端'
      : '请求失败'
  const hint = apiError === undefined ? undefined : STATUS_HINT[apiError.status]

  return (
    <div className="state state-error" role="alert">
      <span className="state-icon">
        <Icon name="alert" size={22} />
      </span>
      <div>
        <strong>{title}</strong>
        <p>{apiError?.message ?? (error instanceof Error ? error.message : String(error))}</p>
        {apiError !== undefined && (
          <div className="state-meta">
            <span className="badge badge-danger">{network ? '网络错误' : `HTTP ${apiError.status}`}</span>
            <code className="code-chip">{apiError.code}</code>
          </div>
        )}
        {hint !== undefined && <p className="hint">{hint}</p>}
      </div>
    </div>
  )
}

/** 空态：图标 + 说明 + 可选建议动作。 */
export function EmptyState({ hint, action }: { hint: string; action?: ReactNode }) {
  return (
    <div className="empty-state" role="status" aria-live="polite">
      <span className="empty-icon">
        <Icon name="inbox" size={40} />
      </span>
      <strong>暂无数据</strong>
      <p>{hint}</p>
      {action}
    </div>
  )
}

/** 加载态：骨架条，不遮全屏；aria-live 让读屏在结果到达前得到提示。 */
export function LoadingState({ rows = 5, label = '正在加载' }: { rows?: number; label?: string }) {
  return (
    <div className="loading-state" role="status" aria-live="polite" aria-busy="true">
      <span className="sr-only">{label}</span>
      <div className="skeleton-stack" aria-hidden="true">
        {Array.from({ length: rows }, (_, index) => (
          <span key={index} className="skeleton-line" />
        ))}
      </div>
    </div>
  )
}

/** 分页：底部说明「第 x–y 条 / 共 N 条」+ 上一页/下一页（后端 total 是过滤后的全量条数）。 */
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
  // offset 可能因数据变动落到总数之外（例如最后一页的记录被删除）：范围文案按总数钳制，避免出现「第 251–200 条」。
  const from = total === 0 ? 0 : Math.min(offset + 1, total)
  const to = Math.min(offset + limit, total)
  const canPrev = offset > 0
  const canNext = offset + limit < total

  return (
    <div className="pagination">
      <span className="pagination-info">
        第 {from}–{to} 条 / 共 {total} 条
      </span>
      <div className="pagination-actions">
        <button
          type="button"
          className="btn btn-secondary btn-sm"
          disabled={!canPrev}
          onClick={() => onChange(Math.max(0, offset - limit))}
        >
          <Icon name="chevron-left" size={16} />
          上一页
        </button>
        <button
          type="button"
          className="btn btn-secondary btn-sm"
          disabled={!canNext}
          onClick={() => onChange(offset + limit)}
        >
          下一页
          <Icon name="chevron-right" size={16} />
        </button>
      </div>
    </div>
  )
}
