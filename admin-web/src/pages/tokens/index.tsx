import { gsap } from 'gsap'
import { useMemo, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  deleteAccessToken,
  fetchAccessTokens,
  reissueAccessToken,
  revokeAccessToken,
} from '../../api/admin'
import { indexGroups, indexRoles, useGroupsQuery, useRolesQuery } from '../../api/queries'
import type { AccessTokenCreated, AccessTokenItem, AccessTokenStatus } from '../../api/types'
import { Card } from '../../components/Card'
import { InfoBar } from '../../components/InfoBar'
import { EmptyState, ErrorState, LoadingState, PageShell } from '../../components/PageShell'
import { MOTION_DURATION, MOTION_EASE, useGsapScope } from '../../motion/useGsap'
import { noticeMessage, type Notice } from '../../notice'
import { TokenCard } from './TokenCard'
import { TokenDialogs } from './TokenDialogs'
import {
  STATUS_FILTERS,
  copyToClipboard,
  mcpConfigJson,
  tokenDeleteMessage,
} from './tokenConfig'

export default function TokensPage() {
  const queryClient = useQueryClient()
  const [notice, setNotice] = useState<Notice | undefined>(undefined)
  const [createOpen, setCreateOpen] = useState(false)
  const [created, setCreated] = useState<AccessTokenCreated | null>(null)
  const [editTarget, setEditTarget] = useState<AccessTokenItem | null>(null)
  const [revokeTarget, setRevokeTarget] = useState<AccessTokenItem | null>(null)
  const [reissueTarget, setReissueTarget] = useState<AccessTokenItem | null>(null)
  /**
   * 凭据明文内存暂存：
   * 仅来自令牌创建或重新签发的响应体，刷新页面或跳转后立即丢弃；
   * 严禁持久化至 localStorage 或 sessionStorage，杜绝长期留存凭据。
   */
  const [knownSecrets, setKnownSecrets] = useState<Record<string, string>>({})
  const [deleteTarget, setDeleteTarget] = useState<AccessTokenItem | null>(null)
  const [deleteError, setDeleteError] = useState<string | undefined>(undefined)
  const [revokeError, setRevokeError] = useState<string | undefined>(undefined)
  const [reissueError, setReissueError] = useState<string | undefined>(undefined)
  const [statusFilter, setStatusFilter] = useState<'' | AccessTokenStatus>('')

  const tokensQuery = useQuery({
    queryKey: ['access-tokens'],
    queryFn: ({ signal }) => fetchAccessTokens(signal),
  })
  const groupsQuery = useGroupsQuery()
  const rolesQuery = useRolesQuery()

  const tokens = tokensQuery.data?.items ?? []
  const roles = rolesQuery.data?.items ?? []
  const groupIndex = useMemo(() => indexGroups(groupsQuery.data), [groupsQuery.data])
  const roleIndex = useMemo(() => indexRoles(rolesQuery.data), [rolesQuery.data])

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

  /**
   * 动效触发约束：卡片网格进场 stagger 仅在卡片集合（ID 与状态）发生增删或筛选变化时触发，
   * 依赖由卡片签名拼接字符串 cardSignature 控制，避免无关渲染引起频繁闪烁。
   */
  const cardSignature = visibleTokens.map((item) => `${item.id}:${item.status}`).join('\u0000')
  const gridRef = useGsapScope<HTMLDivElement>(
    (scope) => {
      gsap.from(scope.children, {
        opacity: 0,
        y: 12,
        duration: MOTION_DURATION.enter,
        ease: MOTION_EASE,
        stagger: 0.04,
        clearProps: 'opacity,transform',
      })
    },
    [cardSignature],
  )

  /**
   * 拓扑联动失效约束：访问令牌增删改会改变所属组的 token_count 引用计数，
   * 必须使令牌列表与组清单查询同时失效，确保拓扑计数一致。
   */
  const refreshTokens = () => {
    void queryClient.invalidateQueries({ queryKey: ['access-tokens'] })
    void queryClient.invalidateQueries({ queryKey: ['groups'] })
  }

  /**
   * MCP 接入配置导出：将包含 Authorization Header 的标准 Streamable HTTP 配置写入剪贴板；
   * 结果通过全局 InfoBar 提示，明文仅在剪贴板操作瞬间由内存组装，不持久化。
   */
  const copyMcpConfig = async (item: AccessTokenItem, token: string) => {
    const copied = await copyToClipboard(mcpConfigJson(token))
    setNotice(
      copied
        ? {
            severity: 'success',
            title: 'MCP 接入配置已复制',
            description: `${item.name} 的 HTTP 接入配置已写入剪贴板，可直接粘贴到 MCP 客户端配置文件中。`,
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
   * 凭据不可逆设计约束：后端仅持久化 sha256 摘要，不存在任何反查或读取已有明文的接口；
   * 若当前会话内存中未缓存该明文，则触发「重新签发」流程通过凭据轮转生成新明文。
   */
  const openMcp = (item: AccessTokenItem) => {
    const token = knownSecrets[item.id]
    if (token !== undefined) {
      void copyMcpConfig(item, token)
      return
    }
    setNotice(undefined)
    setReissueError(undefined)
    setReissueTarget(item)
  }

  const reissueMutation = useMutation({
    mutationFn: (item: AccessTokenItem) => reissueAccessToken(item.id),
    onSuccess: (result) => {
      setReissueTarget(null)
      setReissueError(undefined)
      setKnownSecrets((current) => ({ ...current, [result.item.id]: result.token }))
      void copyMcpConfig(result.item, result.token)
      refreshTokens()
    },
    onError: (error) => {
      // 错误呈现约束：重新签发确认弹层位于 top layer，操作异常保留在弹窗内部以避免被遮挡。
      setReissueError(noticeMessage(error, '重新签发访问令牌失败'))
    },
  })

  const revokeMutation = useMutation({
    mutationFn: (id: string) => revokeAccessToken(id),
    onSuccess: (item) => {
      setRevokeTarget(null)
      setRevokeError(undefined)
      setNotice({
        severity: 'success',
        title: '访问令牌已撤销',
        description: `访问令牌 ${item.name}（${item.token_prefix}）已撤销，其 MCP 面(8000)请求将立即被前置拦截。`,
        meta: undefined,
      })
      refreshTokens()
    },
    onError: (error) => {
      setRevokeError(noticeMessage(error, '撤销访问令牌失败'))
    },
  })

  /** 硬删除约束：后端仅允许删除已撤销或已过期的孤立记录；审计事件永久保留在审计日志中。 */
  const deleteMutation = useMutation({
    mutationFn: (item: AccessTokenItem) => deleteAccessToken(item.id),
    onSuccess: (_result, item) => {
      setDeleteTarget(null)
      setDeleteError(undefined)
      setNotice({
        severity: 'success',
        title: '访问令牌已删除',
        description: `访问令牌 ${item.name}（${item.token_prefix}）记录已删除，关联的审计事件仍将保留。`,
        meta: undefined,
      })
      refreshTokens()
    },
    onError: (error) => setDeleteError(tokenDeleteMessage(error)),
  })

  /**
   * 表单保存的唯一出口：创建返回带明文的 AccessTokenCreated（要弹一次明文弹层），
   * 编辑返回更新后的条目（只提示已保存）；两条路径都以 id 为键，编辑不改 id 也不轮换密钥。
   */
  const handleFormSaved = (result: AccessTokenCreated | AccessTokenItem) => {
    if ('token' in result) {
      setCreateOpen(false)
      setCreated(result)
      setKnownSecrets((current) => ({ ...current, [result.item.id]: result.token }))
    } else {
      setEditTarget(null)
      setNotice({
        severity: 'success',
        title: '访问令牌已保存',
        description: `访问令牌 ${result.name}（${result.token_prefix}）的名称、组与到期时间已更新，密钥保持不变。`,
        meta: undefined,
      })
    }
    refreshTokens()
  }

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
        title="访问令牌"
        subtitle={
          tokensQuery.data === undefined
            ? tokensQuery.isError
              ? '查询失败'
              : '查询中'
            : `${tokensQuery.data.total} 个访问令牌`
        }
        actions={
          <button
            type="button"
            className="btn btn-primary btn-sm"
            disabled={rolesQuery.isPending || groupsQuery.isPending}
            onClick={() => setCreateOpen(true)}
          >
            生成访问令牌
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
          <div className="segmented" role="group" aria-label="访问令牌状态筛选">
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
        </div>

        {tokensQuery.isPending && <LoadingState rows={6} label="正在加载访问令牌" />}
        {tokensQuery.isError && <ErrorState error={tokensQuery.error} />}
        {tokensQuery.data !== undefined && tokens.length === 0 && (
          <EmptyState
            hint="暂无访问令牌。生成后明文仅展示一次，后端仅保存哈希与前缀。"
            action={
              <button
                type="button"
                className="btn btn-secondary"
                disabled={rolesQuery.isPending || groupsQuery.isPending}
                onClick={() => setCreateOpen(true)}
              >
                生成访问令牌
              </button>
            }
          />
        )}
        {tokens.length > 0 && visibleTokens.length === 0 && (
          <EmptyState
            hint={`暂无「${
              STATUS_FILTERS.find((option) => option.value === statusFilter)?.label ?? ''
            }」状态的访问令牌。可尝试切换或清空状态筛选。`}
            action={
              <button
                type="button"
                className="btn btn-secondary"
                onClick={() => setStatusFilter('')}
              >
                清空状态筛选
              </button>
            }
          />
        )}
        {visibleTokens.length > 0 && (
          <div className="token-grid" ref={gridRef}>
            {visibleTokens.map((item) => (
              <TokenCard
                key={item.id}
                item={item}
                groupIndex={groupIndex}
                roleIndex={roleIndex}
                secretKnown={knownSecrets[item.id] !== undefined}
                onMcp={() => openMcp(item)}
                onEdit={() => {
                  setNotice(undefined)
                  setEditTarget(item)
                }}
                onRevoke={() => {
                  setNotice(undefined)
                  setRevokeError(undefined)
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
        editTarget={editTarget}
        onCloseEdit={() => setEditTarget(null)}
        roles={roles}
        groups={groupsQuery.data?.items ?? []}
        onSaved={handleFormSaved}
        created={created}
        onCloseCreated={() => {
          setCreated(null)
          refreshTokens()
        }}
        deleteTarget={deleteTarget}
        deleteError={deleteError}
        deletePending={deleteMutation.isPending}
        onDelete={(item) => deleteMutation.mutate(item)}
        onCloseDelete={() => {
          setDeleteTarget(null)
          setDeleteError(undefined)
        }}
        revokeTarget={revokeTarget}
        revokeError={revokeError}
        revokePending={revokeMutation.isPending}
        onRevoke={(id) => revokeMutation.mutate(id)}
        onCloseRevoke={() => {
          setRevokeTarget(null)
          setRevokeError(undefined)
        }}
        reissueTarget={reissueTarget}
        reissueError={reissueError}
        reissuePending={reissueMutation.isPending}
        onReissue={(item) => reissueMutation.mutate(item)}
        onCloseReissue={() => {
          setReissueTarget(null)
          setReissueError(undefined)
        }}
      />
    </PageShell>
  )
}
