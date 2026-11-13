import { useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  fetchAdminMe,
  fetchReviewQueue,
  publishCapability,
  rejectCapability,
  setCapabilityVisibility,
  submitCapability,
} from '../api/admin'
import type { AdminIdentity, ReviewQueueItem } from '../api/types'
import { ApiError } from '../api/client'
import { groupLabel, useGroupIndex } from '../api/queries'
import { LifecycleBadge, RiskBadge } from '../components/Badge'
import { Card } from '../components/Card'
import { DataTable, type DataTableColumn } from '../components/DataTable'
import { DetailField, DetailGrid } from '../components/DetailGrid'
import { InfoBar } from '../components/InfoBar'
import { Modal } from '../components/Modal'
import {
  EmptyState,
  ErrorState,
  LoadingState,
  PageShell,
} from '../components/PageShell'
import { ToggleSwitch } from '../components/ToggleSwitch'
import { SubmitConfirmDialog } from '../components/SubmitConfirmDialog'
import { formatDateTime, formatWaitSince } from '../format'
import { TOOL_KIND_LABEL, visibilityText } from '../labels'
import { noticeFromError, type Notice } from '../notice'
import { useAdminView } from '../viewStack'

const DASH = '—'

type QueueTab = 'review' | 'draft'

type Dialog =
  | { kind: 'submit'; row: ReviewQueueItem }
  | { kind: 'publish'; row: ReviewQueueItem }
  | { kind: 'reject'; row: ReviewQueueItem }
  | null

export default function ReviewQueuePage() {
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
    { key: 'kind', header: '类型', width: '76px', cell: (row) => row.kind },
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
    <PageShell>
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
          onClose={() => setDialog(null)}
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
          onClose={() => setDialog(null)}
          onSubmit={(reason) => rejectMutation.mutate({ row: dialog.row, reason })}
        />
      )}
    </PageShell>
  )
}

/* ------------------------------ 批准发布 --------------------------------- */

function PublishDialog({
  row,
  me,
  mePending,
  meError,
  onClose,
  onDone,
}: {
  row: ReviewQueueItem
  me: AdminIdentity | undefined
  mePending: boolean
  meError: unknown
  onClose: () => void
  onDone: (notice: Notice) => void
}) {
  const [approvalId, setApprovalId] = useState('')
  const [discoverable, setDiscoverable] = useState(true)
  const [callable, setCallable] = useState(true)
  const [formError, setFormError] = useState<string | undefined>(undefined)

  const mutation = useMutation({
    mutationFn: async () => {
      await publishCapability(row.id, approvalId.trim())
      try {
        await setCapabilityVisibility(row.id, { discoverable, callable })
        return { visibilityFailed: false as const }
      } catch (error) {
        return { visibilityFailed: true as const, error }
      }
    },
    onSuccess: (result) => {
      if (result.visibilityFailed) {
        onDone({
          severity: 'warning',
          title: '已发布但未开放',
          description:
            '发布成功，但第二步可见性保存失败：调用方仍看不到该能力。请到能力档案的「审批」分栏重试保存可见性。',
          meta:
            result.error instanceof ApiError
              ? `${result.error.code} · HTTP ${result.error.status}`
              : String(result.error),
        })
        return
      }
      onDone({
        severity: 'success',
        title: '已发布并开放',
        description: `${row.id}@v${row.version} 已发布；可见性已保存为 ${visibilityText(discoverable, callable)}。`,
        meta: `审批单 ${approvalId.trim()}（外部，系统不校验） · 批准人 ${me?.id ?? DASH}`,
      })
    },
    onError: (error) => onDone(noticeFromError(error, '批准发布失败')),
  })

  return (
    <Modal
      size="lg"
      title={`批准发布 · ${row.id}`}
      subtitle={`版本 v${row.version} · 待审 → 已发布`}
      onClose={onClose}
      footer={
        <div className="form-actions">
          <button type="button" className="btn btn-secondary" onClick={onClose}>
            取消
          </button>
          <button
            type="submit"
            form="publish-form"
            className="btn btn-primary"
            disabled={mutation.isPending || mePending || me === undefined}
          >
            {mutation.isPending ? '发布中…' : '批准发布并开放'}
          </button>
        </div>
      }
    >
      <form
        id="publish-form"
        className="create-form"
        onSubmit={(event) => {
          event.preventDefault()
          if (approvalId.trim() === '') {
            setFormError('外部审批单号不能为空。')
            return
          }
          setFormError(undefined)
          mutation.mutate()
        }}
      >
        <DetailGrid>
          <DetailField label="当前主体" mono>
            {mePending ? '读取中…' : (me?.id ?? DASH)}
          </DetailField>
          <DetailField label="当前主体角色">
            {me === undefined ? DASH : `${me.role} · ${me.type}`}
          </DetailField>
          <DetailField label="提审人" mono>
            {row.submitted_by ?? '未记录'}
          </DetailField>
          <DetailField label="提审时间" mono>
            {formatDateTime(row.submitted_at)}
          </DetailField>
          <DetailField label="等待时长">
            {formatWaitSince(row.submitted_at)}
          </DetailField>
          <DetailField label="当前状态">
            <LifecycleBadge status={row.status} />
          </DetailField>
        </DetailGrid>

        {meError !== null && meError !== undefined && <ErrorState error={meError} />}

        <label className="field">
          <span title="approval_id">
            外部审批单号{' '}
            <span className="required-mark">*</span>
          </span>
          <input
            className="input"
            value={approvalId}
            onChange={(event) => setApprovalId(event.target.value)}
            placeholder="例如 ap-biz-water-1"
          />
        </label>
        <p className="hint">
          审批人只读：将记录为当前主体（{me?.id ?? '读取中…'}），批准人由后端按 token 派生，不能手填。
        </p>

        <fieldset className="publish-visibility">
          <legend>发布后可见性（第二步）</legend>
          <ToggleSwitch
            checked={discoverable}
            label="可被发现"
            title="discoverable"
            onChange={setDiscoverable}
          />
          <ToggleSwitch
            checked={callable}
            label="可被调用"
            title="callable"
            onChange={setCallable}
          />
          <p className="hint">
            发布只把状态改为已发布；第二步保存可见性后调用方才能真正使用。第二步失败会明确提示「已发布但未开放」。
          </p>
        </fieldset>

        {formError !== undefined && (
          <p className="inline-error" role="alert">
            {formError}
          </p>
        )}
      </form>
    </Modal>
  )
}

/* -------------------------------- 退回 ---------------------------------- */

function RejectDialog({
  row,
  pending,
  onClose,
  onSubmit,
}: {
  row: ReviewQueueItem
  pending: boolean
  onClose: () => void
  onSubmit: (reason: string) => void
}) {
  const [reason, setReason] = useState('')
  const [formError, setFormError] = useState<string | undefined>(undefined)

  return (
    <Modal
      size="md"
      title={`退回 · ${row.id}`}
      subtitle={`版本 v${row.version} · 待审 → 草稿`}
      onClose={onClose}
      footer={
        <div className="form-actions">
          <button type="button" className="btn btn-secondary" onClick={onClose}>
            取消
          </button>
          <button
            type="submit"
            form="reject-form"
            className="btn btn-danger"
            disabled={pending}
          >
            {pending ? '退回中…' : '确认退回'}
          </button>
        </div>
      }
    >
      <form
        id="reject-form"
        className="create-form"
        onSubmit={(event) => {
          event.preventDefault()
          if (reason.trim() === '') {
            setFormError('退回原因不能为空。')
            return
          }
          setFormError(undefined)
          onSubmit(reason.trim())
        }}
      >
        <label className="field">
          <span>退回原因</span>
          <textarea
            className="input textarea"
            rows={4}
            value={reason}
            onChange={(event) => setReason(event.target.value)}
            placeholder="说明需要修改的内容；退回后版本回到 draft，提审人基准重置"
          />
        </label>
        {formError !== undefined && (
          <p className="inline-error" role="alert">
            {formError}
          </p>
        )}
      </form>
    </Modal>
  )
}
