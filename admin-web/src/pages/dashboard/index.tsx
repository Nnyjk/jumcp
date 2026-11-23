import { useState } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { gsap } from 'gsap'
import { Card } from '../../components/Card'
import { EmptyState, ErrorState, LoadingState, PageShell } from '../../components/PageShell'
import { ToggleSwitch } from '../../components/ToggleSwitch'
import { useGroupIndex, useRoleIndex } from '../../api/queries'
import { formatDateTime } from '../../format'
import { WINDOW_OPTIONS } from '../../constants'
import { MOTION_DURATION, MOTION_EASE, useGsapScope } from '../../motion/useGsap'
import '../../styles/dashboard.css'
import { ActivityFeed } from './ActivityFeed'
import { DASHBOARD_EVENT_LIMIT } from './dashboardData'
import { FailureRanking } from './FailureRanking'
import { HealthDonut } from './HealthDonut'
import { KpiGrid } from './KpiGrid'
import { LifecycleBar } from './LifecycleBar'
import { ToolRanking } from './ToolRanking'
import { TrendChart } from './TrendChart'
import { useDashboardData } from './useDashboardData'

export default function DashboardPage() {
  const queryClient = useQueryClient()
  const [windowDays, setWindowDays] = useState<number>(7)
  const [autoRefresh, setAutoRefresh] = useState(false)
  const groupIndex = useGroupIndex()
  const roleIndex = useRoleIndex()

  const {
    statsQuery,
    reviewQuery,
    stats,
    rows,
    draftCount,
    reviewCount,
    chain,
    chainHint,
    topTools,
    maxCalls,
    failureTools,
    trendPoints,
    trendTruncated,
    trendPending,
    trendBothError,
    trendPartialError,
    trendError,
    governanceItems,
    governancePending,
    governanceBothError,
    governancePartialError,
    governanceError,
    refreshing,
  } = useDashboardData(windowDays, autoRefresh)

  // 卡片节奏：栅格内的卡片依次进入。只在挂载时播放一次，轮询不会重播。
  const gridRef = useGsapScope<HTMLDivElement>((scope) => {
    gsap.from(scope.children, {
      opacity: 0,
      y: 10,
      duration: MOTION_DURATION.enter,
      ease: MOTION_EASE,
      stagger: 0.03,
      clearProps: 'opacity,transform',
    })
  })

  const refreshAll = () => {
    for (const key of ['tool-stats', 'review-queue', 'health', 'dashboard-trend', 'dashboard-governance']) {
      void queryClient.invalidateQueries({ queryKey: [key] })
    }
  }

  const updatedLabel =
    statsQuery.dataUpdatedAt > 0
      ? formatDateTime(new Date(statsQuery.dataUpdatedAt).toISOString())
      : '—'

  // 数据到达时给一次轻微提示：只淡化「数据更新于」这一行，卡片内容不重播入场。
  const updatedRef = useGsapScope<HTMLSpanElement>(
    (scope) => {
      gsap.fromTo(
        scope,
        { opacity: 0.35 },
        { opacity: 1, duration: MOTION_DURATION.fast, ease: MOTION_EASE, clearProps: 'opacity' },
      )
    },
    [updatedLabel],
  )

  return (
    <PageShell>
      <Card>
        <div className="dash-toolbar">
          <div className="dash-toolbar-group">
            <span className="dash-toolbar-label">时间窗</span>
            <div className="segmented" role="group" aria-label="统计时间窗">
              {WINDOW_OPTIONS.map((days) => (
                <button
                  key={days}
                  type="button"
                  aria-pressed={windowDays === days}
                  onClick={() => setWindowDays(days)}
                >
                  {days} 天
                </button>
              ))}
            </div>
          </div>
          <ToggleSwitch
            checked={autoRefresh}
            label="每 30 秒自动刷新"
            onChange={setAutoRefresh}
          />
          <button
            type="button"
            className="btn btn-secondary btn-sm dash-refresh"
            data-pending={refreshing ? 'true' : 'false'}
            onClick={refreshAll}
            disabled={refreshing}
          >
            {refreshing && <span className="dash-spinner" aria-hidden="true" />}
            {refreshing ? '刷新中…' : '刷新'}
          </button>
          <span className="dash-updated" ref={updatedRef}>
            数据更新于 {updatedLabel}
          </span>
        </div>
      </Card>

      <div className="dash-grid" ref={gridRef}>
        <KpiGrid
          windowDays={windowDays}
          stats={stats}
          rows={rows}
          statsPending={statsQuery.isPending}
          statsError={statsQuery.isError ? statsQuery.error : undefined}
          reviewPending={reviewQuery.isPending}
          reviewError={reviewQuery.isError}
          reviewCount={reviewCount}
          draftCount={draftCount}
          chain={chain}
          chainHint={chainHint}
        />

        <div className="dash-span-8">
          <Card title="调用趋势" subtitle={`最近 ${windowDays} 天`}>
            <div className="dash-panel dash-panel-trend">
              {trendPending && trendPoints.length === 0 && (
                <LoadingState rows={4} label="正在加载调用趋势" />
              )}
              {trendBothError && <ErrorState error={trendError} />}
              {!trendBothError && !trendPending && trendPoints.length === 0 && (
                trendPartialError ? (
                  <ErrorState error={trendError} />
                ) : (
                  <EmptyState hint={`最近 ${windowDays} 天没有调用或模板执行事件。`} />
                )
              )}
              {trendPoints.length > 0 && (
                <>
                  <TrendChart points={trendPoints} days={windowDays} />
                  <p className="hint dash-card-note">
                    按浏览器本地日期聚合审计 v2 的 tool_call / template_run 事件，与「总调用」同口径；
                    旧格式事件不计入，最多覆盖最近 {DASHBOARD_EVENT_LIMIT} 条/类型。
                    {trendTruncated
                      ? `当前时间窗内事件超过 ${DASHBOARD_EVENT_LIMIT} 条/类型，图表只统计最近的部分，合计低于「总调用」。`
                      : ''}
                  </p>
                  {trendPartialError && (
                    <p className="hint dash-card-note">
                      部分事件类型查询失败，图表只包含成功返回的一侧。
                    </p>
                  )}
                </>
              )}
            </div>
          </Card>
        </div>

        <div className="dash-span-4">
          <Card title="健康分布">
            <div className="dash-panel dash-panel-donut">
              {statsQuery.isPending && <LoadingState rows={3} label="正在加载健康分布" />}
              {statsQuery.isError && <ErrorState error={statsQuery.error} />}
              {stats !== undefined && rows.length === 0 && (
                <EmptyState hint="没有工具统计行，无法计算健康分布。" />
              )}
              {stats !== undefined && rows.length > 0 && <HealthDonut rows={rows} />}
            </div>
          </Card>
        </div>

        <div className="dash-span-4">
          <Card title="生命周期 / 来源">
            <div className="dash-panel dash-panel-lifecycle">
              {statsQuery.isPending && <LoadingState rows={3} label="正在加载状态分布" />}
              {statsQuery.isError && <ErrorState error={statsQuery.error} />}
              {stats !== undefined && rows.length === 0 && (
                <EmptyState hint="没有工具统计行，无法计算状态分布。" />
              )}
              {stats !== undefined && rows.length > 0 && <LifecycleBar rows={rows} />}
            </div>
          </Card>
        </div>

        <ToolRanking
          windowDays={windowDays}
          topTools={topTools}
          maxCalls={maxCalls}
          statsPending={statsQuery.isPending}
          statsError={statsQuery.isError ? statsQuery.error : undefined}
          hasStats={stats !== undefined}
        />

        <FailureRanking
          windowDays={windowDays}
          failureTools={failureTools}
          statsPending={statsQuery.isPending}
          statsError={statsQuery.isError ? statsQuery.error : undefined}
          hasStats={stats !== undefined}
        />

        <ActivityFeed
          items={governanceItems}
          pending={governancePending}
          bothError={governanceBothError}
          partialError={governancePartialError}
          error={governanceError}
          groupIndex={groupIndex}
          roleIndex={roleIndex}
        />
      </div>
    </PageShell>
  )
}
