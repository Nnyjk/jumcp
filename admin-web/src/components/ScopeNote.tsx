/**
 * 统计口径的固定说明：只聚合带结构化对象的审计事件，没有 object_id 的调用不回填。
 * 没有这条说明时 0 次调用会被误读成「从未调用」。
 */
export function AuditScopeNote({ inline = false }: { inline?: boolean }) {
  return (
    <p className={inline ? 'hint scope-note scope-note-inline' : 'hint scope-note'}>
      统计口径：仅统计带结构化对象的审计事件；没有 object_id 的调用不回填，不计入聚合。
    </p>
  )
}
