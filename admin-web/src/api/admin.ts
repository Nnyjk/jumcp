/**
 * admin API 契约层（admin_* 函数）：把后端 admin HTTP 面的真实响应结构映射成类型化结果。
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
  AdminAccount,
  AdminAccountCreateInput,
  AdminAccountPage,
  AdminAccountUpdateInput,
  AdminIdentity,
  AuditPage,
  AuditQuery,
  CapabilityDetail,
  CapabilityItem,
  CapabilityQuery,
  CapabilityVersionsPage,
  CreatedCapability,
  DeletedAccessToken,
  DeletedMcpInstallation,
  GroupCreateInput,
  GroupItem,
  GroupPage,
  GroupQuery,
  GroupUpdateInput,
  HealthPayload,
  ItemPage,
  McpCatalogInstallation,
  McpCatalogPage,
  McpInstallConfigInput,
  McpInstallInput,
  McpInstallResult,
  Page,
  RejectedCapability,
  ReviewQueueItem,
  RolePage,
  TokenOptions,
  ToolCallsPage,
  ToolStatsPayload,
  TransportKey,
  UpdatedCapabilityDefinition,
  UpdatedBasicTools,
  UpdateBasicToolsInput,
  UpstreamInput,
  UpstreamItem,
  UpstreamToolsPayload,
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
 * 按 admin_audit_queries 的过滤条件分页查询审计。
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
      surface: query.surface,
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

/** 下线：successor_id 必须是已存在的后继能力。 */
export async function deprecateCapability(id: string, successorId: string): Promise<void> {
  await request<void>(`/admin/capabilities/${encodeURIComponent(id)}/deprecate`, {
    method: 'POST',
    body: { successor_id: successorId },
  })
}

/**
 * 组清单：policy 组来自 config/policy.toml（只读），managed 组可在管理台维护。
 * 后端不分页、一次返回全部条目；`limit` 会被忽略，保留它只为兼容旧调用方。
 */
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

/**
 * 上游工具缓存：GET 不触网，无缓存时后端返回 status=unknown 与空 tools。
 * 需要最新工具列表时先调用 refreshUpstream 触发探测。
 */
export async function fetchUpstreamTools(
  id: string,
  signal?: AbortSignal,
): Promise<UpstreamToolsPayload> {
  return request<UpstreamToolsPayload>(
    `/admin/upstreams/${encodeURIComponent(id)}/tools`,
    { signal },
  )
}

/**
 * 批量调整上游工具的基础组归属：只提交变更键。
 * 404=上游不存在；409 为 UNKNOWN / NOT_READ_ONLY / NOT_ELIGIBLE；超过 64 键为 400。
 */
export async function updateUpstreamBasicTools(
  id: string,
  input: UpdateBasicToolsInput,
): Promise<UpdatedBasicTools> {
  return request<UpdatedBasicTools>(
    `/admin/upstreams/${encodeURIComponent(id)}/tools/basic`,
    { method: 'PUT', body: input },
  )
}

/* ----------------------------- MCP 安装目录 ------------------------------ */

/**
 * 安装目录：包定义、安装字段、工具与已安装实例一次返回。
 * 目录项 id 只用于安装接口路径，已安装实例仍以上游 id 为准。
 */
export async function fetchMcpCatalog(signal?: AbortSignal): Promise<McpCatalogPage> {
  return request<McpCatalogPage>('/admin/mcp/catalog', { signal })
}

/**
 * 安装目录项：成功返回新上游（与 GET /admin/upstreams/{id} 同形）。
 * 409=MCP_UPSTREAM_CONFLICT，400 为字段校验失败或上游 id 冲突。
 */
export async function installMcpCatalogEntry(
  id: string,
  input: McpInstallInput,
): Promise<McpInstallResult> {
  return request<McpInstallResult>(`/admin/mcp/catalog/${encodeURIComponent(id)}/install`, {
    method: 'POST',
    body: input,
  })
}

/**
 * 修改已安装实例的配置：非密字段全量提交（空串清空），密字段只提交本次输入、缺键即保持原值。
 * 响应只回传最新 installation（密文依旧不回显），页面按整页失效重取，不做乐观更新。
 */
export async function updateMcpInstallConfig(
  id: string,
  upstreamId: string,
  input: McpInstallConfigInput,
): Promise<{ installation: McpCatalogInstallation }> {
  return request<{ installation: McpCatalogInstallation }>(
    `/admin/mcp/catalog/${encodeURIComponent(id)}/install/${encodeURIComponent(upstreamId)}/config`,
    { method: 'PATCH', body: input },
  )
}

/** 卸载目录项安装的实例：删除上游配置，router 同步移除其工具。 */
export async function uninstallMcpCatalogEntry(
  id: string,
  upstreamId: string,
): Promise<DeletedMcpInstallation> {
  return request<DeletedMcpInstallation>(
    `/admin/mcp/catalog/${encodeURIComponent(id)}/install/${encodeURIComponent(upstreamId)}`,
    { method: 'DELETE' },
  )
}

/* ---------------------------- 传输加密密钥 ------------------------------ */

/**
 * 浏览器端传输加密密钥：密码字段必须用它做 AES-256-GCM 加密后再提交。
 * 同一管理鉴权；密钥未配置时后端返回 503 MCP_TRANSPORT_KEY_MISSING，调用方应提示而不发安装/改配置请求。
 */
export async function fetchTransportKey(signal?: AbortSignal): Promise<TransportKey> {
  return request<TransportKey>('/admin/transport-key', { signal })
}

/* ---------------------------- 管理账户 ------------------------------ */

/** 账户清单：不分页；前端按用户名排序展示。 */
export async function fetchAdminAccounts(signal?: AbortSignal): Promise<AdminAccountPage> {
  return request<AdminAccountPage>('/admin/admins', { signal })
}

/** 新建账户：201 返回创建后的账户（不含密码）。 */
export async function createAdminAccount(input: AdminAccountCreateInput): Promise<AdminAccount> {
  const body = await request<{ admin: AdminAccount }>('/admin/admins', {
    method: 'POST',
    body: input,
  })
  return body.admin
}

/** 改用户名 / 重置密码 / 启用停用：只发送有变化的字段。 */
export async function updateAdminAccount(
  id: string,
  input: AdminAccountUpdateInput,
): Promise<AdminAccount> {
  const body = await request<{ admin: AdminAccount }>(`/admin/admins/${encodeURIComponent(id)}`, {
    method: 'PATCH',
    body: input,
  })
  return body.admin
}

/** 删除账户：204。最后一个可用账户与自己会被后端以 409 拒绝。 */
export async function deleteAdminAccount(id: string): Promise<void> {
  await request<void>(`/admin/admins/${encodeURIComponent(id)}`, { method: 'DELETE' })
}
