import { useState } from 'react'
import { useMutation } from '@tanstack/react-query'
import { createAccessToken } from '../../api/admin'
import type {
  AccessTokenCreateInput,
  AccessTokenCreated,
  GroupItem,
  RoleItem,
  TokenOptions,
} from '../../api/types'
import { Badge } from '../../components/Badge'
import { Field } from '../../components/Field'
import { localInputToIso } from '../../format'
import { GROUP_SOURCE_LABEL } from '../../labels'
import { noticeFromError } from '../../notice'
import {
  DEFAULT_BUILTIN_TOOLS,
  parseUpstreamToolNames,
  toggle,
} from './tokenConfig'

export function TokenForm({
  options,
  roles,
  groups: groupOptions,
  onCreated,
}: {
  options: TokenOptions
  roles: RoleItem[]
  groups: GroupItem[]
  onCreated: (result: AccessTokenCreated) => void
}) {
  const [name, setName] = useState('')
  const [caller, setCaller] = useState('')
  const [source, setSource] = useState('')
  const [role, setRole] = useState('')
  const [groups, setGroups] = useState<string[]>([])
  const [builtinTools, setBuiltinTools] = useState<string[]>(() =>
    options.builtin_tools
      .filter((tool) => tool.group === 'basic' && DEFAULT_BUILTIN_TOOLS.includes(tool.name))
      .map((tool) => tool.name),
  )
  const [expiresAt, setExpiresAt] = useState('')
  const [upstreamToolsText, setUpstreamToolsText] = useState('')
  const [formError, setFormError] = useState<string | undefined>(undefined)

  const selectedRole = roles.find((item) => item.code === role)
  const adminSelected = groups.includes('admin')
  /** basic 等 grantable=false 组是内置通道，不能写进令牌 groups；UI 不提供勾选。 */
  const selectableGroups = groupOptions.filter((group) => group.grantable !== false)

  const mutation = useMutation({
    mutationFn: (input: AccessTokenCreateInput) => createAccessToken(input),
    onSuccess: onCreated,
    onError: (error) => {
      const failure = noticeFromError(error, '生成令牌失败')
      setFormError(`${failure.description}${failure.meta === undefined ? '' : `（${failure.meta}）`}`)
    },
  })

  const pickRole = (nextRole: string) => {
    setRole(nextRole)
    const defaultGroups = roles.find((item) => item.code === nextRole)?.default_groups ?? []
    const known = new Set(selectableGroups.map((group) => group.code))
    setGroups(defaultGroups.filter((group) => known.has(group)))
  }

  const pickGroup = (groupCode: string, checked: boolean) => {
    const next = toggle(groups, groupCode, checked)
    setGroups(next)
    if (groupCode === 'admin' && !checked) {
      const adminTools = new Set(
        options.builtin_tools.filter((tool) => tool.group === 'admin').map((tool) => tool.name),
      )
      setBuiltinTools((current) => current.filter((tool) => !adminTools.has(tool)))
    }
  }

  const submit = () => {
    const trimmedName = name.trim()
    const trimmedCaller = caller.trim()
    const trimmedSource = source.trim()
    if (trimmedName === '') {
      setFormError('名称不能为空。')
      return
    }
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
    const upstreamTools = parseUpstreamToolNames(upstreamToolsText)
    if ('error' in upstreamTools) {
      setFormError(`上游工具名：${upstreamTools.error}`)
      return
    }
    setFormError(undefined)
    mutation.mutate({
      name: trimmedName,
      role,
      groups,
      builtin_tools: [...new Set([...builtinTools, ...upstreamTools.names])],
      source: trimmedSource,
      caller: trimmedCaller,
      expires_at: localInputToIso(expiresAt) ?? null,
    })
  }

  const builtinOptions = options.builtin_tools.filter((tool) => tool.group === 'basic')
  const adminOptions = options.builtin_tools.filter((tool) => tool.group === 'admin')

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
        <Field label="调用者">
          <input
            className="input"
            value={caller}
            onChange={(event) => setCaller(event.target.value)}
            placeholder="例如 claude-code"
          />
        </Field>
        <Field label="来源">
          <input
            className="input"
            value={source}
            onChange={(event) => setSource(event.target.value)}
            placeholder="例如 mcp"
          />
        </Field>
      </div>

      <div className="form-row">
        <Field label="角色">
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

      {selectedRole !== undefined && (
        <p className="hint">角色默认预算 {selectedRole.default_budget}</p>
      )}

      <fieldset className="check-fieldset">
        <legend>组</legend>
        <div className="check-grid">
          {selectableGroups.map((group) => (
            <label key={group.code} className="check-item" title={group.code}>
              <input
                type="checkbox"
                checked={groups.includes(group.code)}
                onChange={(event) => pickGroup(group.code, event.target.checked)}
              />
              <span>{group.name}</span>
              <Badge tone={group.source === 'policy' ? 'accent' : 'neutral'} title={group.source}>
                {GROUP_SOURCE_LABEL[group.source]}
              </Badge>
            </label>
          ))}
        </div>
      </fieldset>

      <fieldset className="check-fieldset">
        <legend>内置工具</legend>
        <div className="check-grid">
          {builtinOptions.map((tool) => (
            <label key={tool.name} className="check-item" title={tool.name}>
              <input
                type="checkbox"
                checked={builtinTools.includes(tool.name)}
                onChange={(event) =>
                  setBuiltinTools((current) =>
                    toggle(current, tool.name, event.target.checked),
                  )
                }
              />
              <span>{tool.label}</span>
            </label>
          ))}
        </div>
        {adminSelected && adminOptions.length > 0 && (
          <div className="check-grid">
            {adminOptions.map((tool) => (
              <label key={tool.name} className="check-item" title={tool.name}>
                <input
                  type="checkbox"
                  checked={builtinTools.includes(tool.name)}
                  onChange={(event) =>
                    setBuiltinTools((current) =>
                      toggle(current, tool.name, event.target.checked),
                    )
                  }
                />
                <span>{tool.label}</span>
                <Badge tone="warning">管理</Badge>
              </label>
            ))}
          </div>
        )}
      </fieldset>

      <Field label="上游工具名（每行一个 <server_id>__<tool>）">
        <textarea
          className="input textarea"
          rows={3}
          value={upstreamToolsText}
          onChange={(event) => setUpstreamToolsText(event.target.value)}
          placeholder={'reporting__daily_summary'}
        />
      </Field>
      <p className="hint">
        名字可从 tools/list 或 MCP 接入后的工具列表获取；与上方勾选的内置工具合并提交。
      </p>

      {formError !== undefined && (
        <p className="inline-error" role="alert">
          {formError}
        </p>
      )}
      <div className="form-actions">
        <button type="submit" className="btn btn-primary" disabled={mutation.isPending}>
          {mutation.isPending ? '生成中…' : '生成令牌'}
        </button>
      </div>
    </form>
  )
}
