/**
 * admin API 契约层（§4.4.4 admin_* 函数）：把后端 admin HTTP 面的真实响应结构映射成类型化结果。
 * 页面层只读本层导出的函数，不直接 fetch；写操作同样经此落审计。
 *
 * 治理身份一律由后端按 token 派生：前端不发送 approved_by，也不存在任何伪造主体的入口。
 */
import { request } from './client'
import type {
  AccessTokenCreateInput,
  AccessTokenCreated,
  AccessTokenItem,
  AccessTokenPage,
  AdminIdentity,
  AuditPage,
  AuditQuery,
  CapabilityDetail,
  CapabilityItem,
  CapabilityQuery,
  CapabilityVersionsPage,
  CreatedCapability,
  DeletedAccessToken,
  GroupCreateInput,
  GroupItem,
  GroupPage,
  GroupQuery,
  GroupUpdateInput,
  HealthPayload,
  ItemPage,
  Page,
  RejectedCapability,
  ReviewQueueItem,
  RolePage,
  TokenOptions,
  ToolCallsPage,
  ToolStatsPayload,
  UpdatedCapabilityDefinition,
  UpstreamInput,
  UpstreamItem,
} from './types'

/** 分页固定条数（后端默认 limit=50，上限 200）。 */
export const PAGE_LIMIT = 50

/** 工具统计固定取上限：本页在客户端做来源/健康/搜索过滤，避免每次筛选都打后端。 */
export const TOOL_STATS_LIMIT = 200

/** 工具调用记录每页条数。 */
export const TOOL_CALLS_LIMIT = 50

/** 拉取待审核队列（draft / review 全量，不分页）。 */
export async function fetchReviewQueue(signal?: AbortSignal): Promise<ItemPage<ReviewQueueItem>> {
  return request<ItemPage<ReviewQueueItem>>('/admin/review-queue', { signal })
}

/** 当前登录主体（后端按 token 派生）：审批人只读展示的唯一来源。 */
export async function fetchAdminMe(signal?: AbortSignal): Promise<AdminIdentity> {
  return request<AdminIdentity>('/admin/me', { signal })
}

/**
 * 按 §4.4.4 admin_audit_queries 的过滤条件分页查询审计。
 * 默认最新优先（order=desc）；时间范围由调用方转成 RFC3339 后传入。
 */
export async function fetchAuditEvents(
  query: AuditQuery,
  offset: number,
  signal?: AbortSignal,
  limit: number = PAGE_LIMIT,
): Promise<AuditPage> {
  return request<AuditPage>('/admin/audit-events', {
    params: {
      tenant: query.tenant,
      event_type: query.event_type,
      object_type: query.object_type,
      object_id: query.object_id,
      actor_id: query.actor_id,
      event_id: query.event_id,
      actor_role: query.actor_role,
      token_id: query.token_id,
      ts_after: query.ts_after,
      ts_before: query.ts_before,
      order: query.order ?? 'desc',
      limit,
      offset,
    },
    signal,
  })
}

/** 取「最近 limit 条」审计事件：后端按 order 排序，offset=0 即最新一页。 */
export async function fetchLatestAuditEvents(
  query: AuditQuery,
  limit: number,
  signal?: AbortSignal,
): Promise<AuditPage> {
  return fetchAuditEvents({ ...query, order: 'desc' }, 0, signal, limit)
}

/** 能力清单（例外处理视图的只读来源）。 */
export async function fetchCapabilities(
  query: CapabilityQuery,
  offset: number,
  signal?: AbortSignal,
): Promise<Page<CapabilityItem>> {
  return request<Page<CapabilityItem>>('/admin/capabilities', {
    params: {
      status: query.status,
      group: query.group,
      q: query.q,
      limit: PAGE_LIMIT,
      offset,
    },
    signal,
  })
}

/** 健康检查：后端返回结构化 JSON（含审计链校验结论）。 */
export async function fetchHealth(signal?: AbortSignal): Promise<HealthPayload> {
  return request<HealthPayload>('/health', { signal })
}

/** 工具统计：按时间窗聚合调用量、token 与健康度。 */
export async function fetchToolStats(
  windowDays: number,
  signal?: AbortSignal,
): Promise<ToolStatsPayload> {
  return request<ToolStatsPayload>('/admin/tools/stats', {
    params: { window_days: windowDays, limit: TOOL_STATS_LIMIT },
    signal,
  })
}

/** 单个工具的最近调用记录：window_days 与台账时间窗保持同一口径。 */
export async function fetchToolCalls(
  id: string,
  offset: number,
  windowDays: number,
  signal?: AbortSignal,
): Promise<ToolCallsPage> {
  return request<ToolCallsPage>(`/admin/tools/${encodeURIComponent(id)}/calls`, {
    params: { window_days: windowDays, limit: TOOL_CALLS_LIMIT, offset },
    signal,
  })
}

/** 能力完整记录：统计行不含 definition，档案页按 id 取完整记录。 */
export async function fetchCapabilityDetail(
  id: string,
  signal?: AbortSignal,
): Promise<CapabilityDetail> {
  return request<CapabilityDetail>(`/admin/capabilities/${encodeURIComponent(id)}`, { signal })
}

/** 版本历史：定义、哈希与提审/批准元数据。 */
export async function fetchCapabilityVersions(
  id: string,
  signal?: AbortSignal,
): Promise<CapabilityVersionsPage> {
  return request<CapabilityVersionsPage>(
    `/admin/capabilities/${encodeURIComponent(id)}/versions`,
    { signal },
  )
}

/**
 * 编辑能力定义：draft 直接保存；published 必须带外部审批单号（批准人来自 token）。
 * approvalId 为空时不下发，避免用空串冒充审批记录。
 */
export async function updateCapabilityDefinition(
  id: string,
  definition: Record<string, unknown>,
  approvalId?: string,
): Promise<UpdatedCapabilityDefinition> {
  const body: Record<string, unknown> = { definition }
  if (approvalId !== undefined && approvalId.trim() !== '') body.approval_id = approvalId.trim()
  return request<UpdatedCapabilityDefinition>(
    `/admin/capabilities/${encodeURIComponent(id)}/definition`,
    { method: 'PUT', body },
  )
}

/** draft → review：提交审核。 */
export async function submitCapability(id: string): Promise<void> {
  await request<void>(`/admin/capabilities/${encodeURIComponent(id)}/submit`, { method: 'POST' })
}

/** review → published：只提交外部审批单号，批准人由后端按 token 派生。 */
export async function publishCapability(id: string, approvalId: string): Promise<void> {
  await request<void>(`/admin/capabilities/${encodeURIComponent(id)}/publish`, {
    method: 'POST',
    body: { approval_id: approvalId },
  })
}

/** review → draft：退回需写明原因，后端把原因记入响应与审计。 */
export async function rejectCapability(id: string, reason: string): Promise<RejectedCapability> {
  return request<RejectedCapability>(`/admin/capabilities/${encodeURIComponent(id)}/reject`, {
    method: 'POST',
    body: { reason },
  })
}

/** 只改双布尔：不触发重新审核，但后端会记 admin_change。 */
export async function setCapabilityVisibility(
  id: string,
  patch: { discoverable?: boolean; callable?: boolean },
): Promise<void> {
  await request<void>(`/admin/capabilities/${encodeURIComponent(id)}/visibility`, {
    method: 'POST',
    body: patch,
  })
}

/** 新建草案：definition 原样透传后端 schema 校验；group 必须是已存在的组。 */
export async function createCapability(
  definition: Record<string, unknown>,
): Promise<CreatedCapability> {
  return request<CreatedCapability>('/admin/capabilities', {
    method: 'POST',
    body: { definition },
  })
}

/** 下架：successor_id 必须是已存在的后继能力。 */
export async function deprecateCapability(id: string, successorId: string): Promise<void> {
  await request<void>(`/admin/capabilities/${encodeURIComponent(id)}/deprecate`, {
    method: 'POST',
    body: { successor_id: successorId },
  })
}

/** 组清单：policy 组来自 config/policy.toml（只读），managed 组可在管理台维护。 */
export async function fetchGroups(query: GroupQuery = {}, signal?: AbortSignal): Promise<GroupPage> {
  return request<GroupPage>('/admin/groups', {
    params: {
      q: query.q?.trim() || undefined,
      source: query.source,
      role: query.role,
      limit: query.limit,
    },
    signal,
  })
}

/** 角色清单：组维护、令牌与审计的角色展示名都取自这里，令牌/审计里仍传 code。 */
export async function fetchRoles(signal?: AbortSignal): Promise<RolePage> {
  return request<RolePage>('/admin/roles', { signal })
}

export async function createGroup(input: GroupCreateInput): Promise<GroupItem> {
  return request<GroupItem>('/admin/groups', { method: 'POST', body: input })
}

export async function updateGroup(code: string, input: GroupUpdateInput): Promise<GroupItem> {
  return request<GroupItem>(`/admin/groups/${encodeURIComponent(code)}`, {
    method: 'PUT',
    body: input,
  })
}

/** 删除受管组；仍被能力/令牌引用时后端返回 409，错误 data 带两个计数。 */
export async function deleteGroup(code: string): Promise<{ deleted: boolean }> {
  return request<{ deleted: boolean }>(`/admin/groups/${encodeURIComponent(code)}`, {
    method: 'DELETE',
  })
}

/** 令牌清单：后端只存 hash 与 prefix，列表不含明文。 */
export async function fetchAccessTokens(signal?: AbortSignal): Promise<AccessTokenPage> {
  return request<AccessTokenPage>('/admin/tokens', { signal })
}

/** 创建令牌：201 响应里的 token 是唯一一次明文返回。 */
export async function createAccessToken(input: AccessTokenCreateInput): Promise<AccessTokenCreated> {
  return request<AccessTokenCreated>('/admin/tokens', { method: 'POST', body: input })
}

/** 撤销令牌：撤销后该 token 的 MCP 请求立即失败。 */
export async function revokeAccessToken(id: string): Promise<AccessTokenItem> {
  return request<AccessTokenItem>(`/admin/tokens/${encodeURIComponent(id)}/revoke`, {
    method: 'POST',
  })
}

/**
 * 重新签发令牌：保留 id 与全部配置，换一把新明文，旧 token 立即失效。
 * 响应与创建同形：`token` 是唯一一次明文返回；仅 active 令牌可重签（409）。
 */
export async function reissueAccessToken(id: string): Promise<AccessTokenCreated> {
  return request<AccessTokenCreated>(`/admin/tokens/${encodeURIComponent(id)}/reissue`, {
    method: 'POST',
  })
}

/**
 * 删除令牌记录：只允许已撤销 / 已过期的令牌，active 会被后端以 409 拒绝。
 * 这是硬删除，列表里不再出现该记录；审计事件保留，撤销入口仍走 revokeAccessToken。
 */
export async function deleteAccessToken(id: string): Promise<DeletedAccessToken> {
  return request<DeletedAccessToken>(`/admin/tokens/${encodeURIComponent(id)}`, {
    method: 'DELETE',
  })
}

/** 创建表单的可选项：角色默认组、组来源与内置工具清单。 */
export async function fetchTokenOptions(signal?: AbortSignal): Promise<TokenOptions> {
  return request<TokenOptions>('/admin/tokens/options', { signal })
}

/**
 * MCP 上游 router 清单：router 持有上游连接，管理台只维护配置并触发探测。
 * 返回不分页的 {items,total}；条目带探测状态与工具数。
 */
export async function fetchUpstreams(signal?: AbortSignal): Promise<ItemPage<UpstreamItem>> {
  return request<ItemPage<UpstreamItem>>('/admin/upstreams', { signal })
}

/** 单条上游：编辑弹层需要最新探测状态时按 id 拉取。 */
export async function fetchUpstream(id: string, signal?: AbortSignal): Promise<UpstreamItem> {
  return request<UpstreamItem>(`/admin/upstreams/${encodeURIComponent(id)}`, { signal })
}

/** 新建上游：id 由调用方指定，创建后 router 按该 id 注册工具。 */
export async function createUpstream(input: UpstreamInput): Promise<UpstreamItem> {
  return request<UpstreamItem>('/admin/upstreams', { method: 'POST', body: input })
}

/** 编辑上游：id 不可改，body 仍是完整 UpstreamInput。 */
export async function updateUpstream(id: string, input: UpstreamInput): Promise<UpstreamItem> {
  return request<UpstreamItem>(`/admin/upstreams/${encodeURIComponent(id)}`, {
    method: 'PUT',
    body: input,
  })
}

/** 删除上游：router 同步移除其工具，审计事件另行保留。 */
export async function deleteUpstream(id: string): Promise<{ deleted: boolean }> {
  return request<{ deleted: boolean }>(`/admin/upstreams/${encodeURIComponent(id)}`, {
    method: 'DELETE',
  })
}

/** 测试连接：重新探测上游并返回最新状态与工具数。 */
export async function refreshUpstream(id: string): Promise<UpstreamItem> {
  return request<UpstreamItem>(`/admin/upstreams/${encodeURIComponent(id)}/refresh`, {
    method: 'POST',
  })
}
