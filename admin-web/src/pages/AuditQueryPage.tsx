import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { fetchAuditEvents, PAGE_LIMIT } from '../api/admin'
import type { AuditEnvelope, AuditEventType, AuditQuery } from '../api/types'
import { Card } from '../components/Card'
import { DataTable, type DataTableColumn } from '../components/DataTable'
import {
  EmptyState,
  ErrorState,
  LoadingState,
  PageShell,
  Pagination,
} from '../components/PageShell'

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

const COLUMNS: Array<DataTableColumn<AuditEnvelope>> = [
  { key: 'ts', header: '时间', mono: true, width: '196px', cell: (row) => row.ts },
  { key: 'event_id', header: '事件 ID', mono: true, width: '150px', cell: (row) => row.event_id },
  { key: 'tenant', header: '租户', width: '76px', cell: (row) => row.tenant },
  { key: 'actor', header: '主体', width: '72px', cell: (row) => row.actor.id || row.actor.type },
  { key: 'role', header: '角色', width: '68px', cell: (row) => row.actor.role },
  { key: 'session', header: '会话 ID', mono: true, width: '88px', cell: (row) => row.session_id || '—' },
  {
    key: 'event_type',
    header: '事件类型',
    width: '150px',
    cell: (row) => <code className="code-chip">{row.event_type}</code>,
  },
  {
    key: 'hash',
    header: '哈希链尾',
    mono: true,
    width: '96px',
    cell: (row) => row.hash,
    title: (row) => `hash=${row.hash}\nprev_hash=${row.prev_hash}`,
  },
]

export default function AuditQueryPage() {
  const [draft, setDraft] = useState<AuditQuery>(EMPTY_FILTER)
  const [filter, setFilter] = useState<AuditQuery>(EMPTY_FILTER)
  const [offset, setOffset] = useState(0)

  const { data, error, isPending, isError } = useQuery({
    queryKey: ['audit-events', filter, offset],
    queryFn: ({ signal }) => fetchAuditEvents(filter, offset, signal),
  })

  return (
    <PageShell>
      <Card title="查询条件" subtitle="过滤键与后端 FILTER_KEYS 白名单一致">
        <form
          className="filter-bar"
          onSubmit={(event) => {
            event.preventDefault()
            setOffset(0)
            setFilter(draft)
          }}
        >
          <label className="field">
            <span>租户</span>
            <input
              className="input"
              value={draft.tenant ?? ''}
              onChange={(event) => setDraft({ ...draft, tenant: event.target.value })}
              placeholder="tenant"
            />
          </label>
          <label className="field">
            <span>事件类型</span>
            <select
              className="select"
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
          <label className="field">
            <span>会话 ID</span>
            <input
              className="input"
              value={draft.session_id ?? ''}
              onChange={(event) => setDraft({ ...draft, session_id: event.target.value })}
              placeholder="session_id"
            />
          </label>
          <label className="field">
            <span>角色</span>
            <input
              className="input"
              value={draft.actor_role ?? ''}
              onChange={(event) => setDraft({ ...draft, actor_role: event.target.value })}
              placeholder="actor_role"
            />
          </label>
          <div className="filter-actions">
            <button type="submit" className="btn btn-primary">
              查询
            </button>
            <button
              type="button"
              className="btn btn-secondary"
              onClick={() => {
                setDraft(EMPTY_FILTER)
                setFilter(EMPTY_FILTER)
                setOffset(0)
              }}
            >
              重置
            </button>
          </div>
        </form>
      </Card>

      <Card
        title="审计事件"
        subtitle={
          data === undefined
            ? isError
              ? '查询失败'
              : '查询中'
            : `共 ${data.total} 条 · 每页 ${PAGE_LIMIT} 条`
        }
        flush
        footer={
          data !== undefined && data.events.length > 0 ? (
            <Pagination
              total={data.total}
              offset={data.offset}
              limit={data.limit}
              onChange={setOffset}
            />
          ) : undefined
        }
      >
        {isPending && <LoadingState rows={8} label="正在查询审计事件" />}
        {isError && <ErrorState error={error} />}
        {data !== undefined && data.events.length === 0 && (
          <EmptyState hint="没有符合条件的审计事件。放宽租户/会话/角色过滤，或清空事件类型后重试。" />
        )}
        {data !== undefined && data.events.length > 0 && (
          <DataTable
            label="审计事件"
            meta={`本页 ${data.events.length} 行`}
            columns={COLUMNS}
            rows={data.events}
            rowKey={(row) => row.event_id}
          />
        )}
      </Card>
    </PageShell>
  )
}
