import { useMemo, useState } from 'react'
import { useSearchParams } from 'react-router-dom'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { createCapability, fetchToolStats } from '../api/admin'
import { groupLabel, indexGroups, indexRoles, roleLabel, useGroupsQuery, useRolesQuery } from '../api/queries'
import type { CapabilityRisk, GroupItem, ToolHealth, ToolSource, ToolStatsRow } from '../api/types'
import {
  CategoryBadge,
  HealthBadge,
  LifecycleBadge,
  OutcomeBadge,
  RiskBadge,
  Badge,
} from '../components/Badge'
import { Card } from '../components/Card'
import { DataTable, type DataTableColumn } from '../components/DataTable'
import { Modal } from '../components/Modal'
import { AuditScopeNote } from '../components/ScopeNote'
import { InfoBar } from '../components/InfoBar'
import { noticeFromError, type Notice } from '../notice'
import {
  EmptyState,
  ErrorState,
  LoadingState,
  PageShell,
} from '../components/PageShell'
import { formatDateTime } from '../format'
import { TOOL_CATEGORY_LABEL, TOOL_KIND_LABEL, UPSTREAM_CATEGORY_LABEL, visibilityText } from '../labels'
import { useAdminView } from '../viewStack'

const DASH = '—'

const HEALTH_OPTIONS: Array<{ value: '' | ToolHealth; label: string }> = [
  { value: '', label: '全部' },
  { value: 'ok', label: '正常' },
  { value: 'degraded', label: '降级' },
  { value: 'failing', label: '故障' },
  { value: 'unknown', label: '未知' },
]

const WINDOW_OPTIONS = [1, 7, 30]

/** 上游分类：三组内置分类使用中文名，custom 为自定义接入。 */
const CATEGORY_OPTIONS: Array<{ value: string; label: string }> = [
  { value: '', label: '全部分类' },
  { value: 'basic', label: UPSTREAM_CATEGORY_LABEL.basic },
  { value: 'analysis', label: UPSTREAM_CATEGORY_LABEL.analysis },
  { value: 'admin', label: UPSTREAM_CATEGORY_LABEL.admin },
  { value: 'custom', label: UPSTREAM_CATEGORY_LABEL.custom },
]

/** 内置工具不归属管理台维护的组：组列只显示「内置」，不露出 builtin code。 */
function toolGroupLabel(index: Map<string, GroupItem>, code: string): string {
  return code === 'builtin' ? '内置' : groupLabel(index, code)
}

/** null / undefined 一律显示破折号，避免把“无数据”误读成 0。 */
function dash(value: string | number | null | undefined): string {
  return value === null || value === undefined || value === '' ? DASH : String(value)
}

function percent(rate: number | null | undefined): string {
  return rate === null || rate === undefined ? DASH : `${(rate * 100).toFixed(1)}%`
}

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

  const rows = useMemo(() => {
    const needle = q.trim().toLowerCase()
    return allTools
      .filter((row) => row.source === tab)
      .filter((row) => health === '' || row.health === health)
      .filter((row) => risk === '' || row.risk === risk)
      .filter((row) => category === '' || row.category === category)
      .filter((row) => groupFilter === '' || row.group === groupFilter)
      .filter(
        (row) =>
          needle === '' ||
          row.id.toLowerCase().includes(needle) ||
          row.group.toLowerCase().includes(needle) ||
          toolGroupLabel(groupIndex, row.group).toLowerCase().includes(needle) ||
          (TOOL_CATEGORY_LABEL[row.category ?? ''] ?? '').toLowerCase().includes(needle),
      )
      .sort((a, b) => b.calls - a.calls || a.id.localeCompare(b.id))
  }, [allTools, tab, health, risk, category, q, groupFilter, groupIndex])

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
          <span>{dash(row.calls)}</span>
          <span className={`cell-sub${row.failure > 0 ? ' cell-sub-danger' : ''}`}>
            失败 {dash(row.failure)}
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
      cell: (row) => percent(row.success_rate),
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
          <span title={toolGroupLabel(groupIndex, row.group)}>
            {toolGroupLabel(groupIndex, row.group)}
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

      <Card title="筛选">
        <div className="filter-bar">
          <label className="field">
            <span>时间窗</span>
            <select
              className="select"
              value={windowDays}
              onChange={(event) => setWindowDays(Number(event.target.value))}
            >
              {WINDOW_OPTIONS.map((days) => (
                <option key={days} value={days}>
                  最近 {days} 天
                </option>
              ))}
            </select>
          </label>
          <label className="field">
            <span>健康状态</span>
            <select
              className="select"
              value={health}
              onChange={(event) => setHealth(event.target.value as '' | ToolHealth)}
            >
              {HEALTH_OPTIONS.map((option) => (
                <option key={option.value} value={option.value}>
                  {option.label}
                </option>
              ))}
            </select>
          </label>
          <label className="field">
            <span>风险</span>
            <select
              className="select"
              value={risk}
              onChange={(event) => setRisk(event.target.value as '' | CapabilityRisk)}
            >
              <option value="">全部</option>
              <option value="read">只读</option>
              <option value="write">写入</option>
              <option value="sensitive">敏感</option>
              <option value="admin">管理</option>
            </select>
          </label>
          <label className="field">
            <span>分类</span>
            <select
              className="select"
              value={category}
              onChange={(event) => setCategory(event.target.value)}
            >
              {CATEGORY_OPTIONS.map((option) => (
                <option key={option.value} value={option.value}>
                  {option.label}
                </option>
              ))}
            </select>
          </label>
          <label className="field">
            <span>组</span>
            <select
              className="select"
              value={groupFilter}
              onChange={(event) => setGroupFilter(event.target.value)}
              disabled={groupsQuery.isPending}
            >
              <option value="">
                {groupsQuery.isPending
                  ? '正在加载组…'
                  : groupsQuery.isError
                    ? '组清单加载失败'
                    : '全部组'}
              </option>
              {groups.map((group) => (
                <option key={group.code} value={group.code} title={group.code}>
                  {group.name}
                </option>
              ))}
            </select>
          </label>
          <label className="field field-grow">
            <span>搜索</span>
            <input
              className="input"
              type="search"
              value={q}
              onChange={(event) => setQ(event.target.value)}
              placeholder="按工具 ID、组名或分类过滤"
            />
          </label>
          <div className="filter-actions">
            <button
              type="button"
              className="btn btn-secondary"
              onClick={() => statsQuery.refetch()}
              disabled={statsQuery.isFetching}
            >
              刷新
            </button>
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
          </div>
        </div>
      </Card>

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

/* ------------------------------- 新建草案 -------------------------------- */

function CreateCapabilityForm({
  onCreated,
  onError,
}: {
  onCreated: (created: { id: string; version: number; definition_hash: string }) => void
  onError: (error: unknown) => void
}) {
  const [templateId, setTemplateId] = useState('')
  const [group, setGroup] = useState('')
  const [description, setDescription] = useState('')
  const [sql, setSql] = useState('')
  const [paramsText, setParamsText] = useState('')
  const [formError, setFormError] = useState<string | undefined>(undefined)

  const groupsQuery = useGroupsQuery()
  const rolesQuery = useRolesQuery()
  const roleIndex = useMemo(() => indexRoles(rolesQuery.data), [rolesQuery.data])

  const mutation = useMutation({
    mutationFn: (definition: Record<string, unknown>) => createCapability(definition),
    onSuccess: (created) => {
      setTemplateId('')
      setGroup('')
      setDescription('')
      setSql('')
      setParamsText('')
      setFormError(undefined)
      onCreated(created)
    },
    // 弹层内的服务端错误必须留在表单里：页面顶部的 notice 会被弹层挡住。
    onError: (error) => {
      setFormError(noticeFromError(error, '新建草案失败').description)
      onError(error)
    },
  })

  const groups = groupsQuery.data?.items ?? []
  const selectedGroup = groups.find((item) => item.code === group)

  return (
    <form
      className="create-form"
      onSubmit={(event) => {
        event.preventDefault()
        if (templateId.trim() === '') {
          setFormError('模板 ID 不能为空。')
          return
        }
        if (group === '') {
          setFormError('必须选择一个已存在的组：组决定哪些调用方能看到该能力。')
          return
        }
        const definition: Record<string, unknown> = { template_id: templateId.trim(), group }
        if (description.trim() !== '') definition.description = description.trim()
        if (sql.trim() !== '') definition.sql = sql.trim()
        if (paramsText.trim() !== '') {
          try {
            definition.params = JSON.parse(paramsText) as unknown
          } catch {
            setFormError('参数不是合法 JSON，请修正后再提交。')
            return
          }
        }
        setFormError(undefined)
        mutation.mutate(definition)
      }}
    >
      <div className="form-row">
        <label className="field">
          <span title="template_id">模板 ID</span>
          <input
            className="input"
            value={templateId}
            onChange={(event) => setTemplateId(event.target.value)}
            placeholder="例如 orders.daily_summary"
          />
        </label>
        <label className="field">
          <span title="group">
            组（必选） <span className="required-mark">*</span>
          </span>
          <select
            className="select"
            value={group}
            onChange={(event) => setGroup(event.target.value)}
            disabled={groupsQuery.isPending}
            required
          >
            <option value="">
              {groupsQuery.isPending
                ? '正在加载组…'
                : groupsQuery.isError
                  ? '组清单加载失败'
                  : '请选择组'}
            </option>
            {groups.map((item) => (
              <option key={item.code} value={item.code} title={item.code}>
                {`${item.name}（${
                  item.roles.length > 0
                    ? item.roles.map((code) => roleLabel(roleIndex, code)).join(' / ')
                    : '未绑定角色'
                }）`}
              </option>
            ))}
          </select>
        </label>
      </div>
      {groupsQuery.isError && <ErrorState error={groupsQuery.error} />}
      {selectedGroup !== undefined && selectedGroup.roles.length === 0 && (
        <p className="inline-error" role="alert">
          该组未绑定任何角色：能力创建后也不会出现在任何调用方的 tools/list 中。
        </p>
      )}
      <label className="field">
        <span title="description">描述</span>
        <input
          className="input"
          value={description}
          onChange={(event) => setDescription(event.target.value)}
          placeholder="给调用方看的一句话说明"
        />
      </label>
      <label className="field">
        <span title="sql">模板 SQL</span>
        <textarea
          className="input textarea"
          rows={4}
          value={sql}
          onChange={(event) => setSql(event.target.value)}
          placeholder="SELECT ..."
        />
      </label>
      <label className="field">
        <span title="params">参数（JSON）</span>
        <textarea
          className="input textarea"
          rows={3}
          value={paramsText}
          onChange={(event) => setParamsText(event.target.value)}
          placeholder='例如 {"order_id":{"type":"string","required":true}}'
        />
      </label>
      {formError !== undefined && (
        <p className="inline-error" role="alert">
          {formError}
        </p>
      )}
      <div className="form-actions">
        <button type="submit" className="btn btn-primary" disabled={mutation.isPending || groupsQuery.isPending}>
          {mutation.isPending ? '正在创建…' : '创建草案'}
        </button>
      </div>
    </form>
  )
}
