import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { fetchAuditEvents } from '../../api/admin'
import { useGroupIndex } from '../../api/queries'
import type { AuditEnvelope } from '../../api/types'
import { auditSummary, isStructuredEvent } from '../../audit'
import { formatDateTime } from '../../format'
import { EVENT_TYPE_LABEL } from '../../labels'
import { AuditDetail } from '../../components/AuditDetail'
import { Card } from '../../components/Card'
import { DataTable, type DataTableColumn } from '../../components/DataTable'
import { Modal } from '../../components/Modal'
import { EmptyState, ErrorState, LoadingState, Pagination } from '../../components/PageShell'

export function AuditTab({ id }: { id: string }) {
  const [offset, setOffset] = useState(0)
  const [selected, setSelected] = useState<AuditEnvelope | null>(null)
  const groupIndex = useGroupIndex()

  const query = useQuery({
    queryKey: ['audit-object', id, offset],
    queryFn: ({ signal }) =>
      fetchAuditEvents({ object_id: id, order: 'desc' }, offset, signal),
  })

  /* 与 /audit 一致：768 下摘要必须紧随时间列出现。 */
  const columns: Array<DataTableColumn<AuditEnvelope>> = [
    {
      key: 'ts',
      header: '时间',
      mono: true,
      width: '168px',
      cell: (row) => formatDateTime(row.ts),
      title: (row) => row.ts,
    },
    {
      key: 'summary',
      header: '摘要',
      width: '300px',
      wrap: true,
      cell: (row) =>
        isStructuredEvent(row)
          ? auditSummary(row, { group: (code) => groupIndex.get(code)?.name })
          : '旧格式事件，仅原始载荷',
    },
    {
      key: 'type',
      header: '事件类型',
      width: '120px',
      cell: (row) => EVENT_TYPE_LABEL[row.event_type] ?? row.event_type,
    },
    {
      key: 'actor',
      header: '主体',
      width: '140px',
      mono: true,
      cell: (row) => (row.actor.id.trim() === '' ? '未认证调用方' : row.actor.id),
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
          onClick={() => setSelected(row)}
          aria-label={`查看事件 ${row.event_id} 详情`}
        >
          详情
        </button>
      ),
    },
  ]

  return (
    <Card
      title="对象审计"
      subtitle={query.data === undefined ? '查询中' : '按时间倒序'}
      flush
      footer={
        query.data !== undefined && query.data.events.length > 0 ? (
          <Pagination
            total={query.data.total}
            offset={query.data.offset}
            limit={query.data.limit}
            onChange={setOffset}
          />
        ) : undefined
      }
    >
      {query.isPending && <LoadingState rows={6} label="正在查询对象审计" />}
      {query.isError && <ErrorState error={query.error} />}
      {query.data !== undefined && query.data.events.length === 0 && (
        <EmptyState hint="该对象还没有审计事件；提交、发布、可见性调整或调用后会出现。" />
      )}
      {query.data !== undefined && query.data.events.length > 0 && (
        <DataTable
          label={`${id} 的审计事件`}
          columns={columns}
          rows={query.data.events}
          rowKey={(row) => row.event_id}
          minWidth="804px"
        />
      )}

      {selected !== null && (
        <Modal
          key={selected.event_id}
          size="xl"
          title={`审计详情 · ${selected.event_id}`}
          onClose={() => setSelected(null)}
        >
          <AuditDetail event={selected} />
        </Modal>
      )}
    </Card>
  )
}
