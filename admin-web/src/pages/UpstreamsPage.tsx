import { useMemo, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  createUpstream,
  deleteUpstream,
  fetchUpstreams,
  refreshUpstream,
  updateUpstream,
} from '../api/admin'
import { groupLabel, indexGroups, useGroupsQuery } from '../api/queries'
import type {
  GroupItem,
  UpstreamCategory,
  UpstreamInput,
  UpstreamItem,
  UpstreamStatus,
  UpstreamTransport,
} from '../api/types'
import { Badge, CategoryBadge, type BadgeTone } from '../components/Badge'
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
import { formatDateTime } from '../format'
import {
  UPSTREAM_CATEGORY_LABEL,
  UPSTREAM_STATUS_LABEL,
  UPSTREAM_TRANSPORT_LABEL,
} from '../labels'
import { noticeFromError, type Notice } from '../notice'

const DASH = '—'

/** 授权组下拉一次取全；组的权威来源仍是 GET /admin/groups。 */
const GROUP_OPTIONS_LIMIT = 200

const CATEGORY_OPTIONS: UpstreamCategory[] = ['basic', 'analysis', 'admin', 'custom']

const STATUS_TONE: Record<UpstreamStatus, BadgeTone> = {
  ok: 'success',
  degraded: 'warning',
  error: 'danger',
  unknown: 'neutral',
}

type EditorTarget = { mode: 'create' } | { mode: 'edit'; item: UpstreamItem }

/** 列表里的目标摘要：http 用 URL，stdio 用 command + args。 */
function targetText(item: UpstreamItem): string {
  if (item.transport === 'http') return item.url ?? DASH
  const parts = [item.command ?? DASH, ...item.args]
  return parts.join(' ')
}

/** 整行回传：PUT 是完整 UpstreamInput，切换启用状态时不改 id 与其他字段。 */
function toInput(item: UpstreamItem, enabled: boolean): UpstreamInput {
  return {
    id: item.id,
    name: item.name,
    transport: item.transport,
    url: item.url,
    command: item.command,
    args: item.args,
    env: item.env,
    headers: item.headers,
    group: item.group,
    category: item.category,
    read_only: item.read_only,
    enabled,
    timeout_ms: item.timeout_ms,
  }
}

/** `KEY=值` 每行一条：空行与 # 注释忽略，值里的 `=` 保留。 */
function parseKeyValueLines(
  raw: string,
): { value: Record<string, string> } | { error: string } {
  const value: Record<string, string> = {}
  const lines = raw.split('\n')
  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index]?.trim() ?? ''
    if (line === '' || line.startsWith('#')) continue
    const separator = line.indexOf('=')
    const key = separator === -1 ? '' : line.slice(0, separator).trim()
    if (separator <= 0 || key === '') {
      return { error: `第 ${index + 1} 行不是 KEY=值 形式：${line}` }
    }
    value[key] = line.slice(separator + 1).trim()
  }
  return { value }
}

function formatKeyValueLines(map: Record<string, string>): string {
  return Object.entries(map)
    .map(([key, value]) => `${key}=${value}`)
    .join('\n')
}

function isHttpUrl(raw: string): boolean {
  try {
    const parsed = new URL(raw)
    return parsed.protocol === 'http:' || parsed.protocol === 'https:'
  } catch {
    return false
  }
}

function StatusBadge({ status }: { status: UpstreamStatus }) {
  return (
    <Badge tone={STATUS_TONE[status]} dot title={status}>
      {UPSTREAM_STATUS_LABEL[status]}
    </Badge>
  )
}

export default function UpstreamsPage() {
  const queryClient = useQueryClient()
  const [notice, setNotice] = useState<Notice | undefined>(undefined)
  const [editor, setEditor] = useState<EditorTarget | null>(null)
  const [deleteTarget, setDeleteTarget] = useState<UpstreamItem | null>(null)
  const [deleteError, setDeleteError] = useState<Notice | undefined>(undefined)

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
   * 列宽合计 1120px：名称 180 + 传输 64 + 状态 120 + 目标 220 + 授权组 120 +
   * 分类 88 + 只读 64 + 操作 264，1440 视口下全部可见（操作列仍 sticky 兜底窄屏）。
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
            <StatusBadge status={row.status} />
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
      width: '264px',
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
            minWidth="1120px"
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

/** 新建/编辑共用表单：id 仅新建可填；env/headers 用 `KEY=值` 文本，敏感值保留 env:NAME 引用。 */
function UpstreamForm({
  target,
  groups,
  groupsLoading,
  groupsError,
  onSaved,
  onError,
}: {
  target: EditorTarget
  groups: GroupItem[]
  groupsLoading: boolean
  groupsError: boolean
  onSaved: (item: UpstreamItem) => void
  onError: (notice: Notice) => void
}) {
  const existing = target.mode === 'edit' ? target.item : undefined
  const [id, setId] = useState(existing?.id ?? '')
  const [name, setName] = useState(existing?.name ?? '')
  const [transport, setTransport] = useState<UpstreamTransport>(existing?.transport ?? 'http')
  const [url, setUrl] = useState(existing?.url ?? '')
  const [command, setCommand] = useState(existing?.command ?? '')
  const [argsText, setArgsText] = useState((existing?.args ?? []).join('\n'))
  const [envText, setEnvText] = useState(formatKeyValueLines(existing?.env ?? {}))
  const [headersText, setHeadersText] = useState(formatKeyValueLines(existing?.headers ?? {}))
  const [group, setGroup] = useState(existing?.group ?? '')
  const [category, setCategory] = useState<UpstreamCategory | ''>(existing?.category ?? '')
  const [readOnly, setReadOnly] = useState(existing?.read_only ?? true)
  const [enabled, setEnabled] = useState(existing?.enabled ?? true)
  const [timeoutMs, setTimeoutMs] = useState(String(existing?.timeout_ms ?? 30000))
  const [formError, setFormError] = useState<string | undefined>(undefined)

  /** 编辑中的组若不在当前清单里（例如清单加载失败），以 code 兜底展示，避免保存时静默改组。 */
  const groupChoices = useMemo(() => {
    const choices = groups.map((item) => ({ code: item.code, name: item.name }))
    if (existing !== undefined && !choices.some((choice) => choice.code === existing.group)) {
      choices.push({ code: existing.group, name: existing.group })
    }
    return choices
  }, [groups, existing])

  const mutation = useMutation({
    mutationFn: (input: UpstreamInput) => {
      if (existing === undefined) return createUpstream(input)
      return updateUpstream(existing.id, input)
    },
    onSuccess: onSaved,
    onError: (error) => {
      const notice = noticeFromError(error, existing === undefined ? '新建上游失败' : '更新上游失败')
      setFormError(notice.description)
      onError(notice)
    },
  })

  const submit = () => {
    const trimmedId = existing?.id ?? id.trim()
    const trimmedName = name.trim()

    if (existing === undefined) {
      if (trimmedId === '') {
        setFormError('上游 id 不能为空。')
        return
      }
      if (!/^[a-z][a-z0-9_-]{1,31}$/.test(trimmedId)) {
        setFormError('首字符小写字母，后续只允许小写字母、数字、_、-，长度 2–32。')
        return
      }
      if (trimmedId.includes('__')) {
        setFormError('id 不能包含连续两个下划线（__ 是工具名分隔符）。')
        return
      }
    }
    if (trimmedName === '') {
      setFormError('名称不能为空。')
      return
    }
    if (group === '') {
      setFormError('必须选择一个授权组。')
      return
    }
    if (category === '') {
      setFormError('必须选择一个分类。')
      return
    }

    const parsedTimeout = Number(timeoutMs)
    if (!Number.isInteger(parsedTimeout) || parsedTimeout < 1 || parsedTimeout > 120000) {
      setFormError('超时必须是 1–120000 之间的整数毫秒。')
      return
    }

    if (transport === 'http') {
      if (url.trim() === '') {
        setFormError('HTTP 传输必须填写 URL。')
        return
      }
      if (!isHttpUrl(url.trim())) {
        setFormError('URL 必须是合法的 http/https 地址。')
        return
      }
    } else if (command.trim() === '') {
      setFormError('stdio 传输必须填写 command。')
      return
    }

    const env = parseKeyValueLines(envText)
    if ('error' in env) {
      setFormError(`env：${env.error}`)
      return
    }
    const headers = parseKeyValueLines(headersText)
    if ('error' in headers) {
      setFormError(`headers：${headers.error}`)
      return
    }

    setFormError(undefined)
    mutation.mutate({
      id: trimmedId,
      name: trimmedName,
      transport,
      url: transport === 'http' ? url.trim() : null,
      command: transport === 'stdio' ? command.trim() : null,
      args:
        transport === 'stdio'
          ? argsText
              .split('\n')
              .map((line) => line.trim())
              .filter((line) => line !== '')
          : [],
      env: env.value,
      headers: headers.value,
      group,
      category,
      read_only: readOnly,
      enabled,
      timeout_ms: parsedTimeout,
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
          <span title="id">id</span>
          <input
            className="input mono"
            value={existing?.id ?? id}
            readOnly={existing !== undefined}
            onChange={(event) => setId(event.target.value)}
            placeholder="reporting"
          />
        </label>
        <label className="field">
          <span title="name">名称</span>
          <input
            className="input"
            value={name}
            onChange={(event) => setName(event.target.value)}
            placeholder="例如 报表分析"
          />
        </label>
      </div>

      <div className="form-row">
        <label className="field">
          <span title="transport">传输</span>
          <select
            className="select"
            value={transport}
            onChange={(event) => setTransport(event.target.value as UpstreamTransport)}
          >
            <option value="http">{UPSTREAM_TRANSPORT_LABEL.http}</option>
            <option value="stdio">{UPSTREAM_TRANSPORT_LABEL.stdio}</option>
          </select>
        </label>
        <label className="field">
          <span title="group">
            授权组 <span className="required-mark">*</span>
          </span>
          <select
            className="select"
            value={group}
            onChange={(event) => setGroup(event.target.value)}
            disabled={groupsLoading}
            required
          >
            <option value="">
              {groupsLoading
                ? '正在加载组…'
                : groupsError
                  ? '组清单加载失败'
                  : '请选择组'}
            </option>
            {groupChoices.map((item) => (
              <option key={item.code} value={item.code} title={item.code}>
                {item.name}
              </option>
            ))}
          </select>
        </label>
      </div>
      {groupsError && (
        <p className="hint">组清单加载失败：授权组下拉不可用，请修复后重试。</p>
      )}

      {transport === 'http' ? (
        <label className="field">
          <span title="url">URL</span>
          <input
            className="input mono"
            value={url}
            onChange={(event) => setUrl(event.target.value)}
            placeholder="https://example.com/mcp"
          />
        </label>
      ) : (
        <>
          <label className="field">
            <span title="command">command</span>
            <input
              className="input mono"
              value={command}
              onChange={(event) => setCommand(event.target.value)}
              placeholder="例如 npx"
            />
          </label>
          <label className="field">
            <span title="args">args（每行一个参数）</span>
            <textarea
              className="input textarea"
              rows={3}
              value={argsText}
              onChange={(event) => setArgsText(event.target.value)}
              placeholder={'-y\n@modelcontextprotocol/server-filesystem\n/tmp'}
            />
          </label>
        </>
      )}

      <div className="form-row">
        <label className="field">
          <span>
            分类 <span className="required-mark">*</span>
          </span>
          <select
            className="select"
            value={category}
            onChange={(event) => setCategory(event.target.value as UpstreamCategory | '')}
            required
          >
            <option value="">请选择分类</option>
            {CATEGORY_OPTIONS.map((value) => (
              <option key={value} value={value}>
                {UPSTREAM_CATEGORY_LABEL[value]}
              </option>
            ))}
          </select>
        </label>
        <label className="field">
          <span title="timeout_ms">超时（ms）</span>
          <input
            className="input"
            type="number"
            min={1}
            max={120000}
            step={1}
            value={timeoutMs}
            onChange={(event) => setTimeoutMs(event.target.value)}
          />
        </label>
      </div>

      <label className="field">
        <span title="env">env（每行 KEY=值；敏感值用 env:NAME 引用环境变量）</span>
        <textarea
          className="input textarea"
          rows={3}
          value={envText}
          onChange={(event) => setEnvText(event.target.value)}
          placeholder={'API_KEY=env:UPSTREAM_API_KEY\nDEBUG=true'}
        />
      </label>
      <label className="field">
        <span title="headers">headers（每行 KEY=值；Authorization/Cookie 已脱敏为 ***，原样回传即保持原值）</span>
        <textarea
          className="input textarea"
          rows={3}
          value={headersText}
          onChange={(event) => setHeadersText(event.target.value)}
          placeholder={'Authorization=Bearer env:UPSTREAM_TOKEN\nX-Tenant=jumcp'}
        />
      </label>
      <p className="hint">
        敏感值建议写成 <span className="mono">env:NAME</span>，避免明文落库；GET 响应中的敏感头显示为{' '}
        <span className="mono">***</span>，编辑时原样回传即保持原值。
      </p>

      <div className="form-row">
        <label className="check-item" title="只读上游不允许写操作；写上游还需部署开关">
          <input
            type="checkbox"
            checked={readOnly}
            onChange={(event) => setReadOnly(event.target.checked)}
          />
          <span>只读</span>
          <span className="hint">写上游还需部署开关</span>
        </label>
        <label className="check-item">
          <input
            type="checkbox"
            checked={enabled}
            onChange={(event) => setEnabled(event.target.checked)}
          />
          <span>启用</span>
        </label>
      </div>

      {formError !== undefined && (
        <p className="inline-error" role="alert">
          {formError}
        </p>
      )}
      <div className="form-actions">
        <button type="submit" className="btn btn-primary" disabled={mutation.isPending}>
          {mutation.isPending ? '保存中…' : existing === undefined ? '创建接入' : '保存修改'}
        </button>
      </div>
    </form>
  )
}
