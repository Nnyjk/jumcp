import type { ReactNode } from 'react'

/** 内联 SVG 图标集：20px 线性风格，与 Fluent 图标观感对齐（不引入图标库）。 */
export type IconName =
  | 'logo'
  | 'review'
  | 'audit'
  | 'exception'
  | 'health'
  | 'sun'
  | 'moon'
  | 'monitor'
  | 'alert'
  | 'info'
  | 'success'
  | 'inbox'
  | 'table'
  | 'sidebar'
  | 'chevron-left'
  | 'chevron-right'

const PATHS: Record<IconName, ReactNode> = {
  logo: (
    <>
      <path d="M12 3.2 19.6 7.6v8.8L12 20.8 4.4 16.4V7.6z" />
      <path d="M9 15.2V9l3 2.6L15 9v6.2" />
    </>
  ),
  review: (
    <>
      <path d="M9 4.2h6v2.3H9z" />
      <path d="M8.6 4.2H7a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2v-13a2 2 0 0 0-2-2h-1.6" />
      <path d="m9 13.2 2.1 2.1 4-4" />
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
  health: (
    <>
      <path d="M3.2 12.4h3.4l2.1-5.1 3.1 9.6 2.4-6.2 1.5 3.1h5.1" />
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
  table: (
    <>
      <rect x="3.2" y="4.4" width="17.6" height="15.2" rx="2" />
      <path d="M3.2 9.4h17.6M9.4 9.4v10.2" />
    </>
  ),
  sidebar: (
    <>
      <rect x="3.2" y="4.4" width="17.6" height="15.2" rx="2" />
      <path d="M9.2 4.4v15.2" />
    </>
  ),
  'chevron-left': <path d="M14.2 5.6 7.8 12l6.4 6.4" />,
  'chevron-right': <path d="M9.8 5.6 16.2 12l-6.4 6.4" />,
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
