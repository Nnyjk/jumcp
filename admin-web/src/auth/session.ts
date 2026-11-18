import { createContext, useContext } from 'react'
import type { AdminActor } from '../api/types'

export type SessionState =
  | { state: 'pending' }
  | { state: 'anonymous' }
  | { state: 'failed'; message: string }
  | { state: 'authenticated'; actor: AdminActor }

export interface AuthValue {
  session: SessionState
  /** 探测失败（非 401，如后端不可达）时重试探测。 */
  retry: () => void
  signIn: (token: string) => Promise<void>
  signOut: () => Promise<void>
}

/** 与 `viewStack.ts` 同样放在非组件文件里，组件文件只导出组件（react-refresh 要求）。 */
export const AuthContext = createContext<AuthValue | undefined>(undefined)

export function useAuth(): AuthValue {
  const value = useContext(AuthContext)
  if (value === undefined) throw new Error('useAuth 必须在 AuthProvider 内使用')
  return value
}
