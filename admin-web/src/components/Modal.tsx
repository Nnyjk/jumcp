import { useCallback, useEffect, useId, useRef } from 'react'
import type { MouseEvent, ReactNode } from 'react'
import { gsap } from 'gsap'
import {
  MOTION_DURATION,
  MOTION_EASE,
  prefersReducedMotion,
  useGsapScope,
} from '../motion/useGsap'

export type ModalSize = 'md' | 'lg' | 'xl'

/**
 * 原生 <dialog> 弹窗：挂载即 showModal，退场动画结束后 close 并回调 onClose。
 * 挂载期间锁住 body 滚动，卸载时还原原值（StrictMode 的 mount → cleanup → mount 不会留下打开的 dialog）。
 * 焦点由原生 top layer 管理（自带焦点圈定与还原），不引入第三方 focus-trap：
 * 优先聚焦 modal-body 里第一个表单控件，没有表单控件时落到浏览器的默认首焦点（右上角关闭按钮）。
 * 遮罩点击、Esc（cancel）与右上角关闭按钮统一走 requestClose；重复请求被吸收，
 * 因此父组件必须等 onClose 再把自身状态清空，过早卸载会截断退场动画。
 * size 只切换弹窗宽度（md 560px / lg 760px / xl 1180px，均以 94vw 收窄），面板高度封顶一屏减 96px。
 */
export function Modal({
  title,
  subtitle,
  size = 'md',
  onClose,
  children,
  footer,
}: {
  title: string
  subtitle?: string | undefined
  size?: ModalSize
  onClose: () => void
  children: ReactNode
  footer?: ReactNode
}) {
  const dialogRef = useRef<HTMLDialogElement | null>(null)
  const closingRef = useRef(false)
  const titleId = useId()
  const onCloseRef = useRef(onClose)
  const panelRef = useGsapScope<HTMLDivElement>((scope) => {
    gsap.from(scope, {
      opacity: 0,
      scale: 0.98,
      y: 8,
      duration: MOTION_DURATION.fast,
      ease: MOTION_EASE,
      clearProps: 'opacity,transform',
    })
  })

  // 用 ref 转交最新回调，避免 effect 因父组件内联函数重建而反复 close/showModal。
  useEffect(() => {
    onCloseRef.current = onClose
  }, [onClose])

  /** 先播退场（MOTION_DURATION.fast，reduced-motion 下瞬时），动画结束再 close + 通知父组件卸载。 */
  const requestClose = useCallback(() => {
    if (closingRef.current) return
    closingRef.current = true

    const dialog = dialogRef.current
    if (dialog === null) return
    const panel = panelRef.current

    if (panel === null || prefersReducedMotion()) {
      if (dialog.open) dialog.close()
      onCloseRef.current()
      return
    }

    gsap.killTweensOf(panel)
    gsap.to(panel, {
      opacity: 0,
      scale: 0.985,
      y: 6,
      duration: MOTION_DURATION.fast,
      ease: MOTION_EASE,
      onComplete: () => {
        if (dialog.open) dialog.close()
        onCloseRef.current()
      },
    })
  }, [panelRef])

  useEffect(() => {
    const dialog = dialogRef.current
    if (dialog === null) return

    const body = document.body
    const panel = panelRef.current
    const previousOverflow = body.style.overflow
    body.style.overflow = 'hidden'
    if (!dialog.open) dialog.showModal()
    // 浏览器会把焦点给关闭按钮；表单弹层应直接落在第一个字段上，键盘用户少按几次 Tab。
    dialog
      .querySelector<HTMLElement>(
        '.modal-body input:not([type="hidden"]), .modal-body select, .modal-body textarea',
      )
      ?.focus()

    const handleCancel = (event: Event) => {
      // 阻止浏览器默认的“只关闭 DOM、不通知 React”的行为，统一由父组件卸载。
      event.preventDefault()
      requestClose()
    }
    dialog.addEventListener('cancel', handleCancel)

    // StrictMode 会 mount → cleanup → mount：cleanup 关闭 dialog 并还原 overflow，
    // 第二次 effect 再 showModal，不会留下两个打开的 dialog 或锁死的滚动。
    return () => {
      dialog.removeEventListener('cancel', handleCancel)
      if (panel !== null) gsap.killTweensOf(panel)
      if (dialog.open) dialog.close()
      body.style.overflow = previousOverflow
    }
  }, [requestClose, panelRef])

  const handleBackdropClick = (event: MouseEvent<HTMLDialogElement>) => {
    // 遮罩区域的点击 target 是 dialog 本身；面板内的点击 target 是面板后代。
    if (event.target === dialogRef.current) requestClose()
  }

  return (
    <dialog
      ref={dialogRef}
      className={`modal modal-${size}`}
      aria-labelledby={titleId}
      onClick={handleBackdropClick}
    >
      <div className="modal-panel" ref={panelRef}>
        <header className="modal-head">
          <div className="modal-head-text">
            <h2 id={titleId}>{title}</h2>
            {subtitle !== undefined && <p className="modal-subtitle">{subtitle}</p>}
          </div>
          <button type="button" className="modal-close" aria-label="关闭" onClick={requestClose}>
            ✕
          </button>
        </header>
        <div className="modal-body">{children}</div>
        {footer !== undefined && <footer className="modal-foot">{footer}</footer>}
      </div>
    </dialog>
  )
}
