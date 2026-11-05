import { useQuery } from '@tanstack/react-query'
import { fetchHealth } from '../api/admin'
import { ErrorState, LoadingState, PageShell } from '../components/PageShell'

export default function HealthPage() {
  const { data, error, isPending, isError, isSuccess } = useQuery({
    queryKey: ['health'],
    queryFn: ({ signal }) => fetchHealth(signal),
  })

  return (
    <PageShell
      title="健康检查"
      mode="只读"
      description="GET /health 的原始响应，用于确认管理台与后端连通性。本页不提供任何执行入口。"
    >
      {isPending && <LoadingState />}
      {isError && <ErrorState error={error} />}
      {isSuccess && (
        <div className="state state-ok">
          <strong>后端可达</strong>
          <pre className="mono">{data?.trim() || '(空响应)'}</pre>
        </div>
      )}
    </PageShell>
  )
}
