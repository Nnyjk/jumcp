import { Icon, type IconName } from './Icon'
import type { ThemePreference } from '../theme'

const OPTIONS: Array<{ value: ThemePreference; label: string; icon: IconName }> = [
  { value: 'light', label: '浅色', icon: 'sun' },
  { value: 'dark', label: '深色', icon: 'moon' },
  { value: 'system', label: '跟随系统', icon: 'monitor' },
]

/**
 * 三段式主题切换：图标按钮 + title/sr-only 文案，命中区不小于 24px。
 * 三个按钮是互斥的 aria-pressed 组（role="group"），不是 tabs/radio——Tab 键逐个经过，Enter/空格选择。
 * 文案只给读屏与 tooltip，视觉上只有图标：改图标时同步改 label，否则鼠标用户无从判断。
 */
export function ThemeSwitch({
  preference,
  onChange,
}: {
  preference: ThemePreference
  onChange: (next: ThemePreference) => void
}) {
  return (
    <div className="theme-switch" role="group" aria-label="主题">
      {OPTIONS.map((option) => (
        <button
          key={option.value}
          type="button"
          aria-pressed={preference === option.value}
          className="theme-option"
          title={option.label}
          onClick={() => onChange(option.value)}
        >
          <Icon name={option.icon} size={16} />
          <span className="sr-only">{option.label}</span>
        </button>
      ))}
    </div>
  )
}
