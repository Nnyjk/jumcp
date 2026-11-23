import type { ReactNode } from 'react'

/**
 * 表单字段壳：标题 + 控件 + 提示。标题的 title 保留后端字段名，便于对表与 tooltip 提示。
 * 整体是 <label>，所以每个 Field 只能包一个可标注控件；需要并排放多个控件时，
 * 由调用方给控件自己加 aria-label，或拆成多个 Field。
 * required 只负责在标题后追加星号，不设 aria-required、也不加原生 required：
 * 需要读屏感知或浏览器自校验时，由调用方在控件上补。
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
