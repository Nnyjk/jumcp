import { gsap } from 'gsap'
import { ThemeSwitch } from './ThemeSwitch'
import { useThemePreference } from '../theme'
import { MOTION_DURATION, MOTION_EASE, useGsapScope } from '../motion/useGsap'

/** 顶部 TitleBar：页面标题 + 主题切换。 */
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
