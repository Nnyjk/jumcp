import { useMemo, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { fetchCapabilities } from '../api/admin'
import { groupLabel, useGroupIndex } from '../api/queries'
import type { CapabilityItem, CapabilityQuery, GroupItem } from '../api/types'
import { LifecycleBadge, RiskBadge } from '../components/Badge'
import { Card } from '../components/Card'
import { DataTable, type DataTableColumn } from '../components/DataTable'
import {
  EmptyState,
  ErrorState,
  LoadingState,
  PageShell,
  Pagination,
} from '../components/PageShell'
import { STATUS_LABEL, TOOL_KIND_LABEL } from '../labels'
import { useAdminView } from '../viewStack'

const DASH = '—'

const EMPTY_FILTER: CapabilityQuery = { status: '', group: '', q: '' }

/* 可发现/可调用是只读字段：用文本表达，不再用禁用开关（会被读成「坏掉的控件」）。 */
function columnsFor(
  groupIndex: Map<string, GroupItem>,
  onDetail: (id: string) => void,
): Array<DataTableColumn<CapabilityItem>> {
  /* 768 下冻结操作列左侧只有约 600px：可发现/可调用/生命周期必须紧跟 ID。 */
  return [
    {
      key: 'id',
      header: '能力 ID',
      mono: true,
      width: '200px',
      cell: (row) => row.id,
    },
    {
      key: 'discoverable',
      header: '可发现',
      width: '88px',
      cell: (row) => (
        <span className={row.discoverable ? 'flag-yes' : 'flag-no'} title={`discoverable=${row.discoverable}`}>
          {row.discoverable ? '是' : '否'}
        </span>
      ),
    },
    {
      key: 'callable',
      header: '可调用',
      width: '88px',
      cell: (row) => (
        <span className={row.callable ? 'flag-yes' : 'flag-no'} title={`callable=${row.callable}`}>
          {row.callable ? '是' : '否'}
        </span>
      ),
    },
    {
      key: 'status',
      header: '生命周期',
      width: '96px',
      cell: (row) => <LifecycleBadge status={row.status} />,
    },
    { key: 'risk', header: '风险', width: '72px', cell: (row) => <RiskBadge risk={row.risk} /> },
    {
      key: 'kind',
      header: '类型',
      width: '84px',
      cell: (row) => TOOL_KIND_LABEL[row.kind] ?? row.kind,
    },
    {
      key: 'group',
      header: '组',
      width: '132px',
      cell: (row) =>
        row.group === '' ? (
          DASH
        ) : (
          <span title={row.group}>{groupLabel(groupIndex, row.group)}</span>
        ),
    },
    {
      key: 'version',
      header: '版本',
      numeric: true,
      width: '52px',
      cell: (row) => row.version,
    },
    {
      key: 'actions',
      header: '操作',
      width: '76px',
      sticky: true,
      cell: (row) => (
        <button
          type="button"
          className="btn btn-secondary btn-sm"
          onClick={() => onDetail(row.id)}
        >
          详情
        </button>
      ),
    },
  ]
}

export default function ExceptionPage() {
  const [draft, setDraft] = useState<CapabilityQuery>(EMPTY_FILTER)
  const [filter, setFilter] = useState<CapabilityQuery>(EMPTY_FILTER)
  const [offset, setOffset] = useState(0)
  const groupIndex = useGroupIndex()
  const { openDossier } = useAdminView()
  const columns = useMemo(
    () => columnsFor(groupIndex, (id) => openDossier({ id })),
    [groupIndex, openDossier],
  )

  const { data, error, isPending, isError } = useQuery({
    queryKey: ['capabilities', filter, offset],
    queryFn: ({ signal }) => fetchCapabilities(filter, offset, signal),
  })

  return (
    <PageShell>
      <Card title="过滤">
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
              <option value="draft">{STATUS_LABEL.draft}</option>
              <option value="review">{STATUS_LABEL.review}</option>
              <option value="published">{STATUS_LABEL.published}</option>
              <option value="deprecated">{STATUS_LABEL.deprecated}</option>
            </select>
          </label>
          <label className="field">
            <span>组</span>
            <input
              className="input"
              value={draft.group ?? ''}
              onChange={(event) => setDraft({ ...draft, group: event.target.value })}
              placeholder="组名"
            />
          </label>
          <label className="field">
            <span>关键字</span>
            <input
              className="input"
              value={draft.q ?? ''}
              onChange={(event) => setDraft({ ...draft, q: event.target.value })}
              placeholder="关键字"
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
            : undefined
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
        <p className="hint scope-note">
          只读清单：可发现决定是否出现在调用方的 tools/list 与检索结果中；可调用决定能否被
          tools/call 直接调用。修改请到能力档案的「审批」分栏。
        </p>
        {isPending && <LoadingState rows={7} label="正在查询能力清单" />}
        {isError && <ErrorState error={error} />}
        {data !== undefined && data.items.length === 0 && (
          <EmptyState hint="没有可展示的能力记录。清空状态/组/关键字过滤后重试，或先在注册表登记能力。" />
        )}
        {data !== undefined && data.items.length > 0 && (
          <DataTable
            label="能力可见性与可调用性"
            columns={columns}
            rows={data.items}
            rowKey={(row) => row.id}
            minWidth="888px"
          />
        )}
      </Card>
    </PageShell>
  )
}
