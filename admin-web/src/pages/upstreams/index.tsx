import { useMemo, useState } from 'react'
import { gsap } from 'gsap'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  deleteUpstream,
  fetchUpstreams,
  refreshUpstream,
  updateUpstream,
} from '../../api/admin'
import { UPSTREAMS_KEY, groupLabel, indexGroups, useGroupsQuery } from '../../api/queries'
import type { UpstreamItem } from '../../api/types'
import { Badge, CategoryBadge, UpstreamStatusBadge } from '../../components/Badge'
import { Card } from '../../components/Card'
import { ConfirmDialog } from '../../components/ConfirmDialog'
import { MOTION_DURATION, MOTION_EASE, useGsapScope } from '../../motion/useGsap'
import { DataTable, type DataTableColumn } from '../../components/DataTable'
import { InfoBar } from '../../components/InfoBar'
import { Modal } from '../../components/Modal'
import { EmptyState, ErrorState, LoadingState, PageShell } from '../../components/PageShell'
import { TabBar, TabPanel, type TabItem } from '../../components/TabBar'
import { DASH, formatDateTime } from '../../format'
import { UPSTREAM_CATEGORY_LABEL, UPSTREAM_STATUS_LABEL, UPSTREAM_TRANSPORT_LABEL } from '../../labels'
import { noticeFromError, type Notice } from '../../notice'
import { BasicToolsModal } from './BasicToolsModal'
import { CatalogTab } from './CatalogTab'
import { Field } from '../../components/Field'
import { UpstreamForm } from './UpstreamForm'
import {
  CATEGORY_OPTIONS,
  GROUP_OPTIONS_LIMIT,
  targetText,
  toInput,
  type EditorTarget,
} from './upstreamTools'

type UpstreamTab = 'list' | 'catalog'

const TABS: TabItem<UpstreamTab>[] = [
  { key: 'list', label: '接入清单' },
  { key: 'catalog', label: '安装目录' },
]

export default function UpstreamsPage() {
  const queryClient = useQueryClient()
  const [tab, setTab] = useState<UpstreamTab>('list')
  const [notice, setNotice] = useState<Notice | undefined>(undefined)
  const [editor, setEditor] = useState<EditorTarget | null>(null)
  const [basicTarget, setBasicTarget] = useState<UpstreamItem | null>(null)
  const [deleteTarget, setDeleteTarget] = useState<UpstreamItem | null>(null)
  const [deleteError, setDeleteError] = useState<unknown>(undefined)
  const [filter, setFilter] = useState({ q: '', group: '', category: '', status: '' })

  const upstreamsQuery = useQuery({
    queryKey: UPSTREAMS_KEY,
    queryFn: ({ signal }) => fetchUpstreams(signal),
  })
  const groupsQuery = useGroupsQuery(GROUP_OPTIONS_LIMIT)
  const groups = useMemo(() => groupsQuery.data?.items ?? [], [groupsQuery.data])
  const groupIndex = useMemo(() => indexGroups(groupsQuery.data), [groupsQuery.data])
  /** 筛选下拉只列可授予的组；组清单拉取失败时下拉退化为空，不会静默过滤掉行。 */
  const groupChoices = useMemo(
    () => groups.filter((item) => item.grantable !== false).map((item) => ({ code: item.code, name: item.name })),
    [groups],
  )
  const rows = useMemo(() => upstreamsQuery.data?.items ?? [], [upstreamsQuery.data])

  /** 筛选只影响展示，不改后端查询；状态与分类沿用契约里的原始取值。 */
  const filteredRows = useMemo(() => {
    const keyword = filter.q.trim().toLowerCase()
    return rows.filter((row) => {
      if (keyword !== '' && !row.name.toLowerCase().includes(keyword) && !row.id.toLowerCase().includes(keyword)) {
        return false
      }
      if (filter.group !== '' && row.group !== filter.group) return false
      if (filter.category !== '' && row.category !== filter.category) return false
      if (filter.status === 'enabled' && !row.enabled) return false
      if (filter.status === 'disabled' && row.enabled) return false
      return true
    })
  }, [rows, filter])
  const filterActive = filter.q !== '' || filter.group !== '' || filter.category !== '' || filter.status !== ''
  const filterSignature = `${filter.q}|${filter.group}|${filter.category}|${filter.status}`
  const rowSignature = rows.map((row) => row.id).join('\u0000')

  /** 列表进入动效：数据或筛选变化时重播一次，表格自身的行动画在其内部叠加。 */
  const listRef = useGsapScope<HTMLDivElement>(
    (scope) => {
      gsap.from(scope, {
        opacity: 0,
        y: 6,
        duration: MOTION_DURATION.fast,
        ease: MOTION_EASE,
        clearProps: 'opacity,transform',
      })
    },
    [rowSignature, filterSignature],
  )

  /** 上游写操作会改变 router 暴露的工具集合，台账一并失效。 */
  const refreshAfterWrite = () => {
    void queryClient.invalidateQueries({ queryKey: UPSTREAMS_KEY })
    void queryClient.invalidateQueries({ queryKey: ['tool-stats'] })
  }

  const refreshMutation = useMutation({
    mutationFn: (id: string) => refreshUpstream(id),
    onSuccess: (item) => {
      const description = `上游 ${item.name}（${item.id}）${UPSTREAM_STATUS_LABEL[item.status]}，当前 ${item.tool_count} 个工具${
        item.status_message.trim() === '' ? '' : ` · ${item.status_message.trim()}`
      }。`
      setNotice({
        severity:
          item.status === 'ok' ? 'success' : item.status === 'error' ? 'danger' : 'warning',
        title: item.status === 'ok' ? '连接正常' : '连接测试完成',
        description,
        meta: `${item.status} · ${formatDateTime(item.last_checked_at)}`,
      })
      refreshAfterWrite()
    },
    onError: (error) => setNotice(noticeFromError(error, '测试连接失败')),
  })

  const toggleMutation = useMutation({
    mutationFn: (item: UpstreamItem) => updateUpstream(item.id, toInput(item, !item.enabled)),
    onSuccess: (item) => {
      setNotice({
        severity: 'success',
        title: item.enabled ? '上游已启用' : '上游已停用',
        description: `上游 ${item.name}（${item.id}）已${item.enabled ? '启用' : '停用'}，router 将${
          item.enabled ? '重新暴露' : '移除'
        }其工具。`,
        meta: undefined,
      })
      refreshAfterWrite()
    },
    onError: (error) => setNotice(noticeFromError(error, '更新启用状态失败')),
  })

  const deleteMutation = useMutation({
    mutationFn: (item: UpstreamItem) => deleteUpstream(item.id),
    onSuccess: (_result, item) => {
      setDeleteTarget(null)
      setDeleteError(undefined)
      setNotice({
        severity: 'success',
        title: '上游已删除',
        description: `上游 ${item.name}（${item.id}）已从 router 移除，其 ${item.tool_count} 个工具不再暴露；审计事件保留。`,
        meta: undefined,
      })
      refreshAfterWrite()
    },
    onError: (error) => setDeleteError(error),
  })

  /**
   * 列宽合计 1184px：名称 180 + 传输 64 + 状态 120 + 目标 220 + 授权组 120 +
   * 分类 88 + 只读 64 + 操作 328，1440 视口下全部可见（操作列仍 sticky 兜底窄屏）。
   */
  const columns: Array<DataTableColumn<UpstreamItem>> = [
    {
      key: 'name',
      header: '名称 / ID',
      width: '180px',
      cell: (row) => (
        <span className="cell-stack">
          <span>{row.name}</span>
          <span className="cell-sub mono">{row.id}</span>
        </span>
      ),
      title: (row) => `${row.name} · ${row.id}`,
    },
    {
      key: 'transport',
      header: '传输',
      width: '64px',
      cell: (row) => (
        <Badge tone={row.transport === 'http' ? 'accent' : 'neutral'} title={row.transport}>
          {UPSTREAM_TRANSPORT_LABEL[row.transport]}
        </Badge>
      ),
    },
    {
      key: 'status',
      header: '状态',
      width: '120px',
      cell: (row) => (
        <span className="cell-stack">
          <span>
            <UpstreamStatusBadge status={row.status} />
          </span>
          <span className="cell-sub">{row.tool_count} 个工具</span>
        </span>
      ),
      title: (row) =>
        `${UPSTREAM_STATUS_LABEL[row.status]} · ${row.tool_count} 个工具 · 最近探测 ${formatDateTime(
          row.last_checked_at,
        )}${row.status_message.trim() === '' ? '' : ` · ${row.status_message.trim()}`}`,
    },
    {
      key: 'target',
      header: '目标',
      mono: true,
      width: '220px',
      cell: (row) => targetText(row),
      title: (row) => targetText(row),
    },
    {
      key: 'group',
      header: '授权组',
      width: '120px',
      cell: (row) =>
        row.group === '' ? DASH : <span title={row.group}>{groupLabel(groupIndex, row.group)}</span>,
      title: (row) => row.group,
    },
    {
      key: 'category',
      header: '分类',
      width: '88px',
      cell: (row) => <CategoryBadge category={row.category} />,
    },
    {
      key: 'actions',
      header: '操作',
      width: '328px',
      sticky: true,
      cell: (row) => {
        const refreshing = refreshMutation.isPending && refreshMutation.variables === row.id
        const toggling = toggleMutation.isPending && toggleMutation.variables?.id === row.id
        return (
          <div className="row-actions">
            <button
              type="button"
              className="btn btn-secondary btn-sm"
              onClick={() => refreshMutation.mutate(row.id)}
              disabled={refreshing}
              aria-label={`测试连接 ${row.id}`}
            >
              {refreshing ? '测试中…' : '测试连接'}
            </button>
            <button
              type="button"
              className="btn btn-secondary btn-sm"
              onClick={() => setBasicTarget(row)}
              aria-label={`管理 ${row.id} 的基础工具`}
            >
              基础工具
            </button>
            <button
              type="button"
              className="btn btn-secondary btn-sm"
              onClick={() => setEditor({ mode: 'edit', item: row })}
            >
              编辑
            </button>
            <button
              type="button"
              className="btn btn-secondary btn-sm"
              onClick={() => toggleMutation.mutate(row)}
              disabled={toggling}
              aria-label={`${row.enabled ? '停用' : '启用'} ${row.id}`}
            >
              {toggling ? '保存中…' : row.enabled ? '停用' : '启用'}
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
          </div>
        )
      },
    },
  ]

  const subtitle =
    upstreamsQuery.data === undefined
      ? upstreamsQuery.isError
        ? '查询失败'
        : '查询中'
      : `${upstreamsQuery.data.total} 个接入 · 状态来自最近一次探测`

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

      <Card title="MCP 接入" subtitle="上游接入清单与内置 MCP 安装目录">
        <TabBar idBase="upstreams" items={TABS} value={tab} onChange={setTab} label="MCP 接入分栏" />
      </Card>

      <TabPanel idBase="upstreams" tabKey={tab}>
        {tab === 'list' && (
          <Card
            title="上游接入清单"
            subtitle={subtitle}
            actions={
              <button
                type="button"
                className="btn btn-primary btn-sm"
                onClick={() => setEditor({ mode: 'create' })}
              >
                新建接入
              </button>
            }
            flush
          >
            {upstreamsQuery.isPending && <LoadingState rows={6} label="正在加载上游接入" />}
            {upstreamsQuery.isError && (
              <div className="state-wrap">
                <ErrorState error={upstreamsQuery.error} />
                <button
                  type="button"
                  className="btn btn-secondary"
                  onClick={() => upstreamsQuery.refetch()}
                  disabled={upstreamsQuery.isFetching}
                >
                  重试
                </button>
              </div>
            )}
            {upstreamsQuery.data !== undefined && rows.length === 0 && (
              <EmptyState
                hint="还没有 MCP 上游接入。可先在「安装目录」页签安装内置包，或用右上角「新建接入」创建自定义上游。"
                action={
                  <button
                    type="button"
                    className="btn btn-secondary"
                    onClick={() => setEditor({ mode: 'create' })}
                  >
                    新建接入
                  </button>
                }
              />
            )}
            {rows.length > 0 && (
              <div className="mcp-upstream-list">
                <div className="filter-bar">
                  <Field label="搜索" title="name / id">
                    <input
                      className="input"
                      type="search"
                      value={filter.q}
                      onChange={(event) => setFilter((previous) => ({ ...previous, q: event.target.value }))}
                      placeholder="按名称或 id 过滤"
                    />
                  </Field>
                  <Field label="授权组" title="group">
                    <select
                      className="select"
                      value={filter.group}
                      onChange={(event) => setFilter((previous) => ({ ...previous, group: event.target.value }))}
                    >
                      <option value="">全部组</option>
                      {groupChoices.map((item) => (
                        <option key={item.code} value={item.code} title={item.code}>
                          {item.name}
                        </option>
                      ))}
                    </select>
                  </Field>
                  <Field label="分类" title="category">
                    <select
                      className="select"
                      value={filter.category}
                      onChange={(event) =>
                        setFilter((previous) => ({ ...previous, category: event.target.value }))
                      }
                    >
                      <option value="">全部分类</option>
                      {CATEGORY_OPTIONS.map((value) => (
                        <option key={value} value={value}>
                          {UPSTREAM_CATEGORY_LABEL[value]}
                        </option>
                      ))}
                    </select>
                  </Field>
                  <Field label="状态" title="enabled">
                    <select
                      className="select"
                      value={filter.status}
                      onChange={(event) => setFilter((previous) => ({ ...previous, status: event.target.value }))}
                    >
                      <option value="">全部状态</option>
                      <option value="enabled">已启用</option>
                      <option value="disabled">已停用</option>
                    </select>
                  </Field>
                  <div className="filter-actions">
                    <button
                      type="button"
                      className="btn btn-secondary"
                      onClick={() => setFilter({ q: '', group: '', category: '', status: '' })}
                      disabled={!filterActive}
                    >
                      清空筛选
                    </button>
                  </div>
                </div>

                <div ref={listRef}>
                  {filteredRows.length === 0 ? (
                    <EmptyState
                      hint="没有符合筛选条件的接入。调整或清空筛选条件后再看。"
                      action={
                        <button
                          type="button"
                          className="btn btn-secondary"
                          onClick={() => setFilter({ q: '', group: '', category: '', status: '' })}
                        >
                          清空筛选
                        </button>
                      }
                    />
                  ) : (
                    <DataTable
                      label="MCP 上游接入清单"
                      meta={`显示 ${filteredRows.length} / 共 ${rows.length}`}
                      columns={columns}
                      rows={filteredRows}
                      rowKey={(row) => row.id}
                      minWidth="1184px"
                    />
                  )}
                </div>
              </div>
            )}
          </Card>
        )}

        {tab === 'catalog' && <CatalogTab onNotice={setNotice} />}
      </TabPanel>

      {editor !== null && (
        <Modal
          size="lg"
          title={editor.mode === 'create' ? '新建上游接入' : `编辑上游 · ${editor.item.name}`}
          subtitle={
            editor.mode === 'edit'
              ? `上游 id 不可修改：${editor.item.id}`
              : 'id 创建后不可修改；路由与工具按该 id 注册'
          }
          onClose={() => setEditor(null)}
        >
          <UpstreamForm
            onCancel={() => setEditor(null)}
            target={editor}
            groups={groups}
            groupsLoading={groupsQuery.isPending}
            groupsError={groupsQuery.isError}
            onSaved={(item) => {
              const created = editor.mode === 'create'
              setEditor(null)
              setNotice({
                severity: 'success',
                title: created ? '上游已创建' : '上游已更新',
                description: `上游 ${item.name}（${item.id}）已保存，当前 ${item.tool_count} 个工具。`,
                meta: `状态 ${item.status} · ${formatDateTime(item.last_checked_at)}`,
              })
              refreshAfterWrite()
            }}
            onError={setNotice}
          />
        </Modal>
      )}

      {basicTarget !== null && (
        <BasicToolsModal upstream={basicTarget} onClose={() => setBasicTarget(null)} />
      )}

      {deleteTarget !== null && (
        <ConfirmDialog
          title={`删除上游 · ${deleteTarget.name}`}
          subtitle={`上游 id：${deleteTarget.id}`}
          confirmLabel="确认删除"
          pendingLabel="删除中…"
          tone="danger"
          pending={deleteMutation.isPending}
          error={deleteError}
          errorTitle="删除上游失败"
          onClose={() => {
            setDeleteTarget(null)
            setDeleteError(undefined)
          }}
          onConfirm={() => deleteMutation.mutate(deleteTarget)}
        >
          <p className="hint">
            将删除上游 {deleteTarget.name}（{deleteTarget.id}）。router 会同步移除其{' '}
            {deleteTarget.tool_count} 个工具，调用方之后不再看到或调用这些工具。删除后不可恢复。
          </p>
        </ConfirmDialog>
      )}
    </PageShell>
  )
}

