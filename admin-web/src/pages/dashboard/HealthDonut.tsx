import { gsap } from 'gsap'
import type { ToolHealth, ToolStatsRow } from '../../api/types'
import { formatPercent } from '../../format'
import { MOTION_DURATION, MOTION_EASE, useGsapScope } from '../../motion/useGsap'
import { formatCount } from './dashboardData'

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
  {
    health: 'unknown',
    label: '未知',
    color: 'var(--text-tertiary)',
    className: 'donut-seg-unknown',
  },
]

const DONUT_RADIUS = 44
const DONUT_CIRCUMFERENCE = 2 * Math.PI * DONUT_RADIUS
const DONUT_GAP = 3

/** 手写 SVG 环形图：分段语义色 + 图例文字计数，颜色不是唯一信息载体。 */
export function HealthDonut({ rows }: { rows: ToolStatsRow[] }) {
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
            .reduce((sum, previous) => sum + (previous.count / total) * DONUT_CIRCUMFERENCE, 0)
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
            <span className="legend-swatch" style={{ background: segment.color }} aria-hidden="true" />
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
