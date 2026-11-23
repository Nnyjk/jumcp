import { ApiError } from '../../api/client'
import type { AccessTokenStatus } from '../../api/types'
import { TOKEN_STATUS_LABEL } from '../../labels'
import { noticeMessage } from '../../notice'

export const MCP_CONFIG_KEY = 'jumcp'

/**
 * 客户端接入约束：生成 MCP Streamable HTTP 接入配置代码片段；
 * - 协议端点：以当前页面 origin + '/mcp' 构建服务端点，生产由网关反向代理至 MCP 面(8000)，开发环境由 Vite 代理；
 * - 安全性约束：明文仅在用户点击时瞬时拼装于内存中，严禁写入 localStorage、Cookie 等持久存储，杜绝凭据泄露。
 */
export function mcpConfigValue(token: string): { mcpServers: Record<string, unknown> } {
  return {
    mcpServers: {
      [MCP_CONFIG_KEY]: {
        type: 'http',
        url: new URL('/mcp', window.location.origin).toString(),
        headers: { Authorization: `Bearer ${token}` },
      },
    },
  }
}

export function mcpConfigJson(token: string): string {
  return JSON.stringify(mcpConfigValue(token), null, 2)
}

/**
 * 剪贴板写入约束：现代浏览器 Clipboard API 在非安全上下文（非 HTTPS/localhost）或权限被拒时会抛出异常；
 * 采用布尔值指示成功状态，失败时由调用方做降级提示。
 */
export async function copyToClipboard(value: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(value)
    return true
  } catch {
    return false
  }
}

/**
 * 默认授权契约：三个元工具（search_capabilities、describe_capability、list_groups）默认勾选，
 * 满足 MCP 基础能力发现协议；admin_* 与上游受管工具需由管理员显式按需授权。
 */
export const DEFAULT_BUILTIN_TOOLS = [
  'search_capabilities',
  'describe_capability',
  'list_groups',
]

/** 客户端筛选：访问令牌列表全量拉取，状态筛选仅在前端内存计算，避免向后端发起重复查询。 */
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

/**
 * 状态机安全边界约束：后端基于安全审计原则，严格禁止直接删除生效中（active）的访问令牌；
 * 只有已撤销（revoked）或已过期（expired）的凭据记录允许物理删除。
 */
export function tokenDeleteMessage(error: unknown): string {
  if (error instanceof ApiError && error.status === 409) {
    const data = error.data as Record<string, unknown> | undefined
    const hint =
      typeof data?.hint === 'string' && data.hint.trim() !== ''
        ? data.hint.trim()
        : '请先撤销访问令牌后再删除'
    return `删除被拒绝：该访问令牌仍在生效中。只有已撤销或已过期的访问令牌允许删除。${hint}（${error.code} · HTTP 409）`
  }
  return noticeMessage(error, '删除访问令牌失败')
}

