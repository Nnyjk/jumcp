import { useMemo, useState } from 'react'
import { keepPreviousData, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { createGroup, deleteGroup, fetchGroups, updateGroup } from '../api/admin'
import { ApiError } from '../api/client'
import { GROUPS_KEY, indexRoles, useRolesQuery } from '../api/queries'
import type { GroupItem, GroupQuery, GroupSource, RoleItem } from '../api/types'
import { Badge } from '../components/Badge'
import { Card } from '../components/Card'
import { DataTable, type DataTableColumn } from '../components/DataTable'
import { InfoBar } from '../components/InfoBar'
import { Modal } from '../components/Modal'
import {
  EmptyState,
  ErrorState,
  LoadingState,
  PageShell,
} from '../components/PageShell'
import { GROUP_SOURCE_LABEL } from '../labels'
import { noticeFromError, type Notice } from '../notice'
import { useAdminView } from '../viewStack'

type EditorTarget = { mode: 'create' } | { mode: 'edit'; group: GroupItem }

interface GroupFilter {
  q: string
  source: '' | GroupSource
  role: string
}

const EMPTY_FILTER: GroupFilter = { q: '', source: '', role: '' }

const SOURCE_OPTIONS: Array<{ value: '' | GroupSource; label: string }> = [
  { value: '', label: '全部来源' },
  { value: 'policy', label: GROUP_SOURCE_LABEL.policy },
  { value: 'managed', label: GROUP_SOURCE_LABEL.managed },
]

/** priority / min_budget 都是整数；后端要求 priority ≥ 0、min_budget > 0，空串不当作 0。 */
function parseInteger(raw: string, minimum: number): number | undefined {
  if (raw.trim() === '') return undefined
  const value = Number(raw)
  return Number.isInteger(value) && value >= minimum ? value : undefined
}

function filterToQuery(filter: GroupFilter): GroupQuery {
  return {
    q: filter.q.trim() || undefined,
    source: filter.source === '' ? undefined : filter.source,
    role: filter.role || undefined,
  }
}

/** 409 表示组仍被 capability / token 引用：两个计数都展示，并保留后端原始消息。 */
function groupDeleteError(error: unknown): Notice {
  if (error instanceof ApiError && error.status === 409) {
    const data = error.data as Record<string, unknown> | undefined
    const capabilityCount = data?.capability_count
    const tokenCount = data?.token_count
    const text = (value: unknown) =>
      typeof value === 'number' || typeof value === 'string' ? String(value) : '—'
    const raw = error.message.trim()
    return {
      severity: 'danger',
      title: '组仍被引用',
      description: `删除被拒绝：能力 ${text(capabilityCount)} 个、令牌 ${text(tokenCount)} 个仍引用该组。${
        raw === '' ? '请先迁移或删除这些引用后重试。' : `后端提示：${raw}`
      }`,
      meta: `${error.code} · HTTP 409`,
    }
  }
  return noticeFromError(error, '删除组失败')
}

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
          tone={row.source === 'policy' ? 'accent' : 'neutral'}
          title={row.source === 'policy' ? '策略组：由 config/policy.toml 维护，不可在此编辑' : 'managed'}
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
          {/* 策略组只读：不给禁用按钮，只保留来源徽标的 title 说明，避免每行重复一段文字。 */}
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
      : `${groupsQuery.data.total} 个组${hasFilter ? ' · 已筛选' : ''} · 策略组来自 config/policy.toml，只读`

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
            hint="还没有组定义。policy 组来自 config/policy.toml；managed 组可用右上角「新建组」创建。"
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

/** 新建/编辑共用表单：code 仅在新建时可填；角色用多选，发送 code。 */
function GroupForm({
  target,
  roles,
  rolesLoading,
  onSaved,
  onError,
}: {
  target: EditorTarget
  roles: RoleItem[]
  rolesLoading: boolean
  onSaved: (item: GroupItem) => void
  onError: (notice: Notice) => void
}) {
  const existing = target.mode === 'edit' ? target.group : undefined
  const [code, setCode] = useState(existing?.code ?? '')
  const [name, setName] = useState(existing?.name ?? '')
  const [priority, setPriority] = useState(String(existing?.priority ?? 0))
  const [minBudget, setMinBudget] = useState(
    existing === undefined ? '800' : String(existing.min_budget),
  )
  const [selectedRoles, setSelectedRoles] = useState<string[]>(existing?.roles ?? [])
  const [formError, setFormError] = useState<string | undefined>(undefined)

  /** 已有组引用的角色若不在角色清单里，以 code 兜底展示，避免保存时静默丢角色。 */
  const roleChoices = useMemo(() => {
    const choices = roles.map((role) => ({ code: role.code, name: role.name }))
    for (const roleCode of existing?.roles ?? []) {
      if (!choices.some((choice) => choice.code === roleCode)) {
        choices.push({ code: roleCode, name: roleCode })
      }
    }
    return choices
  }, [roles, existing])

  const mutation = useMutation({
    mutationFn: (input: {
      code: string
      name: string
      priority: number
      min_budget: number
      roles: string[]
    }) => {
      if (existing === undefined) return createGroup(input)
      return updateGroup(existing.code, {
        name: input.name,
        priority: input.priority,
        min_budget: input.min_budget,
        roles: input.roles,
      })
    },
    onSuccess: onSaved,
    onError: (error) => {
      const notice = noticeFromError(error, existing === undefined ? '新建组失败' : '更新组失败')
      setFormError(notice.description)
      onError(notice)
    },
  })

  const toggleRole = (roleCode: string, checked: boolean) => {
    setSelectedRoles((current) =>
      checked
        ? current.includes(roleCode)
          ? current
          : [...current, roleCode]
        : current.filter((item) => item !== roleCode),
    )
  }

  const submit = () => {
    const trimmedCode = existing?.code ?? code.trim()
    const trimmedName = name.trim()
    if (existing === undefined) {
      if (trimmedCode === '') {
        setFormError('组 code 不能为空。')
        return
      }
      if (!/^[a-z0-9._-]{1,64}$/.test(trimmedCode)) {
        setFormError('组 code 只允许小写字母、数字与 ._-，最长 64 字符。')
        return
      }
    }
    if (trimmedName === '') {
      setFormError('组中文名不能为空。')
      return
    }
    const parsedPriority = parseInteger(priority, 0)
    if (parsedPriority === undefined) {
      setFormError('priority 必须是非负整数。')
      return
    }
    const parsedBudget = parseInteger(minBudget, 1)
    if (parsedBudget === undefined) {
      setFormError('min_budget 必须是正整数。')
      return
    }
    if (selectedRoles.length === 0) {
      setFormError('至少选择一个角色。')
      return
    }
    setFormError(undefined)
    mutation.mutate({
      code: trimmedCode,
      name: trimmedName,
      priority: parsedPriority,
      min_budget: parsedBudget,
      roles: selectedRoles,
    })
  }

  return (
    <form
      className="create-form"
      onSubmit={(event) => {
        event.preventDefault()
        submit()
      }}
    >
      <div className="form-row">
        <label className="field">
          <span title="code">组 code</span>
          <input
            className="input mono"
            value={existing?.code ?? code}
            readOnly={existing !== undefined}
            onChange={(event) => setCode(event.target.value)}
            placeholder="例如 finance"
          />
        </label>
        <label className="field">
          <span title="name">组中文名</span>
          <input
            className="input"
            value={name}
            onChange={(event) => setName(event.target.value)}
            placeholder="例如 财务组"
          />
        </label>
      </div>
      <div className="form-row">
        <label className="field">
          <span title="priority">优先级</span>
          <input
            className="input"
            type="number"
            min={0}
            step={1}
            value={priority}
            onChange={(event) => setPriority(event.target.value)}
          />
        </label>
        <label className="field">
          <span title="min_budget">最小预算</span>
          <input
            className="input"
            type="number"
            min={1}
            step={1}
            value={minBudget}
            onChange={(event) => setMinBudget(event.target.value)}
          />
        </label>
      </div>
      <fieldset className="check-fieldset">
        <legend>角色</legend>
        {rolesLoading ? (
          <p className="hint">正在加载角色清单…</p>
        ) : roleChoices.length === 0 ? (
          <p className="hint">角色清单为空：请先在策略中定义角色。</p>
        ) : (
          <div className="check-grid">
            {roleChoices.map((role) => (
              <label key={role.code} className="check-item" title={role.code}>
                <input
                  type="checkbox"
                  checked={selectedRoles.includes(role.code)}
                  onChange={(event) => toggleRole(role.code, event.target.checked)}
                />
                <span>{role.name}</span>
              </label>
            ))}
          </div>
        )}
      </fieldset>
      {formError !== undefined && (
        <p className="inline-error" role="alert">
          {formError}
        </p>
      )}
      <div className="form-actions">
        <button type="submit" className="btn btn-primary" disabled={mutation.isPending}>
          {mutation.isPending ? '保存中…' : existing === undefined ? '创建组' : '保存修改'}
        </button>
      </div>
    </form>
  )
}
