import { useQuery } from '@tanstack/react-query'
import { fetchHealth } from '../api/admin'
import type { AuditChainState, HealthDatasource } from '../api/types'
import { Badge, type BadgeTone } from '../components/Badge'
import { Card } from '../components/Card'
import { DataTable, type DataTableColumn } from '../components/DataTable'
import { InfoBar } from '../components/InfoBar'
import { EmptyState, ErrorState, LoadingState, PageShell } from '../components/PageShell'
import { HEALTH_STATUS_LABEL } from '../labels'

const CHAIN_LABEL: Record<AuditChainState, string> = {
  valid: '审计链完整',
  tampered: '审计链被篡改',
  error: '审计链校验失败',
}

const CHAIN_TONE: Record<AuditChainState, BadgeTone> = {
  valid: 'success',
  tampered: 'danger',
  error: 'danger',
}

const DATASOURCE_COLUMNS: Array<DataTableColumn<HealthDatasource>> = [
  { key: 'name', header: '名称', mono: true, width: '320px', cell: (row) => row.name },
  { key: 'engine', header: '引擎', width: '180px', cell: (row) => row.engine },
  {
    key: 'readonly',
    header: '只读',
    width: '120px',
    cell: (row) => (
      <Badge tone={row.readonly ? 'success' : 'warning'}>{row.readonly ? '只读' : '可写'}</Badge>
    ),
  },
]

export default function HealthPage() {
  const { data, error, isPending, isError } = useQuery({
    queryKey: ['health'],
    queryFn: ({ signal }) => fetchHealth(signal),
  })

  const chain = data?.audit_chain
  const chainDetail = data?.audit_chain_detail

  return (
    <PageShell>
      {chain !== undefined && chain !== 'valid' && (
        <InfoBar
          severity="danger"
          title={chain === 'tampered' ? '审计链完整性校验未通过' : '审计链校验失败'}
          description={
            chain === 'tampered'
              ? '某条审计记录的 prev_hash / hash 与内容不一致：追加日志可能被绕过应用层改写，须立即排查写入路径与数据库权限。'
              : '链校验过程报错，当前无法确认审计日志完整性；该结论不等同于「完整」。'
          }
          meta={chainDetail === undefined ? undefined : `audit_chain_detail：${chainDetail}`}
        />
      )}

      <Card title="运行状态" subtitle="GET /health · 免鉴权端点">
        {isPending && <LoadingState rows={4} label="正在读取健康状态" />}
        {isError && <ErrorState error={error} />}
        {data !== undefined && chain !== undefined && (
          <>
            <div className="stat-grid">
              <div className="stat">
                <span className="stat-label">status</span>
                <div className="stat-value" title={data.status}>
                  {HEALTH_STATUS_LABEL[data.status] ?? data.status}
                </div>
              </div>
              <div className="stat">
                <span className="stat-label">version</span>
                <div className="stat-value">
                  <span className="mono">{data.version}</span>
                </div>
              </div>
              <div className="stat">
                <span className="stat-label">audit_chain</span>
                <div className="stat-value">
                  <Badge tone={CHAIN_TONE[chain]} dot>
                    {CHAIN_LABEL[chain]}
                  </Badge>
                </div>
              </div>
            </div>
            {data.audit_chain_detail !== undefined && (
              <dl className="detail-list detail-block">
                <dt>audit_chain_detail</dt>
                <dd>
                  <code>{data.audit_chain_detail}</code>
                </dd>
              </dl>
            )}
          </>
        )}
      </Card>

      {data !== undefined && (
        <Card title={`数据源（${data.datasources.length}）`} flush>
          {data.datasources.length === 0 ? (
            <EmptyState hint="未配置数据源。在部署配置中登记数据源后，健康页会显示引擎与只读标记。" />
          ) : (
            <DataTable
              label="数据源"
              columns={DATASOURCE_COLUMNS}
              rows={data.datasources}
              rowKey={(row) => row.name}
            />
          )}
        </Card>
      )}
    </PageShell>
  )
}
