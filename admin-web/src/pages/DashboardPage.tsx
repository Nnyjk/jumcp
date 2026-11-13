import { useLayoutEffect, useRef, useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { gsap } from 'gsap'
import { fetchHealth, fetchLatestAuditEvents, fetchReviewQueue, fetchToolStats } from '../api/admin'
import type {
  AuditChainState,
  AuditEnvelope,
  ToolHealth,
  ToolLifecycle,
  ToolStatsRow,
} from '../api/types'
import { Badge, type BadgeTone } from '../components/Badge'
import { Card } from '../components/Card'
import { EmptyState, ErrorState, LoadingState, PageShell } from '../components/PageShell'
import { ToggleSwitch } from '../components/ToggleSwitch'
import { auditSentence, isStructuredEvent } from '../audit'
import { roleLabel, useGroupIndex, useRoleIndex } from '../api/queries'
import { EVENT_TYPE_LABEL, HEALTH_STATUS_LABEL } from '../labels'
import { formatDateTime } from '../format'
import { MOTION_DURATION, MOTION_EASE, useGsapScope } from '../motion/useGsap'
import '../styles/dashboard.css'

/** 趋势只取一页样本：后端 audit limit 上限 200，避免把「最近 N 条」当成全量。 */
const DASHBOARD_EVENT_LIMIT = 200

/** 治理动态合并两类事件后展示的条数。 */
const GOVERNANCE_LIMIT = 8

const WINDOW_OPTIONS = [1, 7, 30] as const

const REFRESH_MS = 30_000

const CHAIN_LABEL: Record<AuditChainState, string> = {
  valid: '完整',
  tampered: '被篡改',
  error: '校验失败',
}

const CHAIN_TONE: Record<AuditChainState, BadgeTone> = {
  valid: 'success',
  tampered: 'danger',
  error: 'danger',
}

const COUNT_FORMAT = new Intl.NumberFormat('zh-CN')

function formatCount(value: number): string {
  return COUNT_FORMAT.format(value)
}

/** 比率是 0–1 小数；无样本时返回破折号，不用 0% 冒充。 */
function formatPercent(value: number | null | undefined): string {
  if (value === null || value === undefined || Number.isNaN(value)) return '—'
  return `${(value * 100).toFixed(1)}%`
}

function windowStartIso(days: number): string {
  return new Date(Date.now() - days * 24 * 60 * 60 * 1000).toISOString()
}

function localDateKey(ts: string): string | undefined {
  const date = new Date(ts)
  if (Number.isNaN(date.getTime())) return undefined
  const month = String(date.getMonth() + 1).padStart(2, '0')
  const day = String(date.getDate()).padStart(2, '0')
  return `${date.getFullYear()}-${month}-${day}`
}

interface TrendPoint {
  date: string
  count: number
}

/** 按浏览器本地日期把两类事件合并计数；没有事件的日期不补零（样本不代表全量）。 */
function aggregateDaily(events: AuditEnvelope[]): TrendPoint[] {
  const counts = new Map<string, number>()
  for (const event of events) {
    const key = localDateKey(event.ts)
    if (key === undefined) continue
    counts.set(key, (counts.get(key) ?? 0) + 1)
  }
  return [...counts.entries()]
    .sort(([left], [right]) => left.localeCompare(right))
    .map(([date, count]) => ({ date, count }))
}

const TREND_HEIGHT = 240
const TREND_PAD = { top: 14, right: 16, bottom: 30, left: 44 }

/** 手写 SVG 面积/折线图：单序列，y 轴给 0 与最大值刻度，点 title 给完整数字。 */
function TrendChart({ points, days }: { points: TrendPoint[]; days: number }) {
  const wrapRef = useRef<HTMLDivElement | null>(null)
  // viewBox 宽度跟随容器：固定 720 宽会让 10px 轴字号在 350px 卡片里被缩成 5px（刻度变成「-」）。
  const [width, setWidth] = useState(720)

  useLayoutEffect(() => {
    const node = wrapRef.current
    if (node === null) return
    const measure = () => setWidth(Math.max(320, Math.round(node.clientWidth)))
    measure()
    const observer = new ResizeObserver(measure)
    observer.observe(node)
    return () => observer.disconnect()
  }, [])

  const maxCount = Math.max(...points.map((point) => point.count), 1)
  const innerW = width - TREND_PAD.left - TREND_PAD.right
  const innerH = TREND_HEIGHT - TREND_PAD.top - TREND_PAD.bottom
  const baseline = TREND_PAD.top + innerH
  const xAt = (index: number) =>
    TREND_PAD.left +
    (points.length === 1 ? innerW / 2 : (index / (points.length - 1)) * innerW)
  const yAt = (value: number) => TREND_PAD.top + innerH - (value / maxCount) * innerH

  const coords = points.map((point, index) => ({
    x: xAt(index),
    y: yAt(point.count),
    point,
  }))
  const linePath = coords
    .map((coord, index) => `${index === 0 ? 'M' : 'L'}${coord.x.toFixed(1)} ${coord.y.toFixed(1)}`)
    .join(' ')
  const first = coords[0]
  const last = coords[coords.length - 1]
  const areaPath =
    first !== undefined && last !== undefined
      ? `${linePath} L${last.x.toFixed(1)} ${baseline} L${first.x.toFixed(1)} ${baseline} Z`
      : ''

  const tickValues = Array.from(
    new Set(maxCount <= 2 ? [0, maxCount] : [0, Math.round(maxCount / 2), maxCount]),
  )
  const labelStep = Math.max(1, Math.ceil(points.length / 6))
  const total = points.reduce((sum, point) => sum + point.count, 0)
  const chartRef = useGsapScope<SVGSVGElement>((scope) => {
    gsap.from(scope, {
      opacity: 0,
      y: 8,
      duration: MOTION_DURATION.enter,
      ease: MOTION_EASE,
      clearProps: 'opacity,transform',
    })
  })

  return (
    <div className="trend-wrap" ref={wrapRef}>
      <svg
        ref={chartRef}
        className="trend-chart"
        viewBox={`0 0 ${width} ${TREND_HEIGHT}`}
        role="img"
        aria-label={`最近 ${days} 天每日调用趋势，共 ${total} 条事件，峰值 ${maxCount} 条`}
      >
        {tickValues.map((value) => (
          <g key={value}>
            <line
              className="chart-grid"
              x1={TREND_PAD.left}
              x2={width - TREND_PAD.right}
              y1={yAt(value)}
              y2={yAt(value)}
            />
            <text className="chart-axis" x={TREND_PAD.left - 8} y={yAt(value) + 3} textAnchor="end">
              {value}
            </text>
          </g>
        ))}
        <line
          className="chart-axis-line"
          x1={TREND_PAD.left}
          x2={width - TREND_PAD.right}
          y1={baseline}
          y2={baseline}
        />
        {areaPath !== '' && <path className="trend-area" d={areaPath} />}
        {coords.length > 1 && <path className="trend-line" d={linePath} />}
        {coords.map((coord, index) => {
          if (!(index === 0 || index === coords.length - 1 || index % labelStep === 0)) return null
          const anchor = index === 0 ? 'start' : index === coords.length - 1 ? 'end' : 'middle'
          return (
            <text
              key={coord.point.date}
              className="chart-axis"
              x={coord.x}
              y={baseline + 18}
              textAnchor={anchor}
            >
              {coord.point.date.slice(5)}
            </text>
          )
        })}
        {coords.map((coord) => (
          <circle
            key={coord.point.date}
            className="trend-dot"
            cx={coord.x}
            cy={coord.y}
            r={3}
          >
            <title>{`${coord.point.date} · ${coord.point.count} 次`}</title>
          </circle>
        ))}
      </svg>
    </div>
  )
}

interface HealthSegment {
  health: ToolHealth
  label: string
  color: string
  className: string
}

const HEALTH_SEGMENTS: HealthSegment[] = [
  { health: 'ok', label: '正常', color: 'var(--success)', className: 'donut-seg-ok' },
  { health: 'degraded', label: '降级', color: 'var(--warning)', className: 'donut-seg-degraded' },
  { health: 'failing', label: '故障', color: 'var(--danger)', className: 'donut-seg-failing' },
  { health: 'unknown', label: '未知', color: 'var(--text-tertiary)', className: 'donut-seg-unknown' },
]

const DONUT_RADIUS = 44
const DONUT_CIRCUMFERENCE = 2 * Math.PI * DONUT_RADIUS
const DONUT_GAP = 3

/** 手写 SVG 环形图：分段语义色 + 图例文字计数，颜色不是唯一信息载体。 */
function HealthDonut({ rows }: { rows: ToolStatsRow[] }) {
  const total = rows.length
  const segments = HEALTH_SEGMENTS.map((segment) => ({
    ...segment,
    count: rows.filter((row) => row.health === segment.health).length,
  }))

  const gap = segments.filter((segment) => segment.count > 0).length > 1 ? DONUT_GAP : 0
  const arcs = segments.map((segment, index) => {
    const length = total === 0 ? 0 : (segment.count / total) * DONUT_CIRCUMFERENCE
    const start =
      total === 0
        ? 0
        : segments
            .slice(0, index)
            .reduce(
              (sum, previous) => sum + (previous.count / total) * DONUT_CIRCUMFERENCE,
              0,
            )
    const dash = Math.max(length - (segment.count > 0 ? gap : 0), 0)
    return { ...segment, dash, offset: -start }
  })
  const summary = segments.map((segment) => `${segment.label} ${segment.count}`).join('，')
  const donutRef = useGsapScope<HTMLDivElement>((scope) => {
    gsap.from(scope, {
      opacity: 0,
      scale: 0.98,
      duration: MOTION_DURATION.enter,
      ease: MOTION_EASE,
      transformOrigin: '50% 50%',
      clearProps: 'opacity,transform,transformOrigin',
    })
  })

  return (
    <div className="donut-layout" ref={donutRef}>
      <svg
        className="donut-svg"
        viewBox="0 0 120 120"
        role="img"
        aria-label={`工具健康分布：共 ${total} 个，${summary}`}
      >
        <circle className="donut-track" cx="60" cy="60" r={DONUT_RADIUS} />
        {arcs.map(
          (arc) =>
            arc.count > 0 && (
              <circle
                key={arc.health}
                className={`donut-seg ${arc.className}`}
                cx="60"
                cy="60"
                r={DONUT_RADIUS}
                strokeDasharray={`${arc.dash} ${DONUT_CIRCUMFERENCE - arc.dash}`}
                strokeDashoffset={arc.offset}
                transform="rotate(-90 60 60)"
              >
                <title>{`${arc.label} ${arc.count} 个（${formatPercent(arc.count / total)}）`}</title>
              </circle>
            ),
        )}
        <text className="donut-total" x="60" y="56" textAnchor="middle">
          {total}
        </text>
        <text className="donut-caption" x="60" y="74" textAnchor="middle">
          工具总数
        </text>
      </svg>
      <ul className="legend">
        {segments.map((segment) => (
          <li key={segment.health} className="legend-item">
            <span
              className="legend-swatch"
              style={{ background: segment.color }}
              aria-hidden="true"
            />
            <span className="legend-label">{segment.label}</span>
            <span className="legend-value">
              {formatCount(segment.count)} · {formatPercent(segment.count / total)}
            </span>
          </li>
        ))}
      </ul>
    </div>
  )
}

interface LifecycleSegment {
  status: ToolLifecycle
  label: string
}

const LIFECYCLE_SEGMENTS: LifecycleSegment[] = [
  { status: 'published', label: '已发布' },
  { status: 'draft', label: '草稿' },
  { status: 'review', label: '待审' },
  { status: 'deprecated', label: '已下线' },
  { status: 'unknown', label: '未知' },
]

/** 水平堆叠条：分段只表达比例，图例同时给出状态名与计数。 */
function LifecycleBar({ rows }: { rows: ToolStatsRow[] }) {
  const total = rows.length
  const segments = LIFECYCLE_SEGMENTS.map((segment) => ({
    ...segment,
    count: rows.filter((row) => row.lifecycle_status === segment.status).length,
  }))
  const summary = segments.map((segment) => `${segment.label} ${segment.count}`).join('，')
  const builtinCount = rows.filter((row) => row.source === 'builtin').length
  const barRef = useGsapScope<HTMLDivElement>((scope) => {
    gsap.from(scope.children, {
      opacity: 0,
      y: 4,
      duration: MOTION_DURATION.fast,
      ease: MOTION_EASE,
      stagger: 0.04,
      clearProps: 'opacity,transform',
    })
  })

  return (
    <div ref={barRef}>
      <div className="stack-bar" role="img" aria-label={`工具生命周期分布：${summary}`}>
        {segments.map(
          (segment) =>
            segment.count > 0 && (
              <span
                key={segment.status}
                className="stack-seg"
                data-tone={segment.status}
                style={{ width: `${(segment.count / total) * 100}%` }}
                title={`${segment.label}：${segment.count} 个（${formatPercent(segment.count / total)}）`}
              />
            ),
        )}
      </div>
      <p className="hint stack-source">
        来源：内置 {formatCount(builtinCount)} · 受管 {formatCount(total - builtinCount)}
      </p>
      <ul className="stack-legend">
        {segments.map((segment) => (
          <li key={segment.status} className="legend-item">
            <span className="legend-swatch" data-tone={segment.status} aria-hidden="true" />
            <span className="legend-label">{segment.label}</span>
            <span className="legend-value">
              {formatCount(segment.count)} · {formatPercent(segment.count / total)}
            </span>
          </li>
        ))}
      </ul>
    </div>
  )
}

export default function DashboardPage() {
  const queryClient = useQueryClient()
  const [windowDays, setWindowDays] = useState<number>(7)
  const [autoRefresh, setAutoRefresh] = useState(false)
  const refetchInterval = autoRefresh ? REFRESH_MS : false
  const groupIndex = useGroupIndex()
  const roleIndex = useRoleIndex()

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

  const refreshAll = () => {
    for (const key of ['tool-stats', 'review-queue', 'health', 'dashboard-trend', 'dashboard-governance']) {
      void queryClient.invalidateQueries({ queryKey: [key] })
    }
  }

  const stats = statsQuery.data
  const rows = stats?.tools ?? []
  const kpiRef = useGsapScope<HTMLDivElement>((scope) => {
    gsap.from(scope.children, {
      opacity: 0,
      y: 10,
      duration: MOTION_DURATION.enter,
      ease: MOTION_EASE,
      stagger: 0.045,
      clearProps: 'opacity,transform',
    })
  }, [stats !== undefined])

  const totalCalls = rows.reduce((sum, row) => sum + row.calls, 0)
  const totalSuccess = rows.reduce((sum, row) => sum + row.success, 0)
  const totalFailure = rows.reduce((sum, row) => sum + row.failure, 0)
  const activeTools = rows.filter((row) => row.calls > 0).length
  const successRate = totalCalls > 0 ? totalSuccess / totalCalls : null

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
  const trendPending = trendToolQuery.isPending || trendTemplateQuery.isPending
  const trendBothError = trendToolQuery.isError && trendTemplateQuery.isError
  const trendPartialError = trendToolQuery.isError || trendTemplateQuery.isError

  const governanceItems = [
    ...(adminChangeQuery.data?.events ?? []),
    ...(policyQuery.data?.events ?? []),
  ]
    .sort((left, right) => Date.parse(right.ts) - Date.parse(left.ts))
    .slice(0, GOVERNANCE_LIMIT)
  const governancePending = adminChangeQuery.isPending || policyQuery.isPending
  const governanceBothError = adminChangeQuery.isError && policyQuery.isError
  const governancePartialError = adminChangeQuery.isError || policyQuery.isError

  const updatedLabel =
    statsQuery.dataUpdatedAt > 0
      ? formatDateTime(new Date(statsQuery.dataUpdatedAt).toISOString())
      : '—'

  return (
    <PageShell>
      <Card>
        <div className="dash-toolbar">
          <div className="dash-toolbar-group">
            <span className="dash-toolbar-label">时间窗</span>
            <div className="dash-segmented" role="group" aria-label="统计时间窗">
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
            className="btn btn-secondary btn-sm"
            onClick={refreshAll}
            disabled={refreshing}
          >
            {refreshing ? '刷新中…' : '刷新'}
          </button>
          <span className="dash-updated">数据更新于 {updatedLabel}</span>
        </div>
      </Card>

      <div className="dash-grid">
        <div className="dash-span-12">
          <Card
            title="关键指标"
            subtitle={
              stats === undefined
                ? undefined
                : `窗口 ${formatDateTime(stats.window.from)} → ${formatDateTime(stats.window.to)}`
            }
          >
            {statsQuery.isPending && <LoadingState rows={3} label="正在加载关键指标" />}
            {statsQuery.isError && <ErrorState error={statsQuery.error} />}
            {stats !== undefined && rows.length === 0 && (
              <EmptyState hint={`最近 ${windowDays} 天没有工具统计行，无法计算调用量与成功率。`} />
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
                      {reviewQuery.isPending ? (
                        <span className="kpi-muted">…</span>
                      ) : reviewQuery.isError ? (
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
                      {reviewQuery.isPending ? (
                        <span className="kpi-muted">…</span>
                      ) : reviewQuery.isError ? (
                        <span className="kpi-muted">—</span>
                      ) : (
                        <span className="mono">{formatCount(draftCount)}</span>
                      )}
                    </div>
                    <span className="kpi-hint">队列全量，不受时间窗限制</span>
                  </div>
                  <div className="kpi">
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
                  <div className="kpi">
                    <span className="kpi-label">数据源</span>
                    <div className="kpi-value">
                      {healthQuery.data === undefined ? (
                        <span className="kpi-muted">—</span>
                      ) : (
                        <span className="mono">{formatCount(healthQuery.data.datasources.length)}</span>
                      )}
                    </div>
                    <span className="kpi-hint">健康检查</span>
                  </div>
                </div>
                {stats.truncated && (
                  <p className="hint dash-card-note">
                    统计返回已截断：仅覆盖后端返回的工具行，总调用与成功率不是全量口径。
                  </p>
                )}
              </>
            )}
          </Card>
        </div>

        <div className="dash-span-8">
          <Card
            title="调用趋势"
            subtitle={`最近 ${windowDays} 天`}
          >
            {trendPending && trendPoints.length === 0 && (
              <LoadingState rows={4} label="正在加载调用趋势" />
            )}
            {trendBothError && (
              <ErrorState error={trendToolQuery.error ?? trendTemplateQuery.error} />
            )}
            {!trendBothError && !trendPending && trendPoints.length === 0 && (
              trendPartialError ? (
                <ErrorState error={trendToolQuery.error ?? trendTemplateQuery.error} />
              ) : (
                <EmptyState
                  hint={`最近 ${windowDays} 天没有调用或模板执行事件。`}
                />
              )
            )}
            {trendPoints.length > 0 && (
              <>
                <TrendChart points={trendPoints} days={windowDays} />
                <p className="hint dash-card-note">
                  按浏览器本地日期聚合审计 v2 的 tool_call / template_run 事件，与「总调用」同口径；
                  旧格式事件不计入，最多覆盖最近 {DASHBOARD_EVENT_LIMIT} 条/类型。
                </p>
                {trendPartialError && (
                  <p className="hint dash-card-note">
                    部分事件类型查询失败，图表只包含成功返回的一侧。
                  </p>
                )}
              </>
            )}
          </Card>
        </div>

        <div className="dash-span-4">
          <Card title="健康分布">
            {statsQuery.isPending && <LoadingState rows={3} label="正在加载健康分布" />}
            {statsQuery.isError && <ErrorState error={statsQuery.error} />}
            {stats !== undefined && rows.length === 0 && (
              <EmptyState hint="没有工具统计行，无法计算健康分布。" />
            )}
            {stats !== undefined && rows.length > 0 && <HealthDonut rows={rows} />}
          </Card>
        </div>

        <div className="dash-span-4">
          <Card title="生命周期 / 来源">
            {statsQuery.isPending && <LoadingState rows={3} label="正在加载状态分布" />}
            {statsQuery.isError && <ErrorState error={statsQuery.error} />}
            {stats !== undefined && rows.length === 0 && (
              <EmptyState hint="没有工具统计行，无法计算状态分布。" />
            )}
            {stats !== undefined && rows.length > 0 && <LifecycleBar rows={rows} />}
          </Card>
        </div>

        <div className="dash-span-4">
          <Card title="工具调用排行" subtitle="按调用次数前 8">
            {statsQuery.isPending && <LoadingState rows={5} label="正在加载工具排名" />}
            {statsQuery.isError && <ErrorState error={statsQuery.error} />}
            {stats !== undefined && topTools.length === 0 && (
              <EmptyState hint={`最近 ${windowDays} 天没有调用记录，无法排名。`} />
            )}
            {topTools.length > 0 && (
              <ul className="rank-list">
                {topTools.map((row) => {
                  const rate =
                    row.success_rate ?? (row.calls > 0 ? row.success / row.calls : null)
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

        <div className="dash-span-4">
          <Card title="失败榜" subtitle="按失败次数前 5">
            {statsQuery.isPending && <LoadingState rows={4} label="正在加载失败榜" />}
            {statsQuery.isError && <ErrorState error={statsQuery.error} />}
            {stats !== undefined && failureTools.length === 0 && (
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

        <div className="dash-span-6">
          <Card
            title={healthQuery.data === undefined ? '数据源' : `数据源（${datasources.length}）`}
          >
            {healthQuery.isPending && <LoadingState rows={3} label="正在加载数据源" />}
            {healthQuery.isError && <ErrorState error={healthQuery.error} />}
            {healthQuery.data !== undefined && datasources.length === 0 && (
              <EmptyState hint="未配置数据源；部署配置登记后这里会显示名称、引擎与只读标记。" />
            )}
            {datasources.length > 0 && (
              <ul className="ds-list">
                {datasources.map((datasource) => (
                  <li key={datasource.name} className="ds-item">
                    <span className="ds-name" title={datasource.name}>
                      {datasource.name}
                    </span>
                    <span className="ds-engine">{datasource.engine}</span>
                    <Badge tone={datasource.readonly ? 'success' : 'warning'}>
                      {datasource.readonly ? '只读' : '可写'}
                    </Badge>
                  </li>
                ))}
              </ul>
            )}
          </Card>
        </div>

        <div className="dash-span-6">
          <Card
            title="治理动态"
            subtitle={`最近 ${GOVERNANCE_LIMIT} 条`}
          >
            {governancePending && governanceItems.length === 0 && (
              <LoadingState rows={4} label="正在加载治理动态" />
            )}
            {!governancePending && governanceItems.length === 0 && (
              governanceBothError || governancePartialError ? (
                <ErrorState error={adminChangeQuery.error ?? policyQuery.error} />
              ) : (
                <EmptyState hint="最近没有管理变更或策略判定事件。" />
              )
            )}
            {governanceItems.length > 0 && (
              <>
                <ol className="gov-list">
                  {governanceItems.map((event) => {
                    // 摘要句已含结果与错误码；这里只补一句后端原文，不再重复「策略拒绝：…」。
                    const reason =
                      event.payload.action === 'deny'
                        ? event.payload.error_message?.trim() || undefined
                        : undefined
                    return (
                      <li key={event.event_id} className="gov-item">
                        <div className="gov-head">
                          <span className="gov-time" title={event.ts}>
                            {formatDateTime(event.ts)}
                          </span>
                          <Badge
                            tone={event.event_type === 'admin_change' ? 'accent' : 'warning'}
                            title={event.event_type}
                          >
                            {EVENT_TYPE_LABEL[event.event_type] ?? event.event_type}
                          </Badge>
                          <span
                            className="gov-actor"
                            title={`${event.actor.type}:${event.actor.id} · 角色 ${roleLabel(
                              roleIndex,
                              event.actor.role,
                            )}（${event.actor.role}）`}
                          >
                            {event.actor.id || '未认证调用方'} ·{' '}
                            <span title={event.actor.role}>
                              {roleLabel(roleIndex, event.actor.role)}
                            </span>
                          </span>
                        </div>
                        <p className="gov-sentence">
                          {isStructuredEvent(event)
                            ? auditSentence(event, {
                                group: (code) => groupIndex.get(code)?.name,
                              })
                            : '旧格式事件，仅原始载荷'}
                        </p>
                        {reason !== undefined && <p className="gov-reason">原因：{reason}</p>}
                      </li>
                    )
                  })}
                </ol>
                {governancePartialError && (
                  <p className="hint dash-card-note">
                    部分事件类型查询失败，列表只包含成功返回的一侧。
                  </p>
                )}
              </>
            )}
          </Card>
        </div>
      </div>
    </PageShell>
  )
}
