import { useState } from 'react'
import { useMutation } from '@tanstack/react-query'
import { createAccessToken, updateAccessToken } from '../../api/admin'
import type {
  AccessTokenCreateInput,
  AccessTokenCreated,
  AccessTokenItem,
  AccessTokenUpdateInput,
  GroupItem,
  RoleItem,
} from '../../api/types'
import { Badge } from '../../components/Badge'
import { Field } from '../../components/Field'
import { localInputToIso } from '../../format'
import { GROUP_SOURCE_LABEL } from '../../labels'
import { noticeFromError } from '../../notice'
import { toggle } from './tokenConfig'

/**
 * 提交意图：创建走 POST（角色、来源、调用者只在建档时生效），
 * 编辑走 PATCH 且只允许名称、组、过期时间三类键（角色与密钥都不在里面）。
 */
type SaveRequest =
  | { mode: 'create'; input: AccessTokenCreateInput }
  | { mode: 'edit'; id: string; input: AccessTokenUpdateInput }

/**
 * 后端 expires_at 是带纳秒小数秒的 RFC3339，V8 只稳定解析到毫秒；
 * 先截断小数部分，再按浏览器本地时区渲染成 datetime-local 需要的 `YYYY-MM-DDTHH:mm`。
 */
function isoToLocalInput(value: string | null): string {
  if (value === null) return ''
  const date = new Date(value.replace(/(\.\d{3})\d+/, '$1'))
  if (Number.isNaN(date.getTime())) return ''
  const day = `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, '0')}-${String(
    date.getDate(),
  ).padStart(2, '0')}`
  return `${day}T${String(date.getHours()).padStart(2, '0')}:${String(date.getMinutes()).padStart(2, '0')}`
}

/**
 * 访问令牌表单：创建与编辑共用。
 * 传 token 即为编辑态——预填名称、组、到期时间，角色只读，来源/调用者属于建档信息不再出现。
 */
export function TokenForm({
  roles,
  groups: groupOptions,
  token,
  onSaved,
}: {
  roles: RoleItem[]
  groups: GroupItem[]
  token?: AccessTokenItem | undefined
  onSaved: (result: AccessTokenCreated | AccessTokenItem) => void
}) {
  const editing = token !== undefined
  const [name, setName] = useState(token?.name ?? '')
  const [caller, setCaller] = useState('')
  const [source, setSource] = useState('')
  const [role, setRole] = useState(token?.role ?? '')
  const [groups, setGroups] = useState<string[]>(token?.groups ?? [])
  const [expiresAt, setExpiresAt] = useState(isoToLocalInput(token?.expires_at ?? null))
  const [formError, setFormError] = useState<string | undefined>(undefined)

  const selectedRole = roles.find((item) => item.code === role)

  const mutation = useMutation({
    mutationFn: (request: SaveRequest) =>
      request.mode === 'create'
        ? createAccessToken(request.input)
        : updateAccessToken(request.id, request.input),
    onSuccess: onSaved,
    onError: (error) => {
      const failure = noticeFromError(error, editing ? '保存访问令牌失败' : '生成访问令牌失败')
      setFormError(`${failure.description}${failure.meta === undefined ? '' : `（${failure.meta}）`}`)
    },
  })

  /** 换角色时套用角色默认组；只保留组清单里真实存在的组，避免提交出不存在的组码。 */
  const pickRole = (nextRole: string) => {
    setRole(nextRole)
    const defaultGroups = roles.find((item) => item.code === nextRole)?.default_groups ?? []
    const known = new Set(groupOptions.map((group) => group.code))
    setGroups(defaultGroups.filter((group) => known.has(group)))
  }

  const submit = () => {
    const trimmedName = name.trim()
    if (trimmedName === '') {
      setFormError('名称不能为空。')
      return
    }
    const expires = localInputToIso(expiresAt) ?? null
    if (token !== undefined) {
      setFormError(undefined)
      mutation.mutate({
        mode: 'edit',
        id: token.id,
        input: { name: trimmedName, groups, expires_at: expires },
      })
      return
    }
    const trimmedCaller = caller.trim()
    const trimmedSource = source.trim()
    if (trimmedCaller === '') {
      setFormError('调用者不能为空。')
      return
    }
    if (trimmedSource === '') {
      setFormError('来源不能为空。')
      return
    }
    if (role === '') {
      setFormError('请选择角色。')
      return
    }
    setFormError(undefined)
    mutation.mutate({
      mode: 'create',
      input: {
        name: trimmedName,
        role,
        groups,
        source: trimmedSource,
        caller: trimmedCaller,
        expires_at: expires,
      },
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
        <Field label="名称">
          <input
            className="input"
            value={name}
            onChange={(event) => setName(event.target.value)}
            placeholder="例如 数据组只读接入"
          />
        </Field>
        {!editing && (
          <Field label="调用者">
            <input
              className="input"
              value={caller}
              onChange={(event) => setCaller(event.target.value)}
              placeholder="例如 claude-code"
            />
          </Field>
        )}
        {!editing && (
          <Field label="来源">
            <input
              className="input"
              value={source}
              onChange={(event) => setSource(event.target.value)}
              placeholder="例如 mcp"
            />
          </Field>
        )}
      </div>

      <div className="form-row">
        <Field
          label="角色"
          hint={editing ? '角色创建后不可修改；需要换角色请新建一枚令牌。' : undefined}
        >
          {editing ? (
            <input className="input" value={selectedRole?.name ?? role} title={role} readOnly />
          ) : (
            <select
              className="select"
              value={role}
              onChange={(event) => pickRole(event.target.value)}
            >
              <option value="">请选择角色</option>
              {roles.map((item) => (
                <option key={item.code} value={item.code} title={item.code}>
                  {item.name}
                </option>
              ))}
            </select>
          )}
        </Field>
        <Field label="到期时间">
          <input
            className="input"
            type="datetime-local"
            value={expiresAt}
            onChange={(event) => setExpiresAt(event.target.value)}
          />
        </Field>
      </div>

      {!editing && selectedRole !== undefined && (
        <p className="hint">角色默认预算 {selectedRole.default_budget}</p>
      )}

      <fieldset className="check-fieldset">
        <legend>组</legend>
        <div className="check-grid">
          {groupOptions.map((group) => (
            <label key={group.code} className="check-item" title={group.code}>
              <input
                type="checkbox"
                checked={groups.includes(group.code)}
                onChange={(event) => setGroups(toggle(groups, group.code, event.target.checked))}
              />
              <span>{group.name}</span>
              <Badge tone={group.source === 'policy' ? 'accent' : 'neutral'} title={group.source}>
                {GROUP_SOURCE_LABEL[group.source]}
              </Badge>
            </label>
          ))}
        </div>
      </fieldset>

      <p className="hint">
        令牌只决定能进入哪些组；组内可见的上游与工具由「组」页的暴露矩阵决定——整台上游可见性，
        以及单工具按需 include / exclude。这里不再单独勾选工具。
      </p>
      {editing && (
        <p className="hint">编辑不会轮换密钥；需要换新明文请使用卡片上的「重新签发」。</p>
      )}

      {formError !== undefined && (
        <p className="inline-error" role="alert">
          {formError}
        </p>
      )}
      <div className="form-actions">
        <button type="submit" className="btn btn-primary" disabled={mutation.isPending}>
          {mutation.isPending ? '保存中…' : editing ? '保存修改' : '生成访问令牌'}
        </button>
      </div>
    </form>
  )
}
