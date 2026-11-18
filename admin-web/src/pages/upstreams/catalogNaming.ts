import type { McpCatalogEntry } from '../../api/types'
import { validateUpstreamId } from '../../lib/upstreamId'

/**
 * 目录安装的命名约束：上游 id 由目录自动推导（同一包重复安装自动加后缀），
 * 名称在同一包的安装实例内唯一（trim 后大小写不敏感）。规则只在这里维护。
 */
const MAX_UPSTREAM_ID_LENGTH = 32
const MAX_ATTEMPTS = 1000

/** 名称冲突是提交前拦截的错误，文案与校验同源。 */
export const INSTALL_NAME_CONFLICT_MESSAGE = '名称已被该包的其它安装使用，请换一个。'

/**
 * 主名 + 后缀：超长时截断主名再拼 `-N`，保证总长 ≤32；
 * 截断点上的尾部 `-`/`_` 去掉，避免出现 `foo--2` 或残留的 `__`。
 */
function composeUpstreamId(stem: string, index: number): string {
  const suffix = index <= 1 ? '' : `-${index}`
  const maxStemLength = Math.max(1, MAX_UPSTREAM_ID_LENGTH - suffix.length)
  const trimmed = stem.slice(0, maxStemLength).replace(/[-_]+$/, '')
  return `${trimmed === '' ? 'pkg' : trimmed}${suffix}`
}

/** 该包内第一个未被占用的上游 id：default → default-2 → default-3 … */
export function nextUpstreamId(entry: McpCatalogEntry): string {
  // 只保留小写字母/数字/_/-，连续下划线压成一个：拼出来的 id 始终满足后端正则。
  const stem = entry.default_upstream_id
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9_-]/g, '-')
    .replace(/__+/g, '_')
    .replace(/^[^a-z]+/, '')
  const taken = new Set(entry.installations.map((installation) => installation.upstream_id))
  for (let index = 1; index <= MAX_ATTEMPTS; index += 1) {
    const candidate = composeUpstreamId(stem, index)
    if (!taken.has(candidate) && validateUpstreamId(candidate) === undefined) return candidate
  }
  // 兜底：主名序号全被占满时换短前缀，仍最多尝试 MAX_ATTEMPTS 次；找不到就报错，绝不返回未校验的 id。
  const fallbackStart = entry.installations.length + 1
  for (let offset = 0; offset < MAX_ATTEMPTS; offset += 1) {
    const candidate = composeUpstreamId('pkg', fallbackStart + offset)
    if (!taken.has(candidate) && validateUpstreamId(candidate) === undefined) return candidate
  }
  throw new Error('无法推导可用的 upstream_id，请先清理该包的安装实例。')
}

/** 该包内第一个未被占用的安装名称：名称 → 名称 (2) → 名称 (3) … */
export function nextInstallationName(entry: McpCatalogEntry): string {
  const taken = new Set(entry.installations.map((installation) => installation.name.trim().toLowerCase()))
  const base = entry.name.trim() === '' ? entry.id : entry.name.trim()
  if (!taken.has(base.toLowerCase())) return base
  for (let index = 2; index <= MAX_ATTEMPTS; index += 1) {
    const candidate = `${base} (${index})`
    if (!taken.has(candidate.toLowerCase())) return candidate
  }
  return `${base} (${entry.installations.length + 1})`
}

/** 提交前校验名称：非空，且不与同一包的其它安装实例重名。 */
export function installationNameError(entry: McpCatalogEntry, name: string): string | undefined {
  const trimmed = name.trim()
  if (trimmed === '') return '名称不能为空。'
  const normalized = trimmed.toLowerCase()
  if (entry.installations.some((installation) => installation.name.trim().toLowerCase() === normalized)) {
    return INSTALL_NAME_CONFLICT_MESSAGE
  }
  return undefined
}
