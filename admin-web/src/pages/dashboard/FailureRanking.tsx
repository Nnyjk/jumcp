import { gsap } from 'gsap'
import type { ToolStatsRow } from '../../api/types'
import { Card } from '../../components/Card'
import { EmptyState, ErrorState, LoadingState } from '../../components/PageShell'
import { formatPercent } from '../../format'
import { MOTION_DURATION, MOTION_EASE } from '../../motion/useGsap'
import { formatCount } from './dashboardData'
import { useListMotion } from './useListMotion'

/** 失败榜：按失败次数前 5，只列有失败的工具。 */
export function FailureRanking({
  windowDays,
  failureTools,
  statsPending,
  statsError,
  hasStats,
}: {
  windowDays: number
  failureTools: ToolStatsRow[]
  statsPending: boolean
  statsError: unknown
  hasStats: boolean
}) {
  // 签名 = 行 key + 失败次数：行没变时签名不变（轮询不重播入场），
  // 计数变了走一次高亮提示而不是重播。
  const rowSignature = failureTools.map((row) => `${row.id}:${row.failure}`).join('\u0000')
  const listRef = useListMotion<HTMLDivElement>(rowSignature, (scope) => {
    gsap.from(Array.from(scope.children), {
      opacity: 0,
      y: 6,
      duration: MOTION_DURATION.row,
      ease: MOTION_EASE,
      stagger: 0.02,
      clearProps: 'opacity,transform',
    })
  })

  return (
    <div className="dash-span-4">
      <Card title="失败榜" subtitle="按失败次数前 5">
        <div className="dash-panel dash-panel-failures">
          {statsPending && <LoadingState rows={4} label="正在加载失败榜" />}
          {statsError !== undefined && <ErrorState error={statsError} />}
          {hasStats && failureTools.length === 0 && (
            <EmptyState hint={`最近 ${windowDays} 天没有失败调用。`} />
          )}
          {failureTools.length > 0 && (
            <div className="fail-list" ref={listRef}>
              <div className="fail-row fail-head" aria-hidden="true">
                <span>工具</span>
                <span className="fail-num">失败</span>
                <span className="fail-num">调用</span>
                <span className="fail-num">成功率</span>
              </div>
              {failureTools.map((row) => (
                <div
                  key={row.id}
                  className="fail-row"
                  title={`${row.id}：failure=${row.failure} calls=${row.calls}`}
                >
                  <span className="rank-id">{row.id}</span>
                  <span className="fail-num fail-num-danger">{formatCount(row.failure)}</span>
                  <span className="fail-num">{formatCount(row.calls)}</span>
                  <span className="fail-num">
                    {formatPercent(row.calls > 0 ? row.success / row.calls : null)}
                  </span>
                </div>
              ))}
            </div>
          )}
        </div>
      </Card>
    </div>
  )
}
