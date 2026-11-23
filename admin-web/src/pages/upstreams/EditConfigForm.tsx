import { useState } from 'react'
import { useMutation } from '@tanstack/react-query'
import { updateMcpInstallConfig } from '../../api/admin'
import type { McpCatalogEntry, McpCatalogInstallation } from '../../api/types'
import { encryptSecret, withTransportRetry } from '../../lib/transport'
import { noticeFromError, type Notice } from '../../notice'
import { CatalogField } from './CatalogField'
import { isSecretField } from './upstreamTools'

/**
 * 编辑已安装实例的配置：只覆盖包声明的安装字段，名称/授权组/启停仍走上游编辑。
 * 密文字段每次打开都从空开始，保存时只有真正输入过的键才提交，留空即保持原值。
 */
export function EditConfigForm({
  entry,
  installation,
  onSaved,
  onError,
  onCancel,
}: {
  entry: McpCatalogEntry
  installation: McpCatalogInstallation
  onSaved: (installation: McpCatalogInstallation) => void
  onError: (notice: Notice) => void
  onCancel: () => void
}) {
  const [config, setConfig] = useState<Record<string, string>>(() =>
    Object.fromEntries(
      entry.fields
        .filter((field) => !isSecretField(field))
        .map((field) => [field.key, installation.config[field.key] ?? field.default ?? '']),
    ),
  )
  const [secrets, setSecrets] = useState<Record<string, string>>(() =>
    Object.fromEntries(entry.fields.filter(isSecretField).map((field) => [field.key, ''])),
  )
  const [formError, setFormError] = useState<string | undefined>(undefined)

  const mutation = useMutation({
    mutationFn: () =>
      withTransportRetry(async () => {
        const values: Record<string, string> = {}
        for (const field of entry.fields) {
          if (isSecretField(field)) {
            const value = secrets[field.key] ?? ''
            // 只有本次输入的密码才提交，且必须是传输加密后的 `enc:v1:` 密文；加密失败不发请求。
            if (value !== '') values[field.key] = await encryptSecret(value)
          } else {
            // 非密字段全量提交：空串表示清空，后端按提交值整体覆盖。
            values[field.key] = config[field.key] ?? ''
          }
        }
        return updateMcpInstallConfig(entry.id, installation.upstream_id, { config: values })
      }),
    onSuccess: (result) => onSaved(result.installation),
    onError: (error) => {
      const notice = noticeFromError(error, '保存配置失败')
      setFormError(notice.description)
      onError(notice)
    },
  })

  const submit = () => {
    for (const field of entry.fields) {
      if (!field.required) continue
      if (isSecretField(field)) {
        // 已写入过的密文允许留空（保持原值）；从未写入时必须补一次。
        if (installation.secrets_set[field.key] !== true && (secrets[field.key] ?? '').trim() === '') {
          setFormError(`字段「${field.label}」还没有值，必须输入。`)
          return
        }
      } else if ((config[field.key] ?? '').trim() === '') {
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
      <p className="hint">只修改安装字段；名称、授权组与启停状态请在上游编辑里调整。</p>

      {entry.fields.length === 0 && <p className="hint">该包没有安装字段，无需配置。</p>}

      {entry.fields.map((field) => {
        const secret = isSecretField(field)
        const secretPlaceholder =
          installation.secrets_set[field.key] === true ? '已设置，留空保持不变' : field.placeholder ?? ''
        return (
          <CatalogField
            key={field.key}
            field={field}
            value={secret ? secrets[field.key] ?? '' : config[field.key] ?? ''}
            onChange={(value) =>
              secret
                ? setSecrets((current) => ({ ...current, [field.key]: value }))
                : setConfig((current) => ({ ...current, [field.key]: value }))
            }
            placeholder={secret ? secretPlaceholder : field.placeholder ?? ''}
            hint={secret ? '保存后不可查看，只能重新输入。' : undefined}
          />
        )
      })}

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
          disabled={mutation.isPending || entry.fields.length === 0}
        >
          {mutation.isPending ? '保存中…' : '保存'}
        </button>
      </div>
    </form>
  )
}
