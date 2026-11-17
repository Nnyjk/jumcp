import { useMemo, useState } from 'react'
import { useMutation } from '@tanstack/react-query'
import { installMcpCatalogEntry } from '../../api/admin'
import type { GroupItem, McpCatalogEntry, McpCatalogField, UpstreamItem } from '../../api/types'
import { Field } from '../../components/Field'
import { noticeFromError, type Notice } from '../../notice'

/** 与 UpstreamForm 相同的 id 规则：首字符小写字母，后续小写字母/数字/_/-，长度 2–32。 */
const UPSTREAM_ID_PATTERN = /^[a-z][a-z0-9_-]{1,31}$/

/**
 * 目录安装表单：包声明的 fields 决定输入项，password 只进 React 状态、不回显任何已存值。
 * 提交成功后由父组件卸载本表单，密码不会被留在页面状态里。
 */
export function InstallForm({
  entry,
  groups,
  groupsLoading,
  groupsError,
  onInstalled,
  onError,
}: {
  entry: McpCatalogEntry
  groups: GroupItem[]
  groupsLoading: boolean
  groupsError: boolean
  onInstalled: (upstream: UpstreamItem) => void
  onError: (notice: Notice) => void
}) {
  const [upstreamId, setUpstreamId] = useState(entry.default_upstream_id)
  const [name, setName] = useState(entry.name)
  const [group, setGroup] = useState('')
  const [enabled, setEnabled] = useState(true)
  const [config, setConfig] = useState<Record<string, string>>(() =>
    Object.fromEntries(entry.fields.map((field) => [field.key, field.default ?? ''])),
  )
  const [formError, setFormError] = useState<string | undefined>(undefined)

  /** 只允许可授予的组；组清单拉取失败或无可用项时退化为文本输入，不静默清空已填值。 */
  const groupChoices = useMemo(
    () => groups.filter((item) => item.grantable !== false).map((item) => ({ code: item.code, name: item.name })),
    [groups],
  )
  const useGroupSelect = !groupsError && groupChoices.length > 0

  const mutation = useMutation({
    mutationFn: () => {
      // 只提交填写过的字段：可选字段留空时不伪造空串，交给后端按缺省处理。
      const values: Record<string, string> = {}
      for (const field of entry.fields) {
        const value = config[field.key] ?? ''
        if (value !== '') values[field.key] = value
      }
      return installMcpCatalogEntry(entry.id, {
        upstream_id: upstreamId.trim(),
        name: name.trim(),
        group,
        enabled,
        config: values,
      })
    },
    onSuccess: (result) => onInstalled(result.upstream),
    onError: (error) => {
      const notice = noticeFromError(error, '安装失败')
      setFormError(notice.description)
      onError(notice)
    },
  })

  const submit = () => {
    const trimmedId = upstreamId.trim()
    if (trimmedId === '') {
      setFormError('上游 id 不能为空。')
      return
    }
    if (!UPSTREAM_ID_PATTERN.test(trimmedId)) {
      setFormError('首字符小写字母，后续只允许小写字母、数字、_、-，长度 2–32。')
      return
    }
    if (trimmedId.includes('__')) {
      setFormError('id 不能包含连续两个下划线（__ 是工具名分隔符）。')
      return
    }
    if (name.trim() === '') {
      setFormError('名称不能为空。')
      return
    }
    if (group === '') {
      setFormError('必须填写授权组。')
      return
    }
    for (const field of entry.fields) {
      if (field.required && (config[field.key] ?? '').trim() === '') {
        setFormError(`字段「${field.label}」不能为空。`)
        return
      }
    }
    setFormError(undefined)
    mutation.mutate()
  }

  return (
    <form
      className="create-form"
      onSubmit={(event) => {
        event.preventDefault()
        submit()
      }}
    >
      <div className="form-row">
        <Field label="上游 id" title="upstream_id">
          <input
            className="input mono"
            value={upstreamId}
            onChange={(event) => setUpstreamId(event.target.value)}
            placeholder={entry.default_upstream_id}
          />
        </Field>
        <Field label="名称" title="name">
          <input
            className="input"
            value={name}
            onChange={(event) => setName(event.target.value)}
            placeholder={entry.name}
          />
        </Field>
      </div>

      <div className="form-row">
        <Field
          label="授权组"
          title="group"
          required
          hint={groupsError ? '组清单加载失败，按组 code 填写。' : undefined}
        >
          {useGroupSelect ? (
            <select
              className="select"
              value={group}
              onChange={(event) => setGroup(event.target.value)}
              disabled={groupsLoading}
              required
            >
              <option value="">{groupsLoading ? '正在加载组…' : '请选择组'}</option>
              {groupChoices.map((item) => (
                <option key={item.code} value={item.code} title={item.code}>
                  {item.name}
                </option>
              ))}
            </select>
          ) : (
            <input
              className="input mono"
              value={group}
              onChange={(event) => setGroup(event.target.value.trim())}
              placeholder={groupsLoading ? '正在加载组…' : '例如 db.read.core'}
              disabled={groupsLoading}
              required
            />
          )}
        </Field>
        <label className="check-item">
          <input
            type="checkbox"
            checked={enabled}
            onChange={(event) => setEnabled(event.target.checked)}
          />
          <span>安装后启用</span>
        </label>
      </div>

      {entry.fields.map((field) => (
        <CatalogFieldInput
          key={field.key}
          field={field}
          value={config[field.key] ?? ''}
          onChange={(value) =>
            setConfig((current) => ({
              ...current,
              [field.key]: value,
            }))
          }
        />
      ))}

      <p className="hint">
        敏感字段（password 类）只用于本次提交：安装后目录与上游详情都不会回显，需要修改请到上游详情重新填写。
      </p>

      {formError !== undefined && (
        <p className="inline-error" role="alert">
          {formError}
        </p>
      )}
      <div className="form-actions">
        <button type="submit" className="btn btn-primary" disabled={mutation.isPending}>
          {mutation.isPending ? '安装中…' : '安装'}
        </button>
      </div>
    </form>
  )
}

/** 字段控件按 kind 渲染：password 用隐藏输入，number 用数值输入，其余按单行文本。 */
function CatalogFieldInput({
  field,
  value,
  onChange,
}: {
  field: McpCatalogField
  value: string
  onChange: (value: string) => void
}) {
  const hintParts: string[] = []
  if (field.help !== undefined && field.help !== null && field.help.trim() !== '') hintParts.push(field.help)
  if (field.kind === 'password' || field.secret) hintParts.push('保存后不回显。')

  const hint = hintParts.length > 0 ? hintParts.join(' ') : undefined

  return (
    <Field label={field.label} title={field.key} required={field.required} hint={hint}>
      {field.kind === 'password' ? (
        <input
          className="input"
          type="password"
          autoComplete="new-password"
          value={value}
          onChange={(event) => onChange(event.target.value)}
          placeholder={field.placeholder ?? ''}
        />
      ) : field.kind === 'number' ? (
        <input
          className="input"
          type="number"
          value={value}
          onChange={(event) => onChange(event.target.value)}
          placeholder={field.placeholder ?? ''}
        />
      ) : (
        <input
          className="input"
          value={value}
          onChange={(event) => onChange(event.target.value)}
          placeholder={field.placeholder ?? ''}
        />
      )}
    </Field>
  )
}
