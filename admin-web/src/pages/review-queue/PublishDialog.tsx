import { useState } from 'react'
import { useMutation } from '@tanstack/react-query'
import { publishCapability, setCapabilityVisibility } from '../../api/admin'
import { ApiError } from '../../api/client'
import { roleLabel, useRoleIndex } from '../../api/queries'
import type { AdminIdentity, ReviewQueueItem } from '../../api/types'
import { LifecycleBadge } from '../../components/Badge'
import { Field } from '../../components/Field'
import { DetailField, DetailGrid } from '../../components/DetailGrid'
import { Modal } from '../../components/Modal'
import { ErrorState } from '../../components/PageShell'
import { ToggleSwitch } from '../../components/ToggleSwitch'
import { DASH, formatDateTime, formatWaitSince } from '../../format'
import { visibilityText } from '../../labels'
import { noticeFromError, type Notice } from '../../notice'

export function PublishDialog({
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
  const roleIndex = useRoleIndex()

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
          if (mutation.isPending) return
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
            {me === undefined ? DASH : `${roleLabel(roleIndex, me.role)} · ${me.type}`}
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

        <Field label="外部审批单号" title="approval_id" required>
          <input
            className="input"
            value={approvalId}
            onChange={(event) => setApprovalId(event.target.value)}
            placeholder="例如 ap-biz-water-1"
          />
        </Field>
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
