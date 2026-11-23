/**
 * 登录页：独立于管理台外壳（无侧栏/标题栏），单卡片居中。
 *
 * 凭据只作为 POST /admin/login 的请求体瞬时值存在：不写 localStorage/sessionStorage，
 * 也不放进 URL；登录成功后身份由后端下发的 HttpOnly 会话 cookie 维持。
 */
import { useState } from 'react'
import type { FormEvent } from 'react'
import { gsap } from 'gsap'
import { ApiError } from '../../api/client'
import { useAuth } from '../../auth/session'
import { Icon } from '../../components/Icon'
import { MOTION_DURATION, MOTION_EASE, useGsapScope } from '../../motion/useGsap'
import '../../styles/login.css'

/** 把 ApiError 翻成登录页文案：401/429/网络错误各自固定措辞，其余沿用后端消息。 */
function loginError(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.status === 401) return '用户名或密码错误。'
    if (error.status === 429) return '尝试过于频繁，请稍后再试。'
    if (error.status === 0) return '无法连接后端。'
    return error.message
  }
  return '无法连接后端。'
}

export default function LoginPage() {
  const { signIn } = useAuth()
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')
  const [error, setError] = useState<string | undefined>(undefined)
  const [pending, setPending] = useState(false)

  const submit = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    if (pending) return
    if (username.trim() === '') {
      setError('请输入用户名。')
      return
    }
    if (password === '') {
      setError('请输入密码。')
      return
    }
    setPending(true)
    setError(undefined)
    try {
      await signIn(username.trim(), password)
      // 密码不留在内存里：登录成功后 App 的会话闸门会带着原始路径回跳。
      setPassword('')
    } catch (cause) {
      setError(loginError(cause))
      // 401 时清掉密码但保留用户名：绝大多数情况是密码打错，重填用户名反而更费事。
      if (cause instanceof ApiError && cause.status === 401) setPassword('')
    } finally {
      setPending(false)
    }
  }

  const cardRef = useGsapScope<HTMLElement>((scope) => {
    gsap.from(scope, {
      opacity: 0,
      y: 12,
      duration: MOTION_DURATION.enter,
      ease: MOTION_EASE,
      clearProps: 'opacity,transform',
    })
  })

  return (
    <div className="login-shell">
      <main className="login-card" aria-labelledby="login-title" ref={cardRef}>
        <div className="login-brand">
          <span className="login-mark">
            <Icon name="logo" size={22} />
          </span>
          <span className="login-wordmark">jumcp</span>
        </div>
        <h1 id="login-title">jumcp 管理台</h1>
        <p className="login-lede">用管理账户登录。密码只用于换取会话，不会保存在浏览器里。</p>
        <form className="login-form" onSubmit={submit} noValidate>
          <label className="field">
            <span>用户名</span>
            <input
              className="input"
              type="text"
              name="username"
              value={username}
              autoComplete="username"
              autoFocus
              required
              disabled={pending}
              spellCheck={false}
              autoCapitalize="none"
              onChange={(event) => {
                setUsername(event.target.value)
                if (error !== undefined) setError(undefined)
              }}
            />
          </label>
          <label className="field">
            <span>密码</span>
            <input
              className="input"
              type="password"
              name="password"
              value={password}
              autoComplete="current-password"
              required
              disabled={pending}
              onChange={(event) => {
                setPassword(event.target.value)
                if (error !== undefined) setError(undefined)
              }}
            />
          </label>
          {error !== undefined && (
            <p className="login-error" role="alert">
              <Icon name="alert" size={16} />
              {error}
            </p>
          )}
          <button type="submit" className="btn btn-primary login-submit" disabled={pending}>
            {pending ? '登录中…' : '登录'}
          </button>
        </form>
        <p className="login-foot">身份由 HttpOnly 会话 cookie 维持；退出登录后会自动吊销。</p>
      </main>
    </div>
  )
}

/** 会话探测中的骨架：与登录卡片同尺寸，避免首帧布局跳动。 */
export function SessionSplash() {
  return (
    <div className="login-shell">
      <div className="login-card" role="status" aria-live="polite" aria-busy="true">
        <span className="sr-only">正在检查登录状态</span>
        <div className="skeleton-stack" aria-hidden="true">
          <span className="skeleton-line" />
          <span className="skeleton-line" />
          <span className="skeleton-line" />
        </div>
      </div>
    </div>
  )
}

/** 会话探测失败（非 401，通常是后端不可达）：给一次显式重试，而不是把用户踢回登录页。 */
export function SessionUnavailable({ message, onRetry }: { message: string; onRetry: () => void }) {
  return (
    <div className="login-shell">
      <div className="login-card">
        <h1>无法确认登录状态</h1>
        <div className="state state-error login-state">
          <span className="state-icon">
            <Icon name="alert" size={20} />
          </span>
          <div>
            <strong>会话探测失败</strong>
            <p>{message}</p>
          </div>
        </div>
        <button type="button" className="btn btn-primary login-submit" onClick={onRetry}>
          重试
        </button>
      </div>
    </div>
  )
}
