import { useQuery } from '@tanstack/react-query'
import { fetchHealth } from '../api/admin'
import type { AuditChainState, HealthDatasource } from '../api/types'
import { Badge } from '../components/Badge'
import { Card } from '../components/Card'
import { DataTable, type DataTableColumn } from '../components/DataTable'
import { DetailField, DetailGrid } from '../components/DetailGrid'
import { InfoBar } from '../components/InfoBar'
import { EmptyState, ErrorState, LoadingState, PageShell } from '../components/PageShell'
import { DASH, formatDateTime } from '../format'
import { inlineJson } from '../json'
import { CHAIN_TONE, HEALTH_STATUS_LABEL } from '../labels'

const CHAIN_LABEL: Record<AuditChainState, string> = {
  valid: '审计链完整',
  tampered: '审计链被篡改',
  error: '审计链校验失败',
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
  const { data, error, isPending, isError, isFetching, refetch, dataUpdatedAt } = useQuery({
    queryKey: ['health'],
    queryFn: ({ signal }) => fetchHealth(signal),
  })
  const updatedLabel =
    dataUpdatedAt > 0 ? formatDateTime(new Date(dataUpdatedAt).toISOString()) : DASH

  const chain = data?.audit_chain
  const chainDetail = data?.audit_chain_detail
  const chainDetailText =
    chainDetail === undefined
      ? undefined
      : typeof chainDetail === 'string'
        ? chainDetail
        : inlineJson(chainDetail)

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
          meta={chainDetailText === undefined ? undefined : `审计链明细：${chainDetailText}`}
        />
      )}

      <Card
        title="运行状态"
        actions={
          <>
            <span className="dash-updated">数据更新于 {updatedLabel}</span>
            <button
              type="button"
              className="btn btn-secondary btn-sm"
              onClick={() => void refetch()}
              disabled={isFetching}
            >
              {isFetching ? '刷新中…' : '刷新'}
            </button>
          </>
        }
      >
        {isPending && <LoadingState rows={4} label="正在读取健康状态" />}
        {isError && <ErrorState error={error} />}
        {data !== undefined && chain !== undefined && (
          <>
            <div className="stat-grid">
              <div className="stat">
                <span className="stat-label">状态</span>
                <div className="stat-value" title={data.status}>
                  {HEALTH_STATUS_LABEL[data.status] ?? data.status}
                </div>
              </div>
              <div className="stat">
                <span className="stat-label">版本</span>
                <div className="stat-value">
                  <span className="mono">{data.version}</span>
                </div>
              </div>
              <div className="stat">
                <span className="stat-label">审计链</span>
                <div className="stat-value">
                  <Badge tone={CHAIN_TONE[chain]} dot>
                    {CHAIN_LABEL[chain]}
                  </Badge>
                </div>
              </div>
            </div>
            {chainDetailText !== undefined && (
              <div className="detail-block">
                <DetailGrid>
                  <DetailField label="审计链明细" mono title={chainDetailText}>
                    {chainDetailText}
                  </DetailField>
                </DetailGrid>
              </div>
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
