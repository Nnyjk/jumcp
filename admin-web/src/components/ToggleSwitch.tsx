/** Fluent ToggleSwitch：轨道 + 滑块；只读场景用 disabled，但控件本身仍表达当前开/关。 */
export function ToggleSwitch({
  checked,
  disabled = false,
  label,
  ariaLabel,
  onChange,
}: {
  checked: boolean
  disabled?: boolean
  label?: string
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
        disabled={disabled}
        aria-label={ariaLabel ?? label}
        onChange={(event) => onChange?.(event.target.checked)}
      />
      {label !== undefined && <span className="toggle-label">{label}</span>}
    </label>
  )
}
