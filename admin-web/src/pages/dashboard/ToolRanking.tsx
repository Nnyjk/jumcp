import type { ToolStatsRow } from '../../api/types'
import { Card } from '../../components/Card'
import { EmptyState, ErrorState, LoadingState } from '../../components/PageShell'
import { formatPercent } from '../../format'

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
  return (
    <div className="dash-span-4">
      <Card title="工具调用排行" subtitle="按调用次数前 8">
        {statsPending && <LoadingState rows={5} label="正在加载工具排名" />}
        {statsError !== undefined && <ErrorState error={statsError} />}
        {hasStats && topTools.length === 0 && (
          <EmptyState hint={`最近 ${windowDays} 天没有调用记录，无法排名。`} />
        )}
        {topTools.length > 0 && (
          <ul className="rank-list">
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
      </Card>
    </div>
  )
}
