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
 * 上游工具名：每行一个 `<server_id>__<tool>`；trim、去空行、去重。
 * 非法行就地报错，调用方不得继续发请求。
 */
export function parseUpstreamToolNames(
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

