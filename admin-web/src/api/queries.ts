/**
 * 组 / 角色引用数据的共享查询：所有页面都从 fetchGroups、fetchRoles 读同一份缓存。
 * 组写操作按前缀失效 ['groups']，角色写操作（如果将来出现）同理，不需要逐页失效。
 */
import { useMemo } from 'react'
import { useQuery } from '@tanstack/react-query'
import { fetchGroups, fetchRoles } from './admin'
import type { GroupItem, GroupPage, RoleItem, RolePage } from './types'

export const GROUPS_KEY = ['groups'] as const
export const ROLES_KEY = ['roles'] as const

/** 全量组清单：名称映射、下拉选项与审计对象名都取这一份。
 * 传 `limit` 时用独立 query key（同一 ['groups'] 前缀，写操作失效一次即可覆盖），
 * 供需要一次取全的下拉使用，避免默认分页截断。 */
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
