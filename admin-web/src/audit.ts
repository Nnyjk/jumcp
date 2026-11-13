import type { AuditEnvelope, AuditPayload } from './api/types'
import {
  ACTION_LABEL,
  CHANGE_FIELD_LABEL,
  ERROR_CODE_LABEL,
  LIFECYCLE_LABEL,
  OBJECT_TYPE_LABEL,
  OUTCOME_LABEL,
} from './labels'

const DASH = '—'

/**
 * 新契约的结构化事件以 object_type/object_id/action 为必需字段。
 * 三者任一缺失即视为旧格式：页面只折叠展示原始载荷，不再猜测 capability_id/diff 等旧字段。
 */
export function isStructuredEvent(event: AuditEnvelope): boolean {
  const payload = event.payload
  return (
    payload.object_type !== undefined &&
    typeof payload.object_id === 'string' &&
    payload.object_id !== '' &&
    payload.action !== undefined
  )
}

/** 空 id 是未认证请求（后端按 token 派生失败），不能显示成某个叫 agent 的人。 */
export function actorLabel(actor: {
  id: string | null
  role: string | null
}): string {
  const id = (actor.id ?? '').trim()
  return id === '' ? '未认证调用方' : id
}

export function statusText(value: string | null | undefined): string {
  if (value === null || value === undefined || value === '') return DASH
  return LIFECYCLE_LABEL[value] ?? value
}

/** changes 的字段名转中文，保持后端口径可追溯（title 里仍是原字段名）。 */
export function changeFieldLabel(field: string): string {
  return CHANGE_FIELD_LABEL[field] ?? field
}

/**
 * 引用数据的展示名解析器：组/角色的中文名由页面注入（共享 ['groups']/['roles'] 缓存），
 * 映射缺失时解析器返回 undefined，文案保留原 code。
 */
export interface AuditLabelers {
  group?: (code: string) => string | undefined
  role?: (code: string) => string | undefined
}

/** 字段感知的展示值：状态/开关/角色用中文，其余保留原始值文本（raw payload 里仍可看原值）。 */
export function changeValueForField(
  field: string,
  value: unknown,
  labelers?: AuditLabelers,
): string {
  if (field === 'status') return statusText(typeof value === 'string' ? value : null)
  if (field === 'discoverable' || field === 'callable') {
    if (value === true) return '是'
    if (value === false) return '否'
    if (value === null || value === undefined) return '—'
    return changeValueText(value)
  }
  if (field === 'roles') {
    if (Array.isArray(value)) {
      if (value.length === 0) return '—'
      const role = labelers?.role
      return value.map((code) => role?.(String(code)) ?? String(code)).join('、')
    }
    return changeValueText(value)
  }
  return changeValueText(value)
}

export function changeValueText(value: unknown): string {
  if (value === null) return 'null'
  if (value === undefined) return DASH
  if (typeof value === 'string') return value === '' ? '空字符串' : value
  if (typeof value === 'number' || typeof value === 'boolean') return String(value)
  try {
    return JSON.stringify(value)
  } catch {
    return String(value)
  }
}

function objectText(payload: AuditPayload, labelers?: AuditLabelers): string {
  const type = payload.object_type === undefined ? DASH : OBJECT_TYPE_LABEL[payload.object_type]
  const code = payload.object_id ?? DASH
  const label =
    payload.object_type === 'group' && labelers?.group !== undefined
      ? (labelers.group(code) ?? code)
      : code
  return `${type} ${label}`
}

function actionText(payload: AuditPayload): string {
  return payload.action === undefined ? DASH : (ACTION_LABEL[payload.action] ?? payload.action)
}

function resultText(payload: AuditPayload): string {
  if (payload.outcome === undefined) return DASH
  const base = OUTCOME_LABEL[payload.outcome] ?? payload.outcome
  return payload.error_code ? `${base}（${codeText(payload.error_code)}）` : base
}

function codeText(code: string): string {
  const label = ERROR_CODE_LABEL[code]
  return label === undefined ? code : `${label} ${code}`
}

/** 策略拒绝原因：error_code 优先，其次 payload.data 里的 reason_code/detail。 */
export function denialReason(payload: AuditPayload): string | undefined {
  if (payload.error_code !== undefined && payload.error_code !== null && payload.error_code !== '') {
    const label = ERROR_CODE_LABEL[payload.error_code]
    const message = payload.error_message?.trim()
    const head = label === undefined ? payload.error_code : `${label}（${payload.error_code}）`
    return message === undefined || message === '' || message === payload.error_code
      ? head
      : `${head}：${message}`
  }
  const data = payload.data
  const reasonCode = data?.reason_code
  if (typeof reasonCode === 'string' && reasonCode !== '') {
    const label = ERROR_CODE_LABEL[reasonCode]
    return label === undefined ? reasonCode : `${label}（${reasonCode}）`
  }
  const message = payload.error_message?.trim()
  return message === undefined || message === '' ? undefined : message
}

/**
 * 主谓宾一句话：谁 + 对谁 + 做了什么 + 结果。
 * 这是审计列表、看板治理动态与能力档案时间线的统一口径。
 */
export function auditSentence(event: AuditEnvelope, labelers?: AuditLabelers): string {
  if (!isStructuredEvent(event)) return '旧格式事件，仅原始载荷'
  const payload = event.payload
  const parts: string[] = [
    `${actorLabel(event.actor)} 对${objectText(payload, labelers)} 执行${actionText(payload)}`,
  ]
  if (
    payload.from_status !== null &&
    payload.from_status !== undefined &&
    payload.from_status !== payload.to_status
  ) {
    parts.push(`${statusText(payload.from_status)} → ${statusText(payload.to_status)}`)
  }
  const changes = (payload.changes ?? []).filter((change) => change.field !== 'status')
  if (changes.length > 0) {
    parts.push(`变更 ${changes.map((change) => changeFieldLabel(change.field)).join('、')}`)
  }
  if (payload.reason !== null && payload.reason !== undefined && payload.reason !== '') {
    parts.push(`原因：${payload.reason}`)
  }
  parts.push(`结果：${resultText(payload)}`)
  return parts.join('，')
}

/** 表格内的摘要列：与 auditSentence 同口径，但更短，控制行高。 */
export function auditSummary(event: AuditEnvelope, labelers?: AuditLabelers): string {
  if (!isStructuredEvent(event)) return '旧格式事件，仅原始载荷'
  const payload = event.payload
  const move =
    payload.from_status === null ||
    payload.from_status === undefined ||
    payload.from_status === payload.to_status
      ? ''
      : `（${statusText(payload.from_status)}→${statusText(payload.to_status)}）`
  return `${actorLabel(event.actor)} ${actionText(payload)} ${objectText(payload, labelers)}${move} · ${resultText(payload)}`
}

export function approvalText(
  approval: AuditPayload['approval'],
): string {
  if (approval === null || approval === undefined) return '无审批记录'
  if (approval.source === 'none' || approval.id === '') return '无审批单'
  if (approval.source === 'external') {
    return `外部审批单 ${approval.id}（${approval.verified ? '已校验' : '系统不校验'}）`
  }
  return approval.id
}
