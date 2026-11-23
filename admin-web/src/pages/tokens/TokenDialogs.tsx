import type {
  AccessTokenCreated,
  AccessTokenItem,
  GroupItem,
  RoleItem,
  TokenOptions,
} from '../../api/types'
import { ConfirmDialog } from '../../components/ConfirmDialog'
import { InfoBar } from '../../components/InfoBar'
import { JsonBlock } from '../../components/JsonBlock'
import { Modal } from '../../components/Modal'
import { TOKEN_STATUS_LABEL } from '../../labels'
import { CopyButton } from './CopyButton'
import { TokenForm } from './TokenForm'
import { MCP_CONFIG_KEY, mcpConfigJson, mcpConfigValue } from './tokenConfig'

/**
 * 访问令牌生命周期弹窗集合：
 * 包含创建令牌、单次明文呈现、删除确认、撤销确认、重新签发确认五种交互模态；
 * 状态与突变逻辑由父页面统一托管，本组件负责安全警示渲染与提交中（pending）交互锁定。
 */
export function TokenDialogs({
  createOpen,
  onCloseCreate,
  options,
  roles,
  groups,
  onCreated,
  created,
  onCloseCreated,
  deleteTarget,
  deleteError,
  deletePending,
  onDelete,
  onCloseDelete,
  revokeTarget,
  revokeError,
  revokePending,
  onRevoke,
  onCloseRevoke,
  reissueTarget,
  reissueError,
  reissuePending,
  onReissue,
  onCloseReissue,
}: {
  createOpen: boolean
  onCloseCreate: () => void
  options: TokenOptions | undefined
  roles: RoleItem[]
  groups: GroupItem[]
  onCreated: (result: AccessTokenCreated) => void
  created: AccessTokenCreated | null
  onCloseCreated: () => void
  deleteTarget: AccessTokenItem | null
  deleteError: string | undefined
  deletePending: boolean
  onDelete: (item: AccessTokenItem) => void
  onCloseDelete: () => void
  revokeTarget: AccessTokenItem | null
  revokeError: string | undefined
  revokePending: boolean
  onRevoke: (id: string) => void
  onCloseRevoke: () => void
  reissueTarget: AccessTokenItem | null
  reissueError: string | undefined
  reissuePending: boolean
  onReissue: (item: AccessTokenItem) => void
  onCloseReissue: () => void
}) {
  return (
    <>
      {createOpen && options !== undefined && (
        <Modal size="lg" title="生成访问令牌" onClose={onCloseCreate}>
          <TokenForm
            options={options}
            roles={roles}
            groups={groups}
            onCreated={onCreated}
          />
        </Modal>
      )}

      {created !== null && (
        <Modal
          size="lg"
          title="访问令牌已创建"
          onClose={onCloseCreated}
          footer={
            <>
              <CopyButton
                value={mcpConfigJson(created.token)}
                label="复制 MCP 接入配置"
                className="btn btn-secondary"
              />
              <button type="button" className="btn btn-primary" onClick={onCloseCreated}>
                我已保存，关闭
              </button>
            </>
          }
        >
          <div className="token-created">
            <InfoBar
              severity="warning"
              title="明文仅展示一次"
              description="关闭后无法再次查看，请立即复制明文与 MCP 接入配置并妥善保管。"
            />
            <div className="token-secret" role="group" aria-label="令牌明文">
              <code>{created.token}</code>
              <CopyButton value={created.token} />
            </div>
            <p className="hint">
              调用方以该令牌作为 MCP Bearer 凭证。可直接复制下方的 HTTP 协议配置（服务名
              <span className="mono"> {MCP_CONFIG_KEY} </span>，URL 为本页同源的
              <span className="mono"> /mcp</span>）粘贴到 MCP 客户端。
            </p>
            <JsonBlock label="MCP 接入配置" value={mcpConfigValue(created.token)} />
          </div>
        </Modal>
      )}

      {deleteTarget !== null && (
        <ConfirmDialog
          title={`删除访问令牌 · ${deleteTarget.name}`}
          subtitle="记录将从列表移除且不可恢复；该访问令牌立即失效，关联的审计事件仍将保留。"
          confirmLabel="确认删除"
          pendingLabel="删除中…"
          tone="danger"
          pending={deletePending}
          error={deleteError}
          errorTitle="删除访问令牌失败"
          onClose={onCloseDelete}
          onConfirm={() => onDelete(deleteTarget)}
        >
          <p className="hint">
            令牌前缀：<span className="mono">{deleteTarget.token_prefix}</span> · 当前状态：
            {TOKEN_STATUS_LABEL[deleteTarget.status] ?? deleteTarget.status}
          </p>
        </ConfirmDialog>
      )}

      {revokeTarget !== null && (
        <ConfirmDialog
          title={`撤销访问令牌 · ${revokeTarget.name}`}
          subtitle="撤销后该访问令牌的 MCP 面(8000)请求将立即被前置拦截，且不可恢复。"
          confirmLabel="确认撤销"
          pendingLabel="撤销中…"
          tone="danger"
          pending={revokePending}
          error={revokeError}
          errorTitle="撤销访问令牌失败"
          onClose={onCloseRevoke}
          onConfirm={() => onRevoke(revokeTarget.id)}
        >
          <p className="hint">
            令牌前缀：<span className="mono">{revokeTarget.token_prefix}</span>
          </p>
        </ConfirmDialog>
      )}

      {reissueTarget !== null && (
        <ConfirmDialog
          title={`重新签发访问令牌 · ${reissueTarget.name}`}
          subtitle="原地轮转凭据：名称、角色、组、工具白名单与到期时间保持不变，仅更新 token、哈希与前缀，清空最近使用时间。"
          confirmLabel="确认重新签发"
          pendingLabel="重新签发中…"
          pending={reissuePending}
          error={reissueError}
          errorTitle="重新签发访问令牌失败"
          onClose={onCloseReissue}
          onConfirm={() => onReissue(reissueTarget)}
        >
          <div className="token-created">
            <InfoBar
              severity="warning"
              title="旧凭据将立即失效"
              description="后端仅持久化 sha256 哈希，无法回显原明文；重新签发会生成全新明文，原访问令牌立即失效。"
            />
            <p className="hint">
              令牌前缀：<span className="mono">{reissueTarget.token_prefix}</span>
            </p>
            <p className="hint">
              确认后将自动复制最新的 HTTP MCP 接入配置（服务名
              <span className="mono"> {MCP_CONFIG_KEY} </span>），新明文仅在本页内存中临时保留。
            </p>
          </div>
        </ConfirmDialog>
      )}
    </>
  )
}
