import { useMemo, useState } from 'react'
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
import type { CapabilityDetail, CapabilitySource, GroupItem } from '../../api/types'
import { gsap } from 'gsap'
import { MOTION_DURATION, MOTION_EASE, useGsapScope } from '../../motion/useGsap'
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
import { TabBar, TabPanel } from '../../components/TabBar'
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
          <EmptyState hint="暂无该能力数据。能力清单与工具统计中均未检索到此 ID，请确认链接有效性。" />
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

        <TabBar idBase="dossier" items={tabs} value={effectiveTab} onChange={setTab} label="能力档案分栏" />
      </Card>

      <TabPanel idBase="dossier" tabKey={effectiveTab}>
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
        <DefinitionPanel detail={detail} />
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
        <CallsPanel id={id} windowDays={windowDays} onWindowDays={setWindowDays} />
      )}

      {effectiveTab === 'audit' && <AuditTab id={id} />}
      </TabPanel>
    </PageShell>
  )
}

/**
 * 定义面板：受管能力才有的定义正文。
 * 定义内容随 detail 到达后淡入一次——切换页签已有 TabPanel 的整体淡入，这里只补「内容换人」的层次。
 */
function DefinitionPanel({ detail }: { detail: CapabilityDetail | undefined }) {
  const ref = useGsapScope<HTMLDivElement>((scope) => {
    gsap.from(scope, {
      opacity: 0,
      y: 8,
      duration: MOTION_DURATION.enter,
      ease: MOTION_EASE,
      clearProps: 'opacity,transform',
    })
  }, [detail?.definition_hash])

  return (
    <Card title="能力定义">
      {detail === undefined ? (
        <EmptyState hint="暂无受管定义。内置工具由代码直接注册，详情请查看「概览」指标与「调用」记录。" />
      ) : (
        <div ref={ref}>
          <DefinitionView definition={detail.definition} />
        </div>
      )}
    </Card>
  )
}

/**
 * 调用面板：时间窗切换后表格整体淡入一次。
 * 动效约束：表格内部逐行动效由 DataTable 限制前 20 行，此处仅在时间窗切换时做外层过渡。
 */
function CallsPanel({
  id,
  windowDays,
  onWindowDays,
}: {
  id: string
  windowDays: number
  onWindowDays: (days: number) => void
}) {
  const ref = useGsapScope<HTMLDivElement>((scope) => {
    gsap.from(scope, {
      opacity: 0,
      y: 8,
      duration: MOTION_DURATION.enter,
      ease: MOTION_EASE,
      clearProps: 'opacity,transform',
    })
  }, [windowDays])

  return (
    <Card
      title="调用记录"
      actions={
        <div className="segmented" role="group" aria-label="调用时间窗">
          {WINDOW_OPTIONS.map((days) => (
            <button
              key={days}
              type="button"
              aria-pressed={windowDays === days}
              onClick={() => onWindowDays(days)}
            >
              {days} 天
            </button>
          ))}
        </div>
      }
    >
      {/* 约束：.result-region 施加最小高度以在时间窗切换或调用表格重取时维持容器高度稳定，防塌高抖动 */}
      <div className="result-region" ref={ref}>
        <CallTable key={windowDays} id={id} windowDays={windowDays} />
      </div>
    </Card>
  )
}
