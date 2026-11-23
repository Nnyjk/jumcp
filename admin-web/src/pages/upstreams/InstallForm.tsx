import { useMemo, useState } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { installMcpCatalogEntry } from '../../api/admin'
import { ApiError } from '../../api/client'
import type { GroupItem, McpCatalogEntry, UpstreamItem } from '../../api/types'
import { Field } from '../../components/Field'
import { encryptSecret, withTransportRetry } from '../../lib/transport'
import { noticeFromError, type Notice } from '../../notice'
import { CatalogField } from './CatalogField'
import { installationNameError, nextInstallationName, nextUpstreamId } from './catalogNaming'
import { isSecretField } from './upstreamTools'

/**
 * 目录安装表单：上游 id 由目录自动推导（不可编辑），名称默认为包名并在该包内唯一。
 * 包声明的 fields 决定输入项，密文字段只进 React 状态、不回显任何已存值。
 * 提交成功后由父组件卸载本表单，密码不会被留在页面状态里。
 */
export function InstallForm({
  entry,
  groups,
  groupsLoading,
  groupsError,
  onInstalled,
  onError,
  onCancel,
}: {
  entry: McpCatalogEntry
  groups: GroupItem[]
  groupsLoading: boolean
  groupsError: boolean
  onInstalled: (upstream: UpstreamItem) => void
  onError: (notice: Notice) => void
  onCancel: () => void
}) {
  const queryClient = useQueryClient()
  /** 目录推不出可用 id 时不在渲染期抛错：转成内联错误并阻止提交。 */
  const idDerivation = useMemo(() => {
    try {
      return { upstreamId: nextUpstreamId(entry), error: undefined }
    } catch (error) {
      return {
        upstreamId: undefined,
        error:
          error instanceof Error
            ? error.message
            : '无法推导可用的 upstream_id，请先清理该包的安装实例。',
      }
    }
  }, [entry])
  const defaultUpstreamId = idDerivation.upstreamId
  const defaultName = useMemo(() => nextInstallationName(entry), [entry])
  /** null 表示未手动改名：目录刷新（如 id 冲突后）时跟随重新推导的名称。 */
  const [nameDraft, setNameDraft] = useState<string | null>(null)
  const name = nameDraft ?? defaultName
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
    mutationFn: (upstreamId: string) =>
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
          upstream_id: upstreamId,
          name: name.trim(),
          group,
          enabled,
          config: values,
        })
      }),
    onSuccess: (result) => onInstalled(result.upstream),
    onError: (error) => {
      const notice = noticeFromError(error, '安装失败')
      if (error instanceof ApiError && error.code === 'MCP_UPSTREAM_CONFLICT') {
        // 目录刷新后 entry.installations 更新，下次提交的 id/名称自动顺延；不做固定次数重试。
        setFormError('上游 id 已被占用，目录已刷新，请确认新的 upstream_id 后重试。')
        void queryClient.invalidateQueries({ queryKey: ['mcp-catalog'] })
      } else {
        setFormError(notice.description)
      }
      onError(notice)
    },
  })

  const submit = () => {
    if (defaultUpstreamId === undefined) {
      setFormError(
        idDerivation.error ?? '无法推导可用的 upstream_id，请先清理该包的安装实例。',
      )
      return
    }
    const nameError = installationNameError(entry, name)
    if (nameError !== undefined) {
      setFormError(nameError)
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
    mutation.mutate(defaultUpstreamId)
  }

  return (
    <form
      className="create-form"
      onSubmit={(event) => {
        event.preventDefault()
        submit()
      }}
    >
      {defaultUpstreamId === undefined ? (
        <p className="inline-error" role="alert">
          {idDerivation.error ?? '无法推导可用的 upstream_id，请先清理该包的安装实例。'}
        </p>
      ) : (
        <p className="hint">
          将安装为 upstream_id：<code className="mono">{defaultUpstreamId}</code>（由目录自动推导，不可修改）
        </p>
      )}

      <div className="form-row">
        <Field label="名称" title="name">
          <input
            className="input"
            value={name}
            onChange={(event) => setNameDraft(event.target.value)}
            placeholder={defaultName}
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
        <button type="button" className="btn btn-secondary" onClick={onCancel} disabled={mutation.isPending}>
          取消
        </button>
        <button
          type="submit"
          className="btn btn-primary"
          disabled={mutation.isPending || defaultUpstreamId === undefined}
        >
          {mutation.isPending ? '安装中…' : '安装'}
        </button>
      </div>
    </form>
  )
}
