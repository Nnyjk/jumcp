/**
 * 管理台会话契约（admin auth）：用户名+密码登录换取 HttpOnly cookie，登出吊销会话，探测当前会话。
 * 密码只作为登录请求体的瞬时值出现，不落任何存储；身份读取一律经 /admin/session。
 */
import { request } from './client'
import type { AdminActor, AdminSession } from './types'

/** POST /admin/login：204 + Set-Cookie `jumcp_session`（HttpOnly、SameSite=Strict）。 */
export async function login(username: string, password: string): Promise<void> {
  await request<void>('/admin/login', {
    method: 'POST',
    body: { username, password },
    // 登录页自己展示 401/429，不触发全局登出跳转。
    skipUnauthorized: true,
  })
}

/** POST /admin/logout：204，吊销当前会话；幂等，未登录也算成功。 */
export async function logout(): Promise<void> {
  await request<void>('/admin/logout', { method: 'POST', skipUnauthorized: true })
}

/** GET /admin/session：未登录 401；探测失败由调用方决定是否当作未登录。 */
export async function fetchSession(signal?: AbortSignal): Promise<AdminActor> {
  const session = await request<AdminSession>('/admin/session', { signal, skipUnauthorized: true })
  return session.actor
}
