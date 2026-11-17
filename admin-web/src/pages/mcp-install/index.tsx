import { useMemo, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { fetchMcpCatalog, refreshUpstream, uninstallMcpCatalogEntry } from '../../api/admin'
import { groupLabel, indexGroups, useGroupsQuery } from '../../api/queries'
import type {
  McpCatalogEntry,
  McpCatalogInstallation,
  McpCatalogPage,
  UpstreamItem,
} from '../../api/types'
import { Badge, UpstreamStatusBadge } from '../../components/Badge'
import { Card } from '../../components/Card'
import { ConfirmDialog } from '../../components/ConfirmDialog'
import { DataTable, type DataTableColumn } from '../../components/DataTable'
import { InfoBar } from '../../components/InfoBar'
import { Modal } from '../../components/Modal'
import { EmptyState, ErrorState, LoadingState, PageShell } from '../../components/PageShell'
import { formatDateTime } from '../../format'
import { UPSTREAM_STATUS_LABEL, UPSTREAM_TRANSPORT_LABEL } from '../../labels'
import { noticeFromError, type Notice } from '../../notice'
import { GROUP_OPTIONS_LIMIT } from '../upstreams/upstreamTools'
import { InstallForm } from './InstallForm'
import '../../styles/mcp-install.css'

interface UninstallTarget {
  entry: McpCatalogEntry
  installation: McpCatalogInstallation
}

/**
 * MCP 安装目录：每个包一张卡（定义、工具、已安装实例），安装走目录接口，
 * 卸载删除对应上游。已安装实例的启停/编辑仍复用上游页，不在本页重复实现。
 */
export default function McpInstallPage() {
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const [notice, setNotice] = useState<Notice | undefined>(undefined)
  const [installTarget, setInstallTarget] = useState<McpCatalogEntry | null>(null)
  const [uninstallTarget, setUninstallTarget] = useState<UninstallTarget | null>(null)
  const [uninstallError, setUninstallError] = useState<unknown>(undefined)

  const catalogQuery = useQuery({
    queryKey: ['mcp-catalog'],
    queryFn: ({ signal }) => fetchMcpCatalog(signal),
  })
  const groupsQuery = useGroupsQuery(GROUP_OPTIONS_LIMIT)
  const groups = groupsQuery.data?.items ?? []
  const groupIndex = useMemo(() => indexGroups(groupsQuery.data), [groupsQuery.data])
  const entries = catalogQuery.data?.items ?? []
  const totalInstallations = entries.reduce((sum, entry) => sum + entry.installations.length, 0)

  /** 目录写操作会改变上游清单与工具集合，三处缓存一并失效。 */
  const refreshAfterWrite = () => {
    void queryClient.invalidateQueries({ queryKey: ['mcp-catalog'] })
    void queryClient.invalidateQueries({ queryKey: ['upstreams'] })
    void queryClient.invalidateQueries({ queryKey: ['tool-stats'] })
    void queryClient.invalidateQueries({ queryKey: ['upstream-tools'] })
  }

  const uninstallMutation = useMutation({
    mutationFn: (target: UninstallTarget) =>
      uninstallMcpCatalogEntry(target.entry.id, target.installation.upstream_id),
    onSuccess: (_result, target) => {
      setUninstallTarget(null)
      setUninstallError(undefined)
      setNotice({
        severity: 'success',
        title: '实例已卸载',
        description: `已卸载 ${target.installation.name}（${target.installation.upstream_id}），router 已移除其 ${target.installation.tool_count} 个工具。`,
        meta: `目录 ${target.entry.id} · ${target.entry.version}`,
      })
      refreshAfterWrite()
    },
    onError: (error) => setUninstallError(error),
  })

  const [refreshingId, setRefreshingId] = useState<string | null>(null)

  /**
   * 刷新单个已安装上游：先按返回值原地改这一行（状态/工具数/更新时间），
   * 再重取目录与上游列表，保证与后端一致。
   */
  const refreshMutation = useMutation({
    mutationFn: (upstreamId: string) => refreshUpstream(upstreamId),
    onMutate: (upstreamId) => {
      setRefreshingId(upstreamId)
    },
    onSuccess: (upstream: UpstreamItem) => {
      queryClient.setQueryData<McpCatalogPage>(['mcp-catalog'], (current) =>
        current === undefined
          ? current
          : {
              ...current,
              items: current.items.map((entry) => ({
                ...entry,
                installations: entry.installations.map((installation) =>
                  installation.upstream_id === upstream.id
                    ? {
                        ...installation,
                        name: upstream.name,
                        enabled: upstream.enabled,
                        group: upstream.group,
                        status: upstream.status,
                        status_message: upstream.status_message,
                        tool_count: upstream.tool_count,
                        updated_at: upstream.updated_at,
                      }
                    : installation,
                ),
              })),
            },
      )
      setNotice({
        severity: 'success',
        title: '上游已刷新',
        description: `${upstream.name}（${upstream.id}）状态 ${
          UPSTREAM_STATUS_LABEL[upstream.status] ?? upstream.status
        }，暴露 ${upstream.tool_count} 个工具。`,
        meta: `更新于 ${formatDateTime(upstream.updated_at)}`,
      })
      refreshAfterWrite()
    },
    onError: (error) => setNotice(noticeFromError(error, '刷新上游失败')),
    onSettled: () => setRefreshingId(null),
  })

  const installedColumns = (entry: McpCatalogEntry): Array<DataTableColumn<McpCatalogInstallation>> => [
    {
      key: 'upstream',
      header: '实例 / ID',
      width: '180px',
      cell: (row) => (
        <span className="cell-stack">
          <span>{row.name}</span>
          <span className="cell-sub mono">{row.upstream_id}</span>
        </span>
      ),
      title: (row) => `${row.name} · ${row.upstream_id}`,
    },
    {
      key: 'group',
      header: '授权组',
      width: '120px',
      cell: (row) => <span title={row.group}>{groupLabel(groupIndex, row.group)}</span>,
      title: (row) => row.group,
    },
    {
      key: 'status',
      header: '状态',
      width: '110px',
      cell: (row) => (
        <span className="cell-stack">
          <span>
            <UpstreamStatusBadge status={row.status} />
          </span>
          <span className="cell-sub">{row.enabled ? '已启用' : '已停用'}</span>
          {(row.status === 'unknown' || row.status === 'error') && (
            <span className="cell-sub mcp-refresh-hint">点刷新以加载工具</span>
          )}
        </span>
      ),
      title: (row) =>
        `${UPSTREAM_STATUS_LABEL[row.status] ?? row.status} · ${row.enabled ? '已启用' : '已停用'}${
          row.status_message === null || row.status_message.trim() === ''
            ? ''
            : ` · ${row.status_message.trim()}`
        } · 更新于 ${formatDateTime(row.updated_at)}`,
    },
    {
      key: 'tools',
      header: '工具',
      width: '60px',
      numeric: true,
      cell: (row) => String(row.tool_count),
    },
    {
      key: 'version',
      header: '安装版本',
      width: '160px',
      cell: (row) => (
        <span className="cell-stack">
          <span className="mono">v{row.catalog_version}</span>
          {row.catalog_version !== entry.version && (
            <span>
              <Badge tone="warning" title={`安装版本 ${row.catalog_version} · 当前包版本 ${entry.version}`}>
                版本不一致
              </Badge>
            </span>
          )}
        </span>
      ),
      title: (row) => `安装于包版本 ${row.catalog_version} · 当前包版本 ${entry.version}`,
    },
    {
      key: 'actions',
      header: '操作',
      width: '250px',
      sticky: true,
      cell: (row) => (
        <div className="row-actions">
          <button
            type="button"
            className="btn btn-secondary btn-sm"
            disabled={refreshMutation.isPending}
            onClick={() => refreshMutation.mutate(row.upstream_id)}
            aria-label={`刷新 ${row.upstream_id} 的工具`}
          >
            {refreshingId === row.upstream_id ? '刷新中…' : '刷新'}
          </button>
          <button
            type="button"
            className="btn btn-secondary btn-sm"
            onClick={() => navigate('/upstreams')}
            aria-label={`在上游页查看 ${row.upstream_id}`}
          >
            查看上游
          </button>
          <button
            type="button"
            className="btn btn-danger btn-sm"
            onClick={() => {
              setUninstallError(undefined)
              setUninstallTarget({ entry, installation: row })
            }}
          >
            卸载
          </button>
        </div>
      ),
    },
  ]

  const subtitle =
    catalogQuery.data === undefined
      ? catalogQuery.isError
        ? '查询失败'
        : '查询中'
      : `${entries.length} 个安装包 · ${totalInstallations} 个已安装实例`

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
        title="安装目录"
        subtitle={subtitle}
        actions={
          <button
            type="button"
            className="btn btn-secondary btn-sm"
            onClick={() => catalogQuery.refetch()}
            disabled={catalogQuery.isFetching}
          >
            {catalogQuery.isFetching ? '刷新中…' : '刷新目录'}
          </button>
        }
      >
        {catalogQuery.isPending && <LoadingState rows={6} label="正在加载安装目录" />}
        {catalogQuery.isError && (
          <div className="state-wrap">
            <ErrorState error={catalogQuery.error} />
            <button
              type="button"
              className="btn btn-secondary"
              onClick={() => catalogQuery.refetch()}
              disabled={catalogQuery.isFetching}
            >
              重试
            </button>
          </div>
        )}
        {catalogQuery.data !== undefined && entries.length === 0 && (
          <EmptyState hint="后端没有注册任何 MCP 安装包（或目录配置为空）。" />
        )}
        {entries.length > 0 && (
          <div className="mcp-catalog-grid">
            {entries.map((entry) => {
              // stdio 未开启 / 二进制缺失任一存在即不可安装。
              const blockedReasons: string[] = []
              if (entry.requires_stdio && !entry.stdio_enabled) blockedReasons.push('stdio 未开启')
              if (entry.requires_stdio && !entry.binary_present) blockedReasons.push('二进制缺失')
              const installable = blockedReasons.length === 0
              return (
                <article className="mcp-package" key={entry.id}>
                  <header className="mcp-package-head">
                    <div className="mcp-package-title">
                      <h3>{entry.name}</h3>
                      <span className="mono mcp-package-id">{entry.id}</span>
                    </div>
                    {installable ? (
                      <Badge tone="success" dot title="依赖检查通过">
                        可安装
                      </Badge>
                    ) : (
                      <Badge tone="warning" dot title={blockedReasons.join(' · ')}>
                        {blockedReasons.join(' · ')}
                      </Badge>
                    )}
                  </header>

                  <p className="mcp-package-summary">{entry.summary}</p>

                  <div className="mcp-package-meta">
                    <span className="mono mcp-package-version">v{entry.version}</span>
                    <span className="mcp-package-transport">
                      {UPSTREAM_TRANSPORT_LABEL[entry.transport] ?? entry.transport}
                    </span>
                    {entry.tags.map((tag) => (
                      <Badge key={tag} tone="neutral" title={tag}>
                        {tag}
                      </Badge>
                    ))}
                  </div>

                  <div className="mcp-package-tools">
                    <span className="mcp-package-label">工具（{entry.tools.length}）</span>
                    {entry.tools.length === 0 ? (
                      <span className="hint">该包未声明工具。</span>
                    ) : (
                      <ul className="mcp-tool-list">
                        {entry.tools.map((tool) => (
                          <li key={tool.name} title={tool.summary}>
                            <code className="mono">{tool.name}</code>
                            <span className="mcp-tool-summary">{tool.summary}</span>
                          </li>
                        ))}
                      </ul>
                    )}
                  </div>

                  <div className="mcp-package-installs">
                    <div className="mcp-package-install-head">
                      <span className="mcp-package-label">已安装实例</span>
                      <span className="hint">{entry.installations.length} 个</span>
                    </div>
                    {entry.installations.length === 0 ? (
                      <p className="hint">尚未安装。</p>
                    ) : (
                      <DataTable
                        label={`${entry.name} 已安装实例`}
                        meta={`共 ${entry.installations.length} 个`}
                        columns={installedColumns(entry)}
                        rows={entry.installations}
                        rowKey={(row) => row.upstream_id}
                        minWidth="880px"
                      />
                    )}
                  </div>

                  <footer className="mcp-package-foot">
                    {blockedReasons.length > 0 ? (
                      <span className="mcp-package-reason">
                        {blockedReasons.join('；')}，暂不可安装。请联系管理员开启 stdio 或安装对应可执行文件。
                      </span>
                    ) : (
                      <span className="hint">安装后可在上游页启停、编辑或测试连接。</span>
                    )}
                    <button
                      type="button"
                      className="btn btn-primary btn-sm"
                      disabled={!installable}
                      title={installable ? `安装 ${entry.name}` : blockedReasons.join('；')}
                      onClick={() => setInstallTarget(entry)}
                    >
                      安装
                    </button>
                  </footer>
                </article>
              )
            })}
          </div>
        )}
      </Card>

      {installTarget !== null && (
        <Modal
          size="lg"
          title={`安装 · ${installTarget.name}`}
          subtitle={`目录项 ${installTarget.id} · 版本 v${installTarget.version} · ${UPSTREAM_TRANSPORT_LABEL[installTarget.transport] ?? installTarget.transport}`}
          onClose={() => setInstallTarget(null)}
        >
          <InstallForm
            key={installTarget.id}
            entry={installTarget}
            groups={groups}
            groupsLoading={groupsQuery.isPending}
            groupsError={groupsQuery.isError}
            onInstalled={(upstream: UpstreamItem) => {
              setInstallTarget(null)
              setNotice({
                severity: 'success',
                title: '安装完成',
                description: `已安装 ${upstream.name}（${upstream.id}），当前 ${upstream.tool_count} 个工具。`,
                meta: `状态 ${UPSTREAM_STATUS_LABEL[upstream.status] ?? upstream.status} · 目录版本 ${installTarget.version}`,
              })
              refreshAfterWrite()
            }}
            onError={setNotice}
          />
        </Modal>
      )}

      {uninstallTarget !== null && (
        <ConfirmDialog
          title={`卸载实例 · ${uninstallTarget.installation.name}`}
          subtitle={`目录 ${uninstallTarget.entry.id} · 上游 ${uninstallTarget.installation.upstream_id}`}
          confirmLabel="确认卸载"
          pendingLabel="卸载中…"
          tone="danger"
          pending={uninstallMutation.isPending}
          error={uninstallError}
          errorTitle="卸载失败"
          onClose={() => {
            setUninstallTarget(null)
            setUninstallError(undefined)
          }}
          onConfirm={() => uninstallMutation.mutate(uninstallTarget)}
        >
          <p className="hint">
            将删除上游 {uninstallTarget.installation.name}（{uninstallTarget.installation.upstream_id}），
            router 会同步移除其 {uninstallTarget.installation.tool_count} 个工具。卸载后可用本页重新安装；
            相关审计事件保留。此操作不可撤销。
          </p>
        </ConfirmDialog>
      )}
    </PageShell>
  )
}
