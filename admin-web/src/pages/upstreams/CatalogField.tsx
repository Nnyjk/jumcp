import type { McpCatalogField } from '../../api/types'
import { Field } from '../../components/Field'
import { isSecretField } from './upstreamTools'

/**
 * 目录字段控件：安装与编辑配置两处共用，避免同一份 kind→控件映射维护两遍。
 * password 一律 autoComplete="new-password"，阻止浏览器把管理台账号密码回填进这些字段。
 */
export function CatalogField({
  field,
  value,
  onChange,
  placeholder,
  hint,
}: {
  field: McpCatalogField
  value: string
  onChange: (value: string) => void
  placeholder?: string | undefined
  hint?: string | undefined
}) {
  const hintParts: string[] = []
  if (field.help !== undefined && field.help !== null && field.help.trim() !== '') hintParts.push(field.help)
  if (hint !== undefined) hintParts.push(hint)

  return (
    <Field
      label={field.label}
      title={field.key}
      required={field.required}
      hint={hintParts.length > 0 ? hintParts.join(' ') : undefined}
    >
      {isSecretField(field) ? (
        <input
          className="input"
          type="password"
          autoComplete="new-password"
          value={value}
          onChange={(event) => onChange(event.target.value)}
          placeholder={placeholder ?? field.placeholder ?? ''}
        />
      ) : (
        <input
          className="input"
          value={value}
          onChange={(event) => onChange(event.target.value)}
          placeholder={placeholder ?? field.placeholder ?? ''}
        />
      )}
    </Field>
  )
}
