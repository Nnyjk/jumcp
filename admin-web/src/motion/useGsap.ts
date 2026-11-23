import { useLayoutEffect, useRef } from 'react'
import type { DependencyList, RefObject } from 'react'
import { gsap } from 'gsap'

/**
 * 全站统一的动效参数与性能约束：
 * 1. 仅限使用 opacity 与 transform 这两类 GPU 独立合成层属性，严禁操作宽高、外边距等几何属性，杜绝重排（reflow）与重绘（repaint）；
 * 2. 动画时长严格收敛在 150–350ms 区间，使用统一的平滑缓动曲线；
 * 3. 表格行、卡片网格等重复元素使用更短时长（0.16s）与微小 stagger（0.02–0.04s），防止大批量元素阶梯进场时产生剧烈闪烁阻碍信息扫读。
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
 * 在组件根节点上建立 GSAP 动画上下文（GSAP context）：
 * - 渲染时机：采用 useLayoutEffect 确保在首帧绘制（paint）前完成动画注册与初始位置变换，防止页面闪烁出最终渲染态；
 * - 实例清理：由 gsap.context 统一捕获 scope 内所有 tween，在组件卸载或依赖项更新时自动执行 revert，清除内联样式并销毁动画补间；
 * - 框架兼容：天然契合 React 18+ StrictMode 的挂载-清理-重挂载机制，杜绝动画补间重叠累加；
 * - 无障碍降级：当系统偏好 prefers-reduced-motion: reduce 开启时，直接跳过动画注册，使界面元素保持自然的最终排版状态。
 *
 * 约束：build 回调只负责在 scope 范围内创建 gsap 补间，禁止在回调内部触发组件状态变更；
 * deps 用于精确声明重播时机（如异步数据到达、筛选条件切换），纯首屏挂载动效可留空。
 */
export function useGsapScope<T extends Element>(
  build: (scope: T) => void,
  deps: DependencyList = [],
): RefObject<T | null> {
  const scopeRef = useRef<T | null>(null)
  const buildRef = useRef(build)

  // 动机：通过 ref 捕获最新渲染闭包内的变量，防止闭包陈旧；同时避免将 build 列入依赖数组引发不必要的 context 频繁销毁与重建。
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
    // 约束：动效生命周期与重建严格由调用方传入的 deps 决定（数据到达、Tab 切换等状态就绪时重播进入动效）。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps)

  return scopeRef
}
