import { useMemo, useState } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { refreshUpstream, updateUpstreamBasicTools } from '../../api/admin'
import {
  GROUPS_KEY,
  groupLabel,
  indexGroups,
  upstreamToolsQueryKey,
  useGroupsQuery,
  useUpstreamToolsQuery,
} from '../../api/queries'
import type { UpdateBasicToolsInput, UpstreamItem, UpstreamToolsPayload } from '../../api/types'
import { Badge, UpstreamStatusBadge } from '../../components/Badge'
import { DataTable } from '../../components/DataTable'
import { InfoBar } from '../../components/InfoBar'
import { Modal } from '../../components/Modal'
import { ErrorState, LoadingState } from '../../components/PageShell'
import { ToggleSwitch } from '../../components/ToggleSwitch'
import { formatDateTime } from '../../format'
import { toolGroupLabel, UPSTREAM_STATUS_LABEL } from '../../labels'
import { noticeFromError, type Notice } from '../../notice'
import { GROUP_OPTIONS_LIMIT, basicDisabledReason, basicSaveError } from './upstreamTools'

export function BasicToolsModal({
  upstream,
  onClose,
}: {
  upstream: UpstreamItem
  onClose: () => void
}) {
  const queryClient = useQueryClient()
  const toolsQuery = useUpstreamToolsQuery(upstream.id)
  const groupsQuery = useGroupsQuery(GROUP_OPTIONS_LIMIT)
  const groupIndex = useMemo(() => indexGroups(groupsQuery.data), [groupsQuery.data])
  /** 只记录用户改动；提交时再和当前缓存对比，避免把未变更项发给后端。 */
  const [desired, setDesired] = useState<Record<string, boolean | null>>({})
  const [notice, setNotice] = useState<Notice | undefined>(undefined)
  const payload: UpstreamToolsPayload | undefined = toolsQuery.data
  const tools = useMemo(() => payload?.tools ?? [], [payload])
  const toolIndex = useMemo(() => new Map(tools.map((tool) => [tool.name, tool])), [tools])

  const changes = useMemo(() => {
    const result: UpdateBasicToolsInput['tools'] = {}
    for (const [name, value] of Object.entries(desired)) {
      const tool = toolIndex.get(name)
      if (tool === undefined) {
        continue
      }
      if (value === tool.basic || (value === null && !tool.orphaned && !tool.overridden)) {
        continue
      }
      result[name] = value
    }
    return result
  }, [desired, toolIndex])
  const changedCount = Object.keys(changes).length

  const refreshMutation = useMutation({
    mutationFn: () => refreshUpstream(upstream.id),
    onSuccess: (item) => {
      setDesired({})
      const message = item.status_message.trim()
      setNotice({
        severity: item.status === 'ok' ? 'success' : 'warning',
        title: '工具探测已刷新',
        description: `${UPSTREAM_STATUS_LABEL[item.status]}，上游当前暴露 ${item.tool_count} 个工具。`,
        meta: `${item.status}${message === '' ? '' : ` · ${message}`} · ${formatDateTime(item.last_checked_at)}`,
      })
      void queryClient.invalidateQueries({ queryKey: upstreamToolsQueryKey(upstream.id) })
      void queryClient.invalidateQueries({ queryKey: ['upstreams'] })
      void queryClient.invalidateQueries({ queryKey: ['tool-stats'] })
      void queryClient.invalidateQueries({ queryKey: GROUPS_KEY })
    },
    onError: (error) => setNotice(noticeFromError(error, '刷新工具失败')),
  })

  const saveMutation = useMutation({
    mutationFn: (input: UpdateBasicToolsInput) => updateUpstreamBasicTools(upstream.id, input),
    onSuccess: (result) => {
      const detail =
        result.changed.length === 0
          ? '没有实际变更。'
          : result.changed
              .map((item) => `${item.tool}: ${item.before ?? '继承组'} → ${item.after ?? '继承组'}`)
              .join(' · ')
      setDesired({})
      // 先写回最新清单，避免失效重取期间开关跳回旧值。
      queryClient.setQueryData<UpstreamToolsPayload>(
        upstreamToolsQueryKey(upstream.id),
        (current) =>
          current === undefined
            ? current
            : { ...current, tools: result.tools, basic_total: result.basic_total },
      )
      setNotice({
        severity: 'success',
        title: '基础工具已更新',
        description: `提交 ${result.changed.length} 项变更，当前基础工具 ${result.basic_total} 个。`,
        meta: detail,
      })
      void queryClient.invalidateQueries({ queryKey: upstreamToolsQueryKey(upstream.id) })
      void queryClient.invalidateQueries({ queryKey: GROUPS_KEY })
      void queryClient.invalidateQueries({ queryKey: ['tool-stats'] })
      void queryClient.invalidateQueries({ queryKey: ['upstreams'] })
    },
    onError: (error) => {
      // 失败回滚到后端已确认的状态；提示里保留原因，用户可重新勾选后重试。
      setDesired({})
      setNotice(basicSaveError(error))
    },
  })

  const busy = refreshMutation.isPending || saveMutation.isPending
  const status = payload?.status ?? upstream.status
  const statusMessage = payload?.status_message.trim() ?? ''
  const needsRefresh = payload === undefined ? false : payload.tools.length === 0

  return (
    <Modal
      title={`基础工具 · ${upstream.name}`}
      subtitle={`${upstream.id} · 勾选后这些工具会进入内置 basic 组（全局可用，组本身不可编辑）`}
      size="xl"
      onClose={onClose}
      footer={
        <>
          <button type="button" className="btn btn-secondary" onClick={onClose} disabled={saveMutation.isPending}>
            关闭
          </button>
          <button
            type="button"
            className="btn btn-primary"
            disabled={busy || changedCount === 0}
            onClick={() => saveMutation.mutate({ tools: changes })}
          >
            {saveMutation.isPending ? '保存中…' : changedCount === 0 ? '保存变更' : `保存 ${changedCount} 项变更`}
          </button>
        </>
      }
    >
      <div className="basic-tools">
        {notice !== undefined && (
          <InfoBar
            severity={notice.severity}
            title={notice.title}
            description={notice.description}
            meta={notice.meta}
          />
        )}

        <div className="basic-tools-toolbar">
          <div className="basic-tools-status">
            <UpstreamStatusBadge status={status} />
            <span className="basic-tools-meta">
              最近探测 {formatDateTime(payload?.last_checked_at ?? upstream.last_checked_at)}
              {statusMessage === '' ? '' : ` · ${statusMessage}`}
            </span>
          </div>
          <button
            type="button"
            className="btn btn-secondary btn-sm"
            onClick={() => refreshMutation.mutate()}
            disabled={busy}
            title="调用上游重新探测工具清单；打开本面板本身不会触网"
          >
            {refreshMutation.isPending ? '刷新中…' : '刷新工具'}
          </button>
        </div>

        {payload !== undefined && (
          <p className="basic-tools-counters">
            共 <strong className="mono">{payload.total}</strong> 个工具 · 基础{' '}
            <strong className="mono">{payload.basic_total}</strong> · 孤儿{' '}
            <strong className="mono">{payload.orphaned_total}</strong>
          </p>
        )}

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

        {toolsQuery.isSuccess && needsRefresh && (
          <InfoBar
            severity="warning"
            title={payload?.status === 'unknown' ? '还没有工具缓存' : '工具清单为空'}
            description="请先点击“刷新工具”触发一次探测。读取接口本身不会访问上游，因此这里不会自动重试。"
          />
        )}

        {toolsQuery.isSuccess && !needsRefresh && (
          <DataTable
            label={`${upstream.name} 的工具清单`}
            meta={`共 ${tools.length} 个工具`}
            columns={[
              {
                key: 'name',
                header: '工具',
                width: '240px',
                wrap: true,
                cell: (tool) => (
                  <span className="cell-stack">
                    <span className="mono">{tool.name}</span>
                    <span className="cell-sub mono" title="暴露给模型的名字">
                      {tool.exposed_name}
                    </span>
                  </span>
                ),
              },
              {
                key: 'group',
                header: '生效组',
                width: '150px',
                cell: (tool) =>
                  tool.group_source === 'basic' ? (
                    <Badge tone="accent" title="来自内置 basic 组">
                      基础
                    </Badge>
                  ) : (
                    <Badge tone="neutral" title={`${tool.group} · 来自上游授权组`}>
                      {toolGroupLabel(tool.group, groupLabel(groupIndex, tool.group))}
                    </Badge>
                  ),
              },
              {
                key: 'flags',
                header: '类型',
                width: '170px',
                cell: (tool) => (
                  <span className="detail-badges">
                    {tool.orphaned && (
                      <Badge tone="danger" title="上游已不再暴露该工具">
                        孤儿
                      </Badge>
                    )}
                    {!tool.orphaned && !tool.eligible && (
                      <Badge
                        tone="warning"
                        title={
                          tool.overridden
                            ? '存在基础组覆盖但当前不生效，可清理'
                            : '后端判定不满足基础组条件'
                        }
                      >
                        {tool.overridden ? '已失效' : '不可归入'}
                      </Badge>
                    )}
                  </span>
                ),
              },
              {
                key: 'basic',
                header: '基础',
                width: '120px',
                cell: (tool) => {
                  const reason = basicDisabledReason(tool)
                  const pending = desired[tool.name]
                  const checked = pending === undefined ? tool.basic : pending === true
                  return (
                    <span className="basic-tools-toggle">
                      <ToggleSwitch
                        checked={checked}
                        disabled={busy || reason !== undefined}
                        label="基础"
                        title={reason ?? '加入内置 basic 组'}
                        onChange={(next) =>
                          setDesired((previous) => ({ ...previous, [tool.name]: next }))
                        }
                      />
                      {(tool.orphaned || (tool.overridden && !tool.eligible)) && (
                        <button
                          type="button"
                          className="btn btn-subtle btn-sm"
                          disabled={busy}
                          title={
                            tool.orphaned
                              ? '清除该工具遗留的基础组覆盖'
                              : '清除已失效的基础组覆盖'
                          }
                          onClick={() =>
                            setDesired((previous) => {
                              const next = { ...previous }
                              if (next[tool.name] === null) {
                                delete next[tool.name]
                              } else {
                                next[tool.name] = null
                              }
                              return next
                            })
                          }
                        >
                          {desired[tool.name] === null ? '撤销' : '清理'}
                        </button>
                      )}
                    </span>
                  )
                },
              },
              {
                key: 'description',
                header: '说明',
                width: '300px',
                wrap: true,
                cell: (tool) => {
                  const reason = basicDisabledReason(tool)
                  return (
                    <span className="cell-stack">
                      {reason !== undefined && <span className="basic-tools-reason">{reason}</span>}
                      <span className={reason === undefined ? '' : 'cell-sub'}>
                        {tool.description.trim() === '' ? '—' : tool.description}
                      </span>
                    </span>
                  )
                },
              },
            ]}
            rows={tools}
            rowKey={(tool) => tool.name}
            minWidth="980px"
          />
        )}
      </div>
    </Modal>
  )
}
