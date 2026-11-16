import { useState } from 'react'
import { roleLabel } from '../../api/queries'
import type {
  AuditEnvelope,
  CapabilityDetail,
  CapabilitySource,
  GroupItem,
  RoleItem,
  ToolStatsRow,
} from '../../api/types'
import { auditSentence, isStructuredEvent } from '../../audit'
import { DASH, formatDateTime, formatPercent } from '../../format'
import {
  Badge,
  HealthBadge,
  LifecycleBadge,
  OutcomeBadge,
  SourceBadge,
} from '../../components/Badge'
import { Card } from '../../components/Card'
import { DetailField, DetailGrid } from '../../components/DetailGrid'
import { InfoBar } from '../../components/InfoBar'
import { Modal } from '../../components/Modal'
import { EmptyState, ErrorState, LoadingState } from '../../components/PageShell'
import { AuditDetail } from '../../components/AuditDetail'
import { WINDOW_OPTIONS } from '../../constants'

export function OverviewTab({
  detail,
  row,
  group,
  groupInfo,
  groupIndex,
  roleIndex,
  groupsLoading,
  lifecycle,
  source,
  readOnly,
  timeline,
  timelinePending,
  timelineError,
  windowDays,
  onWindowChange,
}: {
  detail: CapabilityDetail | undefined
  row: ToolStatsRow | undefined
  group: string
  groupInfo: GroupItem | undefined
  groupIndex: Map<string, GroupItem>
  roleIndex: Map<string, RoleItem>
  groupsLoading: boolean
  lifecycle: string
  source: CapabilitySource
  readOnly: boolean
  timeline: AuditEnvelope[]
  timelinePending: boolean
  timelineError: unknown
  windowDays: number
  onWindowChange: (days: number) => void
}) {
  const [selected, setSelected] = useState<AuditEnvelope | null>(null)

  return (
    <>
      <div className="dossier-columns">
        <Card title="生效范围">
          {group === '' ? (
            <p className="muted">该对象没有组归属。</p>
          ) : readOnly && source === 'builtin' && group === 'basic' ? (
            <InfoBar
              severity="info"
              title="基础内置工具不按管理组授权"
              description="基础内置工具由发现面直接下发，不归属「组维护」页里的管理组，因此这里没有组→角色映射；维护类内置工具（admin_*）挂在 admin 组，仍受组与 token allowlist 约束。"
            />
          ) : groupsLoading ? (
            <LoadingState rows={2} label="正在加载组信息" />
          ) : groupInfo === undefined ? (
            <InfoBar
              severity="danger"
              title="组不存在"
              description={`组 ${group} 不在管理台的组清单中。调用方的工具列表永远不会出现该能力，请先在「组维护」创建该组。`}
            />
          ) : (
            <>
              <DetailGrid>
                <DetailField label="组" title={groupInfo.code}>
                  {groupInfo.name}
                </DetailField>
                <DetailField label="来源">
                  {groupInfo.source === 'policy' ? '策略组（只读）' : '受管组'}
                </DetailField>
                <DetailField label="角色" wide>
                  {groupInfo.roles.length === 0 ? (
                    '未绑定角色：任何调用方都无法通过 scope 校验'
                  ) : (
                    <span className="detail-badges">
                      {groupInfo.roles.map((code) => (
                        <Badge key={code} title={code}>
                          {roleLabel(roleIndex, code)}
                        </Badge>
                      ))}
                    </span>
                  )}
                </DetailField>
                <DetailField label="预算下限" title="min_budget">
                  {groupInfo.min_budget}
                </DetailField>
                <DetailField label="排序权重" title="priority">
                  {groupInfo.priority}
                </DetailField>
                <DetailField label="组内能力">
                  {groupInfo.capability_count}
                </DetailField>
              </DetailGrid>            </>
          )}
        </Card>

        <Card title="状态与可见性">
          <DetailGrid>
            <DetailField label="生命周期">
              <LifecycleBadge status={lifecycle} />
            </DetailField>
            <DetailField label="来源">
              <SourceBadge source={source} />
            </DetailField>
            <DetailField label="可被发现" title="discoverable">
              {detail === undefined && row === undefined
                ? DASH
                : (detail?.discoverable ?? row?.discoverable) === true
                  ? '是'
                  : '否'}
            </DetailField>
            <DetailField label="可被调用" title="callable">
              {detail === undefined && row === undefined
                ? DASH
                : (detail?.callable ?? row?.callable) === true
                  ? '是'
                  : '否'}
            </DetailField>
            <DetailField
              label="定义哈希"
              mono
              title={detail?.definition_hash === undefined || detail.definition_hash === '' ? undefined : detail.definition_hash}
            >
              {detail?.definition_hash === undefined || detail.definition_hash === ''
                ? DASH
                : detail.definition_hash.slice(0, 12)}
            </DetailField>
            <DetailField label="定义更新于" mono>
              {formatDateTime(detail?.definition_updated_at)}
            </DetailField>
            <DetailField label="替代能力" mono>
              {detail?.successor_id === undefined || detail.successor_id === ''
                ? DASH
                : detail.successor_id}
            </DetailField>
          </DetailGrid>
          {!readOnly && lifecycle === 'published' && (detail?.discoverable === false || detail?.callable === false) && (
            <InfoBar
              severity="warning"
              title="已发布但未完全开放"
              description="已发布不代表调用方可用：当前可见性开关未全部打开，可按下方指标确认实际暴露面。"
            />
          )}
        </Card>
      </div>

      <Card
        title="调用指标"
        actions={
          <div className="segmented" role="group" aria-label="调用指标时间窗">
            {WINDOW_OPTIONS.map((days) => (
              <button
                key={days}
                type="button"
                aria-pressed={windowDays === days}
                onClick={() => onWindowChange(days)}
              >
                {days} 天
              </button>
            ))}
          </div>
        }
      >
        {row === undefined ? (
          <EmptyState hint={`最近 ${windowDays} 天没有该对象的统计行（可能没有任何调用）。`} />
        ) : (
          <div className="stat-grid">
            <div className="stat">
              <span className="stat-label">调用 / 成功 / 失败</span>
              <span className="stat-value stat-value-sm">
                {row.calls} / {row.success} / {row.failure}
              </span>
            </div>
            <div className="stat">
              <span className="stat-label">
                成功率
              </span>
              <span className="stat-value">{formatPercent(row.success_rate)}</span>
            </div>
            <div className="stat">
              <span className="stat-label">
                健康度
              </span>
              <span className="stat-value stat-value-sm">
                <HealthBadge health={row.health} />
              </span>
            </div>
            <div className="stat">
              <span className="stat-label">
                缓存命中
              </span>
              <span className="stat-value stat-value-sm">
                {row.cache_hits} / {formatPercent(row.hit_rate)}
              </span>
            </div>
            <div className="stat">
              <span className="stat-label">
                失败重试
              </span>
              <span className="stat-value stat-value-sm">{row.retries}</span>
            </div>
            <div className="stat">
              <span className="stat-label">
                平均 / p95 / 最大 tokens
              </span>
              <span className="stat-value stat-value-sm">
                {row.avg_response_tokens ?? DASH} / {row.p95_response_tokens ?? DASH} /{' '}
                {row.max_response_tokens ?? DASH}
              </span>
            </div>
            <div className="stat">
              <span className="stat-label">最近调用 · 结果</span>
              <span className="stat-value stat-value-sm">
                {row.last_call_at === null ? (
                  DASH
                ) : (
                  <>
                    {formatDateTime(row.last_call_at)} ·{' '}
                    <OutcomeBadge outcome={row.last_outcome} />
                  </>
                )}
              </span>
            </div>
          </div>
        )}
      </Card>

      <Card title="生命周期时间线" subtitle="最新在前 · 最近 12 条">
        <p className="hint">仅统计新审计事件；旧记录无 object_id，无法关联。</p>
        {timelinePending && <LoadingState rows={4} label="正在加载时间线" />}
        {timelineError !== null && timelineError !== undefined && <ErrorState error={timelineError} />}
        {!timelinePending && timeline.length === 0 && (
          <EmptyState hint="该对象还没有治理事件。提交审核、发布或调整可见性后会出现在这里。" />
        )}
        {timeline.length > 0 && (
          <ol className="timeline">
            {timeline.map((event) => (
              <li key={event.event_id} className="timeline-item">
                <span className="timeline-time mono" title={event.ts}>
                  {formatDateTime(event.ts)}
                </span>
                <span className="timeline-text">
                  {isStructuredEvent(event)
                    ? auditSentence(event, { group: (code) => groupIndex.get(code)?.name })
                    : '旧格式事件，仅原始载荷'}
                </span>
                <button
                  type="button"
                  className="btn btn-subtle btn-sm"
                  onClick={() => setSelected(event)}
                >
                  详情
                </button>
              </li>
            ))}
          </ol>
        )}
      </Card>

      {selected !== null && (
        <Modal
          key={selected.event_id}
          size="xl"
          title={`审计详情 · ${selected.event_id}`}
          onClose={() => setSelected(null)}
        >
          <AuditDetail event={selected} />
        </Modal>
      )}
    </>
  )
}
