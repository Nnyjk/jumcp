import { ApiError } from '../../api/client'
import type { AccessTokenStatus } from '../../api/types'
import { TOKEN_STATUS_LABEL } from '../../labels'
import { noticeFromError, type Notice } from '../../notice'

export const MCP_CONFIG_KEY = 'jumcp'

/**
 * HTTP Streamable 接入配置（一键复制）。
 * URL 用当前页面同源 + `/mcp`：生产由网关同源转发，开发期走 Vite `/mcp` 代理。
 * 明文只在内存里拼进 JSON，不写 localStorage、不落任何持久存储。
 */
export function mcpConfigJson(token: string): string {
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
export async function copyToClipboard(value: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(value)
    return true
  } catch {
    return false
  }
}

/** 三个元工具默认勾选；admin_* 与上游工具按需选择。 */
export const DEFAULT_BUILTIN_TOOLS = [
  'search_capabilities',
  'describe_capability',
  'list_groups',
]

/** 状态过滤只影响客户端展示：列表本来就是全量，切状态不打后端。 */
export const STATUS_FILTERS: Array<{ value: '' | AccessTokenStatus; label: string }> = (
  ['', 'active', 'revoked', 'expired'] as const
).map((value) => ({
  value,
  label: value === '' ? '全部' : (TOKEN_STATUS_LABEL[value] ?? value),
}))

export function toggle(list: string[], value: string, checked: boolean): string[] {
  if (checked) return list.includes(value) ? list : [...list, value]
  return list.filter((item) => item !== value)
}

/**
 * allowlist 通配名的显示文案：`*` → 全部工具，`<id>__*` → `<id> 全部工具`。
 * 精确名返回 undefined，由调用方按原样或内置工具的 label 展示。
 */
export function wildcardToolLabel(name: string): string | undefined {
  if (name === '*') return '全部工具'
  if (name.endsWith('__*')) return `${name.slice(0, -3)} 全部工具`
  return undefined
}

/** 409 表示令牌仍在生效：后端只允许删除已撤销 / 已过期记录，提示先撤销。 */
export function tokenDeleteError(error: unknown): Notice {
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

