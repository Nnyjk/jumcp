import { ApiError } from '../../api/client'
import type {
  McpCatalogField,
  UpstreamCategory,
  UpstreamInput,
  UpstreamItem,
  UpstreamToolItem,
} from '../../api/types'
import { DASH } from '../../format'
import { noticeFromError, type Notice } from '../../notice'

export const GROUP_OPTIONS_LIMIT = 200

export const CATEGORY_OPTIONS: UpstreamCategory[] = ['basic', 'analysis', 'admin', 'custom']

export type EditorTarget = { mode: 'create' } | { mode: 'edit'; item: UpstreamItem }

/** 列表里的目标摘要：http 用 URL，stdio 用 command + args。 */
export function targetText(item: UpstreamItem): string {
  if (item.transport === 'http') return item.url ?? DASH
  const parts = [item.command ?? DASH, ...item.args]
  return parts.join(' ')
}

/** 整行回传：PUT 是完整 UpstreamInput，切换启用状态时不改 id 与其他字段。 */
export function toInput(item: UpstreamItem, enabled: boolean): UpstreamInput {
  return {
    id: item.id,
    name: item.name,
    transport: item.transport,
    url: item.url,
    command: item.command,
    args: item.args,
    env: item.env,
    headers: item.headers,
    group: item.group,
    category: item.category,
    enabled,
    timeout_ms: item.timeout_ms,
  }
}

/** `KEY=值` 每行一条：空行与 # 注释忽略，值里的 `=` 保留。 */
export function parseKeyValueLines(
  raw: string,
): { value: Record<string, string> } | { error: string } {
  const value: Record<string, string> = {}
  const lines = raw.split('\n')
  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index]?.trim() ?? ''
    if (line === '' || line.startsWith('#')) continue
    const separator = line.indexOf('=')
    const key = separator === -1 ? '' : line.slice(0, separator).trim()
    if (separator <= 0 || key === '') {
      return { error: `第 ${index + 1} 行不是 KEY=值 形式：${line}` }
    }
    value[key] = line.slice(separator + 1).trim()
  }
  return { value }
}

export function formatKeyValueLines(map: Record<string, string>): string {
  return Object.entries(map)
    .map(([key, value]) => `${key}=${value}`)
    .join('\n')
}

export function isHttpUrl(raw: string): boolean {
  try {
    const parsed = new URL(raw)
    return parsed.protocol === 'http:' || parsed.protocol === 'https:'
  } catch {
    return false
  }
}

export function basicDisabledReason(tool: UpstreamToolItem): string | undefined {
  if (tool.orphaned) {
    return '上游已不再暴露该工具，只能清理基础组覆盖'
  }
  let reason: string | undefined
  if (tool.group === 'admin' || tool.name.startsWith('admin_')) {
    reason = '管理类工具不能加入基础组'
  } else if (!tool.eligible) {
    reason = '后端判定该工具不满足基础组条件'
  }
  // 覆盖还在但不满足条件：明确说是“已失效的覆盖”，不要让人误以为 basic 仍生效。
  if (reason !== undefined && tool.overridden) {
    return `基础组覆盖已失效：${reason}，可清理`
  }
  return reason
}

/** 409 的 unknown 列表要展开给人看，否则管理员只知道有工具失效，不知道该改哪个。 */
export function basicSaveError(error: unknown): Notice {
  const base = noticeFromError(error, '保存基础工具失败')
  if (!(error instanceof ApiError) || error.code !== 'MCP_UPSTREAM_TOOL_UNKNOWN') {
    return base
  }
  const data: unknown = error.data
  const rawUnknown =
    typeof data === 'object' && data !== null && 'unknown' in data
      ? (data as { unknown?: unknown }).unknown
      : undefined
  const unknown = Array.isArray(rawUnknown)
    ? rawUnknown.filter((item): item is string => typeof item === 'string')
    : []
  if (unknown.length === 0) {
    return base
  }
  return { ...base, description: `${base.description}（未知工具：${unknown.join('、')}）` }
}

/** 密文字段只认后端下发的 `secret === true`：字段 kind 不参与判定。 */
export function isSecretField(field: McpCatalogField): boolean {
  return field.secret === true
}
