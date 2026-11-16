import type {
  AccessTokenCreated,
  AccessTokenItem,
  GroupItem,
  RoleItem,
  TokenOptions,
} from '../../api/types'
import { InfoBar } from '../../components/InfoBar'
import { Modal } from '../../components/Modal'
import { TOKEN_STATUS_LABEL } from '../../labels'
import type { Notice } from '../../notice'
import { CopyButton } from './CopyButton'
import { TokenForm } from './TokenForm'
import { MCP_CONFIG_KEY, mcpConfigJson } from './tokenConfig'

/**
 * 令牌页的五个弹窗：创建、创建成功（明文只展示一次）、删除确认、撤销确认、重新签发。
 * 数据与副作用都留在页面：这里只负责弹窗内容与按钮的 pending 态。
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
  revokePending,
  onRevoke,
  onCloseRevoke,
  reissueTarget,
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
  deleteError: Notice | undefined
  deletePending: boolean
  onDelete: (item: AccessTokenItem) => void
  onCloseDelete: () => void
  revokeTarget: AccessTokenItem | null
  revokePending: boolean
  onRevoke: (id: string) => void
  onCloseRevoke: () => void
  reissueTarget: AccessTokenItem | null
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
          title="令牌已创建"
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
          onClose={onCloseDelete}
          footer={
            <>
              <button
                type="button"
                className="btn btn-secondary"
                disabled={deletePending}
                onClick={onCloseDelete}
              >
                取消
              </button>
              <button
                type="button"
                className="btn btn-danger"
                disabled={deletePending}
                onClick={() => onDelete(deleteTarget)}
              >
                {deletePending ? '删除中…' : '确认删除'}
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
          onClose={onCloseRevoke}
          footer={
            <>
              <button
                type="button"
                className="btn btn-secondary"
                disabled={revokePending}
                onClick={onCloseRevoke}
              >
                取消
              </button>
              <button
                type="button"
                className="btn btn-danger"
                disabled={revokePending}
                onClick={() => onRevoke(revokeTarget.id)}
              >
                {revokePending ? '撤销中…' : '确认撤销'}
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
          onClose={onCloseReissue}
          footer={
            <>
              <button
                type="button"
                className="btn btn-secondary"
                disabled={reissuePending}
                onClick={onCloseReissue}
              >
                取消
              </button>
              <button
                type="button"
                className="btn btn-primary"
                disabled={reissuePending}
                onClick={() => onReissue(reissueTarget)}
              >
                {reissuePending ? '重新签发中…' : '确认重新签发'}
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
    </>
  )
}
