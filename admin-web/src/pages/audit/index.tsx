import type { KeyboardEvent } from 'react'
import { useSearchParams } from 'react-router-dom'
import { BackButton } from '../../components/BackButton'
import { Card } from '../../components/Card'
import { PageShell } from '../../components/PageShell'
import type { AuditInitialFilter } from './auditFilter'
import { AuditEventsPanel } from './AuditEventsPanel'
import { ReviewQueuePanel } from './ReviewQueuePanel'

type AuditTab = 'review' | 'events'

const TABS: Array<{ key: AuditTab; label: string }> = [
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

  const handleTabKey = (event: KeyboardEvent<HTMLButtonElement>, index: number) => {
    const lastIndex = TABS.length - 1
    let target: number | undefined
    if (event.key === 'ArrowRight') target = index === lastIndex ? 0 : index + 1
    else if (event.key === 'ArrowLeft') target = index === 0 ? lastIndex : index - 1
    else if (event.key === 'Home') target = 0
    else if (event.key === 'End') target = lastIndex
    if (target === undefined) return
    event.preventDefault()
    const next = TABS[target]
    if (next === undefined) return
    selectTab(next.key)
    document.getElementById(`audit-tab-${next.key}`)?.focus()
  }

  return (
    <PageShell>
      <Card title="审计" subtitle="审核队列与平台审计事件">
        <nav className="tabbar" role="tablist" aria-label="审计分栏">
          {TABS.map((item, index) => (
            <button
              key={item.key}
              type="button"
              role="tab"
              id={`audit-tab-${item.key}`}
              aria-controls={tab === item.key ? `audit-panel-${item.key}` : undefined}
              aria-selected={tab === item.key}
              tabIndex={tab === item.key ? 0 : -1}
              className="tab"
              onClick={() => selectTab(item.key)}
              onKeyDown={(event) => handleTabKey(event, index)}
            >
              {item.label}
            </button>
          ))}
        </nav>
      </Card>

      <div
        className="tab-panel"
        role="tabpanel"
        id={`audit-panel-${tab}`}
        aria-labelledby={`audit-tab-${tab}`}
      >
        {tab === 'review' ? <ReviewQueuePanel /> : <AuditEventsPanel />}
      </div>
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
