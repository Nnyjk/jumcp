import { useState } from 'react'
import type { ReviewQueueItem } from '../../api/types'
import { Field } from '../../components/Field'
import { Modal } from '../../components/Modal'

export function RejectDialog({
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
        <Field label="退回原因">
          <textarea
            className="input textarea"
            rows={4}
            value={reason}
            onChange={(event) => setReason(event.target.value)}
            placeholder="说明需要修改的内容；退回后版本回到 draft，提审人基准重置"
          />
        </Field>
        {formError !== undefined && (
          <p className="inline-error" role="alert">
            {formError}
          </p>
        )}
      </form>
    </Modal>
  )
}
