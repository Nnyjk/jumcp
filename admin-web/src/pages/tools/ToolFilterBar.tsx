import type { CapabilityRisk, GroupItem, ToolHealth } from '../../api/types'
import { Card } from '../../components/Card'
import { Field } from '../../components/Field'
import { WINDOW_OPTIONS } from '../../constants'
import { toolGroupLabel as groupCodeLabel } from '../../labels'
import { CATEGORY_OPTIONS, HEALTH_OPTIONS, RISK_OPTIONS } from './toolFormat'

/** 工具清单筛选条：全部条件由页面持有，这里只负责渲染与回调。 */
export function ToolFilterBar({
  windowDays,
  onWindowDaysChange,
  health,
  onHealthChange,
  risk,
  onRiskChange,
  category,
  onCategoryChange,
  groupFilter,
  onGroupFilterChange,
  q,
  onQueryChange,
  groups,
  groupsPending,
  groupsError,
  refreshing,
  onRefresh,
  onClear,
}: {
  windowDays: number
  onWindowDaysChange: (days: number) => void
  health: '' | ToolHealth
  onHealthChange: (health: '' | ToolHealth) => void
  risk: '' | CapabilityRisk
  onRiskChange: (risk: '' | CapabilityRisk) => void
  category: string
  onCategoryChange: (category: string) => void
  groupFilter: string
  onGroupFilterChange: (group: string) => void
  q: string
  onQueryChange: (q: string) => void
  groups: GroupItem[]
  groupsPending: boolean
  groupsError: boolean
  refreshing: boolean
  onRefresh: () => void
  onClear: () => void
}) {
  return (
    <Card title="筛选">
      <div className="filter-bar">
        <Field label="时间窗">
          <select
            className="select"
            value={windowDays}
            onChange={(event) => onWindowDaysChange(Number(event.target.value))}
          >
            {WINDOW_OPTIONS.map((days) => (
              <option key={days} value={days}>
                最近 {days} 天
              </option>
            ))}
          </select>
        </Field>
        <Field label="健康状态">
          <select
            className="select"
            value={health}
            onChange={(event) => onHealthChange(event.target.value as '' | ToolHealth)}
          >
            {HEALTH_OPTIONS.map((option) => (
              <option key={option.value} value={option.value}>
                {option.label}
              </option>
            ))}
          </select>
        </Field>
        <Field label="风险">
          <select
            className="select"
            value={risk}
            onChange={(event) => onRiskChange(event.target.value as '' | CapabilityRisk)}
          >
            {RISK_OPTIONS.map((option) => (
              <option key={option.value} value={option.value}>
                {option.label}
              </option>
            ))}
          </select>
        </Field>
        <Field label="分类">
          <select
            className="select"
            value={category}
            onChange={(event) => onCategoryChange(event.target.value)}
          >
            {CATEGORY_OPTIONS.map((option) => (
              <option key={option.value} value={option.value}>
                {option.label}
              </option>
            ))}
          </select>
        </Field>
        <Field label="组">
          <select
            className="select"
            value={groupFilter}
            onChange={(event) => onGroupFilterChange(event.target.value)}
            disabled={groupsPending}
          >
            <option value="">
              {groupsPending ? '正在加载组…' : groupsError ? '组清单加载失败' : '全部组'}
            </option>
            {groups.map((group) => (
              <option key={group.code} value={group.code} title={group.code}>
                {groupCodeLabel(group.code, group.name)}
              </option>
            ))}
          </select>
        </Field>
        <label className="field field-grow">
          <span>搜索</span>
          <input
            className="input"
            type="search"
            value={q}
            onChange={(event) => onQueryChange(event.target.value)}
            placeholder="按工具 ID、组名或分类过滤"
          />
        </label>
        <div className="filter-actions">
          <button type="button" className="btn btn-secondary" onClick={onRefresh} disabled={refreshing}>
            刷新
          </button>
          <button type="button" className="btn btn-secondary" onClick={onClear}>
            清空筛选
          </button>
        </div>
      </div>
    </Card>
  )
}
