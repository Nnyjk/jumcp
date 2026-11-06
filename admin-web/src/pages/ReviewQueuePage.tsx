import { useQuery } from '@tanstack/react-query'
import { fetchReviewQueue } from '../api/admin'
import type { ReviewQueueItem } from '../api/types'
import { RiskBadge, StatusBadge } from '../components/Badge'
import { Card } from '../components/Card'
import { DataTable, type DataTableColumn } from '../components/DataTable'
import {
  DisabledAction,
  EmptyState,
  ErrorState,
  LoadingState,
  PageShell,
} from '../components/PageShell'

const COLUMNS: Array<DataTableColumn<ReviewQueueItem>> = [
  { key: 'id', header: '能力 ID', mono: true, width: '176px', cell: (row) => row.id },
  {
    key: 'kind',
    header: '类型',
    width: '88px',
    cell: (row) => row.kind,
  },
  { key: 'group', header: '组', mono: true, width: '88px', cell: (row) => row.group },
  {
    key: 'risk',
    header: '风险',
    width: '72px',
    cell: (row) => <RiskBadge risk={row.risk} />,
  },
  {
    key: 'status',
    header: '状态',
    width: '106px',
    cell: (row) => <StatusBadge status={row.status} />,
  },
  { key: 'version', header: '版本', numeric: true, width: '52px', cell: (row) => row.version },
  { key: 'updated', header: '更新时间', mono: true, width: '176px', cell: (row) => row.updated || '—' },
  { key: 'hash', header: '定义哈希', mono: true, width: '76px', cell: (row) => row.definition_hash },
  {
    key: 'actions',
    header: '操作',
    width: '136px',
    sticky: true,
    cell: () => <DisabledAction compact label="批准" />,
  },
]

export default function ReviewQueuePage() {
  const { data, error, isPending, isError } = useQuery({
    queryKey: ['review-queue'],
    queryFn: ({ signal }) => fetchReviewQueue(signal),
  })

  return (
    <PageShell>
      <Card flush>
        <div className="command-bar">
          <DisabledAction label="批准发布" />
          <DisabledAction label="退回" />
          <span className="hint">写操作尚未接通：UI 不得直连库，所有变更须经 admin API 落审计。</span>
        </div>
      </Card>

      <Card
        title="待审核版本"
        subtitle={
          data === undefined ? (isError ? '查询失败' : '查询中') : `共 ${data.total} 条`
        }
        flush
      >
        {isPending && <LoadingState rows={6} label="正在加载审核队列" />}
        {isError && <ErrorState error={error} />}
        {data !== undefined && data.items.length === 0 && (
          <EmptyState hint="当前没有 draft / review 状态的条目。新版本提交后会出现在这里。" />
        )}
        {data !== undefined && data.items.length > 0 && (
          <DataTable
            label="待审核能力版本"
            meta={`本页 ${data.items.length} 行 · 共 ${data.total} 条`}
            columns={COLUMNS}
            rows={data.items}
            rowKey={(row) => `${row.id}@${row.version}`}
          />
        )}
      </Card>
    </PageShell>
  )
}
