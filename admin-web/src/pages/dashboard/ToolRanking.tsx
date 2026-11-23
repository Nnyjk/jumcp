import { gsap } from 'gsap'
import type { ToolStatsRow } from '../../api/types'
import { Card } from '../../components/Card'
import { EmptyState, ErrorState, LoadingState } from '../../components/PageShell'
import { formatPercent } from '../../format'
import { MOTION_DURATION, MOTION_EASE } from '../../motion/useGsap'
import { useListMotion } from './useListMotion'

/** 工具调用排行：按调用次数前 8，条宽以窗口内最大调用量为基准。 */
export function ToolRanking({
  windowDays,
  topTools,
  maxCalls,
  statsPending,
  statsError,
  hasStats,
}: {
  windowDays: number
  topTools: ToolStatsRow[]
  maxCalls: number
  statsPending: boolean
  statsError: unknown
  hasStats: boolean
}) {
  // 签名 = 行 key + 调用次数：行没变时签名不变（轮询不重播入场），
  // 调用量变了签名也变，走一次高亮提示而不是重播。
  const rowSignature = topTools.map((row) => `${row.id}:${row.calls}`).join('\u0000')
  const listRef = useListMotion<HTMLUListElement>(rowSignature, (scope) => {
    const items = Array.from(scope.children)
    if (items.length === 0) return
    gsap.from(items, {
      opacity: 0,
      y: 6,
      duration: MOTION_DURATION.row,
      ease: MOTION_EASE,
      stagger: 0.02,
      clearProps: 'opacity,transform',
    })
    // 条宽用 scaleX 展开：轨道宽度固定，缩放不触发布局重排。
    gsap.from(scope.querySelectorAll<HTMLElement>('.rank-bar'), {
      scaleX: 0,
      duration: MOTION_DURATION.enter,
      ease: MOTION_EASE,
      stagger: 0.02,
      transformOrigin: 'left center',
      clearProps: 'transform,transformOrigin',
    })
  })

  return (
    <div className="dash-span-4">
      <Card title="工具调用排行" subtitle="按调用次数前 8">
        <div className="dash-panel dash-panel-tools">
          {statsPending && <LoadingState rows={5} label="正在加载工具排名" />}
          {statsError !== undefined && <ErrorState error={statsError} />}
          {hasStats && topTools.length === 0 && (
            <EmptyState hint={`最近 ${windowDays} 天没有调用记录，无法排名。`} />
          )}
          {topTools.length > 0 && (
            <ul className="rank-list" ref={listRef}>
              {topTools.map((row) => {
                const rate = row.success_rate ?? (row.calls > 0 ? row.success / row.calls : null)
                const barWidth = row.calls > 0 ? (row.calls / maxCalls) * 100 : 0
                const successShare = row.calls > 0 ? (row.success / row.calls) * 100 : 0
                const failureShare = row.calls > 0 ? (row.failure / row.calls) * 100 : 0
                return (
                  <li key={row.id} className="rank-item">
                    <span className="rank-id" title={row.id}>
                      {row.id}
                    </span>
                    <div
                      className="rank-track"
                      title={`calls=${row.calls} · success=${row.success} · failure=${row.failure}`}
                    >
                      <div className="rank-bar" style={{ width: `${barWidth}%` }}>
                        <span
                          className="rank-seg rank-seg-success"
                          style={{ flexBasis: `${successShare}%` }}
                        />
                        <span
                          className="rank-seg rank-seg-failure"
                          style={{ flexBasis: `${failureShare}%` }}
                        />
                      </div>
                    </div>
                    <span className="rank-rate" title={`成功率 ${formatPercent(rate)}`}>
                      {formatPercent(rate)}
                    </span>
                  </li>
                )
              })}
            </ul>
          )}
        </div>
      </Card>
    </div>
  )
}
