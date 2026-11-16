import { useMemo, useState } from 'react'
import { useMutation } from '@tanstack/react-query'
import { createGroup, updateGroup } from '../../api/admin'
import type { GroupItem, RoleItem } from '../../api/types'
import { Field } from '../../components/Field'
import { noticeFromError, type Notice } from '../../notice'
import { parseInteger, type EditorTarget } from './groupFilter'

/** 新建/编辑共用表单：code 仅在新建时可填；角色用多选，发送 code。 */
export function GroupForm({
  target,
  roles,
  rolesLoading,
  onSaved,
  onError,
}: {
  target: EditorTarget
  roles: RoleItem[]
  rolesLoading: boolean
  onSaved: (item: GroupItem) => void
  onError: (notice: Notice) => void
}) {
  const existing = target.mode === 'edit' ? target.group : undefined
  const [code, setCode] = useState(existing?.code ?? '')
  const [name, setName] = useState(existing?.name ?? '')
  const [priority, setPriority] = useState(String(existing?.priority ?? 0))
  const [minBudget, setMinBudget] = useState(
    existing === undefined ? '800' : String(existing.min_budget),
  )
  const [selectedRoles, setSelectedRoles] = useState<string[]>(existing?.roles ?? [])
  const [formError, setFormError] = useState<string | undefined>(undefined)

  /** 已有组引用的角色若不在角色清单里，以 code 兜底展示，避免保存时静默丢角色。 */
  const roleChoices = useMemo(() => {
    const choices = roles.map((role) => ({ code: role.code, name: role.name }))
    for (const roleCode of existing?.roles ?? []) {
      if (!choices.some((choice) => choice.code === roleCode)) {
        choices.push({ code: roleCode, name: roleCode })
      }
    }
    return choices
  }, [roles, existing])

  const mutation = useMutation({
    mutationFn: (input: {
      code: string
      name: string
      priority: number
      min_budget: number
      roles: string[]
    }) => {
      if (existing === undefined) return createGroup(input)
      return updateGroup(existing.code, {
        name: input.name,
        priority: input.priority,
        min_budget: input.min_budget,
        roles: input.roles,
      })
    },
    onSuccess: onSaved,
    onError: (error) => {
      const notice = noticeFromError(error, existing === undefined ? '新建组失败' : '更新组失败')
      setFormError(notice.description)
      onError(notice)
    },
  })

  const toggleRole = (roleCode: string, checked: boolean) => {
    setSelectedRoles((current) =>
      checked
        ? current.includes(roleCode)
          ? current
          : [...current, roleCode]
        : current.filter((item) => item !== roleCode),
    )
  }

  const submit = () => {
    const trimmedCode = existing?.code ?? code.trim()
    const trimmedName = name.trim()
    if (existing === undefined) {
      if (trimmedCode === '') {
        setFormError('组 code 不能为空。')
        return
      }
      if (!/^[a-z0-9._-]{1,64}$/.test(trimmedCode)) {
        setFormError('组 code 只允许小写字母、数字与 ._-，最长 64 字符。')
        return
      }
    }
    if (trimmedName === '') {
      setFormError('组中文名不能为空。')
      return
    }
    const parsedPriority = parseInteger(priority, 0)
    if (parsedPriority === undefined) {
      setFormError('priority 必须是非负整数。')
      return
    }
    const parsedBudget = parseInteger(minBudget, 1)
    if (parsedBudget === undefined) {
      setFormError('min_budget 必须是正整数。')
      return
    }
    if (selectedRoles.length === 0) {
      setFormError('至少选择一个角色。')
      return
    }
    setFormError(undefined)
    mutation.mutate({
      code: trimmedCode,
      name: trimmedName,
      priority: parsedPriority,
      min_budget: parsedBudget,
      roles: selectedRoles,
    })
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
        <Field label="组 code" title="code">
          <input
            className="input mono"
            value={existing?.code ?? code}
            readOnly={existing !== undefined}
            onChange={(event) => setCode(event.target.value)}
            placeholder="例如 finance"
          />
        </Field>
        <Field label="组中文名" title="name">
          <input
            className="input"
            value={name}
            onChange={(event) => setName(event.target.value)}
            placeholder="例如 财务组"
          />
        </Field>
      </div>
      <div className="form-row">
        <Field label="优先级" title="priority">
          <input
            className="input"
            type="number"
            min={0}
            step={1}
            value={priority}
            onChange={(event) => setPriority(event.target.value)}
          />
        </Field>
        <Field label="最小预算" title="min_budget">
          <input
            className="input"
            type="number"
            min={1}
            step={1}
            value={minBudget}
            onChange={(event) => setMinBudget(event.target.value)}
          />
        </Field>
      </div>
      <fieldset className="check-fieldset">
        <legend>角色</legend>
        {rolesLoading ? (
          <p className="hint">正在加载角色清单…</p>
        ) : roleChoices.length === 0 ? (
          <p className="hint">角色清单为空：请先在策略中定义角色。</p>
        ) : (
          <div className="check-grid">
            {roleChoices.map((role) => (
              <label key={role.code} className="check-item" title={role.code}>
                <input
                  type="checkbox"
                  checked={selectedRoles.includes(role.code)}
                  onChange={(event) => toggleRole(role.code, event.target.checked)}
                />
                <span>{role.name}</span>
              </label>
            ))}
          </div>
        )}
      </fieldset>
      {formError !== undefined && (
        <p className="inline-error" role="alert">
          {formError}
        </p>
      )}
      <div className="form-actions">
        <button type="submit" className="btn btn-primary" disabled={mutation.isPending}>
          {mutation.isPending ? '保存中…' : existing === undefined ? '创建组' : '保存修改'}
        </button>
      </div>
    </form>
  )
}
