import { useMemo, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  createAccessToken,
  deleteAccessToken,
  fetchAccessTokens,
  fetchTokenOptions,
  reissueAccessToken,
  revokeAccessToken,
} from '../api/admin'
import { ApiError } from '../api/client'
import { groupLabel, indexGroups, indexRoles, roleLabel, useGroupsQuery, useRolesQuery } from '../api/queries'
import type {
  AccessTokenCreateInput,
  AccessTokenCreated,
  AccessTokenItem,
  AccessTokenStatus,
  GroupItem,
  RoleItem,
  TokenBuiltinToolOption,
  TokenOptions,
} from '../api/types'
import { Badge, type BadgeTone } from '../components/Badge'
import { Card } from '../components/Card'
import { InfoBar } from '../components/InfoBar'
import { Modal } from '../components/Modal'
import { EmptyState, ErrorState, LoadingState, PageShell } from '../components/PageShell'
import { formatDateTime, localInputToIso } from '../format'
import { GROUP_SOURCE_LABEL, TOKEN_STATUS_LABEL } from '../labels'
import { noticeFromError, type Notice } from '../notice'
import { useAdminView } from '../viewStack'

const DASH = '—'

/** MCP 配置键固定为产品名：客户端按这个名字识别服务，不随令牌名变化。 */
const MCP_CONFIG_KEY = 'jumcp'

/**
 * HTTP Streamable 接入配置（一键复制）。
 * URL 用当前页面同源 + `/mcp`：生产由网关同源转发，开发期走 Vite `/mcp` 代理。
 * 明文只在内存里拼进 JSON，不写 localStorage、不落任何持久存储。
 */
function mcpConfigJson(token: string): string {
  return JSON.stringify(
    {
      mcpServers: {
        [MCP_CONFIG_KEY]: {
          type: 'http',
          url: new URL('/mcp', window.location.origin).toString(),
          headers: { Authorization: `Bearer ${token}` },
        },
      },
    },
    null,
    2,
  )
}

/** 剪贴板写入：失败返回 false（HTTP 非安全上下文/权限拒绝），由调用方提示。 */
async function copyToClipboard(value: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(value)
    return true
  } catch {
    return false
  }
}

/** 三个元工具默认勾选；admin_* 与上游工具按需选择。 */
const DEFAULT_BUILTIN_TOOLS = [
  'search_capabilities',
  'describe_capability',
  'list_groups',
]

const STATUS_TONE: Record<string, BadgeTone> = {
  active: 'success',
  revoked: 'danger',
  expired: 'warning',
}

/** 状态过滤只影响客户端展示：列表本来就是全量，切状态不打后端。 */
const STATUS_FILTERS: Array<{ value: '' | AccessTokenStatus; label: string }> = (
  ['', 'active', 'revoked', 'expired'] as const
).map((value) => ({
  value,
  label: value === '' ? '全部' : (TOKEN_STATUS_LABEL[value] ?? value),
}))

function fieldValue(value: string | null | undefined): string {
  return value === null || value === undefined || value === '' ? DASH : value
}

function toggle(list: string[], value: string, checked: boolean): string[] {
  if (checked) return list.includes(value) ? list : [...list, value]
  return list.filter((item) => item !== value)
}

/**
 * 上游工具名：每行一个 `<server_id>__<tool>`；trim、去空行、去重。
 * 非法行就地报错，调用方不得继续发请求。
 */
function parseUpstreamToolNames(
  raw: string,
): { names: string[] } | { error: string } {
  const names: string[] = []
  const lines = raw.split('\n')
  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index]?.trim() ?? ''
    if (line === '') continue
    if (!/^[A-Za-z0-9_-]+__[A-Za-z0-9_-]+$/.test(line)) {
      return { error: `第 ${index + 1} 行不是 <server_id>__<tool> 形式：${line}` }
    }
    if (line.length > 64) {
      return { error: `第 ${index + 1} 行超过 64 字符：${line}` }
    }
    if (!names.includes(line)) names.push(line)
  }
  return { names }
}

/** 409 表示令牌仍在生效：后端只允许删除已撤销 / 已过期记录，提示先撤销。 */
function tokenDeleteError(error: unknown): Notice {
  if (error instanceof ApiError && error.status === 409) {
    const data = error.data as Record<string, unknown> | undefined
    const hint =
      typeof data?.hint === 'string' && data.hint.trim() !== ''
        ? data.hint.trim()
        : '先撤销令牌再删除'
    return {
      severity: 'danger',
      title: '令牌仍在生效',
      description: `只有已撤销或已过期的令牌可以删除：${hint}。`,
      meta: `${error.code} · HTTP 409`,
    }
  }
  return noticeFromError(error, '删除令牌失败')
}

export default function TokensPage() {
  const queryClient = useQueryClient()
  const [notice, setNotice] = useState<Notice | undefined>(undefined)
  const [createOpen, setCreateOpen] = useState(false)
  const [created, setCreated] = useState<AccessTokenCreated | null>(null)
  const [revokeTarget, setRevokeTarget] = useState<AccessTokenItem | null>(null)
  const [reissueTarget, setReissueTarget] = useState<AccessTokenItem | null>(null)
  /** 本页已知的明文：只来自创建/重签发的响应，刷新即丢（绝不落 localStorage）。 */
  const [knownSecrets, setKnownSecrets] = useState<Record<string, string>>({})
  const [deleteTarget, setDeleteTarget] = useState<AccessTokenItem | null>(null)
  const [deleteError, setDeleteError] = useState<Notice | undefined>(undefined)
  const [statusFilter, setStatusFilter] = useState<'' | AccessTokenStatus>('')

  const tokensQuery = useQuery({
    queryKey: ['access-tokens'],
    queryFn: ({ signal }) => fetchAccessTokens(signal),
  })
  const optionsQuery = useQuery({
    queryKey: ['token-options'],
    queryFn: ({ signal }) => fetchTokenOptions(signal),
  })
  const groupsQuery = useGroupsQuery()
  const rolesQuery = useRolesQuery()

  const tokens = tokensQuery.data?.items ?? []
  const options = optionsQuery.data
  const roles = rolesQuery.data?.items ?? []
  const groupIndex = useMemo(() => indexGroups(groupsQuery.data), [groupsQuery.data])
  const roleIndex = useMemo(() => indexRoles(rolesQuery.data), [rolesQuery.data])

  const labelByTool = useMemo(() => {
    const map = new Map<string, TokenBuiltinToolOption>()
    for (const tool of options?.builtin_tools ?? []) map.set(tool.name, tool)
    return map
  }, [options])

  const statusCounts = useMemo(() => {
    const counts: Record<'' | AccessTokenStatus, number> = {
      '': 0,
      active: 0,
      revoked: 0,
      expired: 0,
    }
    for (const item of tokensQuery.data?.items ?? []) {
      counts[''] += 1
      counts[item.status] += 1
    }
    return counts
  }, [tokensQuery.data])

  const visibleTokens = useMemo(() => {
    const items = tokensQuery.data?.items ?? []
    return statusFilter === '' ? items : items.filter((item) => item.status === statusFilter)
  }, [tokensQuery.data, statusFilter])

  /** 令牌写操作会改变组的 token_count，组清单与选项一起失效。 */
  const refreshTokens = () => {
    void queryClient.invalidateQueries({ queryKey: ['access-tokens'] })
    void queryClient.invalidateQueries({ queryKey: ['token-options'] })
    void queryClient.invalidateQueries({ queryKey: ['groups'] })
  }

  /** 复制 MCP 接入配置：成功/失败都回到页面级 InfoBar（明文只在这一刻进剪贴板）。 */
  const copyMcpConfig = async (item: AccessTokenItem, token: string) => {
    const copied = await copyToClipboard(mcpConfigJson(token))
    setNotice(
      copied
        ? {
            severity: 'success',
            title: 'MCP 接入配置已复制',
            description: `${item.name} 的 HTTP 接入 JSON 已写入剪贴板，可直接粘贴到 MCP 客户端配置。`,
            meta: undefined,
          }
        : {
            severity: 'danger',
            title: '复制失败',
            description:
              '浏览器拒绝写入剪贴板：请确认页面在 HTTPS 或本机地址下，并允许剪贴板权限后重试。',
            meta: undefined,
          },
    )
  }

  /**
   * 卡片上的「MCP 接入」：本页已知明文就直接复制；否则只能重新签发。
   * 已存记录只有 sha256，绝不存在读取/回显已有明文的接口。
   */
  const openMcp = (item: AccessTokenItem) => {
    const token = knownSecrets[item.id]
    if (token !== undefined) {
      void copyMcpConfig(item, token)
      return
    }
    setNotice(undefined)
    setReissueTarget(item)
  }

  const reissueMutation = useMutation({
    mutationFn: (item: AccessTokenItem) => reissueAccessToken(item.id),
    onSuccess: (result) => {
      setReissueTarget(null)
      setKnownSecrets((current) => ({ ...current, [result.item.id]: result.token }))
      void copyMcpConfig(result.item, result.token)
      refreshTokens()
    },
    onError: (error) => {
      setReissueTarget(null)
      setNotice(noticeFromError(error, '重新签发令牌失败'))
      refreshTokens()
    },
  })

  const revokeMutation = useMutation({
    mutationFn: (id: string) => revokeAccessToken(id),
    onSuccess: (item) => {
      setRevokeTarget(null)
      setNotice({
        severity: 'success',
        title: '令牌已撤销',
        description: `${item.name}（${item.token_prefix}）已撤销，其 MCP 请求立即失败。`,
        meta: undefined,
      })
      refreshTokens()
    },
    onError: (error) => {
      const failure = noticeFromError(error, '撤销令牌失败')
      setNotice(failure)
    },
  })

  /** 删除是硬删除：记录从列表消失，token 立即失效，但审计事件保留。 */
  const deleteMutation = useMutation({
    mutationFn: (item: AccessTokenItem) => deleteAccessToken(item.id),
    onSuccess: (_result, item) => {
      setDeleteTarget(null)
      setDeleteError(undefined)
      setNotice({
        severity: 'success',
        title: '令牌记录已删除',
        description: `${item.name}（${item.token_prefix}）已从列表移除，该 token 立即失效；审计事件保留。`,
        meta: undefined,
      })
      refreshTokens()
    },
    onError: (error) => setDeleteError(tokenDeleteError(error)),
  })

  const optionsError =
    optionsQuery.isError
      ? noticeFromError(optionsQuery.error, '令牌选项加载失败')
      : undefined

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

      {optionsError !== undefined && (
        <InfoBar
          severity={optionsError.severity}
          title={optionsError.title}
          description={optionsError.description}
          meta={optionsError.meta}
        >
          <button
            type="button"
            className="btn btn-secondary btn-sm"
            onClick={() => optionsQuery.refetch()}
            disabled={optionsQuery.isFetching}
          >
            重试
          </button>
        </InfoBar>
      )}

      <Card
        subtitle={
          tokensQuery.data === undefined
            ? tokensQuery.isError
              ? '查询失败'
              : '查询中'
            : `${tokensQuery.data.total} 个令牌`
        }
        actions={
          <button
            type="button"
            className="btn btn-primary btn-sm"
            disabled={options === undefined || rolesQuery.isPending}
            onClick={() => setCreateOpen(true)}
          >
            生成令牌
          </button>
        }
        flush
      >
        <div className="token-usage">
          <span className="token-usage-label">接入方式</span>
          <code className="code-chip">POST /mcp</code>
          <code className="code-chip">Authorization: Bearer &lt;token&gt;</code>
        </div>
        <div className="token-usage">
          <span className="token-usage-label">状态</span>
          <div className="segmented" role="group" aria-label="令牌状态过滤">
            {STATUS_FILTERS.map((option) => (
              <button
                key={option.value}
                type="button"
                aria-pressed={statusFilter === option.value}
                onClick={() => setStatusFilter(option.value)}
              >
                {option.label}
                <span className="segmented-count">{statusCounts[option.value]}</span>
              </button>
            ))}
          </div>
          <span className="hint token-badge-legend">橙色徽标为管理类工具</span>
        </div>

        {tokensQuery.isPending && <LoadingState rows={6} label="正在加载访问令牌" />}
        {tokensQuery.isError && <ErrorState error={tokensQuery.error} />}
        {tokensQuery.data !== undefined && tokens.length === 0 && (
          <EmptyState
            hint="还没有访问令牌。生成后明文只展示一次，后端仅保存 hash 与前缀。"
            action={
              <button
                type="button"
                className="btn btn-secondary"
                disabled={options === undefined}
                onClick={() => setCreateOpen(true)}
              >
                生成令牌
              </button>
            }
          />
        )}
        {tokens.length > 0 && visibleTokens.length === 0 && (
          <EmptyState
            hint={`当前没有「${
              STATUS_FILTERS.find((option) => option.value === statusFilter)?.label ?? ''
            }」状态的令牌。`}
            action={
              <button
                type="button"
                className="btn btn-secondary"
                onClick={() => setStatusFilter('')}
              >
                清空状态过滤
              </button>
            }
          />
        )}
        {visibleTokens.length > 0 && (
          <div className="token-grid">
            {visibleTokens.map((item) => (
              <TokenCard
                key={item.id}
                item={item}
                labelByTool={labelByTool}
                groupIndex={groupIndex}
                roleIndex={roleIndex}
                secretKnown={knownSecrets[item.id] !== undefined}
                onMcp={() => openMcp(item)}
                onRevoke={() => {
                  setNotice(undefined)
                  setRevokeTarget(item)
                }}
                onDelete={() => {
                  setNotice(undefined)
                  setDeleteError(undefined)
                  setDeleteTarget(item)
                }}
              />
            ))}
          </div>
        )}
      </Card>

      {createOpen && options !== undefined && (
        <Modal
          size="lg"
          title="生成访问令牌"
          onClose={() => setCreateOpen(false)}
        >
          <TokenForm
            options={options}
            roles={roles}
            groups={groupsQuery.data?.items ?? []}
            onCreated={(result) => {
              setCreateOpen(false)
              setCreated(result)
              setKnownSecrets((current) => ({ ...current, [result.item.id]: result.token }))
              refreshTokens()
            }}
          />
        </Modal>
      )}

      {created !== null && (
        <Modal
          size="lg"
          title="令牌已创建"
          onClose={() => {
            setCreated(null)
            refreshTokens()
          }}
          footer={
            <>
              <CopyButton
                value={mcpConfigJson(created.token)}
                label="复制 MCP 接入配置"
                className="btn btn-secondary"
              />
              <button
                type="button"
                className="btn btn-primary"
                onClick={() => {
                  setCreated(null)
                  refreshTokens()
                }}
              >
                我已保存，关闭
              </button>
            </>
          }
        >
          <div className="token-created">
            <InfoBar
              severity="warning"
              title="明文仅展示一次"
              description="关闭后无法再次查看，请立即复制明文与 MCP 接入配置并妥善保存。"
            />
            <div className="token-secret" role="group" aria-label="令牌明文">
              <code>{created.token}</code>
              <CopyButton value={created.token} />
            </div>
            <p className="hint">
              调用方以该 token 作为 MCP Bearer 凭证。可直接复制 HTTP 协议配置（服务名
              <span className="mono"> {MCP_CONFIG_KEY} </span>，URL 为本页同源的
              <span className="mono"> /mcp</span>）粘贴到 MCP 客户端。
            </p>
          </div>
        </Modal>
      )}

      {deleteTarget !== null && (
        <Modal
          size="md"
          title={`删除令牌记录 · ${deleteTarget.name}`}
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
            记录将从列表移除且不可恢复；该 token 立即失效；审计事件保留。前缀：
            <span className="mono">{deleteTarget.token_prefix}</span> · 当前状态：
            {TOKEN_STATUS_LABEL[deleteTarget.status] ?? deleteTarget.status}
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

      {revokeTarget !== null && (
        <Modal
          size="md"
          title={`撤销令牌 · ${revokeTarget.name}`}
          onClose={() => setRevokeTarget(null)}
          footer={
            <>
              <button
                type="button"
                className="btn btn-secondary"
                disabled={revokeMutation.isPending}
                onClick={() => setRevokeTarget(null)}
              >
                取消
              </button>
              <button
                type="button"
                className="btn btn-danger"
                disabled={revokeMutation.isPending}
                onClick={() => revokeMutation.mutate(revokeTarget.id)}
              >
                {revokeMutation.isPending ? '撤销中…' : '确认撤销'}
              </button>
            </>
          }
        >
          <p className="hint">
            撤销后该 token 的 MCP 请求立即失败，且不可恢复。前缀：
            <span className="mono">{revokeTarget.token_prefix}</span>
          </p>
        </Modal>
      )}

      {reissueTarget !== null && (
        <Modal
          size="md"
          title={`重新签发令牌 · ${reissueTarget.name}`}
          onClose={() => setReissueTarget(null)}
          footer={
            <>
              <button
                type="button"
                className="btn btn-secondary"
                disabled={reissueMutation.isPending}
                onClick={() => setReissueTarget(null)}
              >
                取消
              </button>
              <button
                type="button"
                className="btn btn-primary"
                disabled={reissueMutation.isPending}
                onClick={() => reissueMutation.mutate(reissueTarget)}
              >
                {reissueMutation.isPending ? '重新签发中…' : '确认重新签发'}
              </button>
            </>
          }
        >
          <div className="token-created">
            <InfoBar
              severity="warning"
              title="明文不可再次读取"
              description="后端只保存 sha256，无法回显该令牌的明文；重新签发会生成新的明文，旧 token 立即失效。"
            />
            <p className="hint">
              同一记录原地换钥匙：名称、角色、组、工具授权与到期时间保持不变，仅
              token、哈希与前缀更新，最近使用时间清空。前缀：
              <span className="mono">{reissueTarget.token_prefix}</span>
            </p>
            <p className="hint">
              确认后将自动复制最新的 HTTP MCP 接入配置（服务名
              <span className="mono"> {MCP_CONFIG_KEY} </span>），新明文只在本页内存中保留。
            </p>
          </div>
        </Modal>
      )}
    </PageShell>
  )
}

function TokenCard({
  item,
  labelByTool,
  groupIndex,
  roleIndex,
  secretKnown,
  onMcp,
  onRevoke,
  onDelete,
}: {
  item: AccessTokenItem
  labelByTool: Map<string, TokenBuiltinToolOption>
  groupIndex: Map<string, GroupItem>
  roleIndex: Map<string, RoleItem>
  secretKnown: boolean
  onMcp: () => void
  onRevoke: () => void
  onDelete: () => void
}) {
  const { openAudit } = useAdminView()
  const builtinTools = item.builtin_tools ?? []
  return (
    <article className="token-card">
      <header className="token-card-head">
        <div className="token-card-title">
          <strong>{item.name}</strong>
        </div>
        <Badge tone={STATUS_TONE[item.status] ?? 'neutral'} title={item.status} dot>
          {TOKEN_STATUS_LABEL[item.status] ?? item.status}
        </Badge>
      </header>

      <dl className="token-fields">
        <div className="token-field">
          <dt>令牌前缀</dt>
          <dd className="mono">{item.token_prefix}</dd>
        </div>
        <div className="token-field">
          <dt>调用者</dt>
          <dd>{fieldValue(item.caller)}</dd>
        </div>
        <div className="token-field">
          <dt>来源</dt>
          <dd>{fieldValue(item.source)}</dd>
        </div>
        <div className="token-field">
          <dt>角色</dt>
          <dd title={item.role}>{roleLabel(roleIndex, item.role)}</dd>
        </div>
        <div className="token-field">
          <dt>最近使用</dt>
          <dd className="mono">{formatDateTime(item.last_used_at)}</dd>
        </div>
        <div className="token-field">
          <dt>到期</dt>
          <dd className="mono">{item.expires_at === null ? '永不过期' : formatDateTime(item.expires_at)}</dd>
        </div>
        <div className="token-field">
          <dt>创建时间</dt>
          <dd className="mono">{formatDateTime(item.created_at)}</dd>
        </div>
        <div className="token-field token-field-wide">
          <dt>组</dt>
          <dd>
            {item.groups.length === 0 ? (
              DASH
            ) : (
              <span className="token-badges">
                {item.groups.map((code) => (
                  <Badge key={code} tone="accent" title={code}>
                    {groupLabel(groupIndex, code)}
                  </Badge>
                ))}
              </span>
            )}
          </dd>
        </div>
        <div className="token-field token-field-wide">
          <dt>工具 allowlist</dt>
          <dd>
            {builtinTools.length === 0 ? (
              DASH
            ) : (
              <span className="token-badges">
                {builtinTools.map((name) => {
                  const option = labelByTool.get(name)
                  return (
                    <Badge
                      key={name}
                      tone={option?.group === 'admin' ? 'warning' : 'neutral'}
                      title={name}
                    >
                      {option?.label ?? name}
                    </Badge>
                  )
                })}
              </span>
            )}
          </dd>
        </div>
      </dl>

      <footer className="token-card-foot">
        {item.status === 'revoked' && (
          <span className="hint">
            {item.revoked_at === null ? '已撤销' : `撤销于 ${formatDateTime(item.revoked_at)}`}
            ，不能接入 MCP
          </span>
        )}
        {item.status === 'expired' && (
          <span className="hint">
            已过期，{formatDateTime(item.expires_at)} 之后不再接受请求，不能接入 MCP
          </span>
        )}
        <div className="row-actions">
          <button
            type="button"
            className="btn btn-secondary btn-sm"
            disabled={item.status !== 'active'}
            title={
              item.status !== 'active'
                ? '已撤销或已过期的令牌不能接入 MCP'
                : secretKnown
                  ? '复制 HTTP 协议的 MCP JSON 配置'
                  : '明文仅保存哈希、无法回读：将先重新签发该令牌再复制配置'
            }
            onClick={onMcp}
          >
            MCP 接入
          </button>
          <button
            type="button"
            className="btn btn-secondary btn-sm"
            onClick={() => openAudit({ tokenId: item.id })}
          >
            查看调用
          </button>
          {item.status === 'active' ? (
            <button type="button" className="btn btn-danger btn-sm" onClick={onRevoke}>
              撤销
            </button>
          ) : (
            <button type="button" className="btn btn-danger btn-sm" onClick={onDelete}>
              删除记录
            </button>
          )}
        </div>
      </footer>
    </article>
  )
}

function TokenForm({
  options,
  roles,
  groups: groupOptions,
  onCreated,
}: {
  options: TokenOptions
  roles: RoleItem[]
  groups: GroupItem[]
  onCreated: (result: AccessTokenCreated) => void
}) {
  const [name, setName] = useState('')
  const [caller, setCaller] = useState('')
  const [source, setSource] = useState('')
  const [role, setRole] = useState('')
  const [groups, setGroups] = useState<string[]>([])
  const [builtinTools, setBuiltinTools] = useState<string[]>(() =>
    options.builtin_tools
      .filter((tool) => tool.group === 'basic' && DEFAULT_BUILTIN_TOOLS.includes(tool.name))
      .map((tool) => tool.name),
  )
  const [expiresAt, setExpiresAt] = useState('')
  const [upstreamToolsText, setUpstreamToolsText] = useState('')
  const [formError, setFormError] = useState<string | undefined>(undefined)

  const selectedRole = roles.find((item) => item.code === role)
  const adminSelected = groups.includes('admin')
  /** basic 等 grantable=false 组是内置通道，不能写进令牌 groups；UI 不提供勾选。 */
  const selectableGroups = groupOptions.filter((group) => group.grantable !== false)

  const mutation = useMutation({
    mutationFn: (input: AccessTokenCreateInput) => createAccessToken(input),
    onSuccess: onCreated,
    onError: (error) => {
      const failure = noticeFromError(error, '生成令牌失败')
      setFormError(`${failure.description}${failure.meta === undefined ? '' : `（${failure.meta}）`}`)
    },
  })

  const pickRole = (nextRole: string) => {
    setRole(nextRole)
    const defaultGroups = roles.find((item) => item.code === nextRole)?.default_groups ?? []
    const known = new Set(selectableGroups.map((group) => group.code))
    setGroups(defaultGroups.filter((group) => known.has(group)))
  }

  const pickGroup = (groupCode: string, checked: boolean) => {
    const next = toggle(groups, groupCode, checked)
    setGroups(next)
    if (groupCode === 'admin' && !checked) {
      const adminTools = new Set(
        options.builtin_tools.filter((tool) => tool.group === 'admin').map((tool) => tool.name),
      )
      setBuiltinTools((current) => current.filter((tool) => !adminTools.has(tool)))
    }
  }

  const submit = () => {
    const trimmedName = name.trim()
    const trimmedCaller = caller.trim()
    const trimmedSource = source.trim()
    if (trimmedName === '') {
      setFormError('名称不能为空。')
      return
    }
    if (trimmedCaller === '') {
      setFormError('调用者不能为空。')
      return
    }
    if (trimmedSource === '') {
      setFormError('来源不能为空。')
      return
    }
    if (role === '') {
      setFormError('请选择角色。')
      return
    }
    const upstreamTools = parseUpstreamToolNames(upstreamToolsText)
    if ('error' in upstreamTools) {
      setFormError(`上游工具名：${upstreamTools.error}`)
      return
    }
    setFormError(undefined)
    mutation.mutate({
      name: trimmedName,
      role,
      groups,
      builtin_tools: [...new Set([...builtinTools, ...upstreamTools.names])],
      source: trimmedSource,
      caller: trimmedCaller,
      expires_at: localInputToIso(expiresAt) ?? null,
    })
  }

  const builtinOptions = options.builtin_tools.filter((tool) => tool.group === 'basic')
  const adminOptions = options.builtin_tools.filter((tool) => tool.group === 'admin')

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
          <span>名称</span>
          <input
            className="input"
            value={name}
            onChange={(event) => setName(event.target.value)}
            placeholder="例如 数据组只读接入"
          />
        </label>
        <label className="field">
          <span>调用者</span>
          <input
            className="input"
            value={caller}
            onChange={(event) => setCaller(event.target.value)}
            placeholder="例如 claude-code"
          />
        </label>
        <label className="field">
          <span>来源</span>
          <input
            className="input"
            value={source}
            onChange={(event) => setSource(event.target.value)}
            placeholder="例如 mcp"
          />
        </label>
      </div>

      <div className="form-row">
        <label className="field">
          <span>角色</span>
          <select
            className="select"
            value={role}
            onChange={(event) => pickRole(event.target.value)}
          >
            <option value="">请选择角色</option>
            {roles.map((item) => (
              <option key={item.code} value={item.code} title={item.code}>
                {item.name}
              </option>
            ))}
          </select>
        </label>
        <label className="field">
          <span>到期时间</span>
          <input
            className="input"
            type="datetime-local"
            value={expiresAt}
            onChange={(event) => setExpiresAt(event.target.value)}
          />
        </label>
      </div>

      {selectedRole !== undefined && (
        <p className="hint">角色默认预算 {selectedRole.default_budget}</p>
      )}

      <fieldset className="check-fieldset">
        <legend>组</legend>
        <div className="check-grid">
          {selectableGroups.map((group) => (
            <label key={group.code} className="check-item" title={group.code}>
              <input
                type="checkbox"
                checked={groups.includes(group.code)}
                onChange={(event) => pickGroup(group.code, event.target.checked)}
              />
              <span>{group.name}</span>
              <Badge tone={group.source === 'policy' ? 'accent' : 'neutral'} title={group.source}>
                {GROUP_SOURCE_LABEL[group.source]}
              </Badge>
            </label>
          ))}
        </div>
      </fieldset>

      <fieldset className="check-fieldset">
        <legend>内置工具</legend>
        <div className="check-grid">
          {builtinOptions.map((tool) => (
            <label key={tool.name} className="check-item" title={tool.name}>
              <input
                type="checkbox"
                checked={builtinTools.includes(tool.name)}
                onChange={(event) =>
                  setBuiltinTools((current) =>
                    toggle(current, tool.name, event.target.checked),
                  )
                }
              />
              <span>{tool.label}</span>
            </label>
          ))}
        </div>
        {adminSelected && adminOptions.length > 0 && (
          <div className="check-grid">
            {adminOptions.map((tool) => (
              <label key={tool.name} className="check-item" title={tool.name}>
                <input
                  type="checkbox"
                  checked={builtinTools.includes(tool.name)}
                  onChange={(event) =>
                    setBuiltinTools((current) =>
                      toggle(current, tool.name, event.target.checked),
                    )
                  }
                />
                <span>{tool.label}</span>
                <Badge tone="warning">管理</Badge>
              </label>
            ))}
          </div>
        )}
      </fieldset>

      <label className="field">
        <span>
          上游工具名（每行一个 &lt;server_id&gt;__&lt;tool&gt;）
        </span>
        <textarea
          className="input textarea"
          rows={3}
          value={upstreamToolsText}
          onChange={(event) => setUpstreamToolsText(event.target.value)}
          placeholder={'reporting__daily_summary'}
        />
      </label>
      <p className="hint">
        名字可从 tools/list 或 MCP 接入后的工具列表获取；与上方勾选的内置工具合并提交。
      </p>

      {formError !== undefined && (
        <p className="inline-error" role="alert">
          {formError}
        </p>
      )}
      <div className="form-actions">
        <button type="submit" className="btn btn-primary" disabled={mutation.isPending}>
          {mutation.isPending ? '生成中…' : '生成令牌'}
        </button>
      </div>
    </form>
  )
}

function CopyButton({
  value,
  label = '复制',
  copiedLabel = '已复制',
  className = 'btn btn-secondary btn-sm',
}: {
  value: string
  label?: string
  copiedLabel?: string
  className?: string
}) {
  const [text, setText] = useState(label)
  const copy = async () => {
    const copied = await copyToClipboard(value)
    setText(copied ? copiedLabel : '复制失败')
  }
  return (
    <button type="button" className={className} onClick={() => void copy()}>
      {text}
    </button>
  )
}
