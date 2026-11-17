import type { AccessTokenItem, GroupItem, RoleItem, TokenBuiltinToolOption } from '../../api/types'
import { Badge } from '../../components/Badge'
import { DASH, dashText, formatDateTime } from '../../format'
import { groupLabel, roleLabel } from '../../api/queries'
import { STATUS_TONE, TOKEN_STATUS_LABEL } from '../../labels'
import { useAdminView } from '../../viewStack'
import { wildcardToolLabel } from './tokenConfig'

export function TokenCard({
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
          <dd>{dashText(item.caller)}</dd>
        </div>
        <div className="token-field">
          <dt>来源</dt>
          <dd>{dashText(item.source)}</dd>
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
                      {option?.label ?? wildcardToolLabel(name) ?? name}
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
