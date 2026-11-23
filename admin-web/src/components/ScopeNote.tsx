/**
 * 调用统计口径的固定说明，跟着数字一起出现，避免 0 次调用被误读成「从未调用」。
 *
 * 口径来自管理面的聚合实现（`crates/jumcp-server/src/admin/stats.rs`）：
 * 只有 `tool_call` 与 `template_run` 两种事件、且 `payload.object_id` 非空，才归一化成一次调用；
 * 登录/审核/发布等平台自身管理操作，以及 `capability_search` / `capability_describe` 这类查询事件都不计入。
 * `inline` 只切换排版（行内提示 vs 独立段落），文案完全相同。
 */
export function AuditScopeNote({ inline = false }: { inline?: boolean }) {
  return (
    <p className={inline ? 'hint scope-note scope-note-inline' : 'hint scope-note'}>
      统计口径：仅统计带结构化对象的审计事件；没有 object_id 的调用不回填，不计入聚合；
      平台自身的管理操作不计入调用统计。
    </p>
  )
}
