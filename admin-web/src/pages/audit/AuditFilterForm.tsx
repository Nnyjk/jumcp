import type { AuditEventType, AuditObjectType, AuditOrder } from '../../api/types'
import { Card } from '../../components/Card'
import { Field } from '../../components/Field'
import { EVENT_TYPE_LABEL, OBJECT_TYPE_LABEL } from '../../labels'
import { TIME_PRESETS, type FilterState, type TimePreset } from './auditFilter'

const EVENT_TYPES: AuditEventType[] = [
  'tools_list_served',
  'capability_search',
  'capability_describe',
  'tool_call',
  'template_run',
  'policy_decision',
  'admin_change',
]

const OBJECT_TYPES: AuditObjectType[] = [
  'capability',
  'group',
  'tool',
  'template',
  'catalog',
  'request',
  'token',
  'upstream',
]

/** 审计事件表单：草稿状态由面板持有，提交时才转成实际查询条件。 */
export function AuditFilterForm({
  draft,
  onChange,
  onSubmit,
  onReset,
  roleChoices,
  rolesPending,
}: {
  draft: FilterState
  onChange: (draft: FilterState) => void
  onSubmit: () => void
  onReset: () => void
  roleChoices: Array<{ code: string; name: string }>
  rolesPending: boolean
}) {
  return (
    <Card title="查询条件">
      <form
        className="filter-bar"
        onSubmit={(event) => {
          event.preventDefault()
          onSubmit()
        }}
      >
        <Field label="时间范围">
          <select
            className="select"
            value={draft.preset}
            onChange={(event) => onChange({ ...draft, preset: event.target.value as TimePreset })}
          >
            {TIME_PRESETS.map((preset) => (
              <option key={preset.value} value={preset.value}>
                {preset.label}
              </option>
            ))}
          </select>
        </Field>
        {draft.preset === 'custom' && (
          <>
            <Field label="起始时间">
              <input
                className="input"
                type="datetime-local"
                value={draft.customFrom}
                onChange={(event) => onChange({ ...draft, customFrom: event.target.value })}
              />
            </Field>
            <Field label="结束时间">
              <input
                className="input"
                type="datetime-local"
                value={draft.customTo}
                onChange={(event) => onChange({ ...draft, customTo: event.target.value })}
              />
            </Field>
          </>
        )}
        <Field label="排序">
          <select
            className="select"
            value={draft.order}
            onChange={(event) => onChange({ ...draft, order: event.target.value as AuditOrder })}
          >
            <option value="desc">最新优先</option>
            <option value="asc">最旧优先</option>
          </select>
        </Field>
        <Field label="来源面" title="surface">
          <select
            className="select"
            value={draft.surface}
            onChange={(event) =>
              onChange({ ...draft, surface: event.target.value as FilterState['surface'] })
            }
          >
            <option value="">全部来源</option>
            <option value="admin">平台面（admin）</option>
            <option value="mcp">MCP 面（mcp）</option>
          </select>
        </Field>
        <Field label="事件 ID" title="event_id">
          <input
            className="input"
            value={draft.event_id}
            onChange={(event) => onChange({ ...draft, event_id: event.target.value })}
            placeholder="精确匹配"
          />
        </Field>
        <Field label="对象类型" title="object_type">
          <select
            className="select"
            value={draft.object_type}
            onChange={(event) => onChange({ ...draft, object_type: event.target.value })}
          >
            <option value="">全部</option>
            {OBJECT_TYPES.map((type) => (
              <option key={type} value={type}>
                {OBJECT_TYPE_LABEL[type]}
              </option>
            ))}
          </select>
        </Field>
        <Field label="对象 ID" title="object_id">
          <input
            className="input"
            value={draft.object_id}
            onChange={(event) => onChange({ ...draft, object_id: event.target.value })}
            placeholder="能力/组/工具 ID"
          />
        </Field>
        <Field label="主体 ID" title="actor_id">
          <input
            className="input"
            value={draft.actor_id}
            onChange={(event) => onChange({ ...draft, actor_id: event.target.value })}
            placeholder="精确匹配"
          />
        </Field>
        <Field label="令牌 ID" title="token_id">
          <input
            className="input"
            value={draft.token_id}
            onChange={(event) => onChange({ ...draft, token_id: event.target.value })}
            placeholder="精确匹配"
          />
        </Field>
        <Field label="角色" title="actor_role">
          <select
            className="select"
            value={draft.actor_role}
            onChange={(event) => onChange({ ...draft, actor_role: event.target.value })}
            disabled={rolesPending}
          >
            <option value="">全部角色</option>
            {roleChoices.map((role) => (
              <option key={role.code} value={role.code} title={role.code}>
                {role.name}
              </option>
            ))}
          </select>
        </Field>
        <Field label="事件类型">
          <select
            className="select"
            value={draft.event_type}
            onChange={(event) => onChange({ ...draft, event_type: event.target.value })}
          >
            <option value="">全部</option>
            {EVENT_TYPES.map((type) => (
              <option key={type} value={type} title={type}>
                {EVENT_TYPE_LABEL[type] ?? type}
              </option>
            ))}
          </select>
        </Field>
        <Field label="租户">
          <input
            className="input"
            value={draft.tenant}
            onChange={(event) => onChange({ ...draft, tenant: event.target.value })}
            placeholder="租户标识"
          />
        </Field>
        <div className="filter-actions">
          <button type="submit" className="btn btn-primary">
            查询
          </button>
          <button type="button" className="btn btn-secondary" onClick={onReset}>
            重置
          </button>
        </div>
      </form>
    </Card>
  )
}
