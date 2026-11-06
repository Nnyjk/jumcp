import type { AuditEventType, CapabilityRisk, CapabilityStatus } from './api/types'

/**
 * 展示用中文名。后端原值仍是唯一契约：徽标/单元格的 title 保留英文原值，
 * 过滤参数与错误码一律使用原值，避免界面翻译与状态机脱节。
 */
export const STATUS_LABEL: Record<CapabilityStatus, string> = {
  draft: '草稿',
  review: '待审',
  published: '已发布',
  deprecated: '已下线',
}

export const RISK_LABEL: Record<CapabilityRisk, string> = {
  read: '读',
  write: '写',
  sensitive: '敏感',
  admin: '管理',
}

export const EVENT_TYPE_LABEL: Record<AuditEventType, string> = {
  session_init: '会话初始化',
  tools_list_served: '工具列表下发',
  capability_search: '能力检索',
  capability_describe: '能力描述',
  tool_call: '工具调用',
  template_run: '模板执行',
  policy_decision: '策略判定',
  admin_change: '管理变更',
}

/** /health 的 status 是后端自由字符串：已知值翻译，未知值原样展示。 */
export const HEALTH_STATUS_LABEL: Record<string, string> = {
  ok: '正常',
  degraded: '降级',
  error: '故障',
}
