import { useMemo, useState } from 'react'
import { useSearchParams } from 'react-router-dom'
import { useQuery } from '@tanstack/react-query'
import { fetchAuditEvents } from '../../api/admin'
import { groupLabel, roleLabel, useGroupIndex, useRoleIndex, useRolesQuery } from '../../api/queries'
import type { AuditEnvelope, AuditQuery, GroupItem } from '../../api/types'
import { auditSummary, approvalText, isStructuredEvent } from '../../audit'
import { AuditDetail } from '../../components/AuditDetail'
import { BackButton } from '../../components/BackButton'
import { Card } from '../../components/Card'
import { DataTable, type DataTableColumn } from '../../components/DataTable'
import { Modal } from '../../components/Modal'
import {
  EmptyState,
  ErrorState,
  LoadingState,
  PageShell,
  Pagination,
} from '../../components/PageShell'
import { formatDateTime } from '../../format'
import { ACTION_LABEL, EVENT_TYPE_LABEL, OBJECT_TYPE_LABEL } from '../../labels'
import {
  EMPTY_FILTER,
  initialFilter,
  queryFromFilter,
  type AuditInitialFilter,
  type FilterState,
} from './auditFilter'
import { AuditFilterForm } from './AuditFilterForm'

function actionCell(row: AuditEnvelope): string {
  const action = row.payload.action
  if (action !== undefined) return ACTION_LABEL[action] ?? action
  return EVENT_TYPE_LABEL[row.event_type] ?? row.event_type
}

function objectCell(row: AuditEnvelope, groupIndex: Map<string, GroupItem>): string {
  const payload = row.payload
  // 非结构化事件没有 object_*；摘要列已写明「仅原始载荷」，这里给破折号而不是重复同一句。
  if (!isStructuredEvent(row)) return '—'
  const type = payload.object_type === undefined ? '—' : OBJECT_TYPE_LABEL[payload.object_type]
  const id = payload.object_id ?? '—'
  const label =
    payload.object_type === 'group' && payload.object_id !== undefined
      ? groupLabel(groupIndex, payload.object_id)
      : id
  return `${type} ${label}`
}



export default function AuditQueryPage({
  initial,
  onBack,
  backLabel,
}: {
  /** 视图栈入口（令牌 / 能力档案）带来的预设筛选。 */
  initial?: AuditInitialFilter | undefined
  /** 视图内的返回入口；直接路由访问时缺省。 */
  onBack?: (() => void) | undefined
  backLabel?: string | undefined
}) {
  const [searchParams] = useSearchParams()
  const [draft, setDraft] = useState<FilterState>(() => initialFilter(initial, searchParams))
  const [filter, setFilter] = useState<AuditQuery>(() =>
    queryFromFilter(initialFilter(initial, searchParams)),
  )
  const [offset, setOffset] = useState(0)
  const [selected, setSelected] = useState<AuditEnvelope | null>(null)
  const groupIndex = useGroupIndex()
  const roleIndex = useRoleIndex()
  const rolesQuery = useRolesQuery()
  const roleChoices = useMemo(() => {
    const choices = (rolesQuery.data?.items ?? []).map((role) => ({
      code: role.code,
      name: role.name,
    }))
    if (draft.actor_role !== '' && !choices.some((choice) => choice.code === draft.actor_role)) {
      choices.push({ code: draft.actor_role, name: draft.actor_role })
    }
    return choices
  }, [rolesQuery.data, draft.actor_role])

  const { data, error, isPending, isError } = useQuery({
    queryKey: ['audit-events', filter, offset],
    queryFn: ({ signal }) => fetchAuditEvents(filter, offset, signal),
  })

  /**
   * 列顺序按首屏可见性排：768 下冻结操作列左侧约 600px，
   * 因此「时间 + 摘要」必须排在最前（摘要固定 300px，不用弹性宽度）。
   */
  const columns: Array<DataTableColumn<AuditEnvelope>> = [
    {
      key: 'ts',
      header: '时间',
      mono: true,
      width: '168px',
      cell: (row) => formatDateTime(row.ts),
      title: (row) => row.ts,
    },
    {
      key: 'summary',
      header: '摘要',
      width: '300px',
      wrap: true,
      cell: (row) =>
        auditSummary(row, { group: (code) => groupIndex.get(code)?.name }),
      title: (row) => {
        const approval = row.payload.approval
        const base = auditSummary(row, { group: (code) => groupIndex.get(code)?.name })
        return approval === null || approval === undefined
          ? base
          : `${base} · ${approvalText(approval)}`
      },
    },
    {
      key: 'actor',
      header: '主体',
      width: '240px',
      wrap: true,
      cell: (row) => {
        const source = row.actor.source?.trim() ? row.actor.source : '—'
        const tokenId = row.actor.token_id?.trim() ? row.actor.token_id : '—'
        // 令牌 ID 有 27 位：表格里只留前缀，完整值在 title 与详情弹层里。
        const tokenShort = tokenId.length > 12 ? `${tokenId.slice(0, 12)}…` : tokenId
        return (
          <span className="actor-cell">
            <span className="mono">{row.actor.id.trim() === '' ? '未认证调用方' : row.actor.id}</span>
            <span className="actor-meta">
              来源 {source} · 令牌 {tokenShort} · 角色{' '}
              <span title={row.actor.role}>{roleLabel(roleIndex, row.actor.role)}</span>
            </span>
          </span>
        )
      },
      title: (row) =>
        `${row.actor.id || '(empty)'} · 角色 ${roleLabel(roleIndex, row.actor.role)}（${
          row.actor.role
        }） · ${row.actor.type} · 来源 ${row.actor.source ?? '—'} · 令牌 ${row.actor.token_id ?? '—'}`,
    },
    {
      key: 'action',
      header: '动作',
      width: '120px',
      cell: (row) => actionCell(row),
      title: (row) => row.payload.action ?? row.event_type,
    },
    {
      key: 'object',
      header: '对象',
      width: '160px',
      cell: (row) => objectCell(row, groupIndex),
      title: (row) => row.payload.object_id ?? '',
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
          onClick={() => setSelected(row)}
          aria-label={`查看事件 ${row.event_id} 详情`}
        >
          详情
        </button>
      ),
    },
  ]

  const reset = () => {
    setDraft(EMPTY_FILTER)
    setFilter(queryFromFilter(EMPTY_FILTER))
    setOffset(0)
  }

  return (
    <PageShell>
      {onBack !== undefined && <BackButton label={backLabel ?? '返回'} onClick={onBack} />}
      <AuditFilterForm
        draft={draft}
        onChange={setDraft}
        onSubmit={() => {
          setOffset(0)
          setFilter(queryFromFilter(draft))
        }}
        onReset={reset}
        roleChoices={roleChoices}
        rolesPending={rolesQuery.isPending}
      />

      <Card
        title="审计事件"
        subtitle={
          data === undefined
            ? isError
              ? '查询失败'
              : '查询中'
            : `按时间${(data.order ?? filter.order) === 'asc' ? '正序' : '倒序'}`
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
          <EmptyState hint="没有符合条件的审计事件。放宽时间范围/对象/主体过滤后重试。" />
        )}
        {data !== undefined && data.events.length > 0 && (
          <DataTable
            label="审计事件"
            columns={columns}
            rows={data.events}
            rowKey={(row) => row.event_id}
            minWidth="1064px"
          />
        )}
      </Card>

      {selected !== null && (
        <Modal
          key={selected.event_id}
          size="xl"
          title={`审计详情 · ${selected.event_id}`}
          onClose={() => setSelected(null)}
        >
          <AuditDetail event={selected} />
        </Modal>
      )}
    </PageShell>
  )
}
