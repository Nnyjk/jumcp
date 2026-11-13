import type { ReactNode } from 'react'
import { formatJson } from '../json'

/** 等宽只读代码块：横向/纵向都可滚动，超长 SQL 或 payload 不撑破 Modal。 */
export function CodeBlock({ children, tone }: { children: ReactNode; tone?: 'sql' }) {
  return (
    <pre className={tone === 'sql' ? 'code-block code-block-sql' : 'code-block'} tabIndex={0}>
      <code>{children}</code>
    </pre>
  )
}

export function JsonBlock({ label, value }: { label?: string; value: unknown }) {
  return (
    <figure className="json-block">
      {label !== undefined && <figcaption>{label}</figcaption>}
      <CodeBlock>{formatJson(value)}</CodeBlock>
    </figure>
  )
}
