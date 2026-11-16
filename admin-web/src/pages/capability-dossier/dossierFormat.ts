import type { CapabilityVersion } from '../../api/types'
import type { DossierTab } from '../../viewStack'

export const MANAGED_TABS: Array<{ key: DossierTab; label: string }> = [
  { key: 'overview', label: '概览' },
  { key: 'definition', label: '定义' },
  { key: 'versions', label: '版本' },
  { key: 'approval', label: '审批' },
  { key: 'calls', label: '调用' },
  { key: 'audit', label: '审计' },
]

export const BUILTIN_TABS: Array<{ key: DossierTab; label: string }> = [
  { key: 'overview', label: '概览' },
  { key: 'calls', label: '调用' },
]

/** 直接访问 /capabilities/:id?tab=approval 时预选分栏；视图内的分栏不再写 URL。 */
export const TAB_KEYS: DossierTab[] = ['overview', 'definition', 'versions', 'approval', 'calls', 'audit']

export const STATUS_EXPLAIN: Record<string, string> = {
  draft: '草稿：尚未提交审核，只有管理台可见；定义可继续修改。',
  review: '待审：等待审批。',
  published: '已发布：调用方能否看到与调用，仍由「可被发现」「可被调用」两个开关决定。',
  deprecated: '已下线：不再接收新调用，调用方应迁移到替代能力。',
  builtin: '内置工具：由进程内注册，没有草稿/审批流程，也不能调整可见性。',
  unknown: '未知状态：后端未返回生命周期。',
}

/** 版本行可能没有审批元数据（批准人/批准时间/审批单号全空）；据此决定审批单说明，不假装能查到。 */
export function hasMissingApprovalMeta(item: CapabilityVersion | undefined): boolean {
  return (
    item !== undefined &&
    item.approved_by === null &&
    item.approved_at === null &&
    (item.approval_id === null || item.approval_id === '')
  )
}

export function latestVersion(items: CapabilityVersion[]): CapabilityVersion | undefined {
  return items.reduce<CapabilityVersion | undefined>(
    (latest, item) => (latest === undefined || item.version > latest.version ? item : latest),
    undefined,
  )
}
