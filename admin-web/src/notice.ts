import { ApiError } from './api/client'
import type { InfoBarSeverity } from './components/InfoBar'

/** 写操作统一提示：页面级 InfoBar 与 Modal 内错误共用。 */
export interface Notice {
  severity: InfoBarSeverity
  title: string
  description: string
  meta: string | undefined
}

/** 写失败统一展示 error.code + message，并保留 HTTP 状态供定位。 */
export function noticeFromError(error: unknown, title: string): Notice {
  if (error instanceof ApiError) {
    return {
      severity: 'danger',
      title,
      description: error.message,
      meta: error.status === 0 ? error.code : `${error.code} · HTTP ${error.status}`,
    }
  }
  return {
    severity: 'danger',
    title,
    description: error instanceof Error ? error.message : String(error),
    meta: undefined,
  }
}
