import { Badge } from './Badge'
import { ThemeSwitch } from './ThemeSwitch'
import { useThemePreference } from '../theme'

export type PageMode = '只读' | '未实现'

/** 顶部 TitleBar：页面标题 + 性质副标题 + 只读/未实现徽标 + 主题切换。 */
export function TitleBar({
  title,
  subtitle,
  mode,
}: {
  title: string
  subtitle: string
  mode: PageMode
}) {
  const { preference, selectPreference } = useThemePreference()

  return (
    <header className="titlebar">
      <div className="titlebar-inner">
        <div className="titlebar-text">
          <h1>{title}</h1>
          <p>{subtitle}</p>
        </div>
        <div className="titlebar-actions">
          <Badge tone={mode === '只读' ? 'accent' : 'warning'} dot>
            {mode === '只读' ? '只读视图' : '未实现'}
          </Badge>
          <span className="divider-v" aria-hidden="true" />
          <ThemeSwitch preference={preference} onChange={selectPreference} />
        </div>
      </div>
    </header>
  )
}
