import { useMemo, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  deleteAccessToken,
  fetchAccessTokens,
  fetchTokenOptions,
  reissueAccessToken,
  revokeAccessToken,
} from '../../api/admin'
import { indexGroups, indexRoles, useGroupsQuery, useRolesQuery } from '../../api/queries'
import type {
  AccessTokenCreated,
  AccessTokenItem,
  AccessTokenStatus,
  TokenBuiltinToolOption,
} from '../../api/types'
import { Card } from '../../components/Card'
import { InfoBar } from '../../components/InfoBar'
import { EmptyState, ErrorState, LoadingState, PageShell } from '../../components/PageShell'
import { noticeFromError, type Notice } from '../../notice'
import { TokenCard } from './TokenCard'
import { TokenDialogs } from './TokenDialogs'
import {
  STATUS_FILTERS,
  copyToClipboard,
  mcpConfigJson,
  tokenDeleteError,
} from './tokenConfig'

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
      // 后端新增状态值时不让计数变成 NaN；未知状态仍按原值出现在卡片状态徽标里。
      counts[item.status] = (counts[item.status] ?? 0) + 1
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

      <TokenDialogs
        createOpen={createOpen}
        onCloseCreate={() => setCreateOpen(false)}
        options={options}
        roles={roles}
        groups={groupsQuery.data?.items ?? []}
        onCreated={(result) => {
          setCreateOpen(false)
          setCreated(result)
          setKnownSecrets((current) => ({ ...current, [result.item.id]: result.token }))
          refreshTokens()
        }}
        created={created}
        onCloseCreated={() => {
          setCreated(null)
          refreshTokens()
        }}
        deleteTarget={deleteTarget}
        deleteError={deleteError}
        deletePending={deleteMutation.isPending}
        onDelete={(item) => deleteMutation.mutate(item)}
        onCloseDelete={() => setDeleteTarget(null)}
        revokeTarget={revokeTarget}
        revokePending={revokeMutation.isPending}
        onRevoke={(id) => revokeMutation.mutate(id)}
        onCloseRevoke={() => setRevokeTarget(null)}
        reissueTarget={reissueTarget}
        reissuePending={reissueMutation.isPending}
        onReissue={(item) => reissueMutation.mutate(item)}
        onCloseReissue={() => setReissueTarget(null)}
      />
    </PageShell>
  )
}
