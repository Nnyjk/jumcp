/**
 * 登录回跳的统一规则：`?from=`（刷新/分享可用）与 history `state.from`（无刷新跳转）指向同一条路径。
 * 闸门（App.tsx）与登录页共用，避免两处各推导一遍导致回跳目标不一致。
 */
interface LocationLike {
  pathname: string
  search: string
  state: unknown
}

interface FromState {
  from?: { pathname?: string; search?: string } | undefined
}

/**
 * 不是登录页自身的站内路径才算合法回跳目标：必须以 `/` 开头且不以 `/login` 开头
 * （前缀判定，`/login` 下的任何路径都排除，否则会出现 /login → /login 死循环）。
 */
function usable(path: string | undefined): path is string {
  return path !== undefined && path.startsWith('/') && !path.startsWith('/login')
}

/** 回跳目标：history `state.from` 优先（无刷新跳转），其次 `?from=`，都没有回首页。 */
export function loginTarget(location: LocationLike): string {
  const state = location.state as FromState | null
  const fromState = state?.from
  const fromHistory =
    fromState?.pathname === undefined ? undefined : `${fromState.pathname}${fromState.search ?? ''}`
  if (usable(fromHistory)) return fromHistory
  const fromQuery = new URLSearchParams(location.search).get('from') ?? undefined
  if (usable(fromQuery)) return fromQuery
  return '/'
}

/** 未登录时跳登录页的地址：带上原始路径，登录成功后由 `loginTarget` 还原。 */
export function loginUrl(location: LocationLike): string {
  const from = `${location.pathname}${location.search}`
  return usable(from) ? `/login?from=${encodeURIComponent(from)}` : '/login'
}
