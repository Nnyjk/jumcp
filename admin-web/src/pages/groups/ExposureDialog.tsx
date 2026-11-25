import { useMemo, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { fetchGroupExposure, updateGroupExposure } from '../../api/admin'
import { ApiError } from '../../api/client'
import type {
  GroupExposureInput,
  GroupExposurePayload,
  GroupExposureTool,
  GroupToolRuleState,
} from '../../api/types'
import { Badge, CategoryBadge } from '../../components/Badge'
import { DataTable } from '../../components/DataTable'
import { InfoBar } from '../../components/InfoBar'
import { Modal } from '../../components/Modal'
import { ErrorState, LoadingState } from '../../components/PageShell'
import { ToggleSwitch } from '../../components/ToggleSwitch'
import { noticeFromError, type Notice } from '../../notice'

/** 三态规则：inherit 表示没有规则行，工具可见性跟随第一层；include/exclude 是逐工具例外。 */
type RuleDraft = GroupToolRuleState | 'inherit'

const RULE_STATES: RuleDraft[] = ['inherit', 'include', 'exclude']

const RULE_LABEL: Record<RuleDraft, string> = {
  inherit: '继承',
  include: '强制纳入',
  exclude: '强制排除',
}

const RULE_TITLE: Record<RuleDraft, string> = {
  inherit: '不写规则行：上游授予本组时该工具可见',
  include: '即使上游未授予本组，也强制把这个工具纳入',
  exclude: '即使上游已授予本组，也强制把这个工具排除',
}

/**
 * 保存失败的提示：PUT 是整请求校验，409 时按业务 code 翻成可操作的中文；
 * 其它错误沿用统一的 notice 呈现（保留 HTTP 状态与原始 message）。
 */
function exposureSaveNotice(error: unknown): Notice {
  if (error instanceof ApiError && error.status === 409) {
    const description =
      error.code === 'MCP_UPSTREAM_TOOL_UNKNOWN'
        ? '工具已不在上游缓存里，请先刷新上游工具。'
        : error.code === 'MCP_UPSTREAM_TOOL_NOT_ELIGIBLE'
          ? '维护通道工具只能进 admin 组。'
          : undefined
    if (description !== undefined) {
      return {
        severity: 'danger',
        title: '暴露矩阵未保存',
        description,
        meta: `${error.code} · HTTP 409`,
      }
    }
  }
  return noticeFromError(error, '保存暴露矩阵失败')
}

/**
 * 一台上游的逐工具规则表。
 * `granted` 是本次编辑中第一层的生效值；未授予时规则行保留但不生效（撤销授予会删掉 exclude，
 * 只留 include，因此这里如实提示「整台不可见」）。
 * 孤儿行（orphaned）工具已不在上游缓存里：只提供清除规则，避免无法归位的失效配置留在库里。
 */
function ToolRulesTable({
  label,
  rows,
  draft,
  granted,
  isAdminGroup,
  busy,
  onChange,
}: {
  label: string
  rows: GroupExposureTool[]
  draft: Record<string, RuleDraft>
  granted: boolean
  isAdminGroup: boolean
  busy: boolean
  onChange: (tool: string, rule: RuleDraft) => void
}) {
  return (
    <DataTable
      label={label}
      columns={[
        {
          key: 'tool',
          header: '工具',
          width: '260px',
          wrap: true,
          cell: (tool) => (
            <span className="cell-stack">
              <span className="mono">{tool.exposed}</span>
              {tool.exposed !== tool.tool && (
                <span className="cell-sub mono" title="上游原始工具名">
                  {tool.tool}
                </span>
              )}
            </span>
          ),
        },
        {
          key: 'flags',
          header: '标记',
          width: '160px',
          cell: (tool) =>
            tool.admin_only || tool.orphaned ? (
              <span className="detail-badges">
                {tool.admin_only && <Badge tone="warning" title="维护通道工具只能进 admin 组">维护通道</Badge>}
                {tool.orphaned && <Badge tone="danger" title="规则还在，工具已不在上游缓存里">工具已失效</Badge>}
              </span>
            ) : (
              <span className="muted">—</span>
            ),
        },
        {
          key: 'rule',
          header: '规则',
          width: '300px',
          cell: (tool) => {
            const rule = draft[tool.tool] ?? tool.state
            if (tool.orphaned) {
              const cleared = rule === 'inherit'
              return (
                <span className="exposure-rules">
                  <span className="cell-sub">
                    {cleared ? '已标记清除，保存后生效' : '工具已失效，只能清除规则'}
                  </span>
                  <button
                    type="button"
                    className="btn btn-subtle btn-sm"
                    disabled={busy}
                    title={cleared ? '撤销清除，保留原有规则' : '删除这条失效规则，回到继承第一层'}
                    onClick={() => onChange(tool.tool, cleared ? tool.state : 'inherit')}
                  >
                    {cleared ? '撤销清除' : '清除规则'}
                  </button>
                </span>
              )
            }
            return (
              <span className="segmented" role="group" aria-label={`${tool.exposed} 的规则`}>
                {RULE_STATES.map((state) => {
                  const blocked = state === 'include' && tool.admin_only && !isAdminGroup
                  return (
                    <button
                      key={state}
                      type="button"
                      aria-pressed={rule === state}
                      disabled={blocked || busy}
                      title={blocked ? '维护通道工具只能进 admin 组' : RULE_TITLE[state]}
                      onClick={() => onChange(tool.tool, state)}
                    >
                      {RULE_LABEL[state]}
                    </button>
                  )
                })}
              </span>
            )
          },
        },
        {
          key: 'visible',
          header: '可见性',
          width: '180px',
          cell: (tool) => {
            if (tool.orphaned) return <span className="muted">—</span>
            const rule = draft[tool.tool] ?? tool.state
            // include 恒可见、exclude 恒不可见、inherit 跟随第一层（含本次未保存的勾选）。
            const visible = rule === 'include' || (rule === 'inherit' && granted)
            return (
              <Badge tone={visible ? 'success' : 'neutral'}>
                {rule === 'inherit'
                  ? `跟随上游授予：${visible ? '可见' : '不可见'}`
                  : visible
                    ? '可见'
                    : '不可见'}
              </Badge>
            )
          },
        },
      ]}
      rows={rows}
      rowKey={(tool) => `${tool.upstream_id}\u0000${tool.tool}`}
      minWidth="900px"
    />
  )
}

/**
 * 组的两层暴露矩阵。
 * 一层是「整台上游对本组可见」（mcp_group_upstreams），二层是逐工具例外
 * （mcp_group_tool_rules：include 强制纳入 / exclude 强制排除），没有规则行的工具继承第一层。
 * 保存是合并语义：只提交与加载时快照不同的键；未变更的键不出现，避免覆盖他人刚写入的授予与规则。
 */
export function ExposureDialog({
  code,
  name,
  onClose,
  onSaved,
}: {
  code: string
  name: string
  onClose: () => void
  onSaved: () => void
}) {
  const queryClient = useQueryClient()
  const exposureKey = ['group-exposure', code] as const
  const exposureQuery = useQuery({
    queryKey: exposureKey,
    queryFn: ({ signal }) => fetchGroupExposure(code, signal),
  })
  const payload: GroupExposurePayload | undefined = exposureQuery.data
  /** 维护通道（admin 类别上游、admin_ 前缀工具）只允许落在 admin 组。 */
  const isAdminGroup = code === 'admin'

  /** 草稿只记录用户改过的键；生效值 = 草稿 ?? 服务端快照。 */
  const [upstreamDraft, setUpstreamDraft] = useState<Record<string, boolean>>({})
  const [toolDraft, setToolDraft] = useState<Record<string, Record<string, RuleDraft>>>({})
  const [expanded, setExpanded] = useState<Record<string, boolean>>({})
  const [notice, setNotice] = useState<Notice | undefined>(undefined)

  const upstreams = useMemo(() => payload?.upstreams ?? [], [payload])
  const tools = useMemo(() => payload?.tools ?? [], [payload])
  const upstreamIds = useMemo(() => new Set(upstreams.map((upstream) => upstream.id)), [upstreams])

  const toolsByUpstream = useMemo(() => {
    const grouped = new Map<string, GroupExposureTool[]>()
    for (const tool of tools) {
      const bucket = grouped.get(tool.upstream_id)
      if (bucket === undefined) grouped.set(tool.upstream_id, [tool])
      else bucket.push(tool)
    }
    return grouped
  }, [tools])

  /**
   * 停用/已删除上游仍可能带着规则行：后端把上游行只列 enabled 的，规则行照旧返回，
   * 这些行必须单独列出来，否则失效配置无处清理。
   */
  const detached = useMemo(
    () =>
      [...toolsByUpstream.entries()]
        .filter(([upstreamId]) => !upstreamIds.has(upstreamId))
        .map(([upstreamId, rows]) => ({ upstreamId, rows })),
    [toolsByUpstream, upstreamIds],
  )

  /** 快照索引：把草稿折成「相对服务端状态」的最小变更集。 */
  const snapshot = useMemo(
    () => new Map(tools.map((tool) => [`${tool.upstream_id}\u0000${tool.tool}`, tool.state])),
    [tools],
  )

  const upstreamChanges = useMemo(() => {
    const changes: Record<string, boolean> = {}
    for (const upstream of upstreams) {
      const draft = upstreamDraft[upstream.id]
      if (draft !== undefined && draft !== upstream.granted) changes[upstream.id] = draft
    }
    return changes
  }, [upstreams, upstreamDraft])

  const toolChanges = useMemo(() => {
    const changes: Record<string, Record<string, GroupToolRuleState | null>> = {}
    for (const [upstreamId, rules] of Object.entries(toolDraft)) {
      for (const [tool, rule] of Object.entries(rules)) {
        if (rule === (snapshot.get(`${upstreamId}\u0000${tool}`) ?? 'inherit')) continue
        // null 表示删除规则行、回到继承第一层。
        changes[upstreamId] = { ...(changes[upstreamId] ?? {}), [tool]: rule === 'inherit' ? null : rule }
      }
    }
    return changes
  }, [toolDraft, snapshot])

  const toolChangeCount = Object.values(toolChanges).reduce(
    (count, rules) => count + Object.keys(rules).length,
    0,
  )
  const changeCount = Object.keys(upstreamChanges).length + toolChangeCount

  const saveMutation = useMutation({
    mutationFn: (input: GroupExposureInput) => updateGroupExposure(code, input),
    onSuccess: (result) => {
      // 先写回最新视图，避免失效重取期间弹层跳回旧状态；组清单的统计交给父组件刷新。
      queryClient.setQueryData<GroupExposurePayload>(exposureKey, result.exposure)
      void queryClient.invalidateQueries({ queryKey: ['tool-stats'] })
      onSaved()
    },
    onError: (error) => setNotice(exposureSaveNotice(error)),
  })

  const save = () => {
    if (changeCount === 0) {
      onClose()
      return
    }
    const input: GroupExposureInput = {}
    if (Object.keys(upstreamChanges).length > 0) input.upstreams = upstreamChanges
    if (toolChangeCount > 0) input.tools = toolChanges
    setNotice(undefined)
    saveMutation.mutate(input)
  }

  const setRule = (upstreamId: string, tool: string, rule: RuleDraft) =>
    setToolDraft((current) => ({
      ...current,
      [upstreamId]: { ...(current[upstreamId] ?? {}), [tool]: rule },
    }))

  const busy = saveMutation.isPending

  return (
    <Modal
      title={`暴露矩阵 · ${name}`}
      subtitle={`组 ${code} · 第一层整台上游授予 + 第二层逐工具强制纳入/排除；保存只提交变更项`}
      size="xl"
      onClose={onClose}
      footer={
        <>
          <button type="button" className="btn btn-secondary" onClick={onClose} disabled={busy}>
            取消
          </button>
          <button type="button" className="btn btn-primary" onClick={save} disabled={busy}>
            {busy ? '保存中…' : changeCount === 0 ? '关闭' : `保存 ${changeCount} 项变更`}
          </button>
        </>
      }
    >
      <div className="exposure">
        {notice !== undefined && (
          <InfoBar
            severity={notice.severity}
            title={notice.title}
            description={notice.description}
            meta={notice.meta}
          />
        )}

        {exposureQuery.isPending && <LoadingState rows={4} label="正在读取组暴露矩阵" />}

        {exposureQuery.isError && (
          <div className="state-wrap">
            <ErrorState error={exposureQuery.error} />
            <button
              type="button"
              className="btn btn-secondary"
              onClick={() => exposureQuery.refetch()}
              disabled={exposureQuery.isFetching}
            >
              重试
            </button>
          </div>
        )}

        {payload !== undefined && (
          <section>
            <h3 className="detail-heading">第一层 · 上游整台授予</h3>
            <p className="hint">
              授予后该上游的全部工具对本组可见；展开任一行设置第二层逐工具规则。
              维护通道上游（admin 类别或 admin_ 前缀工具）只能进入 admin 组。
            </p>

            {tools.length === 0 && (
              <InfoBar
                severity="info"
                title="尚未拉取工具清单，请先在「上游」页刷新"
                description="上游工具缓存为空时这里只有上游行；刷新后每个上游下会出现可展开的工具行。"
              />
            )}

            {upstreams.length === 0 ? (
              <p className="hint">还没有登记任何上游，请先在「上游」页接入。</p>
            ) : (
              <div className="exposure-upstreams">
                {upstreams.map((upstream) => {
                  const granted = upstreamDraft[upstream.id] ?? upstream.granted
                  const locked = upstream.admin_only && !isAdminGroup
                  const rows = toolsByUpstream.get(upstream.id) ?? []
                  const ruleRows = rows.filter((tool) => tool.state !== 'inherit').length
                  // 已设规则的上游默认展开：否则例外规则藏在折叠区里，管理员会以为没生效。
                  const open = expanded[upstream.id] ?? (ruleRows > 0)
                  return (
                    <div className="exposure-upstream" key={upstream.id}>
                      <div className="exposure-upstream-head">
                        <div className="exposure-upstream-text">
                          <span>{upstream.name}</span>
                          <span className="cell-sub mono" title="上游 id">
                            {upstream.id}
                          </span>
                        </div>
                        <span className="detail-badges">
                          <CategoryBadge category={upstream.category} />
                          {upstream.admin_only && (
                            <Badge tone="warning" title="维护通道上游只能进 admin 组">维护通道</Badge>
                          )}
                          {!upstream.enabled && (
                            <Badge tone="neutral" title="上游已停用：授予与规则保留，调用阶段不参与投影">已停用</Badge>
                          )}
                          <Badge tone="neutral" title="上游工具缓存里的工具数">
                            {upstream.tool_count} 个工具
                          </Badge>
                        </span>
                        <div className="exposure-upstream-actions">
                          <ToggleSwitch
                            checked={granted}
                            disabled={locked || busy}
                            label={granted ? '已授予' : '未授予'}
                            title={locked ? '维护通道上游只能进 admin 组' : '整台上游对本组可见'}
                            onChange={(next) =>
                              setUpstreamDraft((current) => ({ ...current, [upstream.id]: next }))
                            }
                          />
                        </div>
                      </div>

                      {locked && (
                        <p className="hint">
                          维护通道上游只能进 admin 组：本组不是 admin 组，无法授予。
                        </p>
                      )}

                      {rows.length === 0 ? (
                        // 全表都没有工具行时由上面的 InfoBar 统一说明，这里逐台上游不再重复。
                        tools.length === 0 ? null : (
                          <p className="hint">该上游还没有工具行，请先在「上游」页刷新工具清单。</p>
                        )
                      ) : (
                        <>
                          <div className="exposure-section-head">
                            <span className="cell-sub">
                              {rows.length} 个工具行 · 已设规则 {ruleRows} 条
                            </span>
                            <button
                              type="button"
                              className="btn btn-subtle btn-sm"
                              aria-expanded={open}
                              onClick={() =>
                                setExpanded((current) => ({ ...current, [upstream.id]: !open }))
                              }
                            >
                              {open ? '收起逐工具规则' : '展开逐工具规则'}
                            </button>
                          </div>
                          {open && !granted && (
                            <p className="hint">
                              该上游未授予本组：强制纳入的规则仍生效，强制排除与继承行不生效。
                            </p>
                          )}
                          {open && (
                            <ToolRulesTable
                              label={`${upstream.name} 的逐工具规则`}
                              rows={rows}
                              draft={toolDraft[upstream.id] ?? {}}
                              granted={granted}
                              isAdminGroup={isAdminGroup}
                              busy={busy}
                              onChange={(tool, rule) => setRule(upstream.id, tool, rule)}
                            />
                          )}
                        </>
                      )}
                    </div>
                  )
                })}
              </div>
            )}

            {detached.length > 0 && (
              <>
                <h3 className="detail-heading">已停用或已移除的上游</h3>
                <p className="hint">
                  上游已不在启用清单里，但这里的规则行仍然存在。重新启用该上游前它们不生效；
                  可以在这里清除失效规则，保存同样只提交变更项。
                </p>
                {detached.map(({ upstreamId, rows }) => (
                  <div className="exposure-upstream" key={upstreamId}>
                    <div className="exposure-upstream-head">
                      <div className="exposure-upstream-text">
                        <span className="mono" title="上游 id">
                          {upstreamId}
                        </span>
                      </div>
                    </div>
                    <ToolRulesTable
                      label={`${upstreamId} 的残留规则`}
                      rows={rows}
                      draft={toolDraft[upstreamId] ?? {}}
                      granted={false}
                      isAdminGroup={isAdminGroup}
                      busy={busy}
                      onChange={(tool, rule) => setRule(upstreamId, tool, rule)}
                    />
                  </div>
                ))}
              </>
            )}
          </section>
        )}
      </div>
    </Modal>
  )
}
