import { useMemo, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  deleteUpstream,
  fetchUpstreams,
  refreshUpstream,
  updateUpstream,
} from '../../api/admin'
import { groupLabel, indexGroups, useGroupsQuery } from '../../api/queries'
import type { UpstreamItem } from '../../api/types'
import { Badge, CategoryBadge, UpstreamStatusBadge } from '../../components/Badge'
import { Card } from '../../components/Card'
import { DataTable, type DataTableColumn } from '../../components/DataTable'
import { InfoBar } from '../../components/InfoBar'
import { Modal } from '../../components/Modal'
import { EmptyState, ErrorState, LoadingState, PageShell } from '../../components/PageShell'
import { DASH, formatDateTime } from '../../format'
import { UPSTREAM_STATUS_LABEL, UPSTREAM_TRANSPORT_LABEL } from '../../labels'
import { noticeFromError, type Notice } from '../../notice'
import { BasicToolsModal } from './BasicToolsModal'
import { UpstreamForm } from './UpstreamForm'
import {
  GROUP_OPTIONS_LIMIT,
  targetText,
  toInput,
  type EditorTarget,
} from './upstreamTools'

export default function UpstreamsPage() {
  const queryClient = useQueryClient()
  const [notice, setNotice] = useState<Notice | undefined>(undefined)
  const [editor, setEditor] = useState<EditorTarget | null>(null)
  const [deleteTarget, setDeleteTarget] = useState<UpstreamItem | null>(null)
  const [deleteError, setDeleteError] = useState<Notice | undefined>(undefined)
  const [basicTarget, setBasicTarget] = useState<UpstreamItem | null>(null)

  const upstreamsQuery = useQuery({
    queryKey: ['upstreams'],
    queryFn: ({ signal }) => fetchUpstreams(signal),
  })
  const groupsQuery = useGroupsQuery(GROUP_OPTIONS_LIMIT)
  const groups = groupsQuery.data?.items ?? []
  const groupIndex = useMemo(() => indexGroups(groupsQuery.data), [groupsQuery.data])
  const rows = upstreamsQuery.data?.items ?? []

  /** 上游写操作会改变 router 暴露的工具集合，台账一并失效。 */
  const refreshAfterWrite = () => {
    void queryClient.invalidateQueries({ queryKey: ['upstreams'] })
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
    onError: (error) => setDeleteError(noticeFromError(error, '删除上游失败')),
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
      key: 'read_only',
      header: '只读',
      width: '64px',
      cell: (row) => (row.read_only ? '是' : '否'),
      title: (row) => `read_only=${row.read_only}`,
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
            hint="还没有 MCP 上游接入。内置的基础 / 分析 / 审计三组工具由后端注册；自定义上游可用右上角「新建接入」创建。"
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
          <DataTable
            label="MCP 上游接入清单"
            meta={`共 ${rows.length} 行`}
            columns={columns}
            rows={rows}
            rowKey={(row) => row.id}
            minWidth="1184px"
          />
        )}
      </Card>

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
        <Modal
          size="md"
          title={`删除上游 · ${deleteTarget.name}`}
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
            将删除上游 {deleteTarget.name}（{deleteTarget.id}）。router 会同步移除其{' '}
            {deleteTarget.tool_count} 个工具，调用方之后不再看到或调用这些工具。删除后不可恢复。
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

