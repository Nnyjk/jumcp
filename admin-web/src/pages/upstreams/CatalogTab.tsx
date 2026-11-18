import { useMemo, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { fetchMcpCatalog, uninstallMcpCatalogEntry } from '../../api/admin'
import { groupLabel, indexGroups, useGroupsQuery } from '../../api/queries'
import type { McpCatalogEntry, McpCatalogInstallation, UpstreamItem } from '../../api/types'
import { Badge } from '../../components/Badge'
import { Card } from '../../components/Card'
import { ConfirmDialog } from '../../components/ConfirmDialog'
import { DataTable, type DataTableColumn } from '../../components/DataTable'
import { Modal } from '../../components/Modal'
import { EmptyState, ErrorState, LoadingState } from '../../components/PageShell'
import { formatDateTime } from '../../format'
import { UPSTREAM_TRANSPORT_LABEL } from '../../labels'
import type { Notice } from '../../notice'
import { EditConfigForm } from './EditConfigForm'
import { InstallForm } from './InstallForm'
import { GROUP_OPTIONS_LIMIT } from './upstreamTools'
import '../../styles/upstreams.css'

interface InstallationTarget {
  entry: McpCatalogEntry
  installation: McpCatalogInstallation
}

/**
 * 「MCP 接入」的安装目录页签：每个内置包一张卡（定义、字段、工具、已安装实例）。
 * 安装/卸载/改配置都走目录接口；名称、授权组与启停仍由「接入清单」页签的上游编辑负责。
 */
export function CatalogTab({ onNotice }: { onNotice: (notice: Notice) => void }) {
  const queryClient = useQueryClient()
  const [installTarget, setInstallTarget] = useState<McpCatalogEntry | null>(null)
  const [editTarget, setEditTarget] = useState<InstallationTarget | null>(null)
  const [uninstallTarget, setUninstallTarget] = useState<InstallationTarget | null>(null)
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

  /** 目录写操作会改变上游清单与工具集合，相关缓存一并失效。 */
  const refreshAfterWrite = () => {
    void queryClient.invalidateQueries({ queryKey: ['mcp-catalog'] })
    void queryClient.invalidateQueries({ queryKey: ['upstreams'] })
    void queryClient.invalidateQueries({ queryKey: ['tool-stats'] })
    void queryClient.invalidateQueries({ queryKey: ['upstream-tools'] })
  }

  const uninstallMutation = useMutation({
    mutationFn: (target: InstallationTarget) =>
      uninstallMcpCatalogEntry(target.entry.id, target.installation.upstream_id),
    onSuccess: (_result, target) => {
      setUninstallTarget(null)
      setUninstallError(undefined)
      onNotice({
        severity: 'success',
        title: '实例已卸载',
        description: `已卸载 ${target.installation.name}（${target.installation.upstream_id}），router 已移除其工具。`,
        meta: `目录 ${target.entry.id} · ${target.entry.version}`,
      })
      refreshAfterWrite()
    },
    onError: (error) => setUninstallError(error),
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
      key: 'enabled',
      header: '状态',
      width: '90px',
      cell: (row) => (
        <Badge tone={row.enabled ? 'success' : 'neutral'} dot title={row.enabled ? '已启用' : '已停用'}>
          {row.enabled ? '已启用' : '已停用'}
        </Badge>
      ),
    },
    {
      key: 'version',
      header: '安装版本',
      width: '140px',
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
      key: 'timeline',
      header: '创建 / 更新',
      width: '170px',
      cell: (row) => (
        <span className="cell-stack">
          <span>{formatDateTime(row.created_at)}</span>
          <span className="cell-sub">{formatDateTime(row.updated_at)}</span>
        </span>
      ),
      title: (row) => `创建于 ${formatDateTime(row.created_at)} · 更新于 ${formatDateTime(row.updated_at)}`,
    },
    {
      key: 'actions',
      header: '操作',
      width: '170px',
      sticky: true,
      cell: (row) => (
        <div className="row-actions">
          <button
            type="button"
            className="btn btn-secondary btn-sm"
            onClick={() => setEditTarget({ entry, installation: row })}
            aria-label={`编辑 ${row.upstream_id} 的配置`}
          >
            编辑配置
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
    <>
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
            {entries.map((entry) => (
              <article className="mcp-package" key={entry.id}>
                <header className="mcp-package-head">
                  <div className="mcp-package-title">
                    <h3>{entry.name}</h3>
                    <span className="mono mcp-package-id">{entry.id}</span>
                  </div>
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

                <div className="mcp-package-default">
                  <span className="mcp-package-label">默认上游 id</span>
                  <code className="mono">{entry.default_upstream_id}</code>
                </div>

                <div className="mcp-package-fields">
                  <span className="mcp-package-label">安装字段（{entry.fields.length}）</span>
                  {entry.fields.length === 0 ? (
                    <span className="hint">该包没有安装字段。</span>
                  ) : (
                    <ul className="mcp-def-list">
                      {entry.fields.map((field) => (
                        <li key={field.key} title={field.help ?? undefined}>
                          <code className="mono">{field.key}</code>
                          <span className="mcp-def-summary">{field.label}</span>
                          {field.required && (
                            <Badge tone="warning" title="安装与编辑配置时必填">
                              必填
                            </Badge>
                          )}
                          {field.secret && (
                            <Badge tone="neutral" title="只写：保存后不可查看">
                              密文
                            </Badge>
                          )}
                        </li>
                      ))}
                    </ul>
                  )}
                </div>

                <div className="mcp-package-tools">
                  <span className="mcp-package-label">工具（{entry.tools.length}）</span>
                  {entry.tools.length === 0 ? (
                    <span className="hint">该包未声明工具。</span>
                  ) : (
                    <ul className="mcp-def-list">
                      {entry.tools.map((tool) => (
                        <li key={tool.name} title={tool.summary}>
                          <code className="mono">{tool.name}</code>
                          <span className="mcp-def-summary">{tool.summary}</span>
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
                      minWidth="900px"
                    />
                  )}
                </div>

                <footer className="mcp-package-foot">
                  <span className="hint">安装后可在「接入清单」页签启停、编辑或测试连接；安装字段用实例行的「编辑配置」修改。</span>
                  <button
                    type="button"
                    className="btn btn-primary btn-sm"
                    title={`安装 ${entry.name}`}
                    onClick={() => setInstallTarget(entry)}
                  >
                    安装
                  </button>
                </footer>
              </article>
            ))}
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
              onNotice({
                severity: 'success',
                title: '安装完成',
                description: `已安装 ${upstream.name}（${upstream.id}），可在「接入清单」页签继续启停或测试连接。`,
                meta: `目录版本 ${installTarget.version}`,
              })
              refreshAfterWrite()
            }}
            onError={onNotice}
          />
        </Modal>
      )}

      {editTarget !== null && (
        <Modal
          size="lg"
          title={`编辑配置 · ${editTarget.installation.name}`}
          subtitle={`目录 ${editTarget.entry.id} · 上游 ${editTarget.installation.upstream_id}`}
          onClose={() => setEditTarget(null)}
        >
          <EditConfigForm
            key={editTarget.installation.upstream_id}
            entry={editTarget.entry}
            installation={editTarget.installation}
            onSaved={(installation) => {
              setEditTarget(null)
              onNotice({
                severity: 'success',
                title: '配置已保存',
                description: `已更新 ${installation.name}（${installation.upstream_id}）的安装配置。`,
                meta: `目录 ${editTarget.entry.id} · 更新于 ${formatDateTime(installation.updated_at)}`,
              })
              refreshAfterWrite()
            }}
            onError={onNotice}
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
            router 会同步移除其工具。卸载后可用本页重新安装；相关审计事件保留。此操作不可撤销。
          </p>
        </ConfirmDialog>
      )}
    </>
  )
}
