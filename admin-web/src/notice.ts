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

/** 行内失败文案：description + 可选 meta；用于必须留在表单/弹层内的错误（页面顶部 InfoBar 会被弹层挡住）。 */
export function noticeMessage(error: unknown, title: string): string {
  const notice = noticeFromError(error, title)
  return notice.meta === undefined ? notice.description : `${notice.description}（${notice.meta}）`
}
