import { useEffect, useRef, useState } from 'react'
import type { ReactNode } from 'react'
import { gsap } from 'gsap'
import { MOTION_EASE, MOTION_DURATION, useGsapScope } from '../motion/useGsap'

export interface DataTableColumn<T> {
  /** 列标识，用作 col/th/td 的 React key，与行数据字段无关。 */
  key: string
  /** 列头文本。 */
  header: string
  cell: (row: T) => ReactNode
  /** 覆盖单元格 title（默认取 cell 返回的字符串/数字；返回元素时没有默认 tooltip）。 */
  title?: (row: T) => string
  /** 允许内容换行（摘要、错误原因等长文本列）；默认单行省略，完整值放 title。 */
  wrap?: boolean
  /** 等宽列：ID、哈希、时间戳。 */
  mono?: boolean
  /** 右对齐等宽数值列：版本号等，配合 tabular-nums。 */
  numeric?: boolean
  /** 列宽建议值（如 `168px`）；表格是 table-layout: fixed，未给宽度的列均分剩余宽度。 */
  width?: string
  /** 固定在滚动容器右侧：列多到出现横向滚动时，操作列仍然可见。 */
  sticky?: boolean
}

/**
 * 只负责渲染的行列表格：粘性表头、行悬浮高亮、偶数行条纹、单元格溢出省略并把原始值放进 title。
 * 不排序也不分页——顺序由调用方给定 rows，翻页用 PageShell 的 Pagination 配合外部 offset。
 *
 * `label` 是无障碍表格名（caption 且 sr-only）；`meta` 显示在工具条左侧，通常是总数或时间窗。
 * 表格比容器宽时在工具条右侧给出「可横向滚动」提示，并把滚动容器升级为可聚焦的 region，
 * 避免被 sticky 列遮住的列被误读成缺失；不溢出时不额外引入 Tab 停留点。
 * 空 rows 不渲染占位，空态由调用方决定（见 PageShell 的 EmptyState）。
 * `rowKey` 约束：必须稳定且唯一。作为行动画重放签名，翻页或行增删重放入场交错，同一批数据重渲染跳过动效以消除微调颤烁。
 */
export function DataTable<T>({
  label,
  meta,
  columns,
  rows,
  rowKey,
  minWidth,
}: {
  label: string
  meta?: string
  columns: Array<DataTableColumn<T>>
  rows: T[]
  rowKey: (row: T) => string
  /** 宽表覆盖默认 880px 最小宽度：列多到需要横向滚动时按内容给足空间。 */
  minWidth?: string
}) {
  const rowSignature = rows.map(rowKey).join('\u0000')
  const scrollRef = useRef<HTMLDivElement | null>(null)
  const [overflowing, setOverflowing] = useState(false)

  useEffect(() => {
    const node = scrollRef.current
    if (node === null) return
    const measure = () => setOverflowing(node.scrollWidth - node.clientWidth > 2)
    measure()
    const observer = new ResizeObserver(measure)
    observer.observe(node)
    return () => observer.disconnect()
  }, [rowSignature, minWidth, columns.length])

  const tableRef = useGsapScope<HTMLTableElement>((scope) => {
    const bodyRows = scope.tBodies[0]?.rows
    if (bodyRows === undefined || bodyRows.length === 0) return
    // 动效约束：仅对前 20 行应用入场交错动效。全量行逐行动画在长列表（50 行以上）会产生过长视觉拖尾并阻塞交互，限制前 20 行兼顾入场感知与阅读响应。
    const animated = Array.from(bodyRows).slice(0, 20)
    gsap.from(animated, {
      opacity: 0,
      duration: MOTION_DURATION.row,
      ease: MOTION_EASE,
      stagger: 0.008,
      clearProps: 'opacity',
    })
  }, [rowSignature])

  return (
    <div>
      {(meta !== undefined || overflowing) && (
        <div className="table-tools">
          <span className="table-count">{meta}</span>
          {overflowing && (
            <span className="table-overflow-hint" role="note">
              列较多，可横向滚动
            </span>
          )}
        </div>
      )}
      <div
        className="table-scroll"
        ref={scrollRef}
        data-overflowing={overflowing ? 'true' : 'false'}
        // 出现横向滚动时给键盘用户一个可聚焦的滚动区域；不溢出时不额外增加 Tab 停留点。
        tabIndex={overflowing ? 0 : undefined}
        role={overflowing ? 'region' : undefined}
        aria-label={overflowing ? `${label}（可横向滚动）` : undefined}
      >
        <table
          ref={tableRef}
          className="data-table"
          style={minWidth !== undefined ? { minWidth } : undefined}
        >
          <caption className="sr-only">{label}</caption>
          <colgroup>
            {columns.map((column) => (
              <col key={column.key} style={column.width !== undefined ? { width: column.width } : undefined} />
            ))}
          </colgroup>
          <thead>
            <tr>
              {columns.map((column) => (
                <th
                  key={column.key}
                  scope="col"
                  className={[
                    column.numeric === true ? 'th-num' : '',
                    column.sticky === true ? 'cell-sticky' : '',
                  ]
                    .filter(Boolean)
                    .join(' ') || undefined}
                >
                  {column.header}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {rows.map((row) => (
              <tr key={rowKey(row)}>
                {columns.map((column) => {
                  const content = column.cell(row)
                  const title =
                    column.title?.(row) ??
                    (typeof content === 'string' || typeof content === 'number'
                      ? String(content)
                      : undefined)
                  const className = [
                    'cell',
                    column.mono === true ? 'cell-mono' : '',
                    column.numeric === true ? 'cell-num' : '',
                    column.sticky === true ? 'cell-sticky' : '',
                  ]
                    .filter(Boolean)
                    .join(' ')
                  return (
                    <td key={column.key} className={className}>
                      <span className={column.wrap === true ? 'cell-text cell-text-wrap' : 'cell-text'} title={title}>
                        {content}
                      </span>
                    </td>
                  )
                })}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  )
}
