/**
 * 管理台后端访问的唯一出口（§4.4.5：UI 只是 admin API 的薄前端，禁止直连数据库）。
 * 页面层不得直接调用 fetch，统一经此模块换取类型化结果或 ApiError。
 */

/** 后端错误统一形态：HTTP 状态 + 业务错误码 + 可读消息。 */
export class ApiError extends Error {
  readonly status: number
  readonly code: string

  constructor(status: number, code: string, message: string) {
    super(message)
    this.name = 'ApiError'
    this.status = status
    this.code = code
  }
}

export type QueryParams = Record<string, string | number | boolean | undefined>

export interface RequestOptions {
  method?: 'GET' | 'POST' | undefined
  params?: QueryParams | undefined
  body?: unknown | undefined
  signal?: AbortSignal | undefined
}

const BASE_URL = (import.meta.env.VITE_API_BASE ?? '').replace(/\/+$/, '')

function buildInit(options: RequestOptions): RequestInit {
  const init: RequestInit = { method: options.method ?? 'GET' }
  if (options.body !== undefined) {
    init.headers = { 'content-type': 'application/json' }
    init.body = JSON.stringify(options.body)
  }
  if (options.signal !== undefined) init.signal = options.signal
  return init
}

function buildUrl(path: string, params?: QueryParams): string {
  const search = new URLSearchParams()
  for (const [key, value] of Object.entries(params ?? {})) {
    if (value === undefined || value === '') continue
    search.set(key, String(value))
  }
  const query = search.toString()
  return `${BASE_URL}${path}${query ? `?${query}` : ''}`
}

function errorFromResponse(status: number, bodyText: string): ApiError {
  let code = `HTTP_${status}`
  let message = bodyText.trim() || `请求失败（HTTP ${status}）`
  try {
    const parsed: unknown = JSON.parse(bodyText)
    if (parsed && typeof parsed === 'object') {
      const outer = parsed as Record<string, unknown>
      const inner =
        outer.error && typeof outer.error === 'object'
          ? (outer.error as Record<string, unknown>)
          : undefined
      const rawCode = outer.code ?? inner?.code
      const rawMessage = outer.message ?? inner?.message ?? (typeof outer.error === 'string' ? outer.error : undefined)
      if (typeof rawCode === 'string' && rawCode !== '') code = rawCode
      if (typeof rawMessage === 'string' && rawMessage !== '') message = rawMessage
    }
  } catch {
    // 非 JSON 错误体（如反向代理的 HTML 错误页）保留原始文本作为消息。
  }
  return new ApiError(status, code, message)
}

/** 发起请求并返回解析后的 JSON；失败一律抛 ApiError。 */
export async function request<T>(path: string, options: RequestOptions = {}): Promise<T> {
  const url = buildUrl(path, options.params)
  let response: Response
  try {
    response = await fetch(url, buildInit(options))
  } catch (cause) {
    if (cause instanceof DOMException && cause.name === 'AbortError') throw cause
    throw new ApiError(0, 'NETWORK_ERROR', `无法连接后端 ${url}`)
  }
  if (!response.ok) {
    throw errorFromResponse(response.status, await response.text())
  }
  const text = await response.text()
  if (text.trim() === '') return undefined as T
  try {
    return JSON.parse(text) as T
  } catch {
    throw new ApiError(response.status, 'INVALID_RESPONSE', '后端返回的不是合法 JSON')
  }
}

/** 发起请求并返回原始文本（健康检查当前返回纯文本）。 */
export async function requestText(path: string, options: RequestOptions = {}): Promise<string> {
  const url = buildUrl(path, options.params)
  let response: Response
  try {
    response = await fetch(url, buildInit(options))
  } catch (cause) {
    if (cause instanceof DOMException && cause.name === 'AbortError') throw cause
    throw new ApiError(0, 'NETWORK_ERROR', `无法连接后端 ${url}`)
  }
  if (!response.ok) {
    throw errorFromResponse(response.status, await response.text())
  }
  return response.text()
}
