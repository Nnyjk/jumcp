import { gsap } from 'gsap'
import type { AuditChainState, ToolStatsPayload, ToolStatsRow } from '../../api/types'
import { Badge } from '../../components/Badge'
import { Card } from '../../components/Card'
import { EmptyState, ErrorState, LoadingState } from '../../components/PageShell'
import { formatDateTime, formatPercent } from '../../format'
import { CHAIN_TONE } from '../../labels'
import { MOTION_DURATION, MOTION_EASE, useGsapScope } from '../../motion/useGsap'
import { CHAIN_LABEL, formatCount } from './dashboardData'

/** 关键指标卡：统计行只来自 tool-stats，审批/草稿与审计链依赖队列和健康检查。 */
export function KpiGrid({
  windowDays,
  stats,
  rows,
  statsPending,
  statsError,
  reviewPending,
  reviewError,
  reviewCount,
  draftCount,
  chain,
  chainHint,
}: {
  windowDays: number
  stats: ToolStatsPayload | undefined
  rows: ToolStatsRow[]
  statsPending: boolean
  statsError: unknown
  reviewPending: boolean
  reviewError: boolean
  reviewCount: number
  draftCount: number
  chain: AuditChainState | undefined
  chainHint: string
}) {
  // 依赖 stats 是否已到达：只在首次拿到数据时播一次，轮询刷新不重播。
  const kpiRef = useGsapScope<HTMLDivElement>(
    (scope) => {
      gsap.from(scope.children, {
        opacity: 0,
        y: 10,
        duration: MOTION_DURATION.enter,
        ease: MOTION_EASE,
        stagger: 0.02,
        clearProps: 'opacity,transform',
      })
    },
    [stats !== undefined],
  )

  const totalCalls = rows.reduce((sum, row) => sum + row.calls, 0)
  const totalSuccess = rows.reduce((sum, row) => sum + row.success, 0)
  const totalFailure = rows.reduce((sum, row) => sum + row.failure, 0)
  const activeTools = rows.filter((row) => row.calls > 0).length
  const successRate = totalCalls > 0 ? totalSuccess / totalCalls : null

  return (
    <div className="dash-span-12">
      <Card
        title="关键指标"
        subtitle={
          stats === undefined
            ? undefined
            : `窗口 ${formatDateTime(stats.window.from)} → ${formatDateTime(stats.window.to)}`
        }
      >
        <div className="dash-panel dash-panel-kpi">
          {statsPending && <LoadingState rows={3} label="正在加载关键指标" />}
          {statsError !== undefined && <ErrorState error={statsError} />}
          {stats !== undefined && rows.length === 0 && (
            <EmptyState
              hint={`最近 ${windowDays} 天没有工具统计行，无法计算调用量与成功率。`}
            />
          )}
          {stats !== undefined && rows.length > 0 && (
            <>
              <div className="kpi-grid" ref={kpiRef}>
                <div className="kpi">
                  <span className="kpi-label">总调用</span>
                  <div className="kpi-value">
                    <span className="mono">{formatCount(totalCalls)}</span>
                  </div>
                  <span className="kpi-hint">
                    时间窗 {windowDays} 天 · {formatCount(rows.length)} 个工具
                  </span>
                </div>
                <div className="kpi">
                  <span className="kpi-label">成功率</span>
                  <div className="kpi-value">
                    <span className="mono">{formatPercent(successRate)}</span>
                  </div>
                  <span className="kpi-hint">
                    成功 {formatCount(totalSuccess)} / 调用 {formatCount(totalCalls)}
                  </span>
                </div>
                <div className="kpi">
                  <span className="kpi-label">失败数</span>
                  <div className="kpi-value">
                    <span className="mono">{formatCount(totalFailure)}</span>
                  </div>
                  <span className="kpi-hint">
                    失败率 {formatPercent(totalCalls > 0 ? totalFailure / totalCalls : null)}
                  </span>
                </div>
                <div className="kpi">
                  <span className="kpi-label">活跃工具</span>
                  <div className="kpi-value">
                    <span className="mono">{formatCount(activeTools)}</span>
                  </div>
                  <span className="kpi-hint">调用 &gt; 0 · 共 {formatCount(rows.length)} 个</span>
                </div>
                <div className="kpi">
                  <span className="kpi-label">待审批</span>
                  <div className="kpi-value">
                    {reviewPending ? (
                      <span className="kpi-muted">…</span>
                    ) : reviewError ? (
                      <span className="kpi-muted">—</span>
                    ) : (
                      <span className="mono">{formatCount(reviewCount)}</span>
                    )}
                  </div>
                  <span className="kpi-hint">队列全量，不受时间窗限制</span>
                </div>
                <div className="kpi">
                  <span className="kpi-label">未提交草稿</span>
                  <div className="kpi-value">
                    {reviewPending ? (
                      <span className="kpi-muted">…</span>
                    ) : reviewError ? (
                      <span className="kpi-muted">—</span>
                    ) : (
                      <span className="mono">{formatCount(draftCount)}</span>
                    )}
                  </div>
                  <span className="kpi-hint">队列全量，不受时间窗限制</span>
                </div>
                <div className="kpi kpi-wide">
                  <span className="kpi-label">审计链</span>
                  <div className="kpi-value">
                    {chain === undefined ? (
                      <span className="kpi-muted">—</span>
                    ) : (
                      <Badge tone={CHAIN_TONE[chain]} dot>
                        {CHAIN_LABEL[chain]}
                      </Badge>
                    )}
                  </div>
                  <span className="kpi-hint">{chainHint}</span>
                </div>
              </div>
              {stats.truncated && (
                <p className="hint dash-card-note">
                  统计返回已截断：仅覆盖后端返回的工具行，总调用与成功率不是全量口径。
                </p>
              )}
            </>
          )}
        </div>
      </Card>
    </div>
  )
}
