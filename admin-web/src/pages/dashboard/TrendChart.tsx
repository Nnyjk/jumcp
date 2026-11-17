import { useLayoutEffect, useRef, useState } from 'react'
import { gsap } from 'gsap'
import { MOTION_DURATION, MOTION_EASE, useGsapScope } from '../../motion/useGsap'
import type { TrendPoint } from './dashboardData'

const TREND_HEIGHT = 240
const TREND_PAD = { top: 14, right: 16, bottom: 30, left: 44 }

/** 手写 SVG 面积/折线图：单序列，y 轴给 0 与最大值刻度，点 title 给完整数字。 */
export function TrendChart({ points, days }: { points: TrendPoint[]; days: number }) {
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
          <circle key={coord.point.date} className="trend-dot" cx={coord.x} cy={coord.y} r={3}>
            <title>{`${coord.point.date} · ${coord.point.count} 次`}</title>
          </circle>
        ))}
      </svg>
    </div>
  )
}
