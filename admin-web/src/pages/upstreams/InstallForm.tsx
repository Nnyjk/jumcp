import { useMemo, useState } from 'react'
import { useMutation } from '@tanstack/react-query'
import { installMcpCatalogEntry } from '../../api/admin'
import type { GroupItem, McpCatalogEntry, UpstreamItem } from '../../api/types'
import { Field } from '../../components/Field'
import { encryptSecret, withTransportRetry } from '../../lib/transport'
import { validateUpstreamId } from '../../lib/upstreamId'
import { noticeFromError, type Notice } from '../../notice'
import { CatalogField } from './CatalogField'
import { isSecretField } from './upstreamTools'

/** 安装表单沿用共享的 id 规则（`src/lib/upstreamId.ts`），不在页面里重复正则与文案。 */

/**
 * 目录安装表单：包声明的 fields 决定输入项，密文字段只进 React 状态、不回显任何已存值。
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
    mutationFn: () =>
      withTransportRetry(async () => {
        // 只提交填写过的字段：可选字段留空时不伪造空串，交给后端按缺省处理。
        // 密文字段必须是传输加密后的 `enc:v1:` 密文；加密失败直接抛错（不发请求）。
        const values: Record<string, string> = {}
        for (const field of entry.fields) {
          const value = config[field.key] ?? ''
          if (value === '') continue
          values[field.key] = isSecretField(field) ? await encryptSecret(value) : value
        }
        return installMcpCatalogEntry(entry.id, {
          upstream_id: upstreamId.trim(),
          name: name.trim(),
          group,
          enabled,
          config: values,
        })
      }),
    onSuccess: (result) => onInstalled(result.upstream),
    onError: (error) => {
      const notice = noticeFromError(error, '安装失败')
      setFormError(notice.description)
      onError(notice)
    },
  })

  const submit = () => {
    const trimmedId = upstreamId.trim()
    const idError = validateUpstreamId(trimmedId)
    if (idError !== undefined) {
      setFormError(idError)
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
        <CatalogField
          key={field.key}
          field={field}
          value={config[field.key] ?? ''}
          onChange={(value) =>
            setConfig((current) => ({
              ...current,
              [field.key]: value,
            }))
          }
          hint={isSecretField(field) ? '保存后不回显。' : undefined}
        />
      ))}

      <p className="hint">
        密文字段只用于本次提交：安装后目录与上游详情都不会回显，需要修改请用实例行的「编辑配置」重新输入。
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
