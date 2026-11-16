import { useMemo, useState } from 'react'
import { keepPreviousData, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { deleteGroup, fetchGroups } from '../../api/admin'
import { GROUPS_KEY, indexRoles, useRolesQuery } from '../../api/queries'
import type { GroupItem, GroupSource } from '../../api/types'
import { Badge } from '../../components/Badge'
import { Card } from '../../components/Card'
import { DataTable, type DataTableColumn } from '../../components/DataTable'
import { InfoBar } from '../../components/InfoBar'
import { Modal } from '../../components/Modal'
import { EmptyState, ErrorState, LoadingState, PageShell } from '../../components/PageShell'
import type { Notice } from '../../notice'
import { GROUP_SOURCE_LABEL } from '../../labels'
import { useAdminView } from '../../viewStack'
import { GroupForm } from './GroupForm'
import {
  EMPTY_FILTER,
  SOURCE_OPTIONS,
  filterToQuery,
  groupDeleteError,
  type EditorTarget,
  type GroupFilter,
} from './groupFilter'
export default function GroupsPage() {
  const queryClient = useQueryClient()
  const { openTools } = useAdminView()
  const [notice, setNotice] = useState<Notice | undefined>(undefined)
  const [editor, setEditor] = useState<EditorTarget | null>(null)
  const [deleteTarget, setDeleteTarget] = useState<GroupItem | null>(null)
  const [deleteError, setDeleteError] = useState<Notice | undefined>(undefined)
  const [draftFilter, setDraftFilter] = useState<GroupFilter>(EMPTY_FILTER)
  const [filter, setFilter] = useState<GroupFilter>(EMPTY_FILTER)

  const rolesQuery = useRolesQuery()
  const hasFilter = filter.q.trim() !== '' || filter.source !== '' || filter.role !== ''
  const appliedQuery = filterToQuery(filter)
  /**
   * 无筛选时复用共享的 ['groups'] 缓存（其他页面也在用），有筛选才单独取一页；
   * 两者都以 ['groups'] 为前缀，写操作失效一次即可覆盖。
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
   * 组写操作会改变工具台账的 group 归属；managed 组的 roles 还会反查进角色默认组，
   * 因此角色清单与令牌选项（默认组/组来源）也一起失效，避免令牌表单读到旧的默认组。
   */
  const refreshAfterWrite = () => {
    void queryClient.invalidateQueries({ queryKey: ['groups'] })
    void queryClient.invalidateQueries({ queryKey: ['roles'] })
    void queryClient.invalidateQueries({ queryKey: ['token-options'] })
    void queryClient.invalidateQueries({ queryKey: ['tool-stats'] })
  }

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
    onError: (error) => setDeleteError(groupDeleteError(error)),
  })

  /**
   * 列宽合计 1120px（190+104+80+96+314+76+76+184），与表格 minWidth 一致：
   * 1440 下 content 区正好容纳，表格不再被拉伸，冻结操作列实际渲染 184px；768 下仍横向滚动。
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
              ? '策略组：由 config/policy.toml 维护，不可在此编辑'
              : row.source === 'builtin'
                ? '内置组：后端合成的只读组，不可编辑或删除'
                : '受管组：可在管理台编辑或删除'
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
      numeric: true,
      width: '76px',
      cell: (row) => row.capability_count,
    },
    {
      key: 'token_count',
      header: '令牌数',
      numeric: true,
      width: '76px',
      cell: (row) => row.token_count,
    },
    {
      key: 'actions',
      header: '操作',
      width: '184px',
      sticky: true,
      cell: (row) => (
        <div className="row-actions">
          <button
            type="button"
            className="btn btn-secondary btn-sm"
            title={`查看 ${row.name} 的组能力`}
            aria-label={`查看 ${row.name} 的组能力`}
            onClick={() => openTools({ group: row.code })}
          >
            查看
          </button>
          {/* 策略组/内置组只读：不给禁用按钮，只用短提示与来源徽标说明，避免每行重复一段文字。 */}
          {row.source === 'builtin' && (
            <span className="row-note" title="内置组由后端合成，grantable=false">
              不可编辑
            </span>
          )}
          {row.source === 'managed' && row.grantable !== false && (
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
      : `${groupsQuery.data.total} 个组${hasFilter ? ' · 已筛选' : ''} · 策略组来自 config/policy.toml，内置组由后端合成；受管组可在此维护`

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
          <label className="field field-grow">
            <span>搜索</span>
            <input
              className="input"
              type="search"
              value={draftFilter.q}
              onChange={(event) => setDraftFilter({ ...draftFilter, q: event.target.value })}
              placeholder="组 code 或中文名"
            />
          </label>
          <label className="field">
            <span>来源</span>
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
          </label>
          <label className="field">
            <span>角色</span>
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
          </label>
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
            hint="没有符合搜索/来源/角色条件的组。清空筛选后可看到全部组。"
            action={
              <button type="button" className="btn btn-secondary" onClick={clearFilter}>
                清空筛选
              </button>
            }
          />
        )}
        {groupsQuery.data !== undefined && groups.length === 0 && !hasFilter && (
          <EmptyState
            hint="还没有组定义。policy 组来自 config/policy.toml；内置组由后端合成；managed 组可用右上角「新建组」创建。"
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
            minWidth="1120px"
          />
        )}
      </Card>

      {editor !== null && (
        <Modal
          size="lg"
          title={
            editor.mode === 'create'
              ? '新建组'
              : `编辑组 · ${editor.group.name}`
          }
          subtitle={editor.mode === 'edit' ? `组 code 不可修改：${editor.group.code}` : undefined}
          onClose={() => setEditor(null)}
        >
          <GroupForm
            target={editor}
            roles={roles}
            rolesLoading={rolesQuery.isPending}
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
            onError={setNotice}
          />
        </Modal>
      )}

      {deleteTarget !== null && (
        <Modal
          size="md"
          title={`删除组 · ${deleteTarget.name}`}
          onClose={() => setDeleteTarget(null)}
          footer={
            <>
              <button
                type="button"
                className="btn btn-secondary"
                disabled={deleteMutation.isPending}
                onClick={() => setDeleteTarget(null)}
              >
                取消
              </button>
              <button
                type="button"
                className="btn btn-danger"
                disabled={deleteMutation.isPending}
                onClick={() => deleteMutation.mutate(deleteTarget)}
              >
                {deleteMutation.isPending ? '删除中…' : '确认删除'}
              </button>
            </>
          }
        >
          <p className="hint">
            将删除受管组 {deleteTarget.name}（{deleteTarget.code}）。当前被{' '}
            {deleteTarget.capability_count} 个能力、{deleteTarget.token_count} 个令牌引用。
            删除后不可恢复；仍被引用时删除会被拒绝。
          </p>
          {deleteError !== undefined && (
            <InfoBar
              severity={deleteError.severity}
              title={deleteError.title}
              description={deleteError.description}
              meta={deleteError.meta}
            />
          )}
        </Modal>
      )}
    </PageShell>
  )
}
