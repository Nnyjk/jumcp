import { useState } from 'react'
import { keepPreviousData, useQuery } from '@tanstack/react-query'
import { fetchAuditEvents } from '../../api/admin'
import { useGroupIndex } from '../../api/queries'
import type { AuditEnvelope } from '../../api/types'
import { actorLabel, auditSummary, isStructuredEvent } from '../../audit'
import { formatDateTime } from '../../format'
import { EVENT_TYPE_LABEL } from '../../labels'
import { AuditDetail } from '../../components/AuditDetail'
import { Card } from '../../components/Card'
import { DataTable, type DataTableColumn } from '../../components/DataTable'
import { Modal } from '../../components/Modal'
import { EmptyState, ErrorState, LoadingState, Pagination } from '../../components/PageShell'
import { gsap } from 'gsap'
import { MOTION_DURATION, MOTION_EASE, useGsapScope } from '../../motion/useGsap'

export function AuditTab({ id }: { id: string }) {
  const [offset, setOffset] = useState(0)
  const [selected, setSelected] = useState<AuditEnvelope | null>(null)
  const groupIndex = useGroupIndex()

  const query = useQuery({
    queryKey: ['audit-object', id, offset],
    queryFn: ({ signal }) =>
      fetchAuditEvents({ object_id: id, order: 'desc' }, offset, signal),
    // 动机约束：翻页查询期间保留当前页数据，防止重新加载时容器塌陷为骨架屏导致视口跳动与分页条漂移。
    placeholderData: keepPreviousData,
  })

  /* 审计面板随数据到达淡入一次；翻页时只换内容，不重播整块动效。 */
  const bodyRef = useGsapScope<HTMLDivElement>((scope) => {
    gsap.from(scope, {
      opacity: 0,
      y: 8,
      duration: MOTION_DURATION.enter,
      ease: MOTION_EASE,
      clearProps: 'opacity,transform',
    })
  }, [query.data === undefined ? 'pending' : `data:${query.data.events.length}`])

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
      cell: (row) => actorLabel(row.actor),
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
        query.data !== undefined && query.data.total > 0 ? (
          <Pagination
            total={query.data.total}
            // 约束：绑定本地请求态 offset 而非响应体 query.data.offset，避免在 keepPreviousData 保留旧数据期间产生高亮页码回跳。
            offset={offset}
            limit={query.data.limit}
            onChange={setOffset}
          />
        ) : undefined
      }
    >
      {/* 约束：.result-region 施加最小高度以在加载、空态与数据表格切换时保持高度稳定，防止视口高度跳动 */}
      <div className="result-region" ref={bodyRef}>
        {query.isPending && <LoadingState rows={6} label="正在加载对象审计" />}
        {query.isError && <ErrorState error={query.error} />}
        {query.data !== undefined && query.data.events.length === 0 && (
          <EmptyState
            hint={
              query.data.total > 0
                ? '暂无当前页记录（数据在翻页后可能发生变动），请返回上一页。'
                : '暂无审计事件记录。提交审核、批准发布、调整可见性或产生工具调用后将在此记录。'
            }
            action={
              query.data.total > 0 ? (
                <button
                  type="button"
                  className="btn btn-secondary"
                  onClick={() => setOffset(Math.max(0, offset - (query.data?.limit || 50)))}
                >
                  返回上一页
                </button>
              ) : undefined
            }
          />
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
      </div>

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
