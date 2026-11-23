import type { ReactNode } from 'react'
import { gsap } from 'gsap'
import { ApiError } from '../api/client'
import { Icon } from './Icon'
import { MOTION_DURATION, MOTION_EASE, useGsapScope } from '../motion/useGsap'

/**
 * 页面内容容器：纵向堆叠，块间距 16px（不是 CSS grid，列布局交给内部卡片）。
 * 进入时对直接子元素做一次透明度/位移 stagger——动画只认顶层子块，包在子组件内部的元素不参与；
 * 只播一次，不随数据到达重播（表格另有自己的行动画）。reduced-motion 下不注册动画，元素直接是最终态。
 */
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
  401: '会话已过期，请重新登录。',
  403: '当前主体没有该 admin 接口的角色授权；拒绝已写入 policy_decision 审计。',
}

/**
 * 错误态：原样呈现 ApiError 的 HTTP 状态与业务错误码，并针对 401/403/网络失败给出处置线索。
 * 不吞异常、不伪造默认数据；未知错误（非 ApiError）只显示 message，不给状态徽标。
 * 标题按状态分三档：401「未授权访问管理面」、status 0「无法连接后端」、其余「请求失败」。
 * role="alert" 让错误立即被读出；重试按钮不在本组件里，由调用方按需附加。
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

/** 空态：图标 + 固定标题「暂无数据」+ 说明 + 可选建议动作；role="status" 让读屏在数据为空时读出 hint。 */
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

/**
 * 加载态：骨架条，不遮全屏、不锁滚动；rows 只决定骨架条数，label 只给读屏
 * （role="status" + aria-live="polite" + aria-busy，骨架本身 aria-hidden）。
 * 表格类区域用它占位，避免高度先塌再撑开。
 */
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

/**
 * 分页：底部说明「第 x–y 条 / 共 N 条」+ 上一页/下一页。
 * 受控组件，不持有页码：onChange 给的是下一个 offset（不是页码），调用方直接把它写进查询参数。
 * limit 由调用方固定，后端 total 是过滤后的全量条数；首页无上一页、末页无下一页时按钮 disabled。
 * 说明文案按 total 钳制 offset：数据变动让 offset 落到总数之外时不会出现「第 251–200 条」。
 */
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
  // offset 可能因数据变动落到总数之外（例如最后一页的记录被删除）：范围文案按总数钳制。
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
