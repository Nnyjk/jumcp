import { useLayoutEffect, useRef } from 'react'
import type { RefObject } from 'react'
import { prefersReducedMotion } from '../../motion/useGsap'

/**
 * 列表动效的两个阶段：
 * - 首次数据到达：enter 回调播一次入场（只动 opacity / transform）；
 * - 之后数据变化：容器打上 dash-flash 标记，条目背景淡入一次作为提示，不重播入场。
 *
 * 依赖用行 key 拼成的签名而不是数组引用：轮询拿到同样的行时签名不变，
 * 入场不会重播，看板每 30 秒刷新时不会整页闪。
 */

/** 高亮提示的时长，与 CSS 里的 dash-flash 动画时长一致（--motion-enter 默认 280ms）。 */
const FLASH_DURATION_MS = 300

export function useListMotion<T extends HTMLElement>(
  signature: string,
  enter: (scope: T) => void,
): RefObject<T | null> {
  const scopeRef = useRef<T | null>(null)
  const enterRef = useRef(enter)
  const enteredKey = useRef('')
  const lastKey = useRef('')
  const flashTimer = useRef<number | undefined>(undefined)

  useLayoutEffect(() => {
    enterRef.current = enter
  })

  useLayoutEffect(() => {
    const scope = scopeRef.current
    if (scope === null || signature === '') return

    if (enteredKey.current === '') {
      enteredKey.current = signature
      if (!prefersReducedMotion()) enterRef.current(scope)
    } else if (signature !== lastKey.current) {
      // 刷新提示：移除标记再强制回流，让 CSS 动画在下一次数据变化时能重新播。
      scope.classList.remove('dash-flash')
      void scope.offsetWidth
      scope.classList.add('dash-flash')
      // 动画结束后摘掉标记：标记留着会让之后新增的行也闪一下。
      window.clearTimeout(flashTimer.current)
      flashTimer.current = window.setTimeout(() => {
        scope.classList.remove('dash-flash')
      }, FLASH_DURATION_MS)
    }
    lastKey.current = signature
  }, [signature])

  useLayoutEffect(
    () => () => {
      window.clearTimeout(flashTimer.current)
    },
    [],
  )

  return scopeRef
}
