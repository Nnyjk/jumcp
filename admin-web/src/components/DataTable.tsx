import { useEffect, useRef, useState } from 'react'
import type { ReactNode } from 'react'
import { gsap } from 'gsap'
import { MOTION_EASE, MOTION_DURATION, useGsapScope } from '../motion/useGsap'

export interface DataTableColumn<T> {
  key: string
  /** 列头文本。 */
  header: string
  cell: (row: T) => ReactNode
  /** 覆盖单元格 title（文本列默认取 cell 的字符串内容）。 */
  title?: (row: T) => string
  /** 允许内容换行（摘要、错误原因等长文本列）；默认单行省略。 */
  wrap?: boolean
  /** 等宽列：ID、哈希、时间戳。 */
  mono?: boolean
  /** 右对齐等宽数值列：版本号等，配合 tabular-nums。 */
  numeric?: boolean
  width?: string
  /** 固定在滚动容器右侧：列多到出现横向滚动时，操作列仍然可见。 */
  sticky?: boolean
}

/**
 * DataGrid 风格表格：粘性表头、行悬浮高亮、偶数行背景始终生效、单元格溢出省略并保留原始值 title。
 * `label` 作为无障碍表格名；`meta` 显示在工具条左侧（通常是总数）。
 * 表格比容器宽时在工具条右侧给出「可横向滚动」提示，避免被冻结列遮住的列被误读成缺失。
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
    gsap.from(bodyRows, {
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
      <div className="table-scroll" ref={scrollRef} data-overflowing={overflowing ? 'true' : 'false'}>
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
