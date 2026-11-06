import { useState, type ReactNode } from 'react'
import { Icon } from './Icon'

export interface DataTableColumn<T> {
  key: string
  header: string
  cell: (row: T) => ReactNode
  /** 覆盖单元格 title（文本列默认取 cell 的字符串内容）。 */
  title?: (row: T) => string
  /** 等宽列：ID、哈希、时间戳。 */
  mono?: boolean
  /** 右对齐等宽数值列：版本号等，配合 tabular-nums。 */
  numeric?: boolean
  width?: string
  /** 固定在滚动容器右侧：列多到出现横向滚动时，操作列仍然可见。 */
  sticky?: boolean
}

/**
 * DataGrid 风格表格：粘性表头、行悬浮高亮、斑马纹可关、单元格溢出省略并带 title。
 * `label` 作为无障碍表格名；`meta` 显示在工具条左侧（通常是总数）。
 */
export function DataTable<T>({
  label,
  meta,
  columns,
  rows,
  rowKey,
}: {
  label: string
  meta?: string
  columns: Array<DataTableColumn<T>>
  rows: T[]
  rowKey: (row: T) => string
}) {
  const [zebra, setZebra] = useState(true)

  return (
    <div>
      <div className="table-tools">
        <span className="table-count">{meta ?? `本页 ${rows.length} 行`}</span>
        <button
          type="button"
          className="btn btn-subtle btn-sm"
          aria-pressed={zebra}
          onClick={() => setZebra((value) => !value)}
          title="切换表格斑马纹"
        >
          <Icon name="table" size={16} />
          斑马纹
        </button>
      </div>
      <div className="table-scroll">
        <table className="data-table" data-zebra={zebra}>
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
                  className={column.sticky === true ? 'cell-sticky' : undefined}
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
                      <span className="cell-text" title={title}>
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
