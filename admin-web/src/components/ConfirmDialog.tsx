import type { ReactNode } from 'react'
import { noticeMessage } from '../notice'
import { Modal } from './Modal'

/**
 * 不可撤回操作的统一二次确认：标题 + 后果说明 + 取消/确认。
 * 失败原因由调用方通过 mutation.error 传进来，在原地展示——弹层会盖住页面顶部 InfoBar。
 */
export function ConfirmDialog({
  title,
  subtitle,
  confirmLabel,
  pendingLabel,
  tone = 'primary',
  pending,
  error,
  errorTitle,
  onClose,
  onConfirm,
  children,
}: {
  title: string
  subtitle?: string | undefined
  confirmLabel: string
  pendingLabel: string
  /** danger 用于删除/卸载这类破坏性操作。 */
  tone?: 'primary' | 'danger'
  pending: boolean
  error?: unknown
  errorTitle: string
  onClose: () => void
  onConfirm: () => void
  children: ReactNode
}) {
  return (
    <Modal
      size="md"
      title={title}
      subtitle={subtitle}
      onClose={onClose}
      footer={
        <>
          <button type="button" className="btn btn-secondary" onClick={onClose}>
            取消
          </button>
          <button
            type="button"
            className={tone === 'danger' ? 'btn btn-danger' : 'btn btn-primary'}
            disabled={pending}
            onClick={onConfirm}
          >
            {pending ? pendingLabel : confirmLabel}
          </button>
        </>
      }
    >
      {children}
      {error !== undefined && (
        <p className="inline-error" role="alert">
          {noticeMessage(error, errorTitle)}
        </p>
      )}
    </Modal>
  )
}
