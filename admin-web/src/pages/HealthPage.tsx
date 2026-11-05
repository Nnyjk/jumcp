import { useQuery } from '@tanstack/react-query'
import { fetchHealth } from '../api/admin'
import type { AuditChainState } from '../api/types'
import { ErrorState, LoadingState, PageShell } from '../components/PageShell'

const CHAIN_LABEL: Record<AuditChainState, string> = {
  valid: '审计链完整',
  tampered: '审计链被篡改',
  error: '审计链校验失败',
}

export default function HealthPage() {
  const { data, error, isPending, isError, isSuccess } = useQuery({
    queryKey: ['health'],
    queryFn: ({ signal }) => fetchHealth(signal),
  })

  const chain = data?.audit_chain
  const chainClass =
    chain === 'valid' ? 'badge badge-ok' : chain === 'tampered' ? 'badge badge-warn' : 'badge badge-error'

  return (
    <PageShell
      title="健康检查"
      mode="只读"
      description="GET /health 的结构化响应，用于确认管理台与后端连通性及审计链完整性。本页不提供任何执行入口。"
    >
      {isPending && <LoadingState />}
      {isError && <ErrorState error={error} />}
      {isSuccess && data && (
        <>
          <div className="toolbar">
            <span className={chainClass}>
              {CHAIN_LABEL[chain ?? 'error']}
              {chain ? `（${chain}）` : ''}
            </span>
            {data.audit_chain_detail && (
              <span className="hint">详情：{data.audit_chain_detail}</span>
            )}
          </div>

          <table className="table">
            <thead>
              <tr>
                <th>项</th>
                <th>值</th>
              </tr>
            </thead>
            <tbody>
              <tr>
                <td>status</td>
                <td>{data.status}</td>
              </tr>
              <tr>
                <td>version</td>
                <td>{data.version}</td>
              </tr>
              <tr>
                <td>audit_chain</td>
                <td>{data.audit_chain}</td>
              </tr>
              {data.audit_chain_detail && (
                <tr>
                  <td>audit_chain_detail</td>
                  <td className="mono">{data.audit_chain_detail}</td>
                </tr>
              )}
            </tbody>
          </table>

          <h3 className="section-title">数据源（{data.datasources.length}）</h3>
          {data.datasources.length === 0 ? (
            <p className="hint">未配置数据源。</p>
          ) : (
            <table className="table">
              <thead>
                <tr>
                  <th>名称</th>
                  <th>引擎</th>
                  <th>只读</th>
                </tr>
              </thead>
              <tbody>
                {data.datasources.map((ds) => (
                  <tr key={ds.name}>
                    <td>{ds.name}</td>
                    <td>{ds.engine}</td>
                    <td>{ds.readonly ? '是' : '否'}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </>
      )}
    </PageShell>
  )
}
