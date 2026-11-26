import { useState } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { refreshUpstream } from '../../api/admin'
import { upstreamToolsQueryKey, useUpstreamToolsQuery } from '../../api/queries'
import type { UpstreamItem } from '../../api/types'
import { Badge, UpstreamStatusBadge } from '../../components/Badge'
import { DataTable } from '../../components/DataTable'
import { InfoBar } from '../../components/InfoBar'
import { Modal } from '../../components/Modal'
import { ErrorState, LoadingState } from '../../components/PageShell'
import { formatDateTime } from '../../format'
import { UPSTREAM_STATUS_LABEL } from '../../labels'
import { noticeFromError, type Notice } from '../../notice'

/**
 * 上游工具只读视图：工具是否对组可见由「组维护 → 暴露矩阵」配置，这里只回答
 * 「上游当前有哪些工具、暴露名是什么、哪些只能进 admin 组、已授予哪些组」。
 * GET 只读缓存不触网，需要新结果时由「刷新工具」显式触发探测。
 */
export function UpstreamToolsModal({
  upstream,
  onClose,
}: {
  upstream: UpstreamItem
  onClose: () => void
}) {
  const queryClient = useQueryClient()
  const [notice, setNotice] = useState<Notice | undefined>(undefined)
  const toolsQuery = useUpstreamToolsQuery(upstream.id)
  const payload = toolsQuery.data
  const tools = payload?.tools ?? []
  const grantedGroups = payload?.groups ?? []

  const refreshMutation = useMutation({
    mutationFn: () => refreshUpstream(upstream.id),
    onSuccess: (item) => {
      void queryClient.invalidateQueries({ queryKey: upstreamToolsQueryKey(upstream.id) })
      void queryClient.invalidateQueries({ queryKey: ['upstreams'] })
      void queryClient.invalidateQueries({ queryKey: ['tool-stats'] })
      const message = item.status_message.trim()
      setNotice({
        severity: item.status === 'ok' ? 'success' : 'warning',
        title: '工具探测已刷新',
        description: `${UPSTREAM_STATUS_LABEL[item.status]}，上游当前暴露 ${item.tool_count} 个工具。`,
        meta: `${item.status}${message === '' ? '' : ` · ${message}`} · ${formatDateTime(item.last_checked_at)}`,
      })
    },
    onError: (error) => setNotice(noticeFromError(error, '刷新工具失败')),
  })

  const status = payload?.status ?? upstream.status
  const statusMessage = payload?.status_message.trim() ?? ''

  return (
    <Modal
      title={`工具 · ${upstream.name}`}
      subtitle={`${upstream.id} · 只读工具清单；可见性由「组维护」页的暴露矩阵决定`}
      size="xl"
      onClose={onClose}
      footer={
        <button type="button" className="btn btn-secondary" onClick={onClose}>
          关闭
        </button>
      }
    >
      {notice !== undefined && (
        <InfoBar
          severity={notice.severity}
          title={notice.title}
          description={notice.description}
          meta={notice.meta}
        />
      )}

      <div className="toolbar">
        <UpstreamStatusBadge status={status} />
        <span className="hint">
          最近探测 {formatDateTime(payload?.last_checked_at ?? upstream.last_checked_at)}
          {statusMessage === '' ? '' : ` · ${statusMessage}`}
        </span>
        <div className="filter-actions">
          <button
            type="button"
            className="btn btn-secondary btn-sm"
            onClick={() => refreshMutation.mutate()}
            disabled={refreshMutation.isPending}
            title="调用上游重新探测工具清单；打开本面板本身不会触网"
          >
            {refreshMutation.isPending ? '刷新中…' : '刷新工具'}
          </button>
        </div>
      </div>

      <p className="hint">
        已授予组：
        {grantedGroups.length === 0
          ? '未授予任何组（到「组维护」页的暴露矩阵授予后，组令牌才能看到这些工具）'
          : grantedGroups.join('、')}
      </p>

      {toolsQuery.isPending && <LoadingState label="正在读取工具缓存…" />}

      {toolsQuery.isError && (
        <>
          <ErrorState error={toolsQuery.error} />
          <InfoBar
            severity="info"
            title="缓存读取失败时不会自动访问上游"
            description="可点击右上角“刷新工具”重新探测；如果上游不可达，错误会显示在上方提示中。"
          />
        </>
      )}

      {toolsQuery.isSuccess && tools.length === 0 && (
        <InfoBar
          severity="warning"
          title={payload?.status === 'unknown' ? '还没有工具缓存' : '工具清单为空'}
          description="请先点击“刷新工具”触发一次探测。读取接口本身不会访问上游，因此这里不会自动重试。"
        />
      )}

      {tools.length > 0 && (
        <DataTable
          label={`${upstream.name} 的工具清单`}
          meta={`共 ${payload?.total ?? tools.length} 个工具`}
          columns={[
            {
              key: 'name',
              header: '工具',
              width: '260px',
              wrap: true,
              cell: (tool) => (
                <span className="cell-stack">
                  <span className="mono">{tool.name}</span>
                  <span className="cell-sub mono" title="暴露给调用方的名字">
                    {tool.exposed_name}
                  </span>
                </span>
              ),
            },
            {
              key: 'flags',
              header: '类型',
              width: '130px',
              cell: (tool) =>
                tool.admin_only ? (
                  <Badge tone="warning" title="维护通道工具，只能进 admin 组">
                    仅 admin 组
                  </Badge>
                ) : (
                  <span className="cell-sub">普通</span>
                ),
            },
            {
              key: 'description',
              header: '说明',
              wrap: true,
              cell: (tool) => tool.description.trim() === '' ? '—' : tool.description,
              title: (tool) => tool.description,
            },
          ]}
          rows={tools}
          rowKey={(tool) => tool.name}
          minWidth="720px"
        />
      )}
    </Modal>
  )
}
