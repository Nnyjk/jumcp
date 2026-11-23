import { gsap } from 'gsap'
import { ThemeSwitch } from './ThemeSwitch'
import { useThemePreference } from '../theme'
import { MOTION_DURATION, MOTION_EASE, useGsapScope } from '../motion/useGsap'

/**
 * 顶部 TitleBar：页面标题 + 主题切换。
 * title 由调用方按当前视图/路由给出（视图打开时是视图名，不是路由页名），变化时重播一次淡入位移。
 * 主题偏好由本组件的 useThemePreference 持有（唯一消费者），页面不需要关心主题状态。
 */
export function TitleBar({ title }: { title: string }) {
  const { preference, selectPreference } = useThemePreference()
  const titlebarRef = useGsapScope<HTMLDivElement>((scope) => {
    gsap.from(scope.querySelectorAll('h1, .titlebar-actions'), {
      opacity: 0,
      y: 6,
      duration: MOTION_DURATION.enter,
      ease: MOTION_EASE,
      stagger: 0.04,
      clearProps: 'opacity,transform',
    })
  }, [title])

  return (
    <header className="titlebar">
      <div className="titlebar-inner" ref={titlebarRef}>
        <div className="titlebar-text">
          <h1>{title}</h1>
        </div>
        <div className="titlebar-actions">
          <ThemeSwitch preference={preference} onChange={selectPreference} />
        </div>
      </div>
    </header>
  )
}
