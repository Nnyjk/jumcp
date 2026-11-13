import { useMemo, useState } from 'react'
import { useSearchParams } from 'react-router-dom'
import { useQuery } from '@tanstack/react-query'
import { fetchAuditEvents } from '../api/admin'
import { groupLabel, roleLabel, useGroupIndex, useRoleIndex, useRolesQuery } from '../api/queries'
import type {
  AuditEnvelope,
  AuditEventType,
  AuditOrder,
  AuditQuery,
  AuditObjectType,
  GroupItem,
} from '../api/types'
import {
  auditSummary,
  approvalText,
  isStructuredEvent,
} from '../audit'
import { AuditDetail } from '../components/AuditDetail'
import { BackButton } from '../components/BackButton'
import { Card } from '../components/Card'
import { DataTable, type DataTableColumn } from '../components/DataTable'
import { Modal } from '../components/Modal'
import {
  EmptyState,
  ErrorState,
  LoadingState,
  PageShell,
  Pagination,
} from '../components/PageShell'
import { formatDateTime, localInputToIso, minutesAgoIso } from '../format'
import {
  ACTION_LABEL,
  EVENT_TYPE_LABEL,
  OBJECT_TYPE_LABEL,
} from '../labels'

const EVENT_TYPES: AuditEventType[] = [
  'tools_list_served',
  'capability_search',
  'capability_describe',
  'tool_call',
  'template_run',
  'policy_decision',
  'admin_change',
]

const OBJECT_TYPES: AuditObjectType[] = ['capability', 'group', 'tool', 'template', 'catalog', 'request', 'token']

type TimePreset = 'all' | '15m' | '1h' | '24h' | '7d' | 'custom'

const TIME_PRESETS: Array<{ value: TimePreset; label: string }> = [
  { value: '15m', label: '最近 15 分钟' },
  { value: '1h', label: '最近 1 小时' },
  { value: '24h', label: '最近 24 小时' },
  { value: '7d', label: '最近 7 天' },
  { value: 'custom', label: '自定义范围' },
  { value: 'all', label: '不限时间' },
]

const PRESET_MINUTES: Record<Exclude<TimePreset, 'all' | 'custom'>, number> = {
  '15m': 15,
  '1h': 60,
  '24h': 24 * 60,
  '7d': 7 * 24 * 60,
}

interface FilterState {
  tenant: string
  event_type: string
  actor_role: string
  actor_id: string
  token_id: string
  object_type: string
  object_id: string
  event_id: string
  order: AuditOrder
  preset: TimePreset
  customFrom: string
  customTo: string
}

const EMPTY_FILTER: FilterState = {
  tenant: '',
  event_type: '',
  actor_role: '',
  actor_id: '',
  token_id: '',
  object_type: '',
  object_id: '',
  event_id: '',
  order: 'desc',
  preset: 'all',
  customFrom: '',
  customTo: '',
}

/** 视图栈入口（访问令牌 / 能力档案）带入的预设筛选。 */
interface AuditInitialFilter {
  tokenId?: string | undefined
  objectId?: string | undefined
  eventId?: string | undefined
}

/** URL query（event_id / object_id / token_id）用于从调用详情、令牌与能力档案深链进入时预填筛选。 */
function filterFromUrl(params: URLSearchParams): FilterState {
  return {
    ...EMPTY_FILTER,
    event_id: params.get('event_id') ?? '',
    object_id: params.get('object_id') ?? '',
    object_type: params.get('object_type') ?? '',
    token_id: params.get('token_id') ?? '',
  }
}

/** 视图栈入口带来的预设筛选优先；直接访问 /audit?event_id=… 时退回 URL 参数。 */
function initialFilter(
  initial: AuditInitialFilter | undefined,
  params: URLSearchParams,
): FilterState {
  if (initial === undefined) return filterFromUrl(params)
  return {
    ...EMPTY_FILTER,
    token_id: initial.tokenId ?? '',
    object_id: initial.objectId ?? '',
    event_id: initial.eventId ?? '',
  }
}

/** 预设/自定义时间范围 → RFC3339；「不限」不下发时间参数。 */
function queryFromFilter(filter: FilterState): AuditQuery {
  const query: AuditQuery = {
    tenant: filter.tenant.trim() || undefined,
    event_type: filter.event_type || undefined,
    actor_role: filter.actor_role.trim() || undefined,
    actor_id: filter.actor_id.trim() || undefined,
    token_id: filter.token_id.trim() || undefined,
    object_type: filter.object_type || undefined,
    object_id: filter.object_id.trim() || undefined,
    event_id: filter.event_id.trim() || undefined,
    order: filter.order,
  }
  if (filter.preset === 'custom') {
    const after = localInputToIso(filter.customFrom)
    const before = localInputToIso(filter.customTo)
    if (after !== undefined) query.ts_after = after
    if (before !== undefined) query.ts_before = before
  } else if (filter.preset !== 'all') {
    query.ts_after = minutesAgoIso(PRESET_MINUTES[filter.preset])
  }
  return query
}

function actionCell(row: AuditEnvelope): string {
  const action = row.payload.action
  if (action !== undefined) return ACTION_LABEL[action] ?? action
  return EVENT_TYPE_LABEL[row.event_type] ?? row.event_type
}

function objectCell(row: AuditEnvelope, groupIndex: Map<string, GroupItem>): string {
  const payload = row.payload
  // 旧格式事件没有 object_*；摘要列已经写明「仅原始载荷」，这里给破折号而不是重复同一句。
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
      <Card title="查询条件">
        <form
          className="filter-bar"
          onSubmit={(event) => {
            event.preventDefault()
            setOffset(0)
            setFilter(queryFromFilter(draft))
          }}
        >
          <label className="field">
            <span>时间范围</span>
            <select
              className="select"
              value={draft.preset}
              onChange={(event) =>
                setDraft({ ...draft, preset: event.target.value as TimePreset })
              }
            >
              {TIME_PRESETS.map((preset) => (
                <option key={preset.value} value={preset.value}>
                  {preset.label}
                </option>
              ))}
            </select>
          </label>
          {draft.preset === 'custom' && (
            <>
              <label className="field">
                <span>起始时间</span>
                <input
                  className="input"
                  type="datetime-local"
                  value={draft.customFrom}
                  onChange={(event) => setDraft({ ...draft, customFrom: event.target.value })}
                />
              </label>
              <label className="field">
                <span>结束时间</span>
                <input
                  className="input"
                  type="datetime-local"
                  value={draft.customTo}
                  onChange={(event) => setDraft({ ...draft, customTo: event.target.value })}
                />
              </label>
            </>
          )}
          <label className="field">
            <span>排序</span>
            <select
              className="select"
              value={draft.order}
              onChange={(event) => setDraft({ ...draft, order: event.target.value as AuditOrder })}
            >
              <option value="desc">最新优先</option>
              <option value="asc">最旧优先</option>
            </select>
          </label>
          <label className="field">
            <span title="event_id">事件 ID</span>
            <input
              className="input"
              value={draft.event_id}
              onChange={(event) => setDraft({ ...draft, event_id: event.target.value })}
              placeholder="精确匹配"
            />
          </label>
          <label className="field">
            <span title="object_type">对象类型</span>
            <select
              className="select"
              value={draft.object_type}
              onChange={(event) => setDraft({ ...draft, object_type: event.target.value })}
            >
              <option value="">全部</option>
              {OBJECT_TYPES.map((type) => (
                <option key={type} value={type}>
                  {OBJECT_TYPE_LABEL[type]}
                </option>
              ))}
            </select>
          </label>
          <label className="field">
            <span title="object_id">对象 ID</span>
            <input
              className="input"
              value={draft.object_id}
              onChange={(event) => setDraft({ ...draft, object_id: event.target.value })}
              placeholder="能力/组/工具 ID"
            />
          </label>
          <label className="field">
            <span title="actor_id">主体 ID</span>
            <input
              className="input"
              value={draft.actor_id}
              onChange={(event) => setDraft({ ...draft, actor_id: event.target.value })}
              placeholder="精确匹配"
            />
          </label>
          <label className="field">
            <span title="token_id">令牌 ID</span>
            <input
              className="input"
              value={draft.token_id}
              onChange={(event) => setDraft({ ...draft, token_id: event.target.value })}
              placeholder="精确匹配"
            />
          </label>
          <label className="field">
            <span title="actor_role">角色</span>
            <select
              className="select"
              value={draft.actor_role}
              onChange={(event) => setDraft({ ...draft, actor_role: event.target.value })}
              disabled={rolesQuery.isPending}
            >
              <option value="">全部角色</option>
              {roleChoices.map((role) => (
                <option key={role.code} value={role.code} title={role.code}>
                  {role.name}
                </option>
              ))}
            </select>
          </label>
          <label className="field">
            <span>事件类型</span>
            <select
              className="select"
              value={draft.event_type}
              onChange={(event) => setDraft({ ...draft, event_type: event.target.value })}
            >
              <option value="">全部</option>
              {EVENT_TYPES.map((type) => (
                <option key={type} value={type} title={type}>
                  {EVENT_TYPE_LABEL[type] ?? type}
                </option>
              ))}
            </select>
          </label>
          <label className="field">
            <span>租户</span>
            <input
              className="input"
              value={draft.tenant}
              onChange={(event) => setDraft({ ...draft, tenant: event.target.value })}
              placeholder="租户标识"
            />
          </label>
          <div className="filter-actions">
            <button type="submit" className="btn btn-primary">
              查询
            </button>
            <button type="button" className="btn btn-secondary" onClick={reset}>
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
