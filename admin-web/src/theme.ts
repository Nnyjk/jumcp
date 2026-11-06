import { useCallback, useEffect, useState } from 'react'

/**
 * 主题偏好：system 表示跟随系统，light/dark 是用户显式覆盖。
 * 解析结果写到 <html data-theme> 与 color-scheme，CSS 只认这一份 token，不做第二套媒体查询。
 */
export type ThemePreference = 'light' | 'dark' | 'system'

export type ResolvedTheme = 'light' | 'dark'

const STORAGE_KEY = 'hjzl-admin-theme'
const DARK_QUERY = '(prefers-color-scheme: dark)'

/** 读取持久化偏好；无记录或存储不可用时回落到 system。 */
export function readThemePreference(): ThemePreference {
  try {
    const stored = window.localStorage.getItem(STORAGE_KEY)
    return stored === 'light' || stored === 'dark' || stored === 'system' ? stored : 'system'
  } catch {
    return 'system'
  }
}

/** 解析成实际生效的明暗，并把结果同步到 <html data-theme> / color-scheme。 */
export function applyTheme(preference: ThemePreference): ResolvedTheme {
  const resolved: ResolvedTheme =
    preference === 'system'
      ? window.matchMedia(DARK_QUERY).matches
        ? 'dark'
        : 'light'
      : preference
  const root = document.documentElement
  root.dataset.theme = resolved
  root.style.colorScheme = resolved
  return resolved
}

/**
 * 管理主题偏好：system 时监听系统配色变化实时跟随；用户选择持久化到 localStorage。
 * 首帧由 main.tsx 在 render 前先行应用，避免闪烁。
 */
export function useThemePreference() {
  const [preference, setPreference] = useState<ThemePreference>(readThemePreference)

  useEffect(() => {
    applyTheme(preference)
    try {
      window.localStorage.setItem(STORAGE_KEY, preference)
    } catch {
      // 隐私模式等场景下存储不可用：本次会话仍按内存状态生效。
    }
    if (preference !== 'system') return
    const media = window.matchMedia(DARK_QUERY)
    const onChange = () => applyTheme('system')
    media.addEventListener('change', onChange)
    return () => media.removeEventListener('change', onChange)
  }, [preference])

  const selectPreference = useCallback((next: ThemePreference) => setPreference(next), [])

  return { preference, selectPreference }
}
