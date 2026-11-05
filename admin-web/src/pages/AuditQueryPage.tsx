import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { fetchAuditEvents } from '../api/admin'
import type { AuditEventType, AuditQuery } from '../api/types'
import { EmptyState, ErrorState, LoadingState, PageShell } from '../components/PageShell'

const EVENT_TYPES: AuditEventType[] = [
  'session_init',
  'tools_list_served',
  'capability_search',
  'capability_describe',
  'tool_call',
  'template_run',
  'policy_decision',
  'admin_change',
]

const EMPTY_FILTER: AuditQuery = {
  tenant: '',
  event_type: '',
  session_id: '',
  actor_role: '',
}

export default function AuditQueryPage() {
  const [draft, setDraft] = useState<AuditQuery>(EMPTY_FILTER)
  const [filter, setFilter] = useState<AuditQuery>(EMPTY_FILTER)

  const { data, error, isPending, isError } = useQuery({
    queryKey: ['audit-events', filter],
    queryFn: ({ signal }) => fetchAuditEvents(filter, signal),
  })

  return (
    <PageShell
      title="审计查询"
      mode="只读"
      description="按 §4.4.3 审计 envelope 查询追加日志。审计视图只读：没有编辑控件，也不得导出后回写状态。"
    >
      <form
        className="filter-bar"
        onSubmit={(event) => {
          event.preventDefault()
          setFilter(draft)
        }}
      >
        <label>
          租户
          <input
            value={draft.tenant ?? ''}
            onChange={(event) => setDraft({ ...draft, tenant: event.target.value })}
            placeholder="tenant"
          />
        </label>
        <label>
          事件类型
          <select
            value={draft.event_type ?? ''}
            onChange={(event) => setDraft({ ...draft, event_type: event.target.value })}
          >
            <option value="">全部</option>
            {EVENT_TYPES.map((type) => (
              <option key={type} value={type}>
                {type}
              </option>
            ))}
          </select>
        </label>
        <label>
          会话 ID
          <input
            value={draft.session_id ?? ''}
            onChange={(event) => setDraft({ ...draft, session_id: event.target.value })}
            placeholder="session_id"
          />
        </label>
        <label>
          角色
          <input
            value={draft.actor_role ?? ''}
            onChange={(event) => setDraft({ ...draft, actor_role: event.target.value })}
            placeholder="actor_role"
          />
        </label>
        <button type="submit" className="btn">
          查询
        </button>
        <button
          type="button"
          className="btn btn-secondary"
          onClick={() => {
            setDraft(EMPTY_FILTER)
            setFilter(EMPTY_FILTER)
          }}
        >
          重置
        </button>
      </form>

      {isPending && <LoadingState />}
      {isError && <ErrorState error={error} />}
      {data && data.length === 0 && <EmptyState hint="没有符合条件的审计事件。" />}
      {data && data.length > 0 && (
        <table className="table">
          <thead>
            <tr>
              <th>时间</th>
              <th>事件 ID</th>
              <th>租户</th>
              <th>主体</th>
              <th>角色</th>
              <th>会话 ID</th>
              <th>事件类型</th>
              <th>哈希链尾</th>
            </tr>
          </thead>
          <tbody>
            {data.map((event) => (
              <tr key={event.event_id}>
                <td>{event.ts}</td>
                <td className="mono">{event.event_id}</td>
                <td>{event.tenant}</td>
                <td>{event.actor.id || event.actor.type}</td>
                <td>{event.actor.role}</td>
                <td className="mono">{event.session_id || '—'}</td>
                <td>{event.event_type}</td>
                <td className="mono" title={`prev_hash=${event.prev_hash}`}>
                  {event.hash}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </PageShell>
  )
}
