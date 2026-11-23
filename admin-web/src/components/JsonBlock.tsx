import type { ReactNode } from 'react'
import { formatJson } from '../json'

/**
 * 等宽只读代码块：高度封顶 340px，超出靠自身双向滚动——不换行、不截断，也不撑破 Modal 与栅格。
 * pre 带 tabIndex=0：纯文本没有别的焦点入口，键盘用户要靠它进入滚动区域。
 * tone="sql" 只切换配色，不改变布局。
 */
export function CodeBlock({ children, tone }: { children: ReactNode; tone?: 'sql' }) {
  return (
    <pre className={tone === 'sql' ? 'code-block code-block-sql' : 'code-block'} tabIndex={0}>
      <code>{children}</code>
    </pre>
  )
}

/**
 * 只读 JSON 视图：label 作 figcaption（不给就只渲染代码块）。
 * 序列化交给 formatJson（2 空格缩进）；无法序列化（BigInt、循环引用）时退回 String，展示层不会整块崩掉。
 */
export function JsonBlock({ label, value }: { label?: string; value: unknown }) {
  return (
    <figure className="json-block">
      {label !== undefined && <figcaption>{label}</figcaption>}
      <CodeBlock>{formatJson(value)}</CodeBlock>
    </figure>
  )
}
