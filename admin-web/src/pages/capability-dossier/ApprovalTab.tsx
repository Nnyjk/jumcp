import type { AdminIdentity, CapabilityDetail } from '../../api/types'
import { Card } from '../../components/Card'
import { InfoBar } from '../../components/InfoBar'
import { EmptyState } from '../../components/PageShell'
import type { Notice } from '../../notice'
import { ApprovalPanel } from './ApprovalPanel'

export function ApprovalTab({
  detail,
  me,
  mePending,
  meError,
  readOnly,
  builtin,
  onNotice,
  onChanged,
}: {
  detail: CapabilityDetail | undefined
  me: AdminIdentity | undefined
  mePending: boolean
  meError: unknown
  readOnly: boolean
  builtin: boolean
  onNotice: (notice: Notice) => void
  onChanged: () => void
}) {
  if (readOnly) {
    return (
      <Card title="审批">
        <InfoBar
          severity="info"
          title={builtin ? '内置工具不参与发布流程' : '没有受管定义，无法审批'}
          description={
            builtin
              ? '内置工具由进程内注册，没有草稿/待审/发布状态，也不能调整可见性。'
              : '该 id 只出现在审计与统计中，注册表没有对应能力记录，因此不提供发布或可见性操作。'
          }
        />
      </Card>
    )
  }

  if (detail === undefined) {
    return (
      <Card title="审批">
        <EmptyState hint="受管能力详情未加载，无法进行审批预检。" />
      </Card>
    )
  }

  return (
    <ApprovalPanel
      key={`${detail.id}@${detail.version}@${detail.status}`}
      detail={detail}
      me={me}
      mePending={mePending}
      meError={meError}
      onNotice={onNotice}
      onChanged={onChanged}
    />
  )
}
