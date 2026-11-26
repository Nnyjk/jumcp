import type {
  McpCatalogField,
  UpstreamCategory,
  UpstreamInput,
  UpstreamItem,
} from '../../api/types'
import { DASH } from '../../format'

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

/** 密文字段只认后端下发的 `secret === true`：字段 kind 不参与判定。 */
export function isSecretField(field: McpCatalogField): boolean {
  return field.secret === true
}
