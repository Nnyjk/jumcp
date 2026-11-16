import type { ReactNode } from 'react'

/**
 * 表单字段壳：标题 + 控件。标题的 title 保留后端字段名，便于对表与 tooltip 提示。
 * required 只负责在标题后追加星号，真正的校验仍由调用方在提交时做。
 */
export function Field({
  label,
  title,
  required = false,
  hint,
  children,
}: {
  label: ReactNode
  title?: string
  required?: boolean
  hint?: ReactNode
  children: ReactNode
}) {
  return (
    <label className="field">
      <span title={title}>
        {label}
        {required && (
          <>
            {' '}
            <span className="required-mark">*</span>
          </>
        )}
      </span>
      {children}
      {hint !== undefined && <span className="hint">{hint}</span>}
    </label>
  )
}
