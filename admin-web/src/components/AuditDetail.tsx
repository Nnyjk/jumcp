import type { AuditEnvelope } from '../api/types'
import { groupLabel, roleLabel, useGroupIndex, useRoleIndex } from '../api/queries'
import {
  auditSentence,
  approvalText,
  changeFieldLabel,
  changeValueForField,
  changeValueText,
  denialReason,
  isStructuredEvent,
  statusText,
} from '../audit'
import { DASH, dashText, formatDateTime } from '../format'
import {
  ACTION_LABEL,
  APPROVAL_SOURCE_LABEL,
  EVENT_TYPE_LABEL,
  OBJECT_TYPE_LABEL,
  OUTCOME_LABEL,
  errorCodeText,
} from '../labels'
import { DetailField, DetailGrid } from './DetailGrid'
import { CodeBlock, JsonBlock } from './JsonBlock'
import { InfoBar } from './InfoBar'

/** data 是结构化事件的扩展载荷容器：只读取明确列出的运行期字段。 */
interface RunData {
  version?: unknown
  executed_script?: unknown
  bound_params?: unknown
  arguments?: unknown
  tokens?: unknown
  elapsed_ms?: unknown
  cache?: unknown
}

function runData(event: AuditEnvelope): RunData {
  const data = event.payload.data
  return data === undefined ? {} : (data as RunData)
}

function scriptText(value: unknown): string | undefined {
  return typeof value === 'string' && value.trim() !== '' ? value : undefined
}

/**
 * 审计详情：结构化摘要 + 变更表 + 审批块 + 执行 SQL/绑定值 + 折叠的原始载荷。
 * 非结构化事件不做字段回退：明确标注「旧格式事件，仅原始载荷」并折叠 payload。
 */
export function AuditDetail({ event }: { event: AuditEnvelope }) {
  const groupIndex = useGroupIndex()
  const roleIndex = useRoleIndex()
  const labelers = { group: (code: string) => groupIndex.get(code)?.name }
  const payload = event.payload
  const structured = isStructuredEvent(event)
  const data = runData(event)
  const executedScript = scriptText(data.executed_script)
  const hasArguments = data.arguments !== undefined && data.arguments !== null
  const hasBoundParams = Array.isArray(data.bound_params) && data.bound_params.length > 0
  const changes = structured ? (payload.changes ?? []) : []
  const denial = structured ? denialReason(payload) : undefined
  const hasErrorCode =
    payload.error_code !== null &&
    payload.error_code !== undefined &&
    payload.error_code.trim() !== ''
  const hasErrorMessage =
    typeof payload.error_message === 'string' && payload.error_message.trim() !== ''
  const hasCorrelation =
    typeof payload.correlation_id === 'string' && payload.correlation_id.trim() !== ''
  const hasStatusMove =
    (payload.from_status !== null && payload.from_status !== undefined) ||
    (payload.to_status !== null && payload.to_status !== undefined)
  const statusChanged = payload.from_status !== payload.to_status
  const showApproval =
    (payload.approval !== null && payload.approval !== undefined) ||
    payload.action === 'publish' ||
    payload.action === 'update_definition'

  return (
    <div className="audit-detail">
      {!structured && (
        <InfoBar
          severity="warning"
          title="旧格式事件，仅原始载荷"
          description="该事件发生在新审计契约之前，缺少 object_type / object_id / action。页面不按旧字段推断语义，请直接查看下方原始载荷。"
        />
      )}

      {structured && (
        <section className="summary-block">
          <h3 className="detail-heading">事件摘要</h3>
          <p className="summary-sentence">{auditSentence(event, labelers)}</p>
          {denial !== undefined && <p className="summary-reason">拒绝原因：{denial}</p>}
        </section>
      )}

      <DetailGrid>
        <DetailField label="事件 ID" mono title={event.event_id}>
          {event.event_id}
        </DetailField>
        <DetailField label="时间" mono title={event.ts}>
          {formatDateTime(event.ts)}
        </DetailField>
        <DetailField label="事件类型" title={event.event_type}>
          {EVENT_TYPE_LABEL[event.event_type] ?? event.event_type}
        </DetailField>
        <DetailField label="租户" title={event.tenant}>
          {dashText(event.tenant)}
        </DetailField>
        <DetailField label="主体" mono title={`${event.actor.type}:${event.actor.id || '(empty)'}`}>
          {event.actor.id.trim() === '' ? '未认证调用方' : event.actor.id}
        </DetailField>
        <DetailField label="主体角色 / 类型" title={event.actor.role}>
          {roleLabel(roleIndex, event.actor.role)} · {dashText(event.actor.type)}
        </DetailField>
        <DetailField label="主体来源" mono title="actor.source">
          {dashText(event.actor.source)}
        </DetailField>
        <DetailField label="令牌 ID" mono title="actor.token_id">
          {dashText(event.actor.token_id)}
        </DetailField>
        {structured && (
          <DetailField
            label="对象"
            title={payload.object_id ?? payload.object_type}
          >
            {(payload.object_type === undefined ? DASH : OBJECT_TYPE_LABEL[payload.object_type]) +
              ' '}
            {payload.object_type === 'group' && payload.object_id !== undefined
              ? groupLabel(groupIndex, payload.object_id)
              : (payload.object_id ?? DASH)}
          </DetailField>
        )}
        {structured && (
          <DetailField label="动作" title={payload.action}>
            {payload.action === undefined ? DASH : (ACTION_LABEL[payload.action] ?? payload.action)}
          </DetailField>
        )}
        {structured && (
          <DetailField label="结果" title={payload.outcome}>
            {payload.outcome === undefined ? DASH : (OUTCOME_LABEL[payload.outcome] ?? payload.outcome)}
          </DetailField>
        )}
        {structured && hasErrorCode && (
          <DetailField label="错误码" mono title={payload.error_code ?? undefined}>
            {errorCodeText(payload.error_code)}
          </DetailField>
        )}
        {structured && hasStatusMove && statusChanged && (
          <DetailField label="状态迁移">
            {statusText(payload.from_status)} → {statusText(payload.to_status)}
          </DetailField>
        )}
        {structured && data.version !== undefined && (
          <DetailField label="能力版本" mono title="data.version">
            {typeof data.version === 'number' ? `v${data.version}` : dashText(String(data.version))}
          </DetailField>
        )}
        {structured &&
          payload.schema_version !== null &&
          payload.schema_version !== undefined && (
            <DetailField label="载荷结构版本" mono title="schema_version">
              {payload.schema_version}
            </DetailField>
          )}
        {structured && hasCorrelation && (
          <DetailField label="关联 ID" mono title={payload.correlation_id ?? undefined}>
            {payload.correlation_id}
          </DetailField>
        )}
        {structured &&
          typeof payload.reason === 'string' &&
          payload.reason.trim() !== '' && (
            <DetailField label="原因" wide>
              {payload.reason}
            </DetailField>
          )}
        {structured && hasErrorMessage && (
          <DetailField label="错误原因" wide>
            {payload.error_message}
          </DetailField>
        )}
      </DetailGrid>

      {changes.length > 0 && (
        <section>
          <h3 className="detail-heading">字段变更</h3>
          <div className="table-scroll">
            <table className="changes-table">
              <thead>
                <tr>
                  <th scope="col">字段</th>
                  <th scope="col">变更前</th>
                  <th scope="col">变更后</th>
                </tr>
              </thead>
              <tbody>
                {changes.map((change, index) => (
                  <tr key={`${change.field}-${index}`}>
                    <td>
                      <span className="mono" title={change.field}>
                        {changeFieldLabel(change.field)}
                      </span>
                    </td>
                    <td className="changes-value">
                      {changeValueForField(change.field, change.before, {
                        role: (code) => roleIndex.get(code)?.name,
                      })}
                    </td>
                    <td className="changes-value">
                      {changeValueForField(change.field, change.after, {
                        role: (code) => roleIndex.get(code)?.name,
                      })}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </section>
      )}

      {structured && showApproval && (
        <section>
          <h3 className="detail-heading">审批</h3>
          <DetailGrid>
            <DetailField label="审批单号" mono>
              {approvalText(payload.approval)}
            </DetailField>
            {/* 约束：审批来源查 APPROVAL_SOURCE_LABEL 映射，未收录来源原样透传兜底，保证向前兼容且不丢失审计现场 */}
            <DetailField label="审批来源">
              {payload.approval === null || payload.approval === undefined
                ? DASH
                : (APPROVAL_SOURCE_LABEL[payload.approval.source] ?? payload.approval.source)}
            </DetailField>
            <DetailField label="提审人" mono>
              {dashText(payload.approval?.submitted_by ?? undefined)}
            </DetailField>
            <DetailField label="批准人" mono>
              {dashText(payload.approval?.approved_by ?? undefined)}
            </DetailField>
            <DetailField label="批准时间" mono>
              {formatDateTime(payload.approval?.approved_at ?? undefined)}
            </DetailField>
          </DetailGrid>
        </section>
      )}

      {executedScript !== undefined && (
        <section>
          <h3 className="detail-heading">执行 SQL</h3>
          <CodeBlock tone="sql">{executedScript}</CodeBlock>
        </section>
      )}

      {hasBoundParams && (
        <section>
          <h3 className="detail-heading">绑定值（按 ? 顺序）</h3>
          <ol className="bound-params">
            {(data.bound_params as unknown[]).map((value, index) => (
              <li key={index} className="mono">
                <span className="bound-index">#{index + 1}</span>
                {changeValueText(value)}
              </li>
            ))}
          </ol>
        </section>
      )}

      {hasArguments && (
        <section>
          <h3 className="detail-heading">调用参数</h3>
          <JsonBlock value={data.arguments} />
        </section>
      )}

      <details className="raw-payload">
        <summary>原始载荷（payload）</summary>
        <JsonBlock value={event.payload} />
      </details>
    </div>
  )
}
