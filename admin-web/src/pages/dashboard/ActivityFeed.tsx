import type { AuditEnvelope, GroupItem, RoleItem } from '../../api/types'
import { auditSentence, isStructuredEvent } from '../../audit'
import { Badge } from '../../components/Badge'
import { Card } from '../../components/Card'
import { EmptyState, ErrorState, LoadingState } from '../../components/PageShell'
import { formatDateTime } from '../../format'
import { EVENT_TYPE_LABEL } from '../../labels'
import { roleLabel } from '../../api/queries'
import { GOVERNANCE_LIMIT } from './dashboardData'

/** 治理动态：管理变更 + 策略判定合并按时间倒序，摘要句复用审计文案。 */
export function ActivityFeed({
  items,
  pending,
  bothError,
  partialError,
  error,
  groupIndex,
  roleIndex,
}: {
  items: AuditEnvelope[]
  pending: boolean
  bothError: boolean
  partialError: boolean
  error: unknown
  groupIndex: Map<string, GroupItem>
  roleIndex: Map<string, RoleItem>
}) {
  return (
    <div className="dash-span-6">
      <Card title="治理动态" subtitle={`最近 ${GOVERNANCE_LIMIT} 条`}>
        {pending && items.length === 0 && (
          <LoadingState rows={4} label="正在加载治理动态" />
        )}
        {!pending && items.length === 0 && (
          bothError || partialError ? (
            <ErrorState error={error} />
          ) : (
            <EmptyState hint="最近没有管理变更或策略判定事件。" />
          )
        )}
        {items.length > 0 && (
          <>
            <ol className="gov-list">
              {items.map((event) => {
                // 摘要句已含结果与错误码；这里只补一句后端原文，不再重复「策略拒绝：…」。
                const reason =
                  event.payload.action === 'deny'
                    ? event.payload.error_message?.trim() || undefined
                    : undefined
                return (
                  <li key={event.event_id} className="gov-item">
                    <div className="gov-head">
                      <span className="gov-time" title={event.ts}>
                        {formatDateTime(event.ts)}
                      </span>
                      <Badge
                        tone={event.event_type === 'admin_change' ? 'accent' : 'warning'}
                        title={event.event_type}
                      >
                        {EVENT_TYPE_LABEL[event.event_type] ?? event.event_type}
                      </Badge>
                      <span
                        className="gov-actor"
                        title={`${event.actor.type}:${event.actor.id} · 角色 ${roleLabel(
                          roleIndex,
                          event.actor.role,
                        )}（${event.actor.role}）`}
                      >
                        {event.actor.id || '未认证调用方'} ·{' '}
                        <span title={event.actor.role}>
                          {roleLabel(roleIndex, event.actor.role)}
                        </span>
                      </span>
                    </div>
                    <p className="gov-sentence">
                      {isStructuredEvent(event)
                        ? auditSentence(event, {
                            group: (code) => groupIndex.get(code)?.name,
                          })
                        : '旧格式事件，仅原始载荷'}
                    </p>
                    {reason !== undefined && <p className="gov-reason">原因：{reason}</p>}
                  </li>
                )
              })}
            </ol>
            {partialError && (
              <p className="hint dash-card-note">
                部分事件类型查询失败，列表只包含成功返回的一侧。
              </p>
            )}
          </>
        )}
      </Card>
    </div>
  )
}
