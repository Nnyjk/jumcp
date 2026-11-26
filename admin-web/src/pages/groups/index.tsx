import { useMemo, useState } from 'react'
import { keepPreviousData, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { deleteGroup, fetchGroups } from '../../api/admin'
import { GROUPS_KEY, indexRoles, useRolesQuery } from '../../api/queries'
import type { GroupItem, GroupSource } from '../../api/types'
import { Badge } from '../../components/Badge'
import { Card } from '../../components/Card'
import { ConfirmDialog } from '../../components/ConfirmDialog'
import { DataTable, type DataTableColumn } from '../../components/DataTable'
import { Field } from '../../components/Field'
import { InfoBar } from '../../components/InfoBar'
import { EmptyState, ErrorState, LoadingState, PageShell } from '../../components/PageShell'
import type { Notice } from '../../notice'
import { GROUP_SOURCE_LABEL } from '../../labels'
import { useAdminView } from '../../viewStack'
import { GroupForm } from './GroupForm'
import { ExposureDialog } from './ExposureDialog'
import {
  EMPTY_FILTER,
  SOURCE_OPTIONS,
  filterToQuery,
  groupDeleteMessage,
  type EditorTarget,
  type GroupFilter,
} from './groupFilter'
export default function GroupsPage() {
  const queryClient = useQueryClient()
  const { openTools } = useAdminView()
  const [notice, setNotice] = useState<Notice | undefined>(undefined)
  const [editor, setEditor] = useState<EditorTarget | null>(null)
  const [exposureTarget, setExposureTarget] = useState<GroupItem | null>(null)
  const [deleteTarget, setDeleteTarget] = useState<GroupItem | null>(null)
  const [deleteError, setDeleteError] = useState<string | undefined>(undefined)
  const [draftFilter, setDraftFilter] = useState<GroupFilter>(EMPTY_FILTER)
  const [filter, setFilter] = useState<GroupFilter>(EMPTY_FILTER)

  const rolesQuery = useRolesQuery()
  const hasFilter = filter.q.trim() !== '' || filter.source !== '' || filter.role !== ''
  const appliedQuery = filterToQuery(filter)
  /**
   * 缓存隔离约束：无筛选时复用全局共享的 ['groups'] 查询缓存，有筛选条件时以 appliedQuery 区分独立缓存项；
   * 两者统一以 ['groups'] 作为键前缀，任何写操作只需整体使 ['groups'] 失效即可保证数据一致性。
   */
  const groupsQuery = useQuery({
    queryKey: hasFilter ? ['groups', appliedQuery] : GROUPS_KEY,
    queryFn: ({ signal }) => fetchGroups(appliedQuery, signal),
    placeholderData: keepPreviousData,
  })

  const roles = rolesQuery.data?.items ?? []
  const roleIndex = useMemo(() => indexRoles(rolesQuery.data), [rolesQuery.data])
  const groups = groupsQuery.data?.items ?? []

  const roleOptions = useMemo(() => {
    const options = (rolesQuery.data?.items ?? []).map((role) => ({
      code: role.code,
      name: role.name,
    }))
    if (filter.role !== '' && !options.some((option) => option.code === filter.role)) {
      options.push({ code: filter.role, name: filter.role })
    }
    return options
  }, [rolesQuery.data, filter.role])

  /**
   * 拓扑联动失效约束：受管组写操作会直接影响受管能力的所属组归属；且受管组所含 roles 会反查写入角色的默认组拓扑，
   * 因此必须联动使角色清单（roles）、令牌配置项（token-options）及工具统计（tool-stats）全量失效，避免关联表单读取脏数据。
   */
  const refreshAfterWrite = () => {
    void queryClient.invalidateQueries({ queryKey: ['groups'] })
    void queryClient.invalidateQueries({ queryKey: ['roles'] })
    void queryClient.invalidateQueries({ queryKey: ['token-options'] })
    void queryClient.invalidateQueries({ queryKey: ['tool-stats'] })
  }

  /**
   * 删除流程约束：采用自定义 ConfirmDialog 代替原生 window.confirm，
   * 支持异步提交中的 pending 加载态锁定，并在 409 等依赖冲突时将错误原因与引用计数呈现在弹窗内部。
   */
  const deleteMutation = useMutation({
    mutationFn: (group: GroupItem) => deleteGroup(group.code),
    onSuccess: (_result, group) => {
      setDeleteTarget(null)
      setDeleteError(undefined)
      setNotice({
        severity: 'success',
        title: '组已删除',
        description: `受管组 ${group.name}（${group.code}）已删除。`,
        meta: undefined,
      })
      refreshAfterWrite()
    },
    onError: (error) => setDeleteError(groupDeleteMessage(error)),
  })

  /**
   * 布局与滚动约束：列宽合计 1204px（190+104+80+96+314+84+84+252），严格与 DataTable minWidth 对齐；
   * 确保 1440px 桌面视口下内容区域自然契合不被异常拉伸，冻结操作列固定 252px，同时在小屏幕下保持横向滚动。
   */
  const columns: Array<DataTableColumn<GroupItem>> = [
    {
      key: 'name',
      header: '组名',
      width: '190px',
      cell: (row) => (
        <span className="cell-stack">
          <span>{row.name}</span>
          <span className="cell-sub mono">{row.code}</span>
        </span>
      ),
      title: (row) => row.code,
    },
    {
      key: 'source',
      header: '来源',
      width: '104px',
      cell: (row) => (
        <Badge
          tone={row.source === 'managed' ? 'neutral' : 'accent'}
          title={
            row.source === 'policy'
              ? '策略组：由 config/policy.toml 静态配置，不可在管理台编辑'
              : row.source === 'builtin'
                ? '内置组：后端内置定义，配置不可编辑；basic 是常驻元工具所属的基础组，可作为授权组'
                : '受管组：由管理台维护，可编辑或删除'
          }
        >
          {GROUP_SOURCE_LABEL[row.source]}
        </Badge>
      ),
    },
    {
      key: 'priority',
      header: '优先级',
      numeric: true,
      width: '80px',
      cell: (row) => row.priority,
    },
    {
      key: 'min_budget',
      header: '最小预算',
      numeric: true,
      width: '96px',
      cell: (row) => row.min_budget,
    },
    {
      key: 'roles',
      header: '角色',
      width: '314px',
      wrap: true,
      cell: (row) =>
        row.roles.length === 0 ? (
          '—'
        ) : (
          <span className="detail-badges">
            {row.roles.map((code) => (
              <Badge key={code} title={code}>
                {roleIndex.get(code)?.name ?? code}
              </Badge>
            ))}
          </span>
        ),
    },
    {
      key: 'capability_count',
      header: '能力数',
      width: '84px',
      cell: (row) => (
        <Badge tone="neutral" title={`该组被 ${row.capability_count} 个受管能力引用`}>
          {row.capability_count}
        </Badge>
      ),
    },
    {
      key: 'token_count',
      header: '令牌数',
      width: '84px',
      cell: (row) => (
        <Badge tone="neutral" title={`该组被 ${row.token_count} 个访问令牌引用`}>
          {row.token_count}
        </Badge>
      ),
    },
    {
      key: 'actions',
      header: '操作',
      width: '252px',
      sticky: true,
      cell: (row) => (
        <div className="row-actions">
          <button
            type="button"
            className="btn btn-secondary btn-sm"
            title={`查看 ${row.name} 的受管能力`}
            aria-label={`查看 ${row.name} 的受管能力`}
            onClick={() => openTools({ group: row.code })}
          >
            查看
          </button>
          {/* 暴露矩阵与组来源无关：任何组都能被授予上游与工具，basic 也是普通可授权组。 */}
          <button
            type="button"
            className="btn btn-secondary btn-sm"
            title={`调整 ${row.name} 的上游授予与逐工具规则`}
            onClick={() => setExposureTarget(row)}
          >
            暴露
          </button>
          {/* 只读约束只针对组定义本身：内置组的名称与角色组合来自后端，不提供编辑/删除按钮。 */}
          {row.source === 'builtin' && (
            <span
              className="row-note"
              title="内置组由后端内置定义，名称与角色组合不可改；basic 是内置基础组：常驻元工具属于它，可作为上游/工具的授权组"
            >
              不可改配置
            </span>
          )}
          {row.source === 'managed' && (
            <>
              <button
                type="button"
                className="btn btn-secondary btn-sm"
                onClick={() => setEditor({ mode: 'edit', group: row })}
              >
                编辑
              </button>
              <button
                type="button"
                className="btn btn-danger btn-sm"
                onClick={() => {
                  setDeleteError(undefined)
                  setDeleteTarget(row)
                }}
              >
                删除
              </button>
            </>
          )}
        </div>
      ),
    },
  ]

  const subtitle =
    groupsQuery.data === undefined
      ? groupsQuery.isError
        ? '查询失败'
        : '查询中'
      : `${groupsQuery.data.total} 个组${hasFilter ? ' · 已筛选' : ''} · 策略组来自 config/policy.toml，内置组由后端内置定义；受管组由管理台维护 · 任何组都可授权上游与工具`

  const clearFilter = () => {
    setDraftFilter(EMPTY_FILTER)
    setFilter(EMPTY_FILTER)
  }

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
        <form
          className="filter-bar"
          onSubmit={(event) => {
            event.preventDefault()
            setFilter(draftFilter)
          }}
        >
          <Field label="搜索">
            <input
              className="input"
              type="search"
              value={draftFilter.q}
              onChange={(event) => setDraftFilter({ ...draftFilter, q: event.target.value })}
              placeholder="组 code 或中文名"
            />
          </Field>
          <Field label="来源">
            <select
              className="select"
              value={draftFilter.source}
              onChange={(event) =>
                setDraftFilter({ ...draftFilter, source: event.target.value as '' | GroupSource })
              }
            >
              {SOURCE_OPTIONS.map((option) => (
                <option key={option.value} value={option.value}>
                  {option.label}
                </option>
              ))}
            </select>
          </Field>
          <Field label="角色">
            <select
              className="select"
              value={draftFilter.role}
              onChange={(event) => setDraftFilter({ ...draftFilter, role: event.target.value })}
            >
              <option value="">全部角色</option>
              {roleOptions.map((option) => (
                <option key={option.code} value={option.code} title={option.code}>
                  {option.name}
                </option>
              ))}
            </select>
          </Field>
          <div className="filter-actions">
            <button type="submit" className="btn btn-primary">
              查询
            </button>
            <button type="button" className="btn btn-secondary" onClick={clearFilter}>
              重置
            </button>
          </div>
        </form>
        {rolesQuery.isError && (
          <p className="hint">角色清单加载失败：角色筛选与表单里的角色多选暂不可用。</p>
        )}
      </Card>

      <Card
        title="组清单"
        subtitle={subtitle}
        actions={
          <button
            type="button"
            className="btn btn-primary btn-sm"
            onClick={() => setEditor({ mode: 'create' })}
          >
            新建组
          </button>
        }
        flush
      >
        {groupsQuery.isPending && <LoadingState rows={6} label="正在加载组清单" />}
        {groupsQuery.isError && (
          <div className="state-wrap">
            <ErrorState error={groupsQuery.error} />
            <button
              type="button"
              className="btn btn-secondary"
              onClick={() => groupsQuery.refetch()}
              disabled={groupsQuery.isFetching}
            >
              重试
            </button>
          </div>
        )}
        {groupsQuery.data !== undefined && groups.length === 0 && hasFilter && (
          <EmptyState
            hint="暂无符合筛选条件的组。清空筛选后可查看全部组。"
            action={
              <button type="button" className="btn btn-secondary" onClick={clearFilter}>
                清空筛选
              </button>
            }
          />
        )}
        {groupsQuery.data !== undefined && groups.length === 0 && !hasFilter && (
          <EmptyState
            hint="暂无组定义。策略组由 config/policy.toml 配置，内置组由后端内置定义；受管组可点击右上角「新建组」创建。"
            action={
              <button
                type="button"
                className="btn btn-secondary"
                onClick={() => setEditor({ mode: 'create' })}
              >
                新建组
              </button>
            }
          />
        )}
        {groups.length > 0 && (
          <DataTable
            label="组清单"
            columns={columns}
            rows={groups}
            rowKey={(row) => row.code}
            minWidth="1204px"
          />
        )}
      </Card>

      {editor !== null && (
        <GroupForm
          target={editor}
          roles={roles}
          rolesLoading={rolesQuery.isPending}
          onClose={() => setEditor(null)}
          onSaved={(item) => {
            const created = editor.mode === 'create'
            setEditor(null)
            setNotice({
              severity: 'success',
              title: created ? '组已创建' : '组已更新',
              description: `${created ? '受管组' : '组'} ${item.name}（${item.code}）已保存。`,
              meta: `优先级 ${item.priority} · 最小预算 ${item.min_budget} · 角色 ${item.roles
                .map((code) => roleIndex.get(code)?.name ?? code)
                .join('、')}`,
            })
            refreshAfterWrite()
          }}
        />
      )}

      {exposureTarget !== null && (
        <ExposureDialog
          code={exposureTarget.code}
          name={exposureTarget.name}
          onClose={() => setExposureTarget(null)}
          onSaved={() => {
            setExposureTarget(null)
            setNotice({
              severity: 'success',
              title: '暴露矩阵已更新',
              description: `组 ${exposureTarget.name}（${exposureTarget.code}）的上游授予与逐工具规则已保存。`,
              meta: undefined,
            })
            refreshAfterWrite()
          }}
        />
      )}

      {deleteTarget !== null && (
        <ConfirmDialog
          title={`删除组 · ${deleteTarget.name}`}
          subtitle="删除后不可恢复；若仍被受管能力或访问令牌引用，删除操作将被后端拦截拒绝。"
          confirmLabel="确认删除"
          pendingLabel="删除中…"
          tone="danger"
          pending={deleteMutation.isPending}
          error={deleteError}
          errorTitle="删除组失败"
          onClose={() => {
            setDeleteTarget(null)
            setDeleteError(undefined)
          }}
          onConfirm={() => deleteMutation.mutate(deleteTarget)}
        >
          <p className="hint">
            即将删除受管组 {deleteTarget.name}（{deleteTarget.code}）。当前被{' '}
            {deleteTarget.capability_count} 个受管能力、{deleteTarget.token_count} 个访问令牌引用。
          </p>
        </ConfirmDialog>
      )}
    </PageShell>
  )
}
