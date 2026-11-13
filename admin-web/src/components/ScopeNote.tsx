/**
 * 统计口径的固定说明：只聚合审计 v2 事件，旧格式调用没有 object_id 也不回填。
 * 没有这条说明时 0 次调用会被误读成「从未调用」。
 */
export function AuditScopeNote({ inline = false }: { inline?: boolean }) {
  return (
    <p className={inline ? 'hint scope-note scope-note-inline' : 'hint scope-note'}>
      统计口径：仅统计新审计事件；改造前的历史调用未回填，不计入聚合。
    </p>
  )
}
