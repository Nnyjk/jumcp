import { useState } from 'react'
import type { FormEvent } from 'react'
import { useMutation } from '@tanstack/react-query'
import { createAdminAccount, updateAdminAccount } from '../../api/admin'
import type { AdminAccount, AdminAccountState } from '../../api/types'
import { Field } from '../../components/Field'
import { Modal } from '../../components/Modal'
import { ToggleSwitch } from '../../components/ToggleSwitch'
import { accountErrorMessage } from './accountErrors'

/**
 * 格式与安全约束：
 * - 用户名：3–32 位，只允许小写字母、数字与 ._-（对齐后端 jumcp-core 门禁校验）；
 * - 密码：长度至少 12 位；
 * 前端表单作为首道输入边界快速校验反馈，后端鉴权层作为最终安全栅栏校验。
 */
const USERNAME_PATTERN = /^[a-z0-9._-]{3,32}$/
const PASSWORD_MIN = 12

export type AccountEditorTarget = { mode: 'create' } | { mode: 'edit'; account: AdminAccount }

/**
 * 管理账户创建与编辑表单模态：
 * - 新建模式：用户名、密码及确认密码均为必填项；
 * - 编辑模式：仅在管理员主动填写「重置密码」时提交密码更新，未填字段不包含在 PATCH 载荷中；
 * - 错误呈现约束：模态弹层位于浏览器 top layer 会遮挡页面 InfoBar，校验与请求异常必须保留在表单内。
 */
export function AccountForm({
  target,
  onClose,
  onSaved,
}: {
  target: AccountEditorTarget
  onClose: () => void
  onSaved: (account: AdminAccount, mode: AccountEditorTarget['mode']) => void
}) {
  const existing = target.mode === 'edit' ? target.account : undefined
  const [username, setUsername] = useState(existing?.username ?? '')
  const [password, setPassword] = useState('')
  const [confirm, setConfirm] = useState('')
  const [state, setState] = useState<AdminAccountState>(existing?.state ?? 'active')
  const [formError, setFormError] = useState<string | undefined>(undefined)

  const mutation = useMutation({
    mutationFn: async (input: { username: string; password: string; state: AdminAccountState }) => {
      if (existing === undefined) {
        return createAdminAccount({ username: input.username, password: input.password })
      }
      const patch: { username?: string; password?: string; state?: AdminAccountState } = {}
      if (input.username !== existing.username) patch.username = input.username
      if (input.password !== '') patch.password = input.password
      if (input.state !== existing.state) patch.state = input.state
      return updateAdminAccount(existing.id, patch)
    },
    onSuccess: (account) => onSaved(account, target.mode),
    onError: (error) => setFormError(accountErrorMessage(error)),
  })

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    if (mutation.isPending) return
    const trimmed = username.trim().toLowerCase()
    if (!USERNAME_PATTERN.test(trimmed)) {
      setFormError('用户名需 3–32 位，只允许小写字母、数字与 ._-。')
      return
    }
    const needsPassword = existing === undefined || password !== ''
    if (needsPassword) {
      if (password.length < PASSWORD_MIN) {
        setFormError(`密码长度至少为 ${PASSWORD_MIN} 位。`)
        return
      }
      if (password !== confirm) {
        setFormError('两次输入的密码不一致。')
        return
      }
    }
    setFormError(undefined)
    mutation.mutate({ username: trimmed, password, state })
  }

  const creating = existing === undefined
  const title = creating ? '新建管理账户' : `编辑管理账户 · ${existing.username}`

  return (
    <Modal
      size="md"
      title={title}
      subtitle={creating ? '新管理账户创建后立即生效，可直接用于登录管理面(8080)。' : '仅提交变更项；密码留空表示保持现有密码不变。'}
      onClose={onClose}
      footer={
        <>
          <button type="button" className="btn btn-secondary" onClick={onClose} disabled={mutation.isPending}>
            取消
          </button>
          <button type="submit" form="account-form" className="btn btn-primary" disabled={mutation.isPending}>
            {mutation.isPending ? '保存中…' : creating ? '创建管理账户' : '保存修改'}
          </button>
        </>
      }
    >
      <form id="account-form" className="create-form" onSubmit={submit} noValidate>
        <Field label="用户名" title="username" required hint="3–32 位小写字母、数字与 ._-">
          <input
            className="input mono"
            value={username}
            autoComplete="off"
            spellCheck={false}
            disabled={mutation.isPending}
            onChange={(event) => {
              setUsername(event.target.value)
              setFormError(undefined)
            }}
          />
        </Field>
        <Field
          label={creating ? '密码' : '重置密码'}
          title="password"
          required={creating}
          hint={`至少 ${PASSWORD_MIN} 位${creating ? '' : '；留空表示保持当前密码'}`}
        >
          <input
            className="input"
            type="password"
            value={password}
            autoComplete="new-password"
            disabled={mutation.isPending}
            onChange={(event) => {
              setPassword(event.target.value)
              setFormError(undefined)
            }}
          />
        </Field>
        <Field label="确认密码" title="confirm_password">
          <input
            className="input"
            type="password"
            value={confirm}
            autoComplete="new-password"
            disabled={mutation.isPending || (!creating && password === '')}
            onChange={(event) => {
              setConfirm(event.target.value)
              setFormError(undefined)
            }}
          />
        </Field>
        {!creating && (
          <Field label="状态" title="state" hint={state === 'active' ? '正常启用，可登录管理面(8080)。' : '停用后禁止登录管理面(8080)，已有会话将在下次请求鉴权时失效。'}>
            <ToggleSwitch
              checked={state === 'active'}
              disabled={mutation.isPending}
              label={state === 'active' ? '启用' : '停用'}
              onChange={(next) => setState(next ? 'active' : 'disabled')}
            />
          </Field>
        )}
        {formError !== undefined && (
          <p className="inline-error" role="alert">
            {formError}
          </p>
        )}
      </form>
    </Modal>
  )
}
