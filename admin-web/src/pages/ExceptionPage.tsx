import { useQuery } from '@tanstack/react-query'
import { fetchCapabilities } from '../api/admin'
import {
  DisabledAction,
  EmptyState,
  ErrorState,
  LoadingState,
  PageShell,
} from '../components/PageShell'

export default function ExceptionPage() {
  const { data, error, isPending, isError } = useQuery({
    queryKey: ['capabilities'],
    queryFn: ({ signal }) => fetchCapabilities(signal),
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

      {isPending && <LoadingState />}
      {isError && <ErrorState error={error} />}
      {data && data.length === 0 && <EmptyState hint="没有可展示的能力记录。" />}
      {data && data.length > 0 && (
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
              <th>操作</th>
            </tr>
          </thead>
          <tbody>
            {data.map((item) => (
              <tr key={item.capability_id}>
                <td>{item.capability_id}</td>
                <td>{item.kind}</td>
                <td>{item.group}</td>
                <td>{item.risk}</td>
                <td>{item.status}</td>
                <td>{item.discoverable ? '是' : '否'}</td>
                <td>{item.callable ? '是' : '否'}</td>
                <td>{item.version}</td>
                <td>
                  <DisabledAction label="调整可见性" />
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </PageShell>
  )
}
