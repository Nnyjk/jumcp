import { useMemo, useState } from 'react'
import { useSearchParams } from 'react-router-dom'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { fetchToolStats } from '../../api/admin'
import { groupLabel, indexGroups, useGroupsQuery } from '../../api/queries'
import type { CapabilityRisk, ToolHealth, ToolSource, ToolStatsRow } from '../../api/types'
import {
  CategoryBadge,
  HealthBadge,
  LifecycleBadge,
  OutcomeBadge,
  RiskBadge,
  Badge,
} from '../../components/Badge'
import { Card } from '../../components/Card'
import { DataTable, type DataTableColumn } from '../../components/DataTable'
import { Modal } from '../../components/Modal'
import { AuditScopeNote } from '../../components/ScopeNote'
import { InfoBar } from '../../components/InfoBar'
import { noticeFromError, type Notice } from '../../notice'
import { EmptyState, ErrorState, LoadingState, PageShell } from '../../components/PageShell'
import { DASH, formatDateTime, formatPercent } from '../../format'
import {
  TOOL_KIND_LABEL,
  toolGroupLabel as groupCodeLabel,
  visibilityText,
} from '../../labels'
import { useAdminView } from '../../viewStack'
import { CreateCapabilityForm } from './CreateCapabilityForm'
import { ToolFilterBar } from './ToolFilterBar'
import { filterTools } from './toolFormat'


export default function ToolsPage({
  initialGroup,
  onOpenDossier,
}: {
  /** 操作视图入口带来的预设组过滤（组维护「查看能力」）；路由页仍兼容读取 ?group。 */
  initialGroup?: string | undefined
  onOpenDossier?: ((id: string) => void) | undefined
}) {
  const queryClient = useQueryClient()
  const [searchParams] = useSearchParams()
  const { openDossier } = useAdminView()
  const [windowDays, setWindowDays] = useState(7)
  const [tab, setTab] = useState<ToolSource>('managed')
  const [health, setHealth] = useState<'' | ToolHealth>('')
  const [q, setQ] = useState('')
  const [risk, setRisk] = useState<'' | CapabilityRisk>('')
  const [category, setCategory] = useState('')
  const [createOpen, setCreateOpen] = useState(false)
  const [notice, setNotice] = useState<Notice | undefined>(undefined)
  /** 组过滤是页内状态：只调整 content 区内容，不写 URL。初始值兼容旧深链 ?group。 */
  const [groupFilter, setGroupFilter] = useState(
    () => initialGroup ?? searchParams.get('group') ?? '',
  )

  const statsQuery = useQuery({
    queryKey: ['tool-stats', windowDays],
    queryFn: ({ signal }) => fetchToolStats(windowDays, signal),
  })

  const groupsQuery = useGroupsQuery()
  const groups = groupsQuery.data?.items ?? []
  const groupIndex = useMemo(() => indexGroups(groupsQuery.data), [groupsQuery.data])

  const allTools = useMemo(() => statsQuery.data?.tools ?? [], [statsQuery.data])
  const builtinCount = allTools.filter((row) => row.source === 'builtin').length
  const managedCount = allTools.length - builtinCount

  const rows = useMemo(
    () => filterTools(allTools, { source: tab, health, risk, category, group: groupFilter, q }, groupIndex),
    [allTools, tab, health, risk, category, q, groupFilter, groupIndex],
  )

  /**
   * 列顺序按首屏可见性排：768 内容区约 672px，冻结操作列占 76px，
   * 所以 ID 之后紧跟 调用/失败、成功率、健康，其余维度顺次右移靠横向滚动补齐。
   * 列宽合计 1212px（含新增分类列），1440 视口下无横向滚动。
   */
  const columns: Array<DataTableColumn<ToolStatsRow>> = [
    {
      key: 'id',
      header: '工具 ID',
      mono: true,
      width: '200px',
      cell: (row) => (
        <span className="cell-stack">
          <span>{row.id}</span>
          <span className="cell-sub">
            {TOOL_KIND_LABEL[row.kind] ?? row.kind}
            {row.version === null ? '' : ` · v${row.version}`}
          </span>
        </span>
      ),
      title: (row) => row.id,
    },
    {
      key: 'calls',
      header: '调用 / 失败',
      numeric: true,
      width: '88px',
      cell: (row) => (
        <span className="cell-stack cell-stack-end">
          <span>{row.calls}</span>
          <span className={`cell-sub${row.failure > 0 ? ' cell-sub-danger' : ''}`}>
            失败 {row.failure}
          </span>
        </span>
      ),
      title: (row) => `调用 ${row.calls} · 失败 ${row.failure} · 失败后重试 ${row.retries}`,
    },
    {
      key: 'success_rate',
      header: '成功率',
      numeric: true,
      width: '76px',
      cell: (row) => formatPercent(row.success_rate),
    },
    {
      key: 'health',
      header: '健康',
      width: '88px',
      cell: (row) => <HealthBadge health={row.health} />,
    },
    {
      key: 'lifecycle',
      header: '生命周期',
      width: '88px',
      cell: (row) => <LifecycleBadge status={row.lifecycle_status} />,
    },
    {
      key: 'risk',
      header: '风险',
      width: '72px',
      cell: (row) => (row.risk === null ? DASH : <RiskBadge risk={row.risk} />),
    },
    {
      key: 'visibility',
      header: '可见性',
      width: '132px',
      cell: (row) =>
        typeof row.discoverable === 'boolean' && typeof row.callable === 'boolean' ? (
          <Badge title={`discoverable=${row.discoverable} callable=${row.callable}`}>
            {visibilityText(row.discoverable, row.callable)}
          </Badge>
        ) : (
          DASH
        ),
    },
    {
      key: 'group',
      header: '组',
      width: '132px',
      cell: (row) =>
        row.group === '' ? (
          DASH
        ) : (
          <span title={groupCodeLabel(row.group, groupLabel(groupIndex, row.group))}>
            {groupCodeLabel(row.group, groupLabel(groupIndex, row.group))}
          </span>
        ),
    },
    {
      key: 'category',
      header: '分类',
      width: '92px',
      cell: (row) =>
        row.category === undefined || row.category === '' ? (
          DASH
        ) : (
          <CategoryBadge category={row.category} />
        ),
    },
    {
      key: 'last_call',
      header: '最近调用',
      mono: true,
      width: '168px',
      cell: (row) => (
        <span className="cell-stack">
          <span>{formatDateTime(row.last_call_at)}</span>
          {row.last_outcome !== null && (
            <span className="cell-sub">
              <OutcomeBadge outcome={row.last_outcome} />
            </span>
          )}
        </span>
      ),
      title: (row) => row.last_call_at ?? DASH,
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
          onClick={() =>
            onOpenDossier === undefined ? openDossier({ id: row.id }) : onOpenDossier(row.id)
          }
          aria-label={`查看 ${row.id} 能力档案`}
        >
          详情
        </button>
      ),
    },
  ]

  return (
    <PageShell>
      {notice !== undefined && (
        <InfoBar
          severity={notice.severity}
          title={notice.title}
          description={notice.description}
          meta={notice.meta}
        >
          <button type="button" className="btn btn-subtle btn-sm" onClick={() => setNotice(undefined)}>
            关闭
          </button>
        </InfoBar>
      )}

      <ToolFilterBar
        windowDays={windowDays}
        onWindowDaysChange={setWindowDays}
        health={health}
        onHealthChange={setHealth}
        risk={risk}
        onRiskChange={setRisk}
        category={category}
        onCategoryChange={setCategory}
        groupFilter={groupFilter}
        onGroupFilterChange={setGroupFilter}
        q={q}
        onQueryChange={setQ}
        groups={groups}
        groupsPending={groupsQuery.isPending}
        groupsError={groupsQuery.isError}
        refreshing={statsQuery.isFetching}
        onRefresh={() => void statsQuery.refetch()}
        onClear={() => {
          setHealth('')
          setRisk('')
          setCategory('')
          setQ('')
          setGroupFilter('')
        }}
      />

      {statsQuery.data?.truncated === true && (
        <InfoBar
          severity="warning"
          title="统计结果已截断"
          description="后端返回的工具数达到上限（200），列表只展示前 200 条；缩窄时间窗可降低基数。"
        />
      )}

      <Card
        title={tab === 'builtin' ? '内置工具清单' : '受管能力清单'}
        subtitle={statsQuery.data === undefined ? (statsQuery.isError ? '查询失败' : '查询中') : `最近 ${windowDays} 天`}
        actions={
          <>
            <div className="segmented" role="group" aria-label="工具来源">
              <button type="button" aria-pressed={tab === 'managed'} onClick={() => setTab('managed')}>
                受管能力 <span className="segmented-count">{managedCount}</span>
              </button>
              <button type="button" aria-pressed={tab === 'builtin'} onClick={() => setTab('builtin')}>
                内置工具 <span className="segmented-count">{builtinCount}</span>
              </button>
            </div>
            <button
              type="button"
              className="btn btn-primary btn-sm"
              onClick={() => setCreateOpen(true)}
            >
              新建草案
            </button>
          </>
        }
        flush
      >
        <AuditScopeNote />
        {statsQuery.isPending && <LoadingState rows={8} label="正在加载工具统计" />}
        {statsQuery.isError && (
          <div className="state-wrap">
            <ErrorState error={statsQuery.error} />
            <button
              type="button"
              className="btn btn-secondary"
              onClick={() => statsQuery.refetch()}
              disabled={statsQuery.isFetching}
            >
              重试
            </button>
          </div>
        )}
        {statsQuery.data !== undefined && allTools.length === 0 && (
          <EmptyState hint="时间窗内没有任何工具调用记录。可先「新建草案」，或放宽时间窗后刷新。" />
        )}
        {allTools.length > 0 && rows.length === 0 && (
          <EmptyState
            hint={`「${tab === 'builtin' ? '内置工具' : '受管能力'}」下没有符合健康/风险/组/搜索条件的记录。`}
            action={
              <button
                type="button"
                className="btn btn-secondary"
                onClick={() => {
                  setHealth('')
                  setRisk('')
                  setCategory('')
                  setQ('')
                  setGroupFilter('')
                }}
              >
                清空筛选
              </button>
            }
          />
        )}
        {rows.length > 0 && (
          <DataTable
            label={`${tab === 'builtin' ? '内置工具' : '受管能力'}台账`}
            meta={`共 ${rows.length} 行 · 按调用次数降序`}
            columns={columns}
            rows={rows}
            rowKey={(row) => row.id}
            minWidth="1212px"
          />
        )}
      </Card>

      {createOpen && (
        <Modal
          size="lg"
          title="新建草案"
          subtitle="创建后进入草稿，需在能力档案的「审批」分栏提交审核"
          onClose={() => setCreateOpen(false)}
        >
          <CreateCapabilityForm
            onCreated={(created) => {
              setCreateOpen(false)
              setTab('managed')
              setNotice({
                severity: 'success',
                title: '草案已创建',
                description: `能力 ${created.id} 已创建为草稿，可在档案页提交审核。`,
                meta: `版本 v${created.version} · 定义哈希 ${created.definition_hash}`,
              })
              void queryClient.invalidateQueries({ queryKey: ['tool-stats'] })
            }}
            onError={(error) => setNotice(noticeFromError(error, '新建草案失败'))}
          />
        </Modal>
      )}
    </PageShell>
  )
}
