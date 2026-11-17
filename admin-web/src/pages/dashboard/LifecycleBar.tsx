import { gsap } from 'gsap'
import type { ToolLifecycle, ToolStatsRow } from '../../api/types'
import { formatPercent } from '../../format'
import { MOTION_DURATION, MOTION_EASE, useGsapScope } from '../../motion/useGsap'
import { formatCount } from './dashboardData'

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
export function LifecycleBar({ rows }: { rows: ToolStatsRow[] }) {
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
