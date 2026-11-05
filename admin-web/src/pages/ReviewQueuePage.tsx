import { useQuery } from '@tanstack/react-query'
import { fetchReviewQueue } from '../api/admin'
import {
  DisabledAction,
  EmptyState,
  ErrorState,
  LoadingState,
  PageShell,
} from '../components/PageShell'

export default function ReviewQueuePage() {
  const { data, error, isPending, isError } = useQuery({
    queryKey: ['review-queue'],
    queryFn: ({ signal }) => fetchReviewQueue(signal),
  })

  return (
    <PageShell
      title="审核队列"
      mode="未实现"
      description="draft / review 状态的能力版本等待审批。提交与批准必须为不同主体（四眼原则），本视图暂不提供任何写操作入口。"
    >
      <div className="toolbar">
        <DisabledAction label="批准发布" />
        <DisabledAction label="退回" />
        <span className="hint">写操作尚未接通：UI 不得直连库，所有变更须经 admin API 落审计。</span>
      </div>

      {isPending && <LoadingState />}
      {isError && <ErrorState error={error} />}
      {data && data.length === 0 && <EmptyState hint="当前没有待审核条目。" />}
      {data && data.length > 0 && (
        <table className="table">
          <thead>
            <tr>
              <th>能力 ID</th>
              <th>类型</th>
              <th>组</th>
              <th>风险</th>
              <th>状态</th>
              <th>版本</th>
              <th>定义哈希</th>
              <th>提交人</th>
              <th>操作</th>
            </tr>
          </thead>
          <tbody>
            {data.map((item) => (
              <tr key={`${item.capability_id}@${item.version}`}>
                <td>{item.capability_id}</td>
                <td>{item.kind}</td>
                <td>{item.group}</td>
                <td>{item.risk}</td>
                <td>{item.status}</td>
                <td>{item.version}</td>
                <td className="mono">{item.definition_hash}</td>
                <td>{item.submitted_by || '—'}</td>
                <td>
                  <DisabledAction label="批准" />
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </PageShell>
  )
}
