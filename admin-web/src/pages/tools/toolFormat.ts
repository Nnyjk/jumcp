import { groupLabel } from '../../api/queries'
import type { CapabilityRisk, GroupItem, ToolHealth, ToolSource, ToolStatsRow } from '../../api/types'
import { RISK_LABEL, TOOL_CATEGORY_LABEL, UPSTREAM_CATEGORY_LABEL, toolGroupLabel } from '../../labels'

export const HEALTH_OPTIONS: Array<{ value: '' | ToolHealth; label: string }> = [
  { value: '', label: '全部' },
  { value: 'ok', label: '正常' },
  { value: 'degraded', label: '降级' },
  { value: 'failing', label: '故障' },
  { value: 'unknown', label: '未知' },
]

/** 来源分段控件的选项：受管能力 / 内置工具，卡片标题与分段按钮共用同一份标签。 */
export const SOURCE_OPTIONS: Array<{ value: ToolSource; label: string }> = [
  { value: 'managed', label: '受管能力' },
  { value: 'builtin', label: '内置工具' },
]

/** 风险筛选项：标签与表格里的风险徽标共用 labels 的中文名，避免两处各写一份。 */
export const RISK_OPTIONS: Array<{ value: '' | CapabilityRisk; label: string }> = [
  { value: '', label: '全部' },
  { value: 'read', label: RISK_LABEL.read },
  { value: 'write', label: RISK_LABEL.write },
  { value: 'sensitive', label: RISK_LABEL.sensitive },
  { value: 'admin', label: RISK_LABEL.admin },
]

/** 上游分类：三组内置分类使用中文名，custom 为自定义接入。 */
export const CATEGORY_OPTIONS: Array<{ value: string; label: string }> = [
  { value: '', label: '全部分类' },
  { value: 'basic', label: UPSTREAM_CATEGORY_LABEL.basic },
  { value: 'analysis', label: UPSTREAM_CATEGORY_LABEL.analysis },
  { value: 'admin', label: UPSTREAM_CATEGORY_LABEL.admin },
  { value: 'custom', label: UPSTREAM_CATEGORY_LABEL.custom },
]

export interface ToolFilters {
  source: ToolSource
  health: '' | ToolHealth
  risk: '' | CapabilityRisk
  category: string
  group: string
  q: string
}

/** 过滤后按调用量倒序（同量用 id 稳定排序），组名与分类名也参与关键字匹配。 */
export function filterTools(
  rows: ToolStatsRow[],
  filters: ToolFilters,
  groupIndex: Map<string, GroupItem>,
): ToolStatsRow[] {
  const needle = filters.q.trim().toLowerCase()
  return rows
    .filter((row) => row.source === filters.source)
    .filter((row) => filters.health === '' || row.health === filters.health)
    .filter((row) => filters.risk === '' || row.risk === filters.risk)
    .filter((row) => filters.category === '' || row.category === filters.category)
    .filter((row) => filters.group === '' || row.group === filters.group)
    .filter(
      (row) =>
        needle === '' ||
        row.id.toLowerCase().includes(needle) ||
        row.group.toLowerCase().includes(needle) ||
        toolGroupLabel(row.group, groupLabel(groupIndex, row.group))
          .toLowerCase()
          .includes(needle) ||
        (TOOL_CATEGORY_LABEL[row.category ?? ''] ?? '').toLowerCase().includes(needle),
    )
    .sort((left, right) => right.calls - left.calls || left.id.localeCompare(right.id))
}
