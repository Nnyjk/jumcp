import type { ReactNode } from 'react'
import { noticeMessage } from '../notice'
import { Modal } from './Modal'
import { DetailField, DetailGrid } from './DetailGrid'

/**
 * 治理状态变更（草稿 → 待审）的二次确认。提交不可自行撤回，只能由审批人退回，
 * 因此审核队列与能力档案两条入口共用同一份确认文案，避免两处行为分叉。
 */
export function SubmitConfirmDialog({
  id,
  version,
  status,
  pending,
  error,
  onClose,
  onConfirm,
}: {
  id: string
  version: number
  status: ReactNode
  pending: boolean
  /** 提交失败原因；弹层会盖住页面顶部 InfoBar，所以必须在这里展示。 */
  error?: unknown
  onClose: () => void
  onConfirm: () => void
}) {
  return (
    <Modal
      size="md"
      title={`提交审核 · ${id}`}
      subtitle={`版本 v${version} · 草稿 → 待审`}
      onClose={onClose}
      footer={
        <>
          <button type="button" className="btn btn-secondary" onClick={onClose}>
            取消
          </button>
          <button
            type="button"
            className="btn btn-primary"
            disabled={pending}
            onClick={() => {
              if (pending) return
              onConfirm()
            }}
          >
            {pending ? '提交中…' : '确认提交审核'}
          </button>
        </>
      }
    >
      <p className="hint">
        提交后该版本进入待审，等待审批人批准或退回；提交人不能自行撤回，只能由审批人退回草稿。
      </p>
      <DetailGrid>
        <DetailField label="能力 ID" mono>
          {id}
        </DetailField>
        <DetailField label="版本" mono>
          v{version}
        </DetailField>
        <DetailField label="当前状态" wide>
          {status}
        </DetailField>
      </DetailGrid>
      {error !== undefined && (
        <p className="inline-error" role="alert">
          {noticeMessage(error, '提交审核失败')}
        </p>
      )}
    </Modal>
  )
}
