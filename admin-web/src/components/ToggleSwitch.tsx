/**
 * Fluent ToggleSwitch：轨道 + 滑块，底层是 input[checkbox] + role="switch"，
 * 键盘用空格切换；role="switch" 让读屏按开/关语义播报，而不是「复选框」。
 * 只读场景用 disabled 表达「当前开/关但不可改」，不要换成纯文本——状态本身仍要能被读出。
 * label 同时充当可见文案与 aria-label 的回退值；两者都缺时控件没有可访问名称，纯展示场景请务必传 ariaLabel。
 * 不传 onChange 时勾选不生效（受控组件），配合 disabled 就是纯状态展示。
 */
export function ToggleSwitch({
  checked,
  disabled = false,
  label,
  title,
  ariaLabel,
  onChange,
}: {
  checked: boolean
  disabled?: boolean
  label?: string
  /** 后端字段名等契约信息：中文标签之外补充悬停提示。 */
  title?: string
  ariaLabel?: string
  onChange?: (next: boolean) => void
}) {
  return (
    <label className="toggle">
      <input
        className="toggle-input"
        type="checkbox"
        role="switch"
        checked={checked}
        aria-checked={checked}
        disabled={disabled}
        aria-label={ariaLabel ?? label}
        onChange={(event) => onChange?.(event.target.checked)}
      />
      {label !== undefined && (
        <span className="toggle-label" title={title}>
          {label}
        </span>
      )}
    </label>
  )
}
