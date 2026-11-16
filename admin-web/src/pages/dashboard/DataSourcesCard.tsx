import type { HealthDatasource } from '../../api/types'
import { Badge } from '../../components/Badge'
import { Card } from '../../components/Card'
import { EmptyState, ErrorState, LoadingState } from '../../components/PageShell'

/** 数据源卡片：名称/引擎/只读标记来自健康检查，标题带数量。 */
export function DataSourcesCard({
  datasources,
  hasHealth,
  healthPending,
  healthError,
}: {
  datasources: HealthDatasource[]
  hasHealth: boolean
  healthPending: boolean
  healthError: unknown
}) {
  return (
    <div className="dash-span-6">
      <Card title={hasHealth ? `数据源（${datasources.length}）` : '数据源'}>
        {healthPending && <LoadingState rows={3} label="正在加载数据源" />}
        {healthError !== undefined && <ErrorState error={healthError} />}
        {hasHealth && datasources.length === 0 && (
          <EmptyState hint="未配置数据源；部署配置登记后这里会显示名称、引擎与只读标记。" />
        )}
        {datasources.length > 0 && (
          <ul className="ds-list">
            {datasources.map((datasource) => (
              <li key={datasource.name} className="ds-item">
                <span className="ds-name" title={datasource.name}>
                  {datasource.name}
                </span>
                <span className="ds-engine">{datasource.engine}</span>
                <Badge tone={datasource.readonly ? 'success' : 'warning'}>
                  {datasource.readonly ? '只读' : '可写'}
                </Badge>
              </li>
            ))}
          </ul>
        )}
      </Card>
    </div>
  )
}
