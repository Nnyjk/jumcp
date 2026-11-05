import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { fetchCapabilities, PAGE_LIMIT } from '../api/admin'
import type { CapabilityQuery } from '../api/types'
import {
  DisabledAction,
  EmptyState,
  ErrorState,
  LoadingState,
  PageShell,
  Pagination,
} from '../components/PageShell'

const EMPTY_FILTER: CapabilityQuery = { status: '', group: '', q: '' }

export default function ExceptionPage() {
  const [draft, setDraft] = useState<CapabilityQuery>(EMPTY_FILTER)
  const [filter, setFilter] = useState<CapabilityQuery>(EMPTY_FILTER)
  const [offset, setOffset] = useState(0)

  const { data, error, isPending, isError } = useQuery({
    queryKey: ['capabilities', filter, offset],
    queryFn: ({ signal }) => fetchCapabilities(filter, offset, signal),
  })

  return (
    <PageShell
      title="例外处理"
      mode="只读"
      description="discoverable / callable 双布尔的只读展示。仅改双布尔与预算不触发重新审核，但必须记 admin_change 审计；本视图尚未接通写操作。"
    >
      <div className="toolbar">
        <DisabledAction label="切换 discoverable" />
        <DisabledAction label="切换 callable" />
        <span className="hint">
          例外处理不提供「以某租户/某 Agent 身份执行工具」的入口；排障走 MCP Inspector + 审计回放。
        </span>
      </div>

      <form
        className="filter-bar"
        onSubmit={(event) => {
          event.preventDefault()
          setOffset(0)
          setFilter(draft)
        }}
      >
        <label>
          状态
          <select
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
        <label>
          组
          <input
            value={draft.group ?? ''}
            onChange={(event) => setDraft({ ...draft, group: event.target.value })}
            placeholder="group"
          />
        </label>
        <label>
          关键字
          <input
            value={draft.q ?? ''}
            onChange={(event) => setDraft({ ...draft, q: event.target.value })}
            placeholder="q"
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
            setOffset(0)
          }}
        >
          重置
        </button>
      </form>

      {isPending && <LoadingState />}
      {isError && <ErrorState error={error} />}
      {data && data.items.length === 0 && <EmptyState hint="没有可展示的能力记录。" />}
      {data && data.items.length > 0 && (
        <table className="table">
          <thead>
            <tr>
              <th>能力 ID</th>
              <th>类型</th>
              <th>组</th>
              <th>风险</th>
              <th>状态</th>
              <th>discoverable</th>
              <th>callable</th>
              <th>版本</th>
              <th>后继</th>
              <th>更新时间</th>
              <th>操作</th>
            </tr>
          </thead>
          <tbody>
            {data.items.map((item) => (
              <tr key={item.id}>
                <td>{item.id}</td>
                <td>{item.kind}</td>
                <td>{item.group}</td>
                <td>{item.risk}</td>
                <td>{item.status}</td>
                <td>{item.discoverable ? '是' : '否'}</td>
                <td>{item.callable ? '是' : '否'}</td>
                <td>{item.version}</td>
                <td>{item.successor_id || '—'}</td>
                <td>{item.updated || '—'}</td>
                <td>
                  <DisabledAction label="调整可见性" />
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {data && (
        <Pagination total={data.total} offset={data.offset} limit={data.limit} onChange={setOffset} />
      )}
      <p className="hint">分页步长固定 {PAGE_LIMIT} 条，与后端默认 limit 一致。</p>
    </PageShell>
  )
}
