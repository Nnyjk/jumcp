import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { fetchCapabilities, PAGE_LIMIT } from '../api/admin'
import type { CapabilityItem, CapabilityQuery } from '../api/types'
import { RiskBadge, StatusBadge } from '../components/Badge'
import { Card } from '../components/Card'
import { DataTable, type DataTableColumn } from '../components/DataTable'
import {
  DisabledAction,
  EmptyState,
  ErrorState,
  LoadingState,
  PageShell,
  Pagination,
} from '../components/PageShell'
import { ToggleSwitch } from '../components/ToggleSwitch'

const EMPTY_FILTER: CapabilityQuery = { status: '', group: '', q: '' }

const COLUMNS: Array<DataTableColumn<CapabilityItem>> = [
  { key: 'id', header: '能力 ID', mono: true, width: '160px', cell: (row) => row.id },
  {
    key: 'kind',
    header: '类型',
    width: '84px',
    cell: (row) => row.kind,
  },
  { key: 'group', header: '组', mono: true, width: '116px', cell: (row) => row.group },
  { key: 'risk', header: '风险', width: '72px', cell: (row) => <RiskBadge risk={row.risk} /> },
  { key: 'status', header: '状态', width: '116px', cell: (row) => <StatusBadge status={row.status} /> },
  {
    key: 'discoverable',
    header: 'discoverable',
    width: '112px',
    cell: (row) => (
      <ToggleSwitch
        checked={row.discoverable}
        disabled
        label={row.discoverable ? '是' : '否'}
        ariaLabel={`discoverable：${row.discoverable ? '是' : '否'}（只读展示）`}
      />
    ),
  },
  {
    key: 'callable',
    header: 'callable',
    width: '100px',
    cell: (row) => (
      <ToggleSwitch
        checked={row.callable}
        disabled
        label={row.callable ? '是' : '否'}
        ariaLabel={`callable：${row.callable ? '是' : '否'}（只读展示）`}
      />
    ),
  },
  { key: 'version', header: '版本', numeric: true, width: '52px', cell: (row) => row.version },
  { key: 'successor', header: '后继', mono: true, width: '76px', cell: (row) => row.successor_id || '—' },
  { key: 'updated', header: '更新时间', mono: true, width: '176px', cell: (row) => row.updated || '—' },
  {
    key: 'actions',
    header: '操作',
    width: '156px',
    sticky: true,
    cell: () => <DisabledAction compact label="调整可见性" />,
  },
]

export default function ExceptionPage() {
  const [draft, setDraft] = useState<CapabilityQuery>(EMPTY_FILTER)
  const [filter, setFilter] = useState<CapabilityQuery>(EMPTY_FILTER)
  const [offset, setOffset] = useState(0)

  const { data, error, isPending, isError } = useQuery({
    queryKey: ['capabilities', filter, offset],
    queryFn: ({ signal }) => fetchCapabilities(filter, offset, signal),
  })

  return (
    <PageShell>
      <Card flush>
        <div className="command-bar">
          <DisabledAction label="切换 discoverable" />
          <DisabledAction label="切换 callable" />
          <span className="hint">
            例外处理不提供「以某租户/某 Agent 身份执行工具」的入口；排障走 MCP Inspector + 审计回放。
          </span>
        </div>
      </Card>

      <Card title="过滤" subtitle="status / group / q 与后端能力列表白名单一致">
        <form
          className="filter-bar"
          onSubmit={(event) => {
            event.preventDefault()
            setOffset(0)
            setFilter(draft)
          }}
        >
          <label className="field">
            <span>状态</span>
            <select
              className="select"
              value={draft.status ?? ''}
              onChange={(event) => setDraft({ ...draft, status: event.target.value })}
            >
              <option value="">全部</option>
              <option value="draft">draft</option>
              <option value="review">review</option>
              <option value="published">published</option>
              <option value="deprecated">deprecated</option>
            </select>
          </label>
          <label className="field">
            <span>组</span>
            <input
              className="input"
              value={draft.group ?? ''}
              onChange={(event) => setDraft({ ...draft, group: event.target.value })}
              placeholder="group"
            />
          </label>
          <label className="field">
            <span>关键字</span>
            <input
              className="input"
              value={draft.q ?? ''}
              onChange={(event) => setDraft({ ...draft, q: event.target.value })}
              placeholder="q"
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
        title="能力可见性"
        subtitle={
          data === undefined
            ? isError
              ? '查询失败'
              : '查询中'
            : `共 ${data.total} 条 · 每页 ${PAGE_LIMIT} 条`
        }
        flush
        footer={
          data !== undefined && data.items.length > 0 ? (
            <Pagination
              total={data.total}
              offset={data.offset}
              limit={data.limit}
              onChange={setOffset}
            />
          ) : undefined
        }
      >
        {isPending && <LoadingState rows={7} label="正在查询能力清单" />}
        {isError && <ErrorState error={error} />}
        {data !== undefined && data.items.length === 0 && (
          <EmptyState hint="没有可展示的能力记录。清空状态/组/关键字过滤后重试，或先在 registry 注册能力。" />
        )}
        {data !== undefined && data.items.length > 0 && (
          <DataTable
            label="能力可见性与可调用性"
            meta={`本页 ${data.items.length} 行`}
            columns={COLUMNS}
            rows={data.items}
            rowKey={(row) => row.id}
          />
        )}
      </Card>
    </PageShell>
  )
}
