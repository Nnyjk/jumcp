import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { fetchToolCalls } from '../api/admin'
import type { RoleItem, ToolCallRecord } from '../api/types'
import { roleLabel, useRoleIndex } from '../api/queries'
import { actorLabel } from '../audit'
import { DASH, dashText, formatDateTime, truncateInline } from '../format'
import { inlineJson } from '../json'
import { EVENT_TYPE_LABEL, TOOL_CACHE_LABEL, errorCodeText } from '../labels'
import { OutcomeBadge } from './Badge'
import { DataTable, type DataTableColumn } from './DataTable'
import { DetailField, DetailGrid } from './DetailGrid'
import { CodeBlock, JsonBlock } from './JsonBlock'
import { Modal } from './Modal'
import { EmptyState, ErrorState, LoadingState, Pagination } from './PageShell'
import { AuditScopeNote } from './ScopeNote'

/** 0/null 都表示没有执行 SQL（缓存命中或策略拒绝），不能读成「极快」。 */
function elapsedText(value: number | null): string {
  if (value === null || value === 0) return '未执行'
  return `${value} ms`
}


function callColumns(
  roleIndex: Map<string, RoleItem>,
  onDetail: (call: ToolCallRecord) => void,
): Array<DataTableColumn<ToolCallRecord>> {
  return [
    {
      key: 'ts',
      header: '时间',
      mono: true,
      width: '168px',
      cell: (row) => formatDateTime(row.ts),
      title: (row) => row.ts,
    },
    {
      key: 'event_type',
      header: '类型',
      width: '96px',
      cell: (row) => EVENT_TYPE_LABEL[row.event_type] ?? row.event_type,
      title: (row) => row.event_type,
    },
    {
      key: 'actor',
      header: '主体',
      width: '180px',
      cell: (row) => (
        <span className="cell-stack">
          <span>{actorLabel(row.actor)}</span>
          <span className="cell-sub" title={row.actor.role ?? undefined}>
            来源 {dashText(row.actor.source)} · 令牌 {dashText(row.actor.token_id)} · 角色{' '}
            {roleLabel(roleIndex, row.actor.role ?? '')}
          </span>
        </span>
      ),
      title: (row) =>
        `${row.actor.id ?? DASH} · 角色 ${roleLabel(roleIndex, row.actor.role ?? '')}（${
          row.actor.role ?? DASH
        }） · 来源 ${dashText(row.actor.source)} · 令牌 ${dashText(row.actor.token_id)}`,
    },
    {
      key: 'outcome',
      header: '结果',
      width: '84px',
      cell: (row) => <OutcomeBadge outcome={row.outcome} />,
    },
    {
      key: 'error',
      header: '错误码 / 原因',
      width: '200px',
      cell: (row) => (
        <span className="cell-stack">
          <span className="mono">{errorCodeText(row.error_code)}</span>
          <span className="cell-sub">{truncateInline(row.error_message)}</span>
        </span>
      ),
      title: (row) =>
        `${row.error_code ?? DASH}${row.error_message === null ? '' : ` · ${row.error_message}`}`,
    },
    {
      key: 'elapsed',
      header: '执行耗时',
      numeric: true,
      width: '84px',
      cell: (row) => elapsedText(row.elapsed_ms),
    },
    {
      key: 'sql',
      header: '执行 SQL',
      width: '200px',
      cell: (row) => truncateInline(row.executed_script),
      title: (row) => row.executed_script ?? DASH,
    },
    {
      key: 'cache',
      header: '模板缓存',
      width: '76px',
      cell: (row) => (row.cache === null ? DASH : (TOOL_CACHE_LABEL[row.cache] ?? row.cache)),
    },
    {
      key: 'tokens',
      header: '响应 tokens',
      numeric: true,
      width: '100px',
      cell: (row) => (row.tokens === null ? DASH : row.tokens),
    },
    {
      key: 'version',
      header: '执行版本',
      numeric: true,
      width: '76px',
      cell: (row) => (row.version === null ? DASH : `v${row.version}`),
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
          onClick={() => onDetail(row)}
          aria-label={`查看调用 ${row.event_id} 详情`}
        >
          详情
        </button>
      ),
    },
  ]
}

/**
 * 能力/工具的调用记录：按与台账一致的时间窗查询，行内含事件类型、错误原因与执行 SQL。
 * 详情展示 SQL + 绑定值 + 原始入参，不再出现会话与 MCP 码字段。
 */
export function CallTable({ id, windowDays }: { id: string; windowDays: number }) {
  const [offset, setOffset] = useState(0)
  const [selected, setSelected] = useState<ToolCallRecord | null>(null)
  const roleIndex = useRoleIndex()

  const query = useQuery({
    queryKey: ['tool-calls', id, windowDays, offset],
    queryFn: ({ signal }) => fetchToolCalls(id, offset, windowDays, signal),
  })

  const data = query.data

  return (
    <div className="detail-block">
      <AuditScopeNote inline />
      {query.isPending && <LoadingState rows={5} label="正在加载调用记录" />}
      {query.isError && (
        <div className="state-wrap">
          <ErrorState error={query.error} />
          <button type="button" className="btn btn-secondary" onClick={() => query.refetch()}>
            重试
          </button>
        </div>
      )}
      {data !== undefined && data.calls.length === 0 && (
        <EmptyState hint={`最近 ${windowDays} 天没有该对象的调用记录。可扩大时间窗后再查。`} />
      )}
      {data !== undefined && data.calls.length > 0 && (
        <>
          <DataTable
            label={`${id} 的调用记录`}
            meta={`最近 ${windowDays} 天 · 按时间倒序`}
            columns={callColumns(roleIndex, setSelected)}
            rows={data.calls}
            rowKey={(call) => call.event_id}
            minWidth="1340px"
          />
          <Pagination
            total={data.total}
            offset={data.offset}
            limit={data.limit}
            onChange={setOffset}
          />
        </>
      )}

      {selected !== null && (
        <Modal
          key={selected.event_id}
          size="lg"
          title={`调用详情 · ${selected.event_id}`}
          onClose={() => setSelected(null)}
        >
          <CallDetail call={selected} />
        </Modal>
      )}
    </div>
  )
}

function CallDetail({ call }: { call: ToolCallRecord }) {
  const roleIndex = useRoleIndex()
  const hasArguments = call.arguments !== null && call.arguments !== undefined
  const hasScript = call.executed_script !== null && call.executed_script.trim() !== ''
  const bound = call.bound_params ?? []

  return (
    <div className="audit-detail">
      <DetailGrid>
        <DetailField label="事件 ID" mono title={call.event_id}>
          {call.event_id}
        </DetailField>
        <DetailField label="时间" mono title={call.ts}>
          {formatDateTime(call.ts)}
        </DetailField>
        <DetailField label="事件类型" title={call.event_type}>
          {EVENT_TYPE_LABEL[call.event_type] ?? call.event_type}
        </DetailField>
        <DetailField label="对象" mono title={call.object_type}>
          {call.object_type} {call.object_id}
        </DetailField>
        <DetailField label="主体" mono>
          {call.actor.id === null || call.actor.id.trim() === '' ? '未认证调用方' : call.actor.id}
        </DetailField>
        <DetailField label="角色" title={call.actor.role ?? undefined}>
          {roleLabel(roleIndex, call.actor.role ?? '')}
        </DetailField>
        <DetailField label="主体来源" mono>
          {dashText(call.actor.source)}
        </DetailField>
        <DetailField label="令牌 ID" mono>
          {dashText(call.actor.token_id)}
        </DetailField>
        <DetailField label="结果">
          <OutcomeBadge outcome={call.outcome} />
        </DetailField>
        <DetailField label="错误码" mono>
          {errorCodeText(call.error_code)}
        </DetailField>
        <DetailField label="错误原因" wide>
          {call.error_message ?? DASH}
        </DetailField>
        <DetailField label="执行耗时">{elapsedText(call.elapsed_ms)}</DetailField>
        <DetailField label="模板缓存">
          {call.cache === null ? DASH : (TOOL_CACHE_LABEL[call.cache] ?? call.cache)}
        </DetailField>
        <DetailField label="响应 tokens">{call.tokens === null ? DASH : call.tokens}</DetailField>
        <DetailField label="执行版本">{call.version === null ? DASH : `v${call.version}`}</DetailField>
      </DetailGrid>

      {hasScript && call.executed_script !== null && (
        <section>
          <h3 className="detail-heading">执行 SQL</h3>
          <CodeBlock tone="sql">{call.executed_script}</CodeBlock>
        </section>
      )}

      <section>
        <h3 className="detail-heading">绑定值（按 ? 顺序）</h3>
        {bound.length === 0 ? (
          <p className="muted">该次执行没有绑定值。</p>
        ) : (
          <ol className="bound-params">
            {bound.map((value, index) => (
              <li key={index} className="mono">
                <span className="bound-index">#{index + 1}</span>
                {typeof value === 'string' ? value : inlineJson(value)}
              </li>
            ))}
          </ol>
        )}
      </section>

      <section>
        <h3 className="detail-heading">调用参数</h3>
        {hasArguments ? <JsonBlock value={call.arguments} /> : <p className="muted">没有记录入参。</p>}
      </section>
    </div>
  )
}
