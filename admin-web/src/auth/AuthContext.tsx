/**
 * 管理台会话：身份只来自 HttpOnly cookie（`jumcp_session`），前端不接触 token。
 *
 * AuthProvider 挂载时探测 `/admin/session`，并把「任意请求 401」接成匿名态：
 * client 因此不必依赖 router，跳转/守卫统一由 App 的会话闸门完成。
 */
import { useCallback, useEffect, useMemo, useState } from 'react'
import type { ReactNode } from 'react'
import { fetchSession, login, logout } from '../api/auth'
import { ApiError, setUnauthorizedHandler } from '../api/client'
import { AuthContext } from './session'
import type { AuthValue, SessionState } from './session'

/** 尚未探测出结果时，鉴权闸门渲染骨架而不是先闪一次登录页。 */
const PENDING: SessionState = { state: 'pending' }

export function AuthProvider({ children }: { children: ReactNode }) {
  // `undefined` = 还没探测（含重试后的中间态），`session` 由它派生，effect 里不写 pending。
  const [result, setResult] = useState<SessionState | undefined>(undefined)
  const [attempt, setAttempt] = useState(0)

  useEffect(() => {
    let cancelled = false
    fetchSession()
      .then((actor) => {
        if (!cancelled) setResult({ state: 'authenticated', actor })
      })
      .catch((error: unknown) => {
        if (cancelled) return
        // 401 = 没有/已过期的会话，属于未登录而非故障。
        if (error instanceof ApiError && error.status === 401) {
          setResult({ state: 'anonymous' })
          return
        }
        setResult({
          state: 'failed',
          message: error instanceof Error ? error.message : '无法连接后端。',
        })
      })
    return () => {
      cancelled = true
    }
  }, [attempt])

  // 全局 401：清登录态，由闸门带着当前路径回登录页（登录/探测请求已豁免）。
  useEffect(() => {
    setUnauthorizedHandler(() => {
      setResult((current) => (current?.state === 'anonymous' ? current : { state: 'anonymous' }))
    })
    return () => setUnauthorizedHandler(undefined)
  }, [])

  const signIn = useCallback(async (token: string) => {
    await login(token)
    // 登录响应只下发 cookie：主体身份以 /admin/session 为准，避免前端自行编造。
    const actor = await fetchSession()
    setResult({ state: 'authenticated', actor })
  }, [])

  const signOut = useCallback(async () => {
    try {
      await logout()
    } finally {
      // 吊销失败（如后端不可达）也要离开管理台：cookie 由后端掌握，本地不留任何登录态。
      setResult({ state: 'anonymous' })
    }
  }, [])

  const value = useMemo<AuthValue>(
    () => ({
      session: result ?? PENDING,
      retry: () => {
        setResult(undefined)
        setAttempt((value) => value + 1)
      },
      signIn,
      signOut,
    }),
    [result, signIn, signOut],
  )

  return <AuthContext.Provider value={value}>{children}</AuthContext.Provider>
}
