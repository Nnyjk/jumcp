import { useSearchParams } from 'react-router-dom'
import { BackButton } from '../../components/BackButton'
import { Card } from '../../components/Card'
import { PageShell } from '../../components/PageShell'
import { TabBar, TabPanel, type TabItem } from '../../components/TabBar'
import type { AuditInitialFilter } from './auditFilter'
import { AuditEventsPanel } from './AuditEventsPanel'
import { ReviewQueuePanel } from './ReviewQueuePanel'

type AuditTab = 'review' | 'events'

const TABS: TabItem<AuditTab>[] = [
  { key: 'review', label: '审核队列' },
  { key: 'events', label: '审计事件' },
]

export default function AuditPage() {
  const [searchParams, setSearchParams] = useSearchParams()
  // 页签是路由状态：`?view=review` 才是审核队列，其余（含缺省）都是事件视图，深链与刷新都还原同一视图。
  const tab: AuditTab = searchParams.get('view') === 'review' ? 'review' : 'events'

  const selectTab = (next: AuditTab) => {
    // 保留其它 query（event_id / object_id / token_id）：从调用详情深链进来时筛选条件不能丢。
    const params = new URLSearchParams(searchParams)
    params.set('view', next)
    setSearchParams(params, { replace: true })
  }

  return (
    <PageShell>
      <Card title="审计" subtitle="审核队列与平台审计事件">
        <TabBar idBase="audit" items={TABS} value={tab} onChange={selectTab} label="审计分栏" />
      </Card>

      <TabPanel idBase="audit" tabKey={tab}>
        {tab === 'review' ? <ReviewQueuePanel /> : <AuditEventsPanel />}
      </TabPanel>
    </PageShell>
  )
}

/**
 * 操作视图里的审计：不是路由页面，没有页签外壳，只渲染事件面板。
 * 由侧栏之外的操作按钮（令牌/能力档案）带入 token_id / object_id / event_id 预设；
 * 预设变化时调用方用 key 重挂载，面板内部的草稿状态随之重置。
 */
export function AuditView({
  initial,
  onBack,
  backLabel,
}: {
  initial?: AuditInitialFilter | undefined
  onBack: () => void
  backLabel: string
}) {
  return (
    <PageShell>
      <BackButton label={backLabel} onClick={onBack} />
      <AuditEventsPanel initial={initial} />
    </PageShell>
  )
}
