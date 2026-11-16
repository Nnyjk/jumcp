import { useState } from 'react'
import { copyToClipboard } from './tokenConfig'

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
  const [text, setText] = useState(label)
  const copy = async () => {
    const copied = await copyToClipboard(value)
    setText(copied ? copiedLabel : '复制失败')
  }
  return (
    <button type="button" className={className} onClick={() => void copy()}>
      {text}
    </button>
  )
}
