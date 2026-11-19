import { useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { fetchAdminMe, fetchReviewQueue, rejectCapability, submitCapability } from '../../api/admin'
import { groupLabel, useGroupIndex } from '../../api/queries'
import type { ReviewQueueItem } from '../../api/types'
import { LifecycleBadge, RiskBadge } from '../../components/Badge'
import { Card } from '../../components/Card'
import { DataTable, type DataTableColumn } from '../../components/DataTable'
import { InfoBar } from '../../components/InfoBar'
import { EmptyState, ErrorState, LoadingState } from '../../components/PageShell'
import { SubmitConfirmDialog } from '../../components/SubmitConfirmDialog'
import { DASH, formatDateTime, formatWaitSince } from '../../format'
import { TOOL_KIND_LABEL } from '../../labels'
import { noticeFromError, type Notice } from '../../notice'
import { useAdminView } from '../../viewStack'
import { PublishDialog } from './PublishDialog'
import { RejectDialog } from './RejectDialog'

type QueueTab = 'review' | 'draft'

type Dialog =
  | { kind: 'submit'; row: ReviewQueueItem }
  | { kind: 'publish'; row: ReviewQueueItem }
  | { kind: 'reject'; row: ReviewQueueItem }
  | null

/** 审核队列视图：待审批与未提交草稿两个队列共用一次 GET /admin/review-queue 的快照。 */
export function ReviewQueuePanel() {
  const queryClient = useQueryClient()
  const { openDossier } = useAdminView()
  const [notice, setNotice] = useState<Notice | null>(null)
  const [tab, setTab] = useState<QueueTab>('review')
  const [dialog, setDialog] = useState<Dialog>(null)

  const queueQuery = useQuery({
    queryKey: ['review-queue'],
    queryFn: ({ signal }) => fetchReviewQueue(signal),
  })

  const meQuery = useQuery({
    queryKey: ['admin-me'],
    queryFn: ({ signal }) => fetchAdminMe(signal),
  })
  const groupIndex = useGroupIndex()

  const refreshAfterWrite = () => {
    for (const key of ['review-queue', 'tool-stats', 'capability-detail', 'audit-timeline']) {
      void queryClient.invalidateQueries({ queryKey: [key] })
    }
  }

  const submitMutation = useMutation({
    mutationFn: (row: ReviewQueueItem) => submitCapability(row.id),
    onSuccess: (_result, row) => {
      setDialog(null)
      setNotice({
        severity: 'success',
        title: '已提交审核',
        description: `${row.id}@v${row.version} 已进入待审状态，等待审批。`,
        meta: undefined,
      })
      refreshAfterWrite()
    },
    onError: (cause) => setNotice(noticeFromError(cause, '提交审核失败')),
  })

  const rejectMutation = useMutation({
    mutationFn: (input: { row: ReviewQueueItem; reason: string }) =>
      rejectCapability(input.row.id, input.reason),
    onSuccess: (result, input) => {
      setDialog(null)
      setNotice({
        severity: 'success',
        title: '已退回',
        description: `${input.row.id}@v${input.row.version} 已退回草稿。`,
        meta: `退回原因：${result.reason}`,
      })
      refreshAfterWrite()
    },
    onError: (cause) => setNotice(noticeFromError(cause, '退回失败')),
  })

  const items = queueQuery.data?.items ?? []
  const reviewItems = items.filter((item) => item.status === 'review')
  const draftItems = items.filter((item) => item.status === 'draft')
  const current = tab === 'review' ? reviewItems : draftItems

  const reviewColumns: Array<DataTableColumn<ReviewQueueItem>> = [
    { key: 'id', header: '能力 ID', mono: true, width: '190px', cell: (row) => row.id },
    {
      key: 'kind',
      header: '类型',
      width: '76px',
      cell: (row) => TOOL_KIND_LABEL[row.kind] ?? row.kind,
    },
    {
      key: 'group',
      header: '组',
      width: '140px',
      cell: (row) =>
        row.group === '' ? (
          DASH
        ) : (
          <span title={row.group}>{groupLabel(groupIndex, row.group)}</span>
        ),
    },
    { key: 'risk', header: '风险', width: '76px', cell: (row) => <RiskBadge risk={row.risk} /> },
    {
      key: 'version',
      header: '版本',
      numeric: true,
      width: '56px',
      cell: (row) => `v${row.version}`,
    },
    {
      key: 'submitted_by',
      header: '提审人',
      mono: true,
      width: '120px',
      cell: (row) => row.submitted_by || '未记录',
    },
    {
      key: 'submitted_at',
      header: '提审时间',
      mono: true,
      width: '176px',
      cell: (row) => formatDateTime(row.submitted_at),
      title: (row) => row.submitted_at ?? DASH,
    },
    {
      key: 'waiting',
      header: '等待时长',
      width: '124px',
      cell: (row) => formatWaitSince(row.submitted_at),
    },
    {
      key: 'actions',
      header: '操作',
      width: '250px',
      sticky: true,
      cell: (row) => (
        <div className="row-actions">
          <button
            type="button"
            className="btn btn-secondary btn-sm"
            onClick={() => openDossier({ id: row.id, tab: 'approval' })}
          >
            详情
          </button>
          <button
            type="button"
            className="btn btn-primary btn-sm"
            disabled={meQuery.isPending}
            title={meQuery.isPending ? '正在读取当前主体' : undefined}
            onClick={() => setDialog({ kind: 'publish', row })}
          >
            批准发布
          </button>
          <button
            type="button"
            className="btn btn-danger btn-sm"
            onClick={() => setDialog({ kind: 'reject', row })}
          >
            退回
          </button>
        </div>
      ),
    },
  ]

  const draftColumns: Array<DataTableColumn<ReviewQueueItem>> = [
    { key: 'id', header: '能力 ID', mono: true, width: '190px', cell: (row) => row.id },
    { key: 'kind', header: '类型', width: '76px', cell: (row) => TOOL_KIND_LABEL[row.kind] ?? row.kind },
    {
      key: 'group',
      header: '组',
      width: '140px',
      cell: (row) =>
        row.group === '' ? (
          DASH
        ) : (
          <span title={row.group}>{groupLabel(groupIndex, row.group)}</span>
        ),
    },
    { key: 'risk', header: '风险', width: '76px', cell: (row) => <RiskBadge risk={row.risk} /> },
    {
      key: 'version',
      header: '版本',
      numeric: true,
      width: '56px',
      cell: (row) => `v${row.version}`,
    },
    {
      key: 'last_changed_at',
      header: '最近变更',
      mono: true,
      width: '176px',
      cell: (row) => formatDateTime(row.last_changed_at),
      title: (row) => row.last_changed_at ?? DASH,
    },
    {
      key: 'actions',
      header: '操作',
      width: '184px',
      sticky: true,
      cell: (row) => (
        <div className="row-actions">
          <button
            type="button"
            className="btn btn-secondary btn-sm"
            onClick={() => openDossier({ id: row.id, tab: 'approval' })}
          >
            详情
          </button>
          <button
            type="button"
            className="btn btn-primary btn-sm"
            onClick={() => setDialog({ kind: 'submit', row })}
          >
            提交审核
          </button>
        </div>
      ),
    },
  ]

  return (
    <>
      {notice !== null && (
        <InfoBar
          severity={notice.severity}
          title={notice.title}
          description={notice.description}
          meta={notice.meta}
        >
          <button type="button" className="btn btn-subtle btn-sm" onClick={() => setNotice(null)}>
            关闭
          </button>
        </InfoBar>
      )}

      <Card
        title={tab === 'review' ? '待审批' : '未提交草稿'}
        subtitle={
          queueQuery.data === undefined
            ? queueQuery.isError
              ? '查询失败'
              : '查询中'
            : `共 ${items.length} 个能力版本 · 队列不分页`
        }
        actions={
          <div className="segmented" role="group" aria-label="队列状态">
            <button type="button" aria-pressed={tab === 'review'} onClick={() => setTab('review')}>
              待审批 <span className="segmented-count">{reviewItems.length}</span>
            </button>
            <button type="button" aria-pressed={tab === 'draft'} onClick={() => setTab('draft')}>
              未提交草稿 <span className="segmented-count">{draftItems.length}</span>
            </button>
          </div>
        }
        flush
      >
        {queueQuery.isPending && <LoadingState rows={6} label="正在加载审核队列" />}
        {queueQuery.isError && <ErrorState error={queueQuery.error} />}
        {queueQuery.data !== undefined && current.length === 0 && (
          <EmptyState
            hint={
              tab === 'review'
                ? '当前没有待审批条目。草稿提交后才会出现在这里。'
                : '没有未提交的草稿。'
            }
          />
        )}
        {current.length > 0 && tab === 'review' && (
          <DataTable
            label="待审批能力版本"
            columns={reviewColumns}
            rows={current}
            rowKey={(row) => `${row.id}@${row.version}`}
            minWidth="1210px"
          />
        )}
        {current.length > 0 && tab === 'draft' && (
          <DataTable
            label="未提交草稿"
            columns={draftColumns}
            rows={current}
            rowKey={(row) => `${row.id}@${row.version}`}
            minWidth="1050px"
          />
        )}
      </Card>

      {dialog !== null && dialog.kind === 'submit' && (
        <SubmitConfirmDialog
          key={`${dialog.row.id}@${dialog.row.version}`}
          id={dialog.row.id}
          version={dialog.row.version}
          status={<LifecycleBadge status={dialog.row.status} />}
          pending={submitMutation.isPending}
          error={submitMutation.error ?? undefined}
          onClose={() => {
            setDialog(null)
            // 失败原因属于本次尝试；不清掉会让下次打开弹层看到旧错误。
            submitMutation.reset()
          }}
          onConfirm={() => submitMutation.mutate(dialog.row)}
        />
      )}

      {dialog !== null && dialog.kind === 'publish' && (
        <PublishDialog
          key={`${dialog.row.id}@${dialog.row.version}`}
          row={dialog.row}
          me={meQuery.data}
          mePending={meQuery.isPending}
          meError={meQuery.error}
          onClose={() => setDialog(null)}
          onDone={(result) => {
            setDialog(null)
            setNotice(result)
            refreshAfterWrite()
          }}
        />
      )}

      {dialog !== null && dialog.kind === 'reject' && (
        <RejectDialog
          key={`${dialog.row.id}@${dialog.row.version}`}
          row={dialog.row}
          pending={rejectMutation.isPending}
          error={rejectMutation.error ?? undefined}
          onClose={() => {
            setDialog(null)
            rejectMutation.reset()
          }}
          onSubmit={(reason) => rejectMutation.mutate({ row: dialog.row, reason })}
        />
      )}
    </>
  )
}
