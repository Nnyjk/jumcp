import { useRef } from 'react'
import type { KeyboardEvent, ReactNode } from 'react'
import { gsap } from 'gsap'
import { MOTION_DURATION, MOTION_EASE, useGsapScope } from '../motion/useGsap'

export interface TabItem<K extends string> {
  key: K
  label: string
}

/**
 * 受控页签条：不持有选中状态，点击与键盘只回调 onChange。
 * id 由 idBase 统一派生——页签 `${idBase}-tab-${key}`、面板 `${idBase}-panel-${key}`，
 * 调用方据此用 TabPanel 渲染面板，aria 关联不需要各自拼接；因此 key 会进 DOM id，只能用 id 安全字符。
 * 键盘遵循 tabs 模式：左右方向键循环、Home/End 跳首尾，选中即跟随焦点（焦点与 onChange 在同一次按键里落地）。
 * roving tabindex：只有选中页签 tabIndex=0，Tab 键进出页签条只停一处；其余为 -1，靠方向键到达。
 * aria-controls 只挂在选中页签上——未选中的面板通常没有渲染，也就没有可指向的目标。
 */
export function TabBar<K extends string>({
  idBase,
  items,
  value,
  onChange,
  label,
}: {
  idBase: string
  items: TabItem<K>[]
  value: K
  onChange: (key: K) => void
  label: string
}) {
  const listRef = useRef<HTMLElement | null>(null)

  const handleKey = (event: KeyboardEvent<HTMLButtonElement>, index: number) => {
    const last = items.length - 1
    let nextIndex: number | undefined
    if (event.key === 'ArrowRight') nextIndex = index === last ? 0 : index + 1
    else if (event.key === 'ArrowLeft') nextIndex = index === 0 ? last : index - 1
    else if (event.key === 'Home') nextIndex = 0
    else if (event.key === 'End') nextIndex = last
    if (nextIndex === undefined) return
    const next = items[nextIndex]
    if (next === undefined) return
    event.preventDefault()
    onChange(next.key)
    // 焦点跟随选中页签，读屏与键盘用户不用再 Tab 一次。
    listRef.current?.querySelectorAll<HTMLButtonElement>('[role="tab"]')[nextIndex]?.focus()
  }

  return (
    <nav className="tabbar" role="tablist" aria-label={label} ref={listRef}>
      {items.map((item, index) => (
        <button
          key={item.key}
          type="button"
          role="tab"
          id={`${idBase}-tab-${item.key}`}
          aria-controls={item.key === value ? `${idBase}-panel-${item.key}` : undefined}
          aria-selected={item.key === value}
          tabIndex={item.key === value ? 0 : -1}
          className="tab"
          onClick={() => onChange(item.key)}
          onKeyDown={(event) => handleKey(event, index)}
        >
          {item.label}
        </button>
      ))}
    </nav>
  )
}

/**
 * 页签面板：tabKey 变化时重播一次淡入上移，让内容切换有方向感。
 * id/aria-labelledby 与 TabBar 的派生规则一致，成对落在一个组件里防止错配。
 * 面板自身不进 Tab 顺序（无 tabIndex），键盘落位由面板内第一个可聚焦元素承接；
 * 只渲染当前页签的面板即可，隐藏页签不需要保留 DOM。
 */
export function TabPanel({
  idBase,
  tabKey,
  children,
}: {
  idBase: string
  tabKey: string
  children: ReactNode
}) {
  const panelRef = useGsapScope<HTMLDivElement>((scope) => {
    gsap.from(scope, {
      opacity: 0,
      y: 6,
      duration: MOTION_DURATION.fast,
      ease: MOTION_EASE,
      clearProps: 'opacity,transform',
    })
  }, [tabKey])

  return (
    <div
      className="tab-panel"
      role="tabpanel"
      id={`${idBase}-panel-${tabKey}`}
      aria-labelledby={`${idBase}-tab-${tabKey}`}
      ref={panelRef}
    >
      {children}
    </div>
  )
}
