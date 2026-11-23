/**
 * 组 / 角色 / 上游引用数据的共享查询：所有页面都从 fetchGroups、fetchRoles、fetchUpstreams 读同一份缓存。
 * 写操作按前缀失效（['groups']、['upstreams']），不需要逐页失效。
 */
import { useMemo } from 'react'
import { useQuery } from '@tanstack/react-query'
import { fetchGroups, fetchRoles, fetchUpstreamTools, fetchUpstreams } from './admin'
import type { GroupItem, GroupPage, RoleItem, RolePage } from './types'

export const GROUPS_KEY = ['groups'] as const
export const ROLES_KEY = ['roles'] as const
export const UPSTREAMS_KEY = ['upstreams'] as const
export const UPSTREAM_TOOLS_KEY = ['upstream-tools'] as const

/** 单个上游的工具缓存；写操作后按该 key 精确失效。 */
export function upstreamToolsQueryKey(id: string) {
  return [...UPSTREAM_TOOLS_KEY, id] as const
}

/** 全量组清单：名称映射、下拉选项与审计对象名都取这一份，后端不分页、一次返回全部。
 * 传 `limit` 只换一个独立 query key（同一 ['groups'] 前缀，写操作失效一次即可覆盖）；
 * 该参数后端不参与过滤，清单内容与不传时相同。 */
export function useGroupsQuery(limit?: number) {
  return useQuery({
    queryKey: limit === undefined ? GROUPS_KEY : [...GROUPS_KEY, { limit }],
    queryFn: ({ signal }) => fetchGroups(limit === undefined ? {} : { limit }, signal),
  })
}

/** 全量角色清单：界面一律显示 name，令牌/组/审计筛选仍发送 code。 */
export function useRolesQuery() {
  return useQuery({
    queryKey: ROLES_KEY,
    queryFn: ({ signal }) => fetchRoles(signal),
  })
}

/** 上游清单：维护页与需要选执行目标的表单共用同一份缓存。 */
export function useUpstreamsQuery() {
  return useQuery({
    queryKey: UPSTREAMS_KEY,
    queryFn: ({ signal }) => fetchUpstreams(signal),
  })
}

/**
 * 上游工具缓存：只读后端缓存（status=unknown、tools=[]），GET 不触网。
 * `id` 为 null 时不发请求（弹层关闭），刷新按钮负责触发探测并失效该 key。
 */
export function useUpstreamToolsQuery(id: string | null) {
  return useQuery({
    queryKey: upstreamToolsQueryKey(id ?? ''),
    queryFn: ({ signal }) => fetchUpstreamTools(id ?? '', signal),
    enabled: id !== null,
  })
}

export function indexGroups(page: GroupPage | undefined): Map<string, GroupItem> {
  const index = new Map<string, GroupItem>()
  for (const group of page?.items ?? []) index.set(group.code, group)
  return index
}

export function indexRoles(page: RolePage | undefined): Map<string, RoleItem> {
  const index = new Map<string, RoleItem>()
  for (const role of page?.items ?? []) index.set(role.code, role)
  return index
}

/** 组的展示名：映射缺失时回退 code，原值不被隐藏。 */
export function groupLabel(index: Map<string, GroupItem>, code: string): string {
  return index.get(code)?.name ?? code
}

/** 角色的展示名：映射缺失时回退 code，原值不被隐藏。 */
export function roleLabel(index: Map<string, RoleItem>, code: string): string {
  return index.get(code)?.name ?? code
}

/** 组件内直接拿组索引：与 useGroupsQuery 共用同一份缓存，不额外请求。 */
export function useGroupIndex(): Map<string, GroupItem> {
  const { data } = useGroupsQuery()
  return useMemo(() => indexGroups(data), [data])
}

/** 组件内直接拿角色索引：与 useRolesQuery 共用同一份缓存，不额外请求。 */
export function useRoleIndex(): Map<string, RoleItem> {
  const { data } = useRolesQuery()
  return useMemo(() => indexRoles(data), [data])
}
