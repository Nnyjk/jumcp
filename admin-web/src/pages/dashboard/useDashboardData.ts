import { useQuery } from '@tanstack/react-query'
import {
  fetchHealth,
  fetchLatestAuditEvents,
  fetchReviewQueue,
  fetchToolStats,
} from '../../api/admin'
import { isStructuredEvent } from '../../audit'
import { HEALTH_STATUS_LABEL } from '../../labels'
import { REFRESH_MS } from '../../constants'
import { DASHBOARD_EVENT_LIMIT, GOVERNANCE_LIMIT, aggregateDaily, windowStartIso } from './dashboardData'

/**
 * 看板的查询与派生数据：7 个 query 的 key/失效粒度保持不变，
 * 容器只负责时间窗与自动刷新开关，其余都在这里算好。
 */
export function useDashboardData(windowDays: number, autoRefresh: boolean) {
  const refetchInterval = autoRefresh ? REFRESH_MS : false

  const statsQuery = useQuery({
    queryKey: ['tool-stats', windowDays],
    queryFn: ({ signal }) => fetchToolStats(windowDays, signal),
    refetchInterval,
  })

  const reviewQuery = useQuery({
    queryKey: ['review-queue'],
    queryFn: ({ signal }) => fetchReviewQueue(signal),
    refetchInterval,
  })

  const healthQuery = useQuery({
    queryKey: ['health'],
    queryFn: ({ signal }) => fetchHealth(signal),
    refetchInterval,
  })

  const trendToolQuery = useQuery({
    queryKey: ['dashboard-trend', 'tool_call', windowDays],
    queryFn: ({ signal }) =>
      fetchLatestAuditEvents(
        { event_type: 'tool_call', ts_after: windowStartIso(windowDays) },
        DASHBOARD_EVENT_LIMIT,
        signal,
      ),
    refetchInterval,
  })

  const trendTemplateQuery = useQuery({
    queryKey: ['dashboard-trend', 'template_run', windowDays],
    queryFn: ({ signal }) =>
      fetchLatestAuditEvents(
        { event_type: 'template_run', ts_after: windowStartIso(windowDays) },
        DASHBOARD_EVENT_LIMIT,
        signal,
      ),
    refetchInterval,
  })

  const adminChangeQuery = useQuery({
    queryKey: ['dashboard-governance', 'admin_change'],
    queryFn: ({ signal }) =>
      fetchLatestAuditEvents({ event_type: 'admin_change' }, GOVERNANCE_LIMIT, signal),
    refetchInterval,
  })

  const policyQuery = useQuery({
    queryKey: ['dashboard-governance', 'policy_decision'],
    queryFn: ({ signal }) =>
      fetchLatestAuditEvents({ event_type: 'policy_decision' }, GOVERNANCE_LIMIT, signal),
    refetchInterval,
  })

  const refreshing =
    statsQuery.isFetching ||
    reviewQuery.isFetching ||
    healthQuery.isFetching ||
    trendToolQuery.isFetching ||
    trendTemplateQuery.isFetching ||
    adminChangeQuery.isFetching ||
    policyQuery.isFetching

  const stats = statsQuery.data
  const rows = stats?.tools ?? []

  const draftCount =
    reviewQuery.data?.items.filter((item) => item.status === 'draft').length ?? 0
  const reviewCount =
    reviewQuery.data?.items.filter((item) => item.status === 'review').length ?? 0

  const chain = healthQuery.data?.audit_chain
  const chainHint =
    healthQuery.data !== undefined
      ? `服务状态：${HEALTH_STATUS_LABEL[healthQuery.data.status] ?? healthQuery.data.status}`
      : healthQuery.isError
        ? '健康检查查询失败'
        : '查询中'

  const topTools = rows
    .filter((row) => row.calls > 0)
    .sort((left, right) => right.calls - left.calls)
    .slice(0, 8)
  const maxCalls = Math.max(...topTools.map((row) => row.calls), 1)

  const failureTools = rows
    .filter((row) => row.failure > 0)
    .sort((left, right) => right.failure - left.failure)
    .slice(0, 5)

  const datasources = healthQuery.data?.datasources ?? []

  const trendEvents = [
    ...(trendToolQuery.data?.events ?? []),
    ...(trendTemplateQuery.data?.events ?? []),
  ].filter((event) => isStructuredEvent(event))
  const trendPoints = aggregateDaily(trendEvents)
  // latest-events 接口按 limit 截断：total 大于返回条数时，趋势图是下界而非全量。
  const trendTruncated =
    (trendToolQuery.data?.total ?? 0) + (trendTemplateQuery.data?.total ?? 0) >
    (trendToolQuery.data?.events.length ?? 0) + (trendTemplateQuery.data?.events.length ?? 0)
  const trendPending = trendToolQuery.isPending || trendTemplateQuery.isPending
  const trendBothError = trendToolQuery.isError && trendTemplateQuery.isError
  const trendPartialError = trendToolQuery.isError || trendTemplateQuery.isError
  const trendError = trendToolQuery.error ?? trendTemplateQuery.error

  const governanceItems = [
    ...(adminChangeQuery.data?.events ?? []),
    ...(policyQuery.data?.events ?? []),
  ]
    .sort((left, right) => Date.parse(right.ts) - Date.parse(left.ts))
    .slice(0, GOVERNANCE_LIMIT)
  const governancePending = adminChangeQuery.isPending || policyQuery.isPending
  const governanceBothError = adminChangeQuery.isError && policyQuery.isError
  const governancePartialError = adminChangeQuery.isError || policyQuery.isError
  const governanceError = adminChangeQuery.error ?? policyQuery.error

  return {
    statsQuery,
    reviewQuery,
    healthQuery,
    stats,
    rows,
    draftCount,
    reviewCount,
    chain,
    chainHint,
    topTools,
    maxCalls,
    failureTools,
    datasources,
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
  }
}
