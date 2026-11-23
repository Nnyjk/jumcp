import { useMemo, useState } from 'react'
import { useSearchParams } from 'react-router-dom'
import { keepPreviousData, useQuery } from '@tanstack/react-query'
import { fetchAuditEvents } from '../../api/admin'
import { groupLabel, roleLabel, useGroupIndex, useRoleIndex, useRolesQuery } from '../../api/queries'
import type { AuditEnvelope, AuditQuery, GroupItem } from '../../api/types'
import { auditSummary, approvalText, isStructuredEvent } from '../../audit'
import { AuditDetail } from '../../components/AuditDetail'
import { Card } from '../../components/Card'
import { DataTable, type DataTableColumn } from '../../components/DataTable'
import { Modal } from '../../components/Modal'
import { EmptyState, ErrorState, LoadingState, Pagination } from '../../components/PageShell'
import { formatDateTime } from '../../format'
import { gsap } from 'gsap'
import { MOTION_DURATION, MOTION_EASE, useGsapScope } from '../../motion/useGsap'
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

/**
 * 审计事件视图：条件筛选 + 事件表 + 分页 + 详情弹层。
 *
 * 后端缺省返回全部来源面的事件（平台面与 MCP 面），可用「来源面」筛选收窄；
 * 请求参数只由 `queryFromFilter` 生成，URL 里只保留可深链的对象/令牌/事件 ID 与来源面。
 */
export function AuditEventsPanel({ initial }: { initial?: AuditInitialFilter | undefined }) {
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
    // 动机约束：翻页或变更查询条件时保持上一页数据可见，防止表格与分页容器坍塌为骨架屏导致视口晃动与高度抖动。
    placeholderData: keepPreviousData,
  })

  /* 结果区在数据到达后淡入一次；同一批数据重渲染（轮询/刷新）签名不变，不会闪。 */
  const resultKey =
    data === undefined ? 'pending' : `${data.offset}:${data.events.length}:${data.events[0]?.event_id ?? ''}`
  const resultRef = useGsapScope<HTMLDivElement>((scope) => {
    gsap.from(scope, {
      opacity: 0,
      y: 8,
      duration: MOTION_DURATION.enter,
      ease: MOTION_EASE,
      clearProps: 'opacity,transform',
    })
  }, [resultKey])

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
      cell: (row) => auditSummary(row, { group: (code) => groupIndex.get(code)?.name }),
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
              <span title={row.actor.role}>{roleLabel(roleIndex, row.actor.role)}</span> · 面{' '}
              {row.surface?.trim() ? row.surface : '—'}
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
    <>
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
          data !== undefined && data.total > 0 ? (
            <Pagination
              total={data.total}
              // 约束：绑定发起请求的本地 offset 而非旧响应数据的 data.offset，确保配合 keepPreviousData 时翻页页码即时响应，防止回跳。
              offset={offset}
              limit={data.limit}
              onChange={setOffset}
            />
          ) : undefined
        }
      >
        {/* 约束：.result-region 施加最小高度以在加载、空态、错误态与实际数据间切换时维持容器高度稳定，防止视口与卡片高度塌陷 */}
        <div className="result-region" ref={resultRef}>
          {isPending && <LoadingState rows={8} label="正在加载审计事件" />}
          {isError && <ErrorState error={error} />}
          {data !== undefined && data.events.length === 0 && (
            <EmptyState
              hint={
                data.total > 0
                  ? '暂无当前页记录（数据在翻页后可能发生变动），请返回上一页。'
                  : '暂无符合条件的审计事件。清空筛选或放宽时间范围后重试。'
              }
              action={
                <button
                  type="button"
                  className="btn btn-secondary"
                  onClick={
                    data.total > 0
                      ? () => setOffset(Math.max(0, offset - (data.limit || 50)))
                      : reset
                  }
                >
                  {data.total > 0 ? '返回上一页' : '清空筛选'}
                </button>
              }
            />
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
        </div>
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
    </>
  )
}
