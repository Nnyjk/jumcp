import { useEffect, useRef, useState } from 'react'
import { copyToClipboard } from './tokenConfig'

type CopyState = 'idle' | 'copied' | 'failed'

const RESET_MS = 2000

export function CopyButton({
  value,
  label = '复制',
  copiedLabel = '已复制',
  className = 'btn btn-secondary btn-sm',
}: {
  value: string
  label?: string
  copiedLabel?: string
  className?: string
}) {
  const [state, setState] = useState<CopyState>('idle')
  const resetTimer = useRef<number | null>(null)

  // 卸载或再次点击时清掉旧定时器，避免对已卸载组件 setState。
  useEffect(
    () => () => {
      if (resetTimer.current !== null) window.clearTimeout(resetTimer.current)
    },
    [],
  )

  const copy = async () => {
    const copied = await copyToClipboard(value)
    setState(copied ? 'copied' : 'failed')
    if (resetTimer.current !== null) window.clearTimeout(resetTimer.current)
    resetTimer.current = window.setTimeout(() => {
      setState('idle')
      resetTimer.current = null
    }, RESET_MS)
  }


  return (
    <>
      <button
        type="button"
        className={`${className} copy-button`}
        data-state={state}
        onClick={() => void copy()}
      >
        <span className="copy-button-label">
          <span className="copy-button-text copy-button-text-idle">{label}</span>
          <span className="copy-button-text copy-button-text-copied">{copiedLabel}</span>
          <span className="copy-button-text copy-button-text-failed">复制失败</span>
        </span>
      </button>
      {/* 按钮文案变化在部分读屏下不会重播，用独立状态区保证「复制成功/失败」被听到。 */}
      <span className="sr-only" role="status">
        {state === 'idle'
          ? ''
          : state === 'copied'
            ? '已复制到剪贴板'
            : '复制失败，请手动选择文本复制'}
      </span>
    </>
  )
}
