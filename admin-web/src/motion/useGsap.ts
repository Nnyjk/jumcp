import { useLayoutEffect, useRef } from 'react'
import type { DependencyList, RefObject } from 'react'
import { gsap } from 'gsap'

/**
 * 全站统一的动效参数：只使用 opacity / transform，150–350ms，统一缓动。
 * 表格行等重复元素用更快的时长与很小的 stagger，避免大面积闪烁。
 */
export const MOTION_EASE = 'power2.out'
export const MOTION_DURATION = {
  enter: 0.28,
  fast: 0.18,
  row: 0.16,
} as const

const REDUCED_MOTION_QUERY = '(prefers-reduced-motion: reduce)'

export function prefersReducedMotion(): boolean {
  return window.matchMedia(REDUCED_MOTION_QUERY).matches
}

/**
 * 在组件根节点上建立 GSAP context：
 * - useLayoutEffect 在首帧绘制前挂载，进入动效不会先闪出最终态；
 * - gsap.context 统一收集动画，卸载或依赖变化时 revert，恢复内联样式并杀掉 tween；
 * - StrictMode 的 mount → cleanup → mount 因此不会叠加两层动画；
 * - prefers-reduced-motion: reduce 时不注册任何动画，元素保持自然最终态。
 *
 * build 回调只负责在 scope 内创建 tween（gsap.from/to），不要在这里做状态更新。
 * deps 用于在数据到达或路由标题变化时重播；首屏挂载动画可省略。
 */
export function useGsapScope<T extends Element>(
  build: (scope: T) => void,
  deps: DependencyList = [],
): RefObject<T | null> {
  const scopeRef = useRef<T | null>(null)
  const buildRef = useRef(build)

  // 每次渲染同步最新回调，但只由显式 deps 决定何时重建 context。
  useLayoutEffect(() => {
    buildRef.current = build
  })

  useLayoutEffect(() => {
    const scope = scopeRef.current
    if (scope === null || prefersReducedMotion()) return

    const context = gsap.context(() => {
      buildRef.current(scope)
    }, scope)

    return () => context.revert()
    // deps 由调用方声明：依赖变化（数据到达、折叠状态、标题切换）时重播进入动效。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps)

  return scopeRef
}
