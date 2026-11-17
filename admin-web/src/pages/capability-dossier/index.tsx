import { useMemo, useState } from 'react'
import type { KeyboardEvent } from 'react'
import { useParams, useSearchParams } from 'react-router-dom'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import {
  fetchAdminMe,
  fetchCapabilityDetail,
  fetchLatestAuditEvents,
  fetchToolStats,
} from '../../api/admin'
import { indexGroups, indexRoles, useGroupsQuery, useRolesQuery } from '../../api/queries'
import { ApiError } from '../../api/client'
import type { CapabilitySource, GroupItem } from '../../api/types'
import { DASH, formatDateTime } from '../../format'
import { WINDOW_OPTIONS } from '../../constants'
import { TOOL_KIND_LABEL, visibilityText } from '../../labels'
import { Badge, LifecycleBadge, RiskBadge, SourceBadge } from '../../components/Badge'
import { BackButton } from '../../components/BackButton'
import { CallTable } from '../../components/CallTable'
import { Card } from '../../components/Card'
import { DefinitionView } from '../../components/DefinitionView'
import { DetailField, DetailGrid } from '../../components/DetailGrid'
import { InfoBar } from '../../components/InfoBar'
import { EmptyState, ErrorState, LoadingState, PageShell } from '../../components/PageShell'
import type { Notice } from '../../notice'
import { useAdminView, type DossierTab } from '../../viewStack'
import { ApprovalTab } from './ApprovalTab'
import { AuditTab } from './AuditTab'
import { OverviewTab } from './OverviewTab'
import { VersionsTab } from './VersionsTab'
import {
  BUILTIN_TABS,
  MANAGED_TABS,
  STATUS_EXPLAIN,
  TAB_KEYS,
} from './dossierFormat'

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
              aria-controls={effectiveTab === item.key ? `dossier-panel-${item.key}` : undefined}
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
          <CallTable key={windowDays} id={id} windowDays={windowDays} />
        </Card>
      )}

      {effectiveTab === 'audit' && <AuditTab id={id} />}
      </div>
    </PageShell>
  )
}
