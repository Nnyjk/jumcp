import { Icon, type IconName } from './Icon'
import type { ThemePreference } from '../theme'

const OPTIONS: Array<{ value: ThemePreference; label: string; icon: IconName }> = [
  { value: 'light', label: '浅色', icon: 'sun' },
  { value: 'dark', label: '深色', icon: 'moon' },
  { value: 'system', label: '跟随系统', icon: 'monitor' },
]

/** 三段式主题切换：图标按钮 + title/sr-only 文案，命中区不小于 24px。 */
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
