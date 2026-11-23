import type { ReactNode } from 'react'
import { noticeMessage } from '../notice'
import { Modal } from './Modal'

/**
 * 不可撤回操作的统一二次确认：标题 + 后果说明 + 取消/确认，外壳就是 Modal（md 宽）。
 * pending 时只禁用确认按钮并把文案换成 pendingLabel；取消与 Esc 始终可用（是否允许中途退出由调用方决定）。
 * 失败原因由调用方通过 error 传进来，在原地展示（noticeMessage：description + 可选 HTTP 状态）——
 * 弹层在 top layer 会盖住页面顶部 InfoBar，错误写在那里等于没写。
 * 组件不卸载自己：onClose 必须是父组件的关弹层动作，confirm 成功后同样由父组件收尾。
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
