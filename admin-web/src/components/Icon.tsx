import type { ReactNode } from 'react'

/**
 * 内联 SVG 图标集：24 视口、1.5px 描边、currentColor 取文字色（不引入图标库）。
 * 可用名称由 IconName 联合约束，写错名字是类型错误，不会得到空白图标。
 * 图标一律 aria-hidden + focusable="false"：它不提供可访问名称，
 * 纯图标按钮/链接必须自己带 aria-label，否则读屏只剩「按钮」。
 * size 只改渲染宽高（默认 20）；描边宽度按 viewBox 等比缩放，图标越小线条越细。
 */
export type IconName =
  | 'logo'
  | 'dashboard'
  | 'audit'
  | 'exception'
  | 'account'
  | 'tools'
  | 'groups'
  | 'key'
  | 'link'
  | 'sun'
  | 'moon'
  | 'monitor'
  | 'alert'
  | 'info'
  | 'success'
  | 'inbox'
  | 'sidebar'
  | 'logout'
  | 'chevron-left'
  | 'chevron-right'

const PATHS: Record<IconName, ReactNode> = {
  logo: (
    <>
      <path d="M12 3.2 19.6 7.6v8.8L12 20.8 4.4 16.4V7.6z" />
      <path d="M9 15.2V9l3 2.6L15 9v6.2" />
    </>
  ),
  dashboard: (
    <>
      <rect x="3.4" y="3.4" width="7.2" height="7.2" rx="1.6" />
      <rect x="13.4" y="3.4" width="7.2" height="7.2" rx="1.6" />
      <rect x="3.4" y="13.4" width="7.2" height="7.2" rx="1.6" />
      <rect x="13.4" y="13.4" width="7.2" height="7.2" rx="1.6" />
    </>
  ),
  audit: (
    <>
      <circle cx="12" cy="12" r="8.2" />
      <path d="M12 7.6V12l2.9 1.7" />
    </>
  ),
  exception: (
    <>
      <path d="M4 8h9.2M18.4 8H20M4 16h3.6M12.8 16H20" />
      <circle cx="15.8" cy="8" r="2.2" />
      <circle cx="10.2" cy="16" r="2.2" />
    </>
  ),
  account: (
    <>
      <circle cx="12" cy="8.4" r="3.4" />
      <path d="M4.8 20.2a7.2 7.2 0 0 1 14.4 0" />
    </>
  ),
  tools: (
    <>
      <path d="M14.6 3.8a4.6 4.6 0 0 0-5.9 5.8l-5.2 5.2a2 2 0 0 0 0 2.8l2.7 2.7a2 2 0 0 0 2.8 0l5.2-5.2a4.6 4.6 0 0 0 5.8-5.9l-2.9 2.9-2.6-.7-.7-2.6 2.9-3Z" />
      <path d="m8.9 14.3 1.6 1.6" />
    </>
  ),
  groups: (
    <>
      <circle cx="12" cy="6.4" r="2.6" />
      <circle cx="6" cy="17.2" r="2.6" />
      <circle cx="18" cy="17.2" r="2.6" />
      <path d="M10.2 8.7 7.3 14.8M13.8 8.7l2.9 6.1M8.6 17.2h6.8" />
    </>
  ),
  key: (
    <>
      <circle cx="8.2" cy="15.8" r="3.9" />
      <path d="m11 13 8.2-8.2M16.4 7.6l2.4 2.4M14.2 9.8l2.4 2.4" />
    </>
  ),
  sun: (
    <>
      <circle cx="12" cy="12" r="3.9" />
      <path d="M12 3v1.9M12 19.1V21M3 12h1.9M19.1 12H21M5.6 5.6l1.4 1.4M17 17l1.4 1.4M18.4 5.6 17 7M7 17l-1.4 1.4" />
    </>
  ),
  moon: <path d="M20 14.4A8.5 8.5 0 0 1 9.6 4a8.6 8.6 0 1 0 10.4 10.4Z" />,
  monitor: (
    <>
      <rect x="3" y="4.2" width="18" height="12" rx="1.8" />
      <path d="M8.4 20h7.2M12 16.2V20" />
    </>
  ),
  alert: (
    <>
      <circle cx="12" cy="12" r="8.6" />
      <path d="M12 7.4v5.2" />
      <circle cx="12" cy="16.1" r="0.95" fill="currentColor" stroke="none" />
    </>
  ),
  info: (
    <>
      <circle cx="12" cy="12" r="8.6" />
      <path d="M12 11v5" />
      <circle cx="12" cy="7.9" r="0.95" fill="currentColor" stroke="none" />
    </>
  ),
  success: (
    <>
      <circle cx="12" cy="12" r="8.6" />
      <path d="m8.4 12.2 2.4 2.4 4.8-5" />
    </>
  ),
  inbox: (
    <>
      <path d="M3.2 13.2h4.5l1.3 2.6h6l1.3-2.6h4.5" />
      <path d="M5.7 4.8h12.6l2.5 8.4v4.4a2 2 0 0 1-2 2h-13.6a2 2 0 0 1-2-2v-4.4z" />
    </>
  ),
  sidebar: (
    <>
      <rect x="3.2" y="4.4" width="17.6" height="15.2" rx="2" />
      <path d="M9.2 4.4v15.2" />
    </>
  ),
  logout: (
    <>
      <path d="M13.4 4.2h4.4a2 2 0 0 1 2 2v11.6a2 2 0 0 1-2 2h-4.4" />
      <path d="M14.6 12H4.2m3.3-3.3L4.2 12l3.3 3.3" />
    </>
  ),
  'chevron-left': <path d="M14.2 5.6 7.8 12l6.4 6.4" />,
  'chevron-right': <path d="M9.8 5.6 16.2 12l-6.4 6.4" />,
  link: (
    <>
      <path d="M10.2 13.8a4 4 0 0 0 5.6 0l2.7-2.7a4 4 0 0 0-5.6-5.6l-1.3 1.3" />
      <path d="M13.8 10.2a4 4 0 0 0-5.6 0l-2.7 2.7a4 4 0 0 0 5.6 5.6l1.3-1.3" />
    </>
  ),
}

export function Icon({
  name,
  size = 20,
  className,
}: {
  name: IconName
  size?: number
  className?: string
}) {
  return (
    <svg
      className={className}
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.5}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
    >
      {PATHS[name]}
    </svg>
  )
}
