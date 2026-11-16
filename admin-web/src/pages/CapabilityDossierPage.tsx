import { useMemo, useState } from 'react'
import type { KeyboardEvent } from 'react'
import { useParams, useSearchParams } from 'react-router-dom'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  fetchAdminMe,
  fetchCapabilityDetail,
  fetchCapabilityVersions,
  fetchAuditEvents,
  fetchLatestAuditEvents,
  fetchToolStats,
  publishCapability,
  rejectCapability,
  setCapabilityVisibility,
  submitCapability,
} from '../api/admin'
import {
  indexGroups,
  indexRoles,
  roleLabel,
  useGroupIndex,
  useGroupsQuery,
  useRoleIndex,
  useRolesQuery,
} from '../api/queries'
import { ApiError } from '../api/client'
import type {
  AdminIdentity,
  AuditEnvelope,
  CapabilityDetail,
  CapabilitySource,
  CapabilityVersion,
  GroupItem,
  RoleItem,
  ToolStatsRow,
} from '../api/types'
import { auditSentence, auditSummary, isStructuredEvent, statusText } from '../audit'
import { formatDateTime } from '../format'
import { EVENT_TYPE_LABEL, TOOL_KIND_LABEL, visibilityText } from '../labels'
import {
  Badge,
  HealthBadge,
  LifecycleBadge,
  OutcomeBadge,
  RiskBadge,
  SourceBadge,
} from '../components/Badge'
import { CallTable } from '../components/CallTable'
import { Card } from '../components/Card'
import { DataTable, type DataTableColumn } from '../components/DataTable'
import { DefinitionView } from '../components/DefinitionView'
import { DetailField, DetailGrid } from '../components/DetailGrid'
import { InfoBar } from '../components/InfoBar'
import { Modal } from '../components/Modal'
import {
  EmptyState,
  ErrorState,
  LoadingState,
  PageShell,
  Pagination,
} from '../components/PageShell'
import { ToggleSwitch } from '../components/ToggleSwitch'
import { SubmitConfirmDialog } from '../components/SubmitConfirmDialog'
import { noticeFromError, type Notice } from '../notice'
import { AuditDetail } from '../components/AuditDetail'
import { BackButton } from '../components/BackButton'
import { useAdminView, type DossierTab } from '../viewStack'

const DASH = '—'
const WINDOW_OPTIONS = [1, 7, 30] as const

const MANAGED_TABS: Array<{ key: DossierTab; label: string }> = [
  { key: 'overview', label: '概览' },
  { key: 'definition', label: '定义' },
  { key: 'versions', label: '版本' },
  { key: 'approval', label: '审批' },
  { key: 'calls', label: '调用' },
  { key: 'audit', label: '审计' },
]

const BUILTIN_TABS: Array<{ key: DossierTab; label: string }> = [
  { key: 'overview', label: '概览' },
  { key: 'calls', label: '调用' },
]

const STATUS_EXPLAIN: Record<string, string> = {
  draft: '草稿：尚未提交审核，只有管理台可见；定义可继续修改。',
  review: '待审：等待审批。',
  published: '已发布：调用方能否看到与调用，仍由「可被发现」「可被调用」两个开关决定。',
  deprecated: '已下线：不再接收新调用，调用方应迁移到替代能力。',
  builtin: '内置工具：由进程内注册，没有草稿/审批流程，也不能调整可见性。',
  unknown: '未知状态：后端未返回生命周期。',
}

/**
 * 路由入口：直接访问 /capabilities/:id 时仍然可用（?tab=approval 兼容）。
 * 返回按钮不再跳路由，而是在 content 区内展示工具台账视图。
 */
export default function CapabilityDossierPage() {
  const { id = '' } = useParams()
  const [searchParams] = useSearchParams()
  const { openTools } = useAdminView()

  if (id === '') {
    return (
      <PageShell>
        <Card title="能力档案">
          <EmptyState hint="URL 缺少能力 ID。请从工具台账或审核队列进入。" />
        </Card>
      </PageShell>
    )
  }

  return (
    <CapabilityDossier
      id={id}
      initialTab={TAB_KEYS.find((key) => key === searchParams.get('tab')) ?? 'overview'}
      onBack={() => openTools()}
    />
  )
}

/** 直接访问 /capabilities/:id?tab=approval 时预选分栏；视图内的分栏不再写 URL。 */
const TAB_KEYS: DossierTab[] = ['overview', 'definition', 'versions', 'approval', 'calls', 'audit']

export function CapabilityDossier({
  id,
  initialTab = 'overview',
  onBack,
  backLabel = '返回工具台账',
}: {
  id: string
  initialTab?: DossierTab | undefined
  /** 返回上一页：视图内是视图栈，路由直访时是工具台账视图。 */
  onBack: () => void
  backLabel?: string | undefined
}) {
  const queryClient = useQueryClient()
  const [tab, setTab] = useState<DossierTab>(initialTab)
  const [windowDays, setWindowDays] = useState<number>(7)
  const [notice, setNotice] = useState<Notice | null>(null)

  const detailQuery = useQuery({
    queryKey: ['capability-detail', id],
    queryFn: ({ signal }) => fetchCapabilityDetail(id, signal),
    retry: false,
  })

  const statsQuery = useQuery({
    queryKey: ['tool-stats', windowDays],
    queryFn: ({ signal }) => fetchToolStats(windowDays, signal),
  })

  const groupsQuery = useGroupsQuery()
  const rolesQuery = useRolesQuery()

  const meQuery = useQuery({
    queryKey: ['admin-me'],
    queryFn: ({ signal }) => fetchAdminMe(signal),
  })

  const timelineQuery = useQuery({
    queryKey: ['audit-timeline', id],
    queryFn: ({ signal }) => fetchLatestAuditEvents({ object_id: id, order: 'desc' }, 12, signal),
  })

  const detail = detailQuery.data
  const row = statsQuery.data?.tools.find((item) => item.id === id)
  const notFound =
    detailQuery.isError &&
    detailQuery.error instanceof ApiError &&
    detailQuery.error.status === 404
  const source: CapabilitySource = detail?.source ?? row?.source ?? (notFound ? 'builtin' : 'managed')
  const builtin = source === 'builtin'
  // 没有受管记录的 id（内置工具或只出现在审计里的历史 id）只提供只读概览与调用。
  const readOnly = builtin || (notFound && detail === undefined)
  const tabs = readOnly ? BUILTIN_TABS : MANAGED_TABS

  const lifecycle = detail?.status ?? row?.lifecycle_status ?? 'unknown'
  const group = detail?.group ?? row?.group ?? ''
  const risk = detail?.risk ?? row?.risk
  const version = detail?.version ?? row?.version
  const discoverable = detail?.discoverable ?? row?.discoverable
  const callable = detail?.callable ?? row?.callable
  const groupIndex = useMemo(() => indexGroups(groupsQuery.data), [groupsQuery.data])
  const roleIndex = useMemo(() => indexRoles(rolesQuery.data), [rolesQuery.data])
  const groupInfo: GroupItem | undefined = groupIndex.get(group)

  const invalidate = () => {
    for (const key of ['capability-detail', 'capability-versions', 'tool-stats', 'audit-timeline', 'review-queue', 'audit-events']) {
      void queryClient.invalidateQueries({ queryKey: [key] })
    }
  }

  const effectiveTab = tabs.some((item) => item.key === tab) ? tab : 'overview'

  /** 标准 tabs 键盘模型：左右循环、Home/End 跳首尾，切换后焦点跟随。 */
  const handleTabKey = (event: KeyboardEvent<HTMLButtonElement>, index: number) => {
    const lastIndex = tabs.length - 1
    let nextIndex = index
    if (event.key === 'ArrowRight') nextIndex = index === lastIndex ? 0 : index + 1
    else if (event.key === 'ArrowLeft') nextIndex = index === 0 ? lastIndex : index - 1
    else if (event.key === 'Home') nextIndex = 0
    else if (event.key === 'End') nextIndex = lastIndex
    else return
    event.preventDefault()
    const target = tabs[nextIndex]
    if (target === undefined) return
    setTab(target.key)
    document.getElementById(`dossier-tab-${target.key}`)?.focus()
  }

  // 404 是内置工具的合法降级路径；其他错误必须显式暴露。
  if (detailQuery.isPending || (detailQuery.isError && notFound && statsQuery.isPending)) {
    return (
      <PageShell>
        <BackButton label={backLabel} onClick={onBack} />
        <LoadingState rows={8} label="正在加载能力档案" />
      </PageShell>
    )
  }

  if (detailQuery.isError && !notFound) {
    return (
      <PageShell>
        <BackButton label={backLabel} onClick={onBack} />
        <Card title="能力档案">
          <ErrorState error={detailQuery.error} />
        </Card>
      </PageShell>
    )
  }

  if (detail === undefined && row === undefined && !detailQuery.isPending) {
    return (
      <PageShell>
        <BackButton label={backLabel} onClick={onBack} />
        <Card title="能力档案">
          <EmptyState hint="能力清单与工具统计里都找不到该 ID。请确认链接是否来自已删除的能力。" />
        </Card>
      </PageShell>
    )
  }

  return (
    <PageShell>
      <BackButton label={backLabel} onClick={onBack} />

      {notice !== null && (
        <InfoBar
          severity={notice.severity}
          title={notice.title}
          description={notice.description}
          meta={notice.meta}
        >
          <button type="button" className="btn btn-subtle btn-sm" onClick={() => setNotice(null)}>
            关闭
          </button>
        </InfoBar>
      )}

      <Card>
        <header className="dossier-head">
          <div className="dossier-title">
            <h2 className="mono">{id}</h2>
            <div className="detail-badges">
              <SourceBadge source={source} />
              <LifecycleBadge status={lifecycle} />
              {risk !== undefined && risk !== null && <RiskBadge risk={risk} />}
              {typeof discoverable === 'boolean' && typeof callable === 'boolean' && (
                <Badge title={`discoverable=${discoverable} callable=${callable}`}>
                  {visibilityText(discoverable, callable)}
                </Badge>
              )}
            </div>
          </div>
          <DetailGrid>
            <DetailField label="类型" title={detail?.kind ?? row?.kind}>
              {TOOL_KIND_LABEL[detail?.kind ?? row?.kind ?? ''] ?? detail?.kind ?? row?.kind ?? DASH}
            </DetailField>
            <DetailField label="组">
              {group === ''
                ? DASH
                : group === 'basic'
                  ? '基础'
                  : (groupInfo?.name ?? group)}
            </DetailField>
            <DetailField label="版本" mono>
              {version === undefined || version === null ? DASH : `v${version}`}
            </DetailField>
            <DetailField label="最近变更" mono>
              {formatDateTime(detail?.last_changed_at ?? row?.last_changed_at)}
            </DetailField>
          </DetailGrid>
          <p className="dossier-status">
            {builtin
              ? STATUS_EXPLAIN.builtin
              : readOnly
                ? '该对象只出现在统计或审计里，没有受管记录：仅提供只读概览与调用记录。'
                : (STATUS_EXPLAIN[lifecycle] ?? STATUS_EXPLAIN.unknown)}
          </p>
        </header>

        <nav className="tabbar" role="tablist" aria-label="能力档案分栏">
          {tabs.map((item, index) => (
            <button
              key={item.key}
              type="button"
              role="tab"
              id={`dossier-tab-${item.key}`}
              aria-controls={`dossier-panel-${item.key}`}
              aria-selected={effectiveTab === item.key}
              tabIndex={effectiveTab === item.key ? 0 : -1}
              className="tab"
              onClick={() => setTab(item.key)}
              onKeyDown={(event) => handleTabKey(event, index)}
            >
              {item.label}
            </button>
          ))}
        </nav>
      </Card>

      <div
        className="tab-panel"
        role="tabpanel"
        id={`dossier-panel-${effectiveTab}`}
        aria-labelledby={`dossier-tab-${effectiveTab}`}
      >
      {effectiveTab === 'overview' && (
        <OverviewTab
          detail={detail}
          row={row}
          group={group}
          groupInfo={groupInfo}
          groupIndex={groupIndex}
          roleIndex={roleIndex}
          groupsLoading={groupsQuery.isPending}
          lifecycle={lifecycle}
          source={source}
          readOnly={readOnly}
          timeline={timelineQuery.data?.events ?? []}
          timelinePending={timelineQuery.isPending}
          timelineError={timelineQuery.error}
          windowDays={windowDays}
          onWindowChange={setWindowDays}
        />
      )}

      {effectiveTab === 'definition' && (
        <Card title="能力定义">
          {detail === undefined ? (
            <EmptyState hint="内置工具没有受管定义；请查看「概览」中的统计与「调用」记录。" />
          ) : (
            <DefinitionView definition={detail.definition} />
          )}
        </Card>
      )}

      {effectiveTab === 'versions' && (
        <VersionsTab id={id} />
      )}

      {effectiveTab === 'approval' && (
        <ApprovalTab
          detail={detail}
          me={meQuery.data}
          mePending={meQuery.isPending}
          meError={meQuery.error}
          readOnly={readOnly}
          builtin={builtin}
          onNotice={setNotice}
          onChanged={invalidate}
        />
      )}

      {effectiveTab === 'calls' && (
        <Card
          title="调用记录"
          actions={
            <div className="segmented" role="group" aria-label="调用时间窗">
              {WINDOW_OPTIONS.map((days) => (
                <button
                  key={days}
                  type="button"
                  aria-pressed={windowDays === days}
                  onClick={() => setWindowDays(days)}
                >
                  {days} 天
                </button>
              ))}
            </div>
          }
        >
          <CallTable id={id} windowDays={windowDays} />
        </Card>
      )}

      {effectiveTab === 'audit' && <AuditTab id={id} />}
      </div>
    </PageShell>
  )
}

/* -------------------------------- 概览 ---------------------------------- */

function percent(rate: number | null | undefined): string {
  return rate === null || rate === undefined ? DASH : `${(rate * 100).toFixed(1)}%`
}

function OverviewTab({
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
              <span className="stat-value">{percent(row.success_rate)}</span>
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
                {row.cache_hits} / {percent(row.hit_rate)}
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

/* -------------------------------- 版本 ---------------------------------- */

function VersionsTab({ id }: { id: string }) {
  const { openAudit } = useAdminView()
  const query = useQuery({
    queryKey: ['capability-versions', id],
    queryFn: ({ signal }) => fetchCapabilityVersions(id, signal),
  })

  const items: CapabilityVersion[] = query.data?.items ?? []
  const hasMissingMeta = items.some(
    (item) =>
      item.approved_by === null &&
      item.approved_at === null &&
      (item.approval_id === null || item.approval_id === ''),
  )

  return (
    <Card title="版本历史" subtitle={query.data === undefined ? '查询中' : `共 ${query.data.total} 个版本`}>
      {hasMissingMeta && (
        <p className="hint">
          旧版本缺少审批元数据（v2 之前的历史行未回填）。
          <button
            type="button"
            className="inline-link"
            onClick={() => openAudit({ objectId: id })}
          >
            在审计查询查看
          </button>
        </p>
      )}
      {query.isPending && <LoadingState rows={5} label="正在加载版本历史" />}
      {query.isError && <ErrorState error={query.error} />}
      {query.data !== undefined && items.length === 0 && (
        <EmptyState hint="没有版本记录。首个定义发布后才会生成版本行。" />
      )}
      {items.length > 0 && (
        <div className="version-list">
          {[...items]
            .sort((left, right) => right.version - left.version)
            .map((item) => (
              <details key={item.version} className="version-item">
                <summary className="version-summary">
                  <span className="mono version-no">v{item.version}</span>
                  <span className="version-status">{statusText(item.status)}</span>
                  <span className="version-meta">
                    <span className="mono">创建 {formatDateTime(item.created_at)}</span>
                    <span className="mono">
                      提审 {item.submitted_by ?? DASH}
                      {item.submitted_at ? ` · ${formatDateTime(item.submitted_at)}` : ''}
                    </span>
                    <span className="mono">
                      批准 {item.approved_by ?? DASH}
                      {item.approved_at ? ` · ${formatDateTime(item.approved_at)}` : ''}
                    </span>
                  </span>
                  <span
                    className="mono version-approval"
                    title={item.approval_id ?? DASH}
                  >
                    审批单 {item.approval_id === null || item.approval_id === '' ? DASH : item.approval_id}
                  </span>
                  <span className="mono version-hash" title={item.definition_hash}>
                    {item.definition_hash.slice(0, 12)}
                  </span>
                </summary>
                <div className="version-body">
                  <DefinitionView definition={item.definition} />
                </div>
              </details>
            ))}
        </div>
      )}
    </Card>
  )
}

/* -------------------------------- 审批 ---------------------------------- */

function ApprovalTab({
  detail,
  me,
  mePending,
  meError,
  readOnly,
  builtin,
  onNotice,
  onChanged,
}: {
  detail: CapabilityDetail | undefined
  me: AdminIdentity | undefined
  mePending: boolean
  meError: unknown
  readOnly: boolean
  builtin: boolean
  onNotice: (notice: Notice) => void
  onChanged: () => void
}) {
  if (readOnly) {
    return (
      <Card title="审批">
        <InfoBar
          severity="info"
          title={builtin ? '内置工具不参与发布流程' : '没有受管定义，无法审批'}
          description={
            builtin
              ? '内置工具由进程内注册，没有草稿/待审/发布状态，也不能调整可见性。'
              : '该 id 只出现在审计与统计中，注册表没有对应能力记录，因此不提供发布或可见性操作。'
          }
        />
      </Card>
    )
  }

  if (detail === undefined) {
    return (
      <Card title="审批">
        <EmptyState hint="受管能力详情未加载，无法进行审批预检。" />
      </Card>
    )
  }

  return (
    <ApprovalPanel
      key={`${detail.id}@${detail.version}@${detail.status}`}
      detail={detail}
      me={me}
      mePending={mePending}
      meError={meError}
      onNotice={onNotice}
      onChanged={onChanged}
    />
  )
}

function ApprovalPanel({
  detail,
  me,
  mePending,
  meError,
  onNotice,
  onChanged,
}: {
  detail: CapabilityDetail
  me: AdminIdentity | undefined
  mePending: boolean
  meError: unknown
  onNotice: (notice: Notice) => void
  onChanged: () => void
}) {
  const roleIndex = useRoleIndex()
  const { openAudit } = useAdminView()
  const [approvalId, setApprovalId] = useState('')
  // 旧版本行可能整行缺少审批元数据（v2 之前未回填）；据此决定审批单说明，不假装能查到。
  const versionsQuery = useQuery({
    queryKey: ['capability-versions', detail.id],
    queryFn: ({ signal }) => fetchCapabilityVersions(detail.id, signal),
  })
  const latestVersion = [...(versionsQuery.data?.items ?? [])].sort(
    (left, right) => right.version - left.version,
  )[0]
  const missingApprovalMeta =
    latestVersion !== undefined &&
    latestVersion.approved_by === null &&
    latestVersion.approved_at === null &&
    (latestVersion.approval_id === null || latestVersion.approval_id === '')
  // 已发布时以当前值为准；待审时默认「发布后开放」，管理员可改成系统直调或保持关闭。
  const published = detail.status === 'published'
  const [discoverable, setDiscoverable] = useState(published ? detail.discoverable : true)
  const [callable, setCallable] = useState(published ? detail.callable : true)
  const [reason, setReason] = useState('')
  const [confirmSubmit, setConfirmSubmit] = useState(false)
  // 发布与退回是两个独立表单：共用一份错误态会把退回校验错误渲染到发布表单里，看起来像「点了没反应」。
  const [publishError, setPublishError] = useState<string | undefined>(undefined)
  const [rejectError, setRejectError] = useState<string | undefined>(undefined)

  const submittedBy = (detail.submitted_by ?? '').trim()

  const refresh = () => {
    onChanged()
  }

  const submitMutation = useMutation({
    mutationFn: () => submitCapability(detail.id),
    onSuccess: () => {
      setConfirmSubmit(false)
      onNotice({
        severity: 'success',
        title: '已提交审核',
        description: `${detail.id}@${detail.version} 已进入待审状态，等待审批。`,
        meta: undefined,
      })
      refresh()
    },
    onError: (error) => onNotice(noticeFromError(error, '提交审核失败')),
  })

  const publishMutation = useMutation({
    mutationFn: async () => {
      await publishCapability(detail.id, approvalId.trim())
      try {
        await setCapabilityVisibility(detail.id, { discoverable, callable })
        return { visibilityFailed: false as const }
      } catch (error) {
        return { visibilityFailed: true as const, error }
      }
    },
    onSuccess: (result) => {
      if (result.visibilityFailed) {
        onNotice({
          severity: 'warning',
          title: '已发布但未开放',
          description:
            '发布成功，但第二步可见性保存失败；调用方仍然看不到该能力。请在下方「当前可见性」重试保存。',
          meta:
            result.visibilityFailed && result.error instanceof ApiError
              ? `${result.error.code} · HTTP ${result.error.status}`
              : undefined,
        })
      } else {
        onNotice({
          severity: 'success',
          title: '已发布并开放',
          description: `${detail.id}@${detail.version} 已发布，可见性已按所选值保存。`,
          meta: `可发现=${discoverable ? '是' : '否'} · 可调用=${callable ? '是' : '否'}`,
        })
      }
      setApprovalId('')
      refresh()
    },
    onError: (error) => onNotice(noticeFromError(error, '发布失败')),
  })

  const visibilityMutation = useMutation({
    mutationFn: () => setCapabilityVisibility(detail.id, { discoverable, callable }),
    onSuccess: () => {
      onNotice({
        severity: 'success',
        title: '可见性已保存',
        description: `${detail.id}：${visibilityText(discoverable, callable)}`,
        meta: undefined,
      })
      refresh()
    },
    onError: (error) => onNotice(noticeFromError(error, '保存可见性失败')),
  })

  const rejectMutation = useMutation({
    mutationFn: () => rejectCapability(detail.id, reason.trim()),
    onSuccess: (result) => {
      setReason('')
      onNotice({
        severity: 'success',
        title: '已退回',
        description: `${detail.id} 已退回草稿：${result.reason}`,
        meta: `版本 v${result.version}`,
      })
      refresh()
    },
    onError: (error) => onNotice(noticeFromError(error, '退回失败')),
  })

  const busy =
    submitMutation.isPending ||
    publishMutation.isPending ||
    rejectMutation.isPending ||
    visibilityMutation.isPending

  return (
    <div className="dossier-columns">
      <Card title="主体与审批">
        <DetailGrid>
          <DetailField label="当前主体" mono title="GET /admin/me">
            {mePending ? '读取中…' : (me?.id ?? DASH)}
          </DetailField>
          <DetailField label="当前主体角色" title={me?.role}>
            {mePending
              ? DASH
              : me === undefined
                ? DASH
                : `${roleLabel(roleIndex, me.role)} · ${me.type}`}
          </DetailField>
          <DetailField label="提审人" mono>
            {submittedBy === '' ? '尚未提审' : submittedBy}
          </DetailField>
          <DetailField label="提审时间" mono>
            {formatDateTime(detail.submitted_at)}
          </DetailField>
          <DetailField label="当前状态">
            <LifecycleBadge status={detail.status} />
          </DetailField>
          <DetailField label="版本" mono>
            v{detail.version}
          </DetailField>
        </DetailGrid>

        {meError !== null && meError !== undefined && <ErrorState error={meError} />}

      </Card>

      <Card title="审批动作" subtitle={`当前版本 v${detail.version}`}>
        {detail.status === 'draft' && (
          <div className="approval-action">
            <p className="hint">草稿提交后进入待审，等待审批。</p>
            <button
              type="button"
              className="btn btn-primary"
              disabled={busy}
              onClick={() => setConfirmSubmit(true)}
            >
              {submitMutation.isPending ? '提交中…' : '提交审核'}
            </button>
          </div>
        )}

        {confirmSubmit && (
          <SubmitConfirmDialog
            id={detail.id}
            version={detail.version}
            status={<LifecycleBadge status={detail.status} />}
            pending={submitMutation.isPending}
            onClose={() => setConfirmSubmit(false)}
            onConfirm={() => submitMutation.mutate()}
          />
        )}

        {detail.status === 'review' && (
          <form
            className="create-form"
            onSubmit={(event) => {
              event.preventDefault()
              if (approvalId.trim() === '') {
                setPublishError('外部审批单号不能为空。')
                return
              }
              setPublishError(undefined)
              publishMutation.mutate()
            }}
          >
            <label className="field">
              <span title="approval_id">
                外部审批单号
              </span>
              <input
                className="input"
                value={approvalId}
                onChange={(event) => setApprovalId(event.target.value)}
                placeholder="例如 ap-biz-water-1"
              />
            </label>
            <p className="hint">
              批准人将记录为当前主体（{me?.id ?? '读取中…'}），不由表单填写；系统不校验外部审批单真伪。
            </p>
            <fieldset className="publish-visibility">
              <legend>发布后可见性（第二步）</legend>
              <ToggleSwitch
                checked={discoverable}
                label="可被发现"
                title="discoverable"
                onChange={setDiscoverable}
              />
              <ToggleSwitch
                checked={callable}
                label="可被调用"
                title="callable"
                onChange={setCallable}
              />
              <p className="hint">
                发布只把状态改为已发布；这两个开关决定调用方能否真正使用。第二步失败会明确提示「已发布但未开放」。
              </p>
            </fieldset>
            {publishError !== undefined && (
              <p className="inline-error" role="alert">
                {publishError}
              </p>
            )}
            <div className="form-actions">
              <button
                type="submit"
                className="btn btn-primary"
                disabled={busy || mePending || me === undefined}
              >
                {publishMutation.isPending ? '发布中…' : '批准发布并开放'}
              </button>
            </div>
          </form>
        )}

        {detail.status === 'published' && (
          <div className="approval-action">
            <p className="hint">调整可见性不会新增版本，也不会触发重新审批。</p>
            <ToggleSwitch
              checked={discoverable}
              label="可被发现"
              title="discoverable"
              onChange={setDiscoverable}
            />
            <ToggleSwitch
              checked={callable}
              label="可被调用"
              title="callable"
              onChange={setCallable}
            />
            <button
              type="button"
              className="btn btn-secondary"
              disabled={busy}
              onClick={() => visibilityMutation.mutate()}
            >
              {visibilityMutation.isPending ? '保存中…' : '保存可见性'}
            </button>
          </div>
        )}

        {detail.status === 'review' && (
          <form
            className="create-form approval-reject"
            onSubmit={(event) => {
              event.preventDefault()
              if (reason.trim() === '') {
                setRejectError('退回原因不能为空。')
                return
              }
              setRejectError(undefined)
              rejectMutation.mutate()
            }}
          >
            <label className="field">
              <span>退回原因</span>
              <textarea
                className="input textarea"
                rows={3}
                value={reason}
                onChange={(event) => setReason(event.target.value)}
                placeholder="说明需要修改的内容；退回后回到草稿"
              />
            </label>
            {rejectError !== undefined && (
              <p className="inline-error" role="alert">
                {rejectError}
              </p>
            )}
            <div className="form-actions">
              <button type="submit" className="btn btn-danger" disabled={busy}>
                {rejectMutation.isPending ? '退回中…' : '退回草稿'}
              </button>
            </div>
          </form>
        )}

        {detail.status === 'deprecated' && (
          <InfoBar
            severity="info"
            title="已下线"
            description="该能力不再参与审批；如需重新上线，需要先恢复为草稿或新建替代能力。"
          />
        )}
      </Card>

      <Card title="当前可见性" subtitle="公开给调用方的实际暴露面">
        <DetailGrid>
          <DetailField label="可被发现" title="discoverable">
            {detail.discoverable ? '是' : '否'}
          </DetailField>
          <DetailField label="可被调用" title="callable">
            {detail.callable ? '是' : '否'}
          </DetailField>
          <DetailField label="组合含义" wide>
            {visibilityText(detail.discoverable, detail.callable)}
          </DetailField>
          <DetailField label="审批单" wide>
            {detail.status === 'review' ? (
              '发布时填写的外部审批单号'
            ) : missingApprovalMeta ? (
              <>
                旧版本缺少审批元数据（v2 之前的历史行未回填）。
                <button
                  type="button"
                  className="inline-link"
                  onClick={() => openAudit({ objectId: detail.id })}
                >
                  在审计查询查看
                </button>
              </>
            ) : (
              <>
                历史审批见「版本」分栏；
                <button
                  type="button"
                  className="inline-link"
                  onClick={() => openAudit({ objectId: detail.id })}
                >
                  在审计查询查看
                </button>
              </>
            )}
          </DetailField>
        </DetailGrid>
      </Card>
    </div>
  )
}

/* -------------------------------- 审计 ---------------------------------- */

function AuditTab({ id }: { id: string }) {
  const [offset, setOffset] = useState(0)
  const [selected, setSelected] = useState<AuditEnvelope | null>(null)
  const groupIndex = useGroupIndex()

  const query = useQuery({
    queryKey: ['audit-object', id, offset],
    queryFn: ({ signal }) =>
      fetchAuditEvents({ object_id: id, order: 'desc' }, offset, signal),
  })

  /* 与 /audit 一致：768 下摘要必须紧随时间列出现。 */
  const columns: Array<DataTableColumn<AuditEnvelope>> = [
    {
      key: 'ts',
      header: '时间',
      mono: true,
      width: '168px',
      cell: (row) => formatDateTime(row.ts),
      title: (row) => row.ts,
    },
    {
      key: 'summary',
      header: '摘要',
      width: '300px',
      wrap: true,
      cell: (row) =>
        isStructuredEvent(row)
          ? auditSummary(row, { group: (code) => groupIndex.get(code)?.name })
          : '旧格式事件，仅原始载荷',
    },
    {
      key: 'type',
      header: '事件类型',
      width: '120px',
      cell: (row) => EVENT_TYPE_LABEL[row.event_type] ?? row.event_type,
    },
    {
      key: 'actor',
      header: '主体',
      width: '140px',
      mono: true,
      cell: (row) => (row.actor.id.trim() === '' ? '未认证调用方' : row.actor.id),
    },
    {
      key: 'actions',
      header: '操作',
      width: '76px',
      sticky: true,
      cell: (row) => (
        <button
          type="button"
          className="btn btn-secondary btn-sm"
          onClick={() => setSelected(row)}
          aria-label={`查看事件 ${row.event_id} 详情`}
        >
          详情
        </button>
      ),
    },
  ]

  return (
    <Card
      title="对象审计"
      subtitle={query.data === undefined ? '查询中' : '按时间倒序'}
      flush
      footer={
        query.data !== undefined && query.data.events.length > 0 ? (
          <Pagination
            total={query.data.total}
            offset={query.data.offset}
            limit={query.data.limit}
            onChange={setOffset}
          />
        ) : undefined
      }
    >
      {query.isPending && <LoadingState rows={6} label="正在查询对象审计" />}
      {query.isError && <ErrorState error={query.error} />}
      {query.data !== undefined && query.data.events.length === 0 && (
        <EmptyState hint="该对象还没有审计事件；提交、发布、可见性调整或调用后会出现。" />
      )}
      {query.data !== undefined && query.data.events.length > 0 && (
        <DataTable
          label={`${id} 的审计事件`}
          columns={columns}
          rows={query.data.events}
          rowKey={(row) => row.event_id}
          minWidth="804px"
        />
      )}

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
    </Card>
  )
}
