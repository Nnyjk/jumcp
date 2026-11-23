import { useMemo, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { deleteAdminAccount, fetchAdminAccounts } from '../../api/admin'
import type { AdminAccount } from '../../api/types'
import { useAuth } from '../../auth/session'
import { Badge } from '../../components/Badge'
import { Card } from '../../components/Card'
import { ConfirmDialog } from '../../components/ConfirmDialog'
import { DataTable, type DataTableColumn } from '../../components/DataTable'
import { InfoBar } from '../../components/InfoBar'
import { EmptyState, ErrorState, LoadingState, PageShell } from '../../components/PageShell'
import { DASH, formatDateTime } from '../../format'
import type { Notice } from '../../notice'
import { AccountForm, type AccountEditorTarget } from './AccountForm'
import { accountErrorMessage } from './accountErrors'

const ACCOUNTS_KEY = ['admin-accounts'] as const

export default function AdminsPage() {
  const queryClient = useQueryClient()
  const { session } = useAuth()
  // 鉴权上下文约束：会话 actor.id 存的是 username，而 row.id 是自增主键；
  // 必须使用 actor.user_id 比对识别当前登录用户自身；缺失时回退为 null 并依赖后端 MCP_ADMIN_SELF_DELETE 安全拦截。
  const meId = session.state === 'authenticated' ? (session.actor.user_id ?? null) : null

  const [notice, setNotice] = useState<Notice | undefined>(undefined)
  const [editor, setEditor] = useState<AccountEditorTarget | null>(null)
  const [deleteTarget, setDeleteTarget] = useState<AdminAccount | null>(null)

  const accountsQuery = useQuery({
    queryKey: ACCOUNTS_KEY,
    queryFn: ({ signal }) => fetchAdminAccounts(signal),
  })

  const accounts = useMemo(() => {
    const items = accountsQuery.data?.items ?? []
    // 列表展示约束：管理账户清单不分页，前端按 username 字典序稳定排列，便于快速检索目标账户。
    return [...items].sort((left, right) => left.username.localeCompare(right.username))
  }, [accountsQuery.data])

  const activeCount = accounts.filter((account) => account.state === 'active').length

  const deleteMutation = useMutation({
    mutationFn: (account: AdminAccount) => deleteAdminAccount(account.id),
    onSuccess: (_result, account) => {
      setDeleteTarget(null)
      setNotice({
        severity: 'success',
        title: '管理账户已删除',
        description: `管理账户 ${account.username} 已从管理台移除，无法再登录；关联的历史审计事件仍将保留。`,
        meta: undefined,
      })
      void queryClient.invalidateQueries({ queryKey: ACCOUNTS_KEY })
    },
  })

  const columns: Array<DataTableColumn<AdminAccount>> = [
    {
      key: 'username',
      header: '用户名',
      mono: true,
      width: '200px',
      cell: (row) => row.username,
    },
    {
      key: 'state',
      header: '状态',
      width: '96px',
      cell: (row) =>
        row.state === 'active' ? (
          <Badge tone="success" dot title="active">
            启用
          </Badge>
        ) : (
          <Badge tone="neutral" dot title="disabled">
            已停用
          </Badge>
        ),
    },
    {
      key: 'last_login_at',
      header: '最后登录',
      mono: true,
      width: '176px',
      cell: (row) => (row.last_login_at === null ? '从未登录' : formatDateTime(row.last_login_at)),
      title: (row) => row.last_login_at ?? DASH,
    },
    {
      key: 'created_at',
      header: '创建时间',
      mono: true,
      width: '176px',
      cell: (row) => formatDateTime(row.created_at),
      title: (row) => row.created_at,
    },
    {
      key: 'actions',
      header: '操作',
      width: '148px',
      sticky: true,
      cell: (row) => (
        <div className="row-actions">
          <button
            type="button"
            className="btn btn-secondary btn-sm"
            onClick={() => {
              setNotice(undefined)
              setEditor({ mode: 'edit', account: row })
            }}
          >
            编辑
          </button>
          <button
            type="button"
            className="btn btn-danger btn-sm"
            disabled={row.id === meId}
            title={row.id === meId ? '禁止删除当前已登录的管理账户' : undefined}
            onClick={() => {
              setNotice(undefined)
              setDeleteTarget(row)
            }}
          >
            删除
          </button>
        </div>
      ),
    },
  ]

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
        title="管理账户"
        subtitle={
          accountsQuery.data === undefined
            ? accountsQuery.isError
              ? '查询失败'
              : '查询中'
            : `共 ${accounts.length} 个管理账户 · ${activeCount} 个启用中`
        }
        actions={
          <button
            type="button"
            className="btn btn-primary btn-sm"
            onClick={() => {
              setNotice(undefined)
              setEditor({ mode: 'create' })
            }}
          >
            新建管理账户
          </button>
        }
        flush
      >
        {accountsQuery.isPending && <LoadingState rows={5} label="正在加载管理账户" />}
        {accountsQuery.isError && <ErrorState error={accountsQuery.error} />}
        {accountsQuery.data !== undefined && accounts.length === 0 && (
          <EmptyState
            hint="暂无管理账户。可点击右上角「新建管理账户」添加，新建后可直接登录管理面(8080)。"
            action={
              <button
                type="button"
                className="btn btn-secondary"
                onClick={() => setEditor({ mode: 'create' })}
              >
                新建管理账户
              </button>
            }
          />
        )}
        {accounts.length > 0 && (
          <DataTable
            label="管理账户"
            columns={columns}
            rows={accounts}
            rowKey={(row) => row.id}
            minWidth="900px"
          />
        )}
      </Card>

      {editor !== null && (
        <AccountForm
          key={editor.mode === 'edit' ? `edit:${editor.account.id}` : 'create'}
          target={editor}
          onClose={() => setEditor(null)}
          onSaved={(account, mode) => {
            setEditor(null)
            setNotice({
              severity: 'success',
              title: mode === 'create' ? '管理账户已创建' : '管理账户已更新',
              description:
                mode === 'create'
                  ? `管理账户 ${account.username} 已创建，可立即用于登录管理面(8080)。`
                  : `管理账户 ${account.username} 的资料已更新。`,
              meta: undefined,
            })
            void queryClient.invalidateQueries({ queryKey: ACCOUNTS_KEY })
          }}
        />
      )}

      {deleteTarget !== null && (
        <ConfirmDialog
          title={`删除管理账户 · ${deleteTarget.username}`}
          subtitle="删除后该管理账户无法再登录管理面(8080)，此操作不可撤回。"
          confirmLabel="确认删除"
          pendingLabel="删除中…"
          tone="danger"
          pending={deleteMutation.isPending}
          errorTitle="删除管理账户失败"
          error={deleteMutation.error === null ? undefined : accountErrorMessage(deleteMutation.error)}
          onClose={() => {
            setDeleteTarget(null)
            deleteMutation.reset()
          }}
          onConfirm={() => deleteMutation.mutate(deleteTarget)}
        >
          <p className="hint">
            该账户关联的历史审计事件将永久保留；若该账户为系统中最后一个启用中的管理账户，删除操作将被后端拦截拒绝。
          </p>

        </ConfirmDialog>
      )}
    </PageShell>
  )
}
