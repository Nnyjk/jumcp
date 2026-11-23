import { useQuery } from '@tanstack/react-query'
import { fetchCapabilityVersions } from '../../api/admin'
import type { CapabilityVersion } from '../../api/types'
import { statusText } from '../../audit'
import { Card } from '../../components/Card'
import { DefinitionView } from '../../components/DefinitionView'
import { EmptyState, ErrorState, LoadingState } from '../../components/PageShell'
import { DASH, formatDateTime } from '../../format'
import { useAdminView } from '../../viewStack'
import { hasMissingApprovalMeta } from './dossierFormat'
import { gsap } from 'gsap'
import { MOTION_DURATION, MOTION_EASE, useGsapScope } from '../../motion/useGsap'

export function VersionsTab({ id }: { id: string }) {
  const { openAudit } = useAdminView()
  const query = useQuery({
    queryKey: ['capability-versions', id],
    queryFn: ({ signal }) => fetchCapabilityVersions(id, signal),
  })

  const items: CapabilityVersion[] = query.data?.items ?? []
  const hasMissingMeta = items.some(hasMissingApprovalMeta)

  /* 动效约束：版本列表仅对前 20 条应用入场交错动效，确保多版本能力展开时的响应速度，消除长节点动效卡顿。 */
  const listRef = useGsapScope<HTMLDivElement>((scope) => {
    const items = Array.from(scope.children).slice(0, 20)
    if (items.length === 0) return
    gsap.from(items, {
      opacity: 0,
      y: 8,
      duration: MOTION_DURATION.enter,
      ease: MOTION_EASE,
      stagger: 0.02,
      clearProps: 'opacity,transform',
    })
  }, [items.map((item) => item.version).join('|')])

  return (
    <Card title="版本历史" subtitle={query.data === undefined ? '查询中' : `共 ${query.data.total} 个版本`}>
      {hasMissingMeta && (
        <p className="hint">
          该版本缺少审批元数据（批准人/批准时间/审批单号均为空）。
          <button
            type="button"
            className="inline-link"
            onClick={() => openAudit({ objectId: id })}
          >
            在审计中查看
          </button>
        </p>
      )}
      {query.isPending && <LoadingState rows={5} label="正在加载版本历史" />}
      {query.isError && <ErrorState error={query.error} />}
      {query.data !== undefined && items.length === 0 && (
        <EmptyState hint="暂无版本记录。首个版本定义经批准发布后将自动生成历史归档。" />
      )}
      {items.length > 0 && (
        <div className="version-list" ref={listRef}>
          {[...items]
            .sort((left, right) => right.version - left.version)
            .map((item) => (
              <details key={item.version} className="version-item">
                <summary className="version-summary">
                  <span className="mono version-no">v{item.version}</span>
                  <span className="version-status">{statusText(item.status)}</span>
                  <span className="version-meta">
                    <span className="mono">创建 {formatDateTime(item.created_at)}</span>
                    <span className="mono">
                      提审 {item.submitted_by ?? DASH}
                      {item.submitted_at ? ` · ${formatDateTime(item.submitted_at)}` : ''}
                    </span>
                    <span className="mono">
                      批准 {item.approved_by ?? DASH}
                      {item.approved_at ? ` · ${formatDateTime(item.approved_at)}` : ''}
                    </span>
                  </span>
                  <span
                    className="mono version-approval"
                    title={item.approval_id ?? DASH}
                  >
                    审批单 {item.approval_id === null || item.approval_id === '' ? DASH : item.approval_id}
                  </span>
                  <span className="mono version-hash" title={item.definition_hash}>
                    {item.definition_hash.slice(0, 12)}
                  </span>
                </summary>
                <div className="version-body">
                  <DefinitionView definition={item.definition} />
                </div>
              </details>
            ))}
        </div>
      )}
    </Card>
  )
}
