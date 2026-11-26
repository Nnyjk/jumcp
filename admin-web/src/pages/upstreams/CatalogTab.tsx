import { useMemo, useState } from 'react'
import { gsap } from 'gsap'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { fetchMcpCatalog, uninstallMcpCatalogEntry } from '../../api/admin'
import type {
  McpCatalogEntry,
  McpCatalogField,
  McpCatalogInstallation,
  McpCatalogTool,
  UpstreamItem,
} from '../../api/types'
import { Badge } from '../../components/Badge'
import { Card } from '../../components/Card'
import { ConfirmDialog } from '../../components/ConfirmDialog'
import { DataTable, type DataTableColumn } from '../../components/DataTable'
import { Modal } from '../../components/Modal'
import { EmptyState, ErrorState } from '../../components/PageShell'
import { Field } from '../../components/Field'
import { JsonBlock } from '../../components/JsonBlock'
import { MOTION_DURATION, MOTION_EASE, useGsapScope } from '../../motion/useGsap'
import { formatDateTime } from '../../format'
import { UPSTREAM_TRANSPORT_LABEL } from '../../labels'
import type { Notice } from '../../notice'
import { EditConfigForm } from './EditConfigForm'
import { InstallForm } from './InstallForm'
import '../../styles/upstreams.css'

interface InstallationTarget {
  entry: McpCatalogEntry
  installation: McpCatalogInstallation
}

const FIELD_COLUMNS: Array<DataTableColumn<McpCatalogField>> = [
  { key: 'key', header: 'key', width: '150px', mono: true, cell: (field) => field.key },
  { key: 'label', header: 'label', width: '170px', cell: (field) => field.label, title: (field) => field.label },
  { key: 'kind', header: 'kind', width: '90px', mono: true, cell: (field) => field.kind },
  { key: 'required', header: '必填', width: '64px', cell: (field) => (field.required ? '是' : '否') },
  { key: 'secret', header: '密文', width: '64px', cell: (field) => (field.secret ? '是' : '否') },
]

const TOOL_COLUMNS: Array<DataTableColumn<McpCatalogTool>> = [
  { key: 'name', header: 'name', width: '220px', mono: true, cell: (tool) => tool.name },
  { key: 'summary', header: 'summary', wrap: true, cell: (tool) => tool.summary, title: (tool) => tool.summary },
]


/** 目录分组的展示名：只翻译已知标签，其余原样显示，不隐藏真实标签。 */
const GROUP_LABEL: Record<string, string> = {
  database: '数据库',
  filesystem: '文件系统',
  observability: '可观测性',
}
/**
 * 「MCP 接入」的安装目录页签：小卡片只展示名称/id/版本/摘要/标签/已装数量，
 * 完整定义进「详情」弹窗；安装表单不含上游 id（由目录按已安装实例自动推导）。
 * 安装/卸载/改配置都走目录接口；名称与启停仍由「接入清单」页签的上游编辑负责，
 * 哪些组能看到该上游由「组维护」页的暴露矩阵配置。
 */
export function CatalogTab({ onNotice }: { onNotice: (notice: Notice) => void }) {
  const queryClient = useQueryClient()
  const [filter, setFilter] = useState('')
  const [tagFilter, setTagFilter] = useState('')
  const [installTarget, setInstallTarget] = useState<McpCatalogEntry | null>(null)
  const [detailTarget, setDetailTarget] = useState<McpCatalogEntry | null>(null)
  const [editTarget, setEditTarget] = useState<InstallationTarget | null>(null)
  const [uninstallTarget, setUninstallTarget] = useState<InstallationTarget | null>(null)
  const [uninstallError, setUninstallError] = useState<unknown>(undefined)

  const catalogQuery = useQuery({
    queryKey: ['mcp-catalog'],
    queryFn: ({ signal }) => fetchMcpCatalog(signal),
  })
  const entries = useMemo(() => catalogQuery.data?.items ?? [], [catalogQuery.data])
  const totalInstallations = entries.reduce((sum, entry) => sum + entry.installations.length, 0)

  const normalizedFilter = filter.trim().toLowerCase()
  const visibleEntries = useMemo(() => {
    if (normalizedFilter === '') return entries
    return entries.filter(
      (entry) =>
        entry.name.toLowerCase().includes(normalizedFilter) ||
        entry.id.toLowerCase().includes(normalizedFilter),
    )
  }, [entries, normalizedFilter])

  /** 目录契约没有分组字段，按首个标签归组；组内保持目录原有顺序。 */
  const catalogGroups = useMemo(() => {
    const buckets = new Map<string, McpCatalogEntry[]>()
    for (const entry of visibleEntries) {
      const key = entry.tags[0] ?? ''
      const bucket = buckets.get(key)
      if (bucket === undefined) {
        buckets.set(key, [entry])
      } else {
        bucket.push(entry)
      }
    }
    return [...buckets.entries()].map(([key, items]) => ({ key, items }))
  }, [visibleEntries])

  /** 分组下拉的候选：按首个标签去重，键与展示名一一对应。 */
  const groupOptions = useMemo(() => {
    const options = new Map<string, string>()
    for (const entry of entries) {
      const key = entry.tags[0] ?? ''
      if (!options.has(key)) options.set(key, GROUP_LABEL[key] ?? (key === '' ? '未分类' : key))
    }
    return [...options.entries()]
  }, [entries])

  const filteredGroups = useMemo(
    () => (tagFilter === '' ? catalogGroups : catalogGroups.filter((group) => group.key === tagFilter)),
    [catalogGroups, tagFilter],
  )

  /** 卡片入场动效：目录或过滤变化时重播一次；只动首屏 20 张卡片，长列表不整屏闪。 */
  const gridRef = useGsapScope<HTMLDivElement>(
    (scope) => {
      const cards = scope.querySelectorAll<HTMLElement>('.mcp-package')
      if (cards.length === 0) return
      gsap.from(Array.from(cards).slice(0, 20), {
        opacity: 0,
        y: 8,
        duration: MOTION_DURATION.row,
        ease: MOTION_EASE,
        stagger: 0.008,
        clearProps: 'opacity,transform',
      })
    },
    [visibleEntries.map((entry) => entry.id).join('\u0000'), tagFilter],
  )

  // 弹窗持有的是打开时的快照；目录刷新后用最新条目替换，安装表单因此能重新推导 id/名称。
  const detailEntry =
    detailTarget === null
      ? null
      : (entries.find((entry) => entry.id === detailTarget.id) ?? detailTarget)
  const installEntry =
    installTarget === null
      ? null
      : (entries.find((entry) => entry.id === installTarget.id) ?? installTarget)

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
        {catalogQuery.isPending && (
          <div className="mcp-catalog-grid" aria-hidden="true">
            {Array.from({ length: 6 }, (_, index) => (
              <div className="mcp-package mcp-package-skeleton" key={index}>
                <div className="skeleton-stack">
                  <span className="skeleton-line" />
                  <span className="skeleton-line" />
                  <span className="skeleton-line" />
                </div>
              </div>
            ))}
          </div>
        )}
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
          <>
            <div className="filter-bar mcp-catalog-toolbar">
              <Field label="搜索" title="name / id">
                <input
                  className="input"
                  type="search"
                  value={filter}
                  onChange={(event) => setFilter(event.target.value)}
                  placeholder="按名称或 id 过滤"
                />
              </Field>
              <Field label="分组" title="tag">
                <select
                  className="select"
                  value={tagFilter}
                  onChange={(event) => setTagFilter(event.target.value)}
                >
                  <option value="">全部分组</option>
                  {groupOptions.map(([key, label]) => (
                    <option key={key} value={key}>
                      {label}
                    </option>
                  ))}
                </select>
              </Field>
              <div className="filter-actions">
                <button
                  type="button"
                  className="btn btn-secondary"
                  onClick={() => {
                    setFilter('')
                    setTagFilter('')
                  }}
                  disabled={filter === '' && tagFilter === ''}
                >
                  清空筛选
                </button>
              </div>
              <span className="hint mcp-catalog-count" role="status" aria-live="polite">
                显示 {visibleEntries.length} / 共 {entries.length}
              </span>
            </div>

            {filteredGroups.length === 0 ? (
              <EmptyState hint="没有匹配的安装包" />
            ) : (
              <div className="mcp-catalog-groups" ref={gridRef}>
                {filteredGroups.map((group) => (
                  <section className="mcp-catalog-group" key={group.key}>
                    <header className="mcp-catalog-group-head">
                      <h3 className="mcp-catalog-group-title">
                        {GROUP_LABEL[group.key] ?? (group.key === '' ? '未分类' : group.key)}
                      </h3>
                      <span className="hint mcp-catalog-group-count">{group.items.length} 个安装包</span>
                    </header>
                    <div className="mcp-catalog-grid">
                      {group.items.map((entry) => {
                        const installed = entry.installations.length
                        const hiddenTags = entry.tags.slice(3)
                        return (
                          <article className="mcp-package" key={entry.id}>
                            <header className="mcp-package-head">
                              <div className="mcp-package-title">
                                <h3>{entry.name}</h3>
                                <span className="mono mcp-package-id">{entry.id}</span>
                              </div>
                              <Badge
                                tone={installed > 0 ? 'accent' : 'neutral'}
                                title={installed > 0 ? `已安装 ${installed} 个实例` : '尚未安装'}
                              >
                                已安装 {installed}
                              </Badge>
                            </header>

                            <div className="mcp-package-meta">
                              <span className="mono mcp-package-version">v{entry.version}</span>
                              {entry.tags.slice(0, 3).map((tag) => (
                                <Badge key={tag} tone="neutral" title={tag}>
                                  {tag}
                                </Badge>
                              ))}
                              {hiddenTags.length > 0 && (
                                <Badge tone="neutral" title={hiddenTags.join('、')}>
                                  +{hiddenTags.length}
                                </Badge>
                              )}
                            </div>

                            <p className="mcp-package-summary" title={entry.summary}>
                              {entry.summary}
                            </p>

                            <footer className="mcp-package-foot">
                              <button
                                type="button"
                                className="btn btn-secondary btn-sm"
                                onClick={() => setDetailTarget(entry)}
                              >
                                详情
                              </button>
                              <button
                                type="button"
                                className="btn btn-primary btn-sm"
                                onClick={() => setInstallTarget(entry)}
                              >
                                安装
                              </button>
                            </footer>
                          </article>
                        )
                      })}
                    </div>
                  </section>
                ))}
              </div>
            )}
          </>
        )}
      </Card>

      {installEntry !== null && (
        <Modal
          size="lg"
          title={`安装 · ${installEntry.name}`}
          subtitle={`目录项 ${installEntry.id} · 版本 v${installEntry.version} · ${UPSTREAM_TRANSPORT_LABEL[installEntry.transport] ?? installEntry.transport}`}
          onClose={() => setInstallTarget(null)}
        >
          <InstallForm
            key={installEntry.id}
            onCancel={() => setInstallTarget(null)}
            entry={installEntry}
            onInstalled={(upstream: UpstreamItem) => {
              setInstallTarget(null)
              onNotice({
                severity: 'success',
                title: '安装完成',
                description: `已安装 ${upstream.name}（${upstream.id}）；安装只描述连接，安装后到「组维护」页授予暴露，组令牌才能看到它。`,
                meta: `目录版本 ${installEntry.version}`,
              })
              refreshAfterWrite()
            }}
            onError={onNotice}
          />
        </Modal>
      )}

      {detailEntry !== null && (
        <Modal
          size="xl"
          title={`${detailEntry.name} · 详情`}
          subtitle={`目录项 ${detailEntry.id} · 版本 v${detailEntry.version}`}
          onClose={() => setDetailTarget(null)}
          footer={
            <>
              <button type="button" className="btn btn-secondary" onClick={() => setDetailTarget(null)}>
                关闭
              </button>
              <button
                type="button"
                className="btn btn-primary"
                onClick={() => {
                  const target = detailEntry
                  setDetailTarget(null)
                  setInstallTarget(target)
                }}
              >
                安装
              </button>
            </>
          }
        >
          <p className="mcp-detail-summary">{detailEntry.summary}</p>

          <div className="mcp-detail-facts">
            <span>
              版本 <code className="mono">v{detailEntry.version}</code>
            </span>
            <span>
              传输{' '}
              <code className="mono">
                {UPSTREAM_TRANSPORT_LABEL[detailEntry.transport] ?? detailEntry.transport}
              </code>
            </span>
            <span>
              默认上游 id <code className="mono">{detailEntry.default_upstream_id}</code>
            </span>
            <span className="mcp-detail-tags">
              标签
              {detailEntry.tags.length === 0 ? (
                <span className="hint">无</span>
              ) : (
                detailEntry.tags.map((tag) => (
                  <Badge key={tag} tone="neutral" title={tag}>
                    {tag}
                  </Badge>
                ))
              )}
            </span>
          </div>

          <section className="mcp-detail-section">
            <h3 className="mcp-detail-title">安装字段（{detailEntry.fields.length}）</h3>
            {detailEntry.fields.length === 0 ? (
              <p className="hint">该包没有安装字段。</p>
            ) : (
              <DataTable
                label={`${detailEntry.name} 安装字段`}
                columns={FIELD_COLUMNS}
                rows={detailEntry.fields}
                rowKey={(field) => field.key}
                minWidth="640px"
              />
            )}
          </section>

          <section className="mcp-detail-section">
            <h3 className="mcp-detail-title">工具（{detailEntry.tools.length}）</h3>
            {detailEntry.tools.length === 0 ? (
              <p className="hint">该包未声明工具。</p>
            ) : (
              <DataTable
                label={`${detailEntry.name} 工具`}
                columns={TOOL_COLUMNS}
                rows={detailEntry.tools}
                rowKey={(tool) => tool.name}
                minWidth="560px"
              />
            )}
          </section>

          <section className="mcp-detail-section">
            <h3 className="mcp-detail-title">已安装实例（{detailEntry.installations.length}）</h3>
            {detailEntry.installations.length === 0 ? (
              <p className="hint">尚未安装。</p>
            ) : (
              <DataTable
                label={`${detailEntry.name} 已安装实例`}
                meta={`共 ${detailEntry.installations.length} 个`}
                columns={installedColumns(detailEntry)}
                rows={detailEntry.installations}
                rowKey={(row) => row.upstream_id}
                minWidth="780px"
              />
            )}
          </section>

          <section className="mcp-detail-section">
            <h3 className="mcp-detail-title">安装配置（{detailEntry.installations.length}）</h3>
            {detailEntry.installations.length === 0 ? (
              <p className="hint">尚未安装，没有配置可看。</p>
            ) : (
              <div className="mcp-detail-configs">
                {detailEntry.installations.map((installation) => (
                  <div className="mcp-detail-config" key={installation.upstream_id}>
                    <span className="mcp-detail-config-title">
                      {installation.name}
                      <span className="mono mcp-package-id">{installation.upstream_id}</span>
                    </span>
                    <JsonBlock
                      label="非密字段当前值（密文不回显）"
                      value={installation.config}
                    />
                  </div>
                ))}
              </div>
            )}
          </section>
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
            onCancel={() => setEditTarget(null)}
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
