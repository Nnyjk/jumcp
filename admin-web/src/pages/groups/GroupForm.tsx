import { useMemo, useState } from 'react'
import { useMutation } from '@tanstack/react-query'
import { createGroup, updateGroup } from '../../api/admin'
import type { GroupItem, RoleItem } from '../../api/types'
import { Field } from '../../components/Field'
import { Modal } from '../../components/Modal'
import { noticeMessage } from '../../notice'
import { parseInteger, type EditorTarget } from './groupFilter'

/**
 * 组创建与编辑表单：
 * - 契约约束：组 code 作为受管能力与访问令牌鉴权的主键，仅在新建时允许录入，创建后全局只读禁止篡改；
 * - 角色关联：多选展示，提交时向后端同步已勾选的角色 code 清单；
 * - 预算默认值：新建时默认预算填入 800（对齐 core::budget::MIN_VIABLE_BUDGET 最小可用预算契约）。
 */
export function GroupForm({
  target,
  roles,
  rolesLoading,
  onSaved,
  onClose,
}: {
  target: EditorTarget
  roles: RoleItem[]
  rolesLoading: boolean
  onSaved: (item: GroupItem) => void
  onClose: () => void
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

  /**
   * 容错展示约束：若已有组关联的角色在当前策略角色清单中不存在（可能已被静态策略移除），
   * 必须以 code 兜底追加到选项中，防止管理员编辑保存时静默丢弃原有的角色关联关系。
   */
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
      // 错误呈现约束：模态弹层位于浏览器 top layer，会遮挡页面全局 InfoBar，操作异常必须保留在表单内部。
      setFormError(noticeMessage(error, existing === undefined ? '新建组失败' : '更新组失败'))
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
        setFormError('组 code 需 1–64 位，只允许小写字母、数字与 ._-。')
        return
      }
    }
    if (trimmedName === '') {
      setFormError('组中文名不能为空。')
      return
    }
    const parsedPriority = parseInteger(priority, 0)
    if (parsedPriority === undefined) {
      setFormError('优先级必须是非负整数（≥ 0）。')
      return
    }
    const parsedBudget = parseInteger(minBudget, 1)
    if (parsedBudget === undefined) {
      setFormError('最小预算必须是正整数（> 0）。')
      return
    }
    if (selectedRoles.length === 0) {
      setFormError('至少选择一个关联角色。')
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

  const creating = existing === undefined

  return (
    <Modal
      size="lg"
      title={creating ? '新建组' : `编辑组 · ${existing.name}`}
      subtitle={
        creating
          ? '受管组由管理台维护：code 是受管能力与访问令牌引用的契约标识，创建后不可修改。'
          : `组 code 不可修改：${existing.code}`
      }
      onClose={onClose}
      footer={
        <>
          <button type="button" className="btn btn-secondary" onClick={onClose} disabled={mutation.isPending}>
            取消
          </button>
          <button type="submit" form="group-form" className="btn btn-primary" disabled={mutation.isPending}>
            {mutation.isPending ? '保存中…' : creating ? '创建组' : '保存修改'}
          </button>
        </>
      }
    >
      <form
        id="group-form"
        className="create-form"
        onSubmit={(event) => {
          event.preventDefault()
          submit()
        }}
      >
        <div className="form-row">
          <Field
            label="组 code"
            title="code"
            required={creating}
            hint={creating ? '1–64 位小写字母、数字与 ._-，创建后不可修改' : '契约标识，不可修改'}
          >
            <input
              className="input mono"
              value={existing?.code ?? code}
              readOnly={existing !== undefined}
              onChange={(event) => setCode(event.target.value)}
              placeholder="例如 finance"
            />
          </Field>
          <Field label="组中文名" title="name" required>
            <input
              className="input"
              value={name}
              onChange={(event) => setName(event.target.value)}
              placeholder="例如 财务组"
            />
          </Field>
        </div>
        <div className="form-row">
          <Field label="优先级" title="priority" hint="非负整数（≥ 0）">
            <input
              className="input"
              type="number"
              min={0}
              step={1}
              value={priority}
              onChange={(event) => setPriority(event.target.value)}
            />
          </Field>
          <Field label="最小预算" title="min_budget" hint="正整数（> 0，默认 800）">
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
          <legend>
            角色 <span className="required-mark">*</span>
          </legend>
          {rolesLoading ? (
            <p className="hint">正在加载角色清单…</p>
          ) : roleChoices.length === 0 ? (
            <p className="hint">角色清单为空：请先在策略文件中定义角色。</p>
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
      </form>
    </Modal>
  )
}
