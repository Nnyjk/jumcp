import { useState } from 'react'
import { useMutation, useQuery } from '@tanstack/react-query'
import {
  fetchCapabilityVersions,
  publishCapability,
  rejectCapability,
  setCapabilityVisibility,
  submitCapability,
} from '../../api/admin'
import { ApiError } from '../../api/client'
import { roleLabel, useRoleIndex } from '../../api/queries'
import type { AdminIdentity, CapabilityDetail } from '../../api/types'
import { DASH, formatDateTime } from '../../format'
import { visibilityText } from '../../labels'
import { LifecycleBadge } from '../../components/Badge'
import { Card } from '../../components/Card'
import { DetailField, DetailGrid } from '../../components/DetailGrid'
import { ErrorState } from '../../components/PageShell'
import { InfoBar } from '../../components/InfoBar'
import { SubmitConfirmDialog } from '../../components/SubmitConfirmDialog'
import { ToggleSwitch } from '../../components/ToggleSwitch'
import { noticeMessage, type Notice } from '../../notice'
import { useAdminView } from '../../viewStack'
import { hasMissingApprovalMeta, latestVersion } from './dossierFormat'

export function ApprovalPanel({
  detail,
  me,
  mePending,
  meError,
  onNotice,
  onChanged,
}: {
  detail: CapabilityDetail
  me: AdminIdentity | undefined
  mePending: boolean
  meError: unknown
  onNotice: (notice: Notice) => void
  onChanged: () => void
}) {
  const roleIndex = useRoleIndex()
  const { openAudit } = useAdminView()
  const [approvalId, setApprovalId] = useState('')
  // 版本行可能整行缺少审批元数据；据此决定审批单说明，不假装能查到。
  const versionsQuery = useQuery({
    queryKey: ['capability-versions', detail.id],
    queryFn: ({ signal }) => fetchCapabilityVersions(detail.id, signal),
  })
  const missingApprovalMeta = hasMissingApprovalMeta(latestVersion(versionsQuery.data?.items ?? []))
  // 已发布时以当前值为准；待审时默认「发布后开放」，管理员可改成系统直调或保持关闭。
  const published = detail.status === 'published'
  const [discoverable, setDiscoverable] = useState(published ? detail.discoverable : true)
  const [callable, setCallable] = useState(published ? detail.callable : true)
  const [reason, setReason] = useState('')
  const [confirmSubmit, setConfirmSubmit] = useState(false)
  // 发布与退回是两个独立表单：共用一份错误态会把退回校验错误渲染到发布表单里，看起来像「点了没反应」。
  const [publishError, setPublishError] = useState<string | undefined>(undefined)
  const [rejectError, setRejectError] = useState<string | undefined>(undefined)
  const [visibilityError, setVisibilityError] = useState<string | undefined>(undefined)

  const submittedBy = (detail.submitted_by ?? '').trim()

  const submitMutation = useMutation({
    mutationFn: () => submitCapability(detail.id),
    onSuccess: () => {
      setConfirmSubmit(false)
      onNotice({
        severity: 'success',
        title: '已提交审核',
        description: `${detail.id}@${detail.version} 已进入待审状态，等待审批。`,
        meta: undefined,
      })
      onChanged()
    },
    onError: () => {
      // 失败原因由 SubmitConfirmDialog 通过 mutation.error 原地展示，弹层不关闭。
    },
  })

  const publishMutation = useMutation({
    mutationFn: async () => {
      await publishCapability(detail.id, approvalId.trim())
      try {
        await setCapabilityVisibility(detail.id, { discoverable, callable })
        return { visibilityFailed: false as const }
      } catch (error) {
        return { visibilityFailed: true as const, error }
      }
    },
    onSuccess: (result) => {
      if (result.visibilityFailed) {
        onNotice({
          severity: 'warning',
          title: '已发布但未开放',
          description:
            '发布成功，但第二步可见性保存失败；调用方仍然看不到该能力。请在「审批动作」卡片重试保存可见性。',
          meta:
            result.error instanceof ApiError
              ? `${result.error.code} · HTTP ${result.error.status}`
              : undefined,
        })
      } else {
        onNotice({
          severity: 'success',
          title: '已发布并开放',
          description: `${detail.id}@${detail.version} 已发布，可见性已按所选值保存。`,
          meta: `可发现=${discoverable ? '是' : '否'} · 可调用=${callable ? '是' : '否'}`,
        })
      }
      setApprovalId('')
      setPublishError(undefined)
      onChanged()
    },
    onError: (error) => setPublishError(noticeMessage(error, '发布失败')),
  })

  const visibilityMutation = useMutation({
    mutationFn: () => setCapabilityVisibility(detail.id, { discoverable, callable }),
    onSuccess: () => {
      setVisibilityError(undefined)
      onNotice({
        severity: 'success',
        title: '可见性已保存',
        description: `${detail.id}：${visibilityText(discoverable, callable)}`,
        meta: undefined,
      })
      onChanged()
    },
    onError: (error) => setVisibilityError(noticeMessage(error, '保存可见性失败')),
  })

  const rejectMutation = useMutation({
    mutationFn: () => rejectCapability(detail.id, reason.trim()),
    onSuccess: (result) => {
      setReason('')
      setRejectError(undefined)
      onNotice({
        severity: 'success',
        title: '已退回',
        description: `${detail.id} 已退回草稿：${result.reason}`,
        meta: `版本 v${result.version}`,
      })
      onChanged()
    },
    onError: (error) => setRejectError(noticeMessage(error, '退回失败')),
  })

  const busy =
    submitMutation.isPending ||
    publishMutation.isPending ||
    rejectMutation.isPending ||
    visibilityMutation.isPending

  return (
    <div className="dossier-columns">
      <Card title="主体与审批">
        <DetailGrid>
          <DetailField label="当前主体" mono title="GET /admin/me">
            {mePending ? '读取中…' : (me?.id ?? DASH)}
          </DetailField>
          <DetailField label="当前主体角色" title={me?.role}>
            {mePending
              ? DASH
              : me === undefined
                ? DASH
                : `${roleLabel(roleIndex, me.role)} · ${me.type}`}
          </DetailField>
          <DetailField label="提审人" mono>
            {submittedBy === '' ? '尚未提审' : submittedBy}
          </DetailField>
          <DetailField label="提审时间" mono>
            {formatDateTime(detail.submitted_at)}
          </DetailField>
          <DetailField label="当前状态">
            <LifecycleBadge status={detail.status} />
          </DetailField>
          <DetailField label="版本" mono>
            v{detail.version}
          </DetailField>
        </DetailGrid>

        {meError !== null && meError !== undefined && <ErrorState error={meError} />}

      </Card>

      <Card title="审批动作" subtitle={`当前版本 v${detail.version}`}>
        {detail.status === 'draft' && (
          <div className="approval-action">
            <p className="hint">草稿提交后进入待审，等待审批。</p>
            <button
              type="button"
              className="btn btn-primary"
              disabled={busy}
              onClick={() => {
                submitMutation.reset()
                setConfirmSubmit(true)
              }}
            >
              {submitMutation.isPending ? '提交中…' : '提交审核'}
            </button>
          </div>
        )}

        {confirmSubmit && (
          <SubmitConfirmDialog
            id={detail.id}
            version={detail.version}
            status={<LifecycleBadge status={detail.status} />}
            pending={submitMutation.isPending}
            error={submitMutation.error ?? undefined}
            onClose={() => setConfirmSubmit(false)}
            onConfirm={() => submitMutation.mutate()}
          />
        )}

        {detail.status === 'review' && (
          <form
            className="create-form"
            onSubmit={(event) => {
              event.preventDefault()
              if (busy) return
              if (approvalId.trim() === '') {
                setPublishError('外部审批单号不能为空。')
                return
              }
              setPublishError(undefined)
              publishMutation.mutate()
            }}
          >
            <label className="field">
              <span title="approval_id">
                外部审批单号
              </span>
              <input
                className="input"
                value={approvalId}
                onChange={(event) => setApprovalId(event.target.value)}
                placeholder="例如 ap-biz-water-1"
              />
            </label>
            <p className="hint">
              批准人将记录为当前主体（{me?.id ?? '读取中…'}），不由表单填写；系统不校验外部审批单真伪。
            </p>
            <fieldset className="publish-visibility">
              <legend>发布后可见性（第二步）</legend>
              <ToggleSwitch
                checked={discoverable}
                label="可被发现"
                title="discoverable"
                onChange={setDiscoverable}
              />
              <ToggleSwitch
                checked={callable}
                label="可被调用"
                title="callable"
                onChange={setCallable}
              />
              <p className="hint">
                发布只把状态改为已发布；这两个开关决定调用方能否真正使用。第二步失败会明确提示「已发布但未开放」。
              </p>
            </fieldset>
            {publishError !== undefined && (
              <p className="inline-error" role="alert">
                {publishError}
              </p>
            )}
            <div className="form-actions">
              <button
                type="submit"
                className="btn btn-primary"
                disabled={busy || mePending || me === undefined}
              >
                {publishMutation.isPending ? '发布中…' : '批准发布并开放'}
              </button>
            </div>
          </form>
        )}

        {detail.status === 'published' && (
          <div className="approval-action">
            <p className="hint">调整可见性不会新增版本，也不会触发重新审批。</p>
            <ToggleSwitch
              checked={discoverable}
              label="可被发现"
              title="discoverable"
              onChange={setDiscoverable}
            />
            <ToggleSwitch
              checked={callable}
              label="可被调用"
              title="callable"
              onChange={setCallable}
            />
            <button
              type="button"
              className="btn btn-secondary"
              disabled={busy}
              onClick={() => {
                if (busy) return
                visibilityMutation.mutate()
              }}
            >
              {visibilityMutation.isPending ? '保存中…' : '保存可见性'}
            </button>
            {visibilityError !== undefined && (
              <p className="inline-error" role="alert">
                {visibilityError}
              </p>
            )}
          </div>
        )}

        {detail.status === 'review' && (
          <form
            className="create-form approval-reject"
            onSubmit={(event) => {
              event.preventDefault()
              if (busy) return
              if (reason.trim() === '') {
                setRejectError('退回原因不能为空。')
                return
              }
              setRejectError(undefined)
              rejectMutation.mutate()
            }}
          >
            <label className="field">
              <span>退回原因</span>
              <textarea
                className="input textarea"
                rows={3}
                value={reason}
                onChange={(event) => setReason(event.target.value)}
                placeholder="说明需要修改的内容；退回后回到草稿"
              />
            </label>
            {rejectError !== undefined && (
              <p className="inline-error" role="alert">
                {rejectError}
              </p>
            )}
            <div className="form-actions">
              <button type="submit" className="btn btn-danger" disabled={busy}>
                {rejectMutation.isPending ? '退回中…' : '退回草稿'}
              </button>
            </div>
          </form>
        )}

        {detail.status === 'deprecated' && (
          <InfoBar
            severity="info"
            title="已下线"
            description="该能力不再参与审批；如需重新上线，需要先恢复为草稿或新建替代能力。"
          />
        )}
      </Card>

      <Card title="当前可见性" subtitle="公开给调用方的实际暴露面">
        <DetailGrid>
          <DetailField label="可被发现" title="discoverable">
            {detail.discoverable ? '是' : '否'}
          </DetailField>
          <DetailField label="可被调用" title="callable">
            {detail.callable ? '是' : '否'}
          </DetailField>
          <DetailField label="组合含义" wide>
            {visibilityText(detail.discoverable, detail.callable)}
          </DetailField>
          <DetailField label="审批单" wide>
            {detail.status === 'review' ? (
              '发布时填写的外部审批单号'
            ) : missingApprovalMeta ? (
              <>
                该版本缺少审批元数据（批准人/批准时间/审批单号均为空）。
                <button
                  type="button"
                  className="inline-link"
                  onClick={() => openAudit({ objectId: detail.id })}
                >
                  在审计中查看
                </button>
              </>
            ) : (
              <>
                历史审批见「版本」分栏；
                <button
                  type="button"
                  className="inline-link"
                  onClick={() => openAudit({ objectId: detail.id })}
                >
                  在审计中查看
                </button>
              </>
            )}
          </DetailField>
        </DetailGrid>
      </Card>
    </div>
  )
}
