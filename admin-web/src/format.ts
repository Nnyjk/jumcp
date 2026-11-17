/** 空值占位：表格里「没有数据」不能读成 0 或空串。 */
export const DASH = '—'

/** 0–1 比率 → 百分比；无样本给破折号，不用 0% 冒充。 */
export function formatPercent(value: number | null | undefined): string {
  if (value === null || value === undefined || Number.isNaN(value)) return DASH
  return `${(value * 100).toFixed(1)}%`
}

/** 单值展示：null / undefined / 空白串 → 破折号；其余保留原文本（0 与 false 正常显示）。 */
export function dashText(value: string | number | null | undefined): string {
  if (value === null || value === undefined) return DASH
  if (typeof value === 'string') return value.trim() === '' ? DASH : value
  return String(value)
}

/** 单行预览：折叠连续空白后按字符截断，超长补省略号；空值给破折号。 */
export function truncateInline(value: string | null | undefined, max = 72): string {
  if (value === null || value === undefined) return DASH
  const oneLine = value.replace(/\s+/g, ' ').trim()
  if (oneLine === '') return DASH
  return oneLine.length > max ? `${oneLine.slice(0, max)}…` : oneLine
}

/**
 * RFC3339 允许任意位数的小数秒（Rust chrono 默认带纳秒），而 V8 只稳定解析到毫秒；
 * 这里先把小数部分截到 3 位、补全 ±HHMM 时区，再交给 Date 解析。
 */
const RFC3339 =
  /^(\d{4})-(\d{2})-(\d{2})[T ](\d{2}):(\d{2}):(\d{2})(?:\.(\d+))?(Z|[+-]\d{2}:?\d{2})?$/

function pad(value: number): string {
  return String(value).padStart(2, '0')
}

function normalize(value: string): string {
  const match = RFC3339.exec(value)
  if (match === null) return value
  const [, year, month, day, hour, minute, second, fraction, zone] = match
  const millis = fraction === undefined ? '' : `.${fraction.slice(0, 3).padEnd(3, '0')}`
  const offset =
    zone === undefined || zone === 'Z'
      ? (zone ?? '')
      : zone.includes(':')
        ? zone
        : `${zone.slice(0, 3)}:${zone.slice(3)}`
  return `${year}-${month}-${day}T${hour}:${minute}:${second}${millis}${offset}`
}

function parse(value: string): Date | undefined {
  const raw = value.trim()
  if (raw === '') return undefined
  const date = new Date(normalize(raw))
  return Number.isNaN(date.getTime()) ? undefined : date
}

/**
 * 后端 RFC3339 时间 → 浏览器本地时区 `YYYY-MM-DD HH:mm:ss`。
 * 界面统一到秒：毫秒只用于后端排序，原始 RFC3339 由调用方放进 title/tooltip，
 * 不占用列表主文本的宽度。空值给破折号；无法解析时原样返回，避免把后端原始字符串吞成「无效时间」。
 */
export function formatDateTime(value: string | null | undefined): string {
  if (value === null || value === undefined) return DASH
  const date = parse(value)
  if (date === undefined) return value

  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(
    date.getHours(),
  )}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`
}

/** 等待时长：从某个时间点到 now 的粗粒度人话（分钟级以下才给秒）。 */
export function formatWaitSince(value: string | null | undefined, now: number = Date.now()): string {
  if (value === null || value === undefined) return DASH
  const date = parse(value)
  if (date === undefined) return DASH
  const seconds = Math.max(0, Math.floor((now - date.getTime()) / 1000))
  if (seconds < 60) return `${seconds} 秒`
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return `${minutes} 分钟`
  const hours = Math.floor(minutes / 60)
  if (hours < 24) return `${hours} 小时 ${minutes % 60} 分钟`
  return `${Math.floor(hours / 24)} 天 ${hours % 24} 小时`
}

/** 本地 datetime-local 输入框（无秒）→ RFC3339 UTC；空串返回 undefined。 */
export function localInputToIso(value: string): string | undefined {
  if (value.trim() === '') return undefined
  const date = new Date(value)
  return Number.isNaN(date.getTime()) ? undefined : date.toISOString()
}

/** 当前时间往前推 minutes 分钟的 RFC3339 UTC。 */
export function minutesAgoIso(minutes: number): string {
  return new Date(Date.now() - minutes * 60 * 1000).toISOString()
}
