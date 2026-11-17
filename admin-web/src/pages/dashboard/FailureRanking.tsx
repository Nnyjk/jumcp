import type { ToolStatsRow } from '../../api/types'
import { Card } from '../../components/Card'
import { EmptyState, ErrorState, LoadingState } from '../../components/PageShell'
import { formatPercent } from '../../format'
import { formatCount } from './dashboardData'

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
  return (
    <div className="dash-span-4">
      <Card title="失败榜" subtitle="按失败次数前 5">
        {statsPending && <LoadingState rows={4} label="正在加载失败榜" />}
        {statsError !== undefined && <ErrorState error={statsError} />}
        {hasStats && failureTools.length === 0 && (
          <EmptyState hint={`最近 ${windowDays} 天没有失败调用。`} />
        )}
        {failureTools.length > 0 && (
          <div className="fail-list">
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
      </Card>
    </div>
  )
}
