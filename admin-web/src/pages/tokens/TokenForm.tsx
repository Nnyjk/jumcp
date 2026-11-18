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
  /** 上游工具授权：`*`、`<id>__*` 或精确暴露名，统一写进 builtin_tools。 */
  const [grants, setGrants] = useState<string[]>([])
  const [formError, setFormError] = useState<string | undefined>(undefined)

  const wildcards = options.tool_wildcards ?? []
  const upstreams = options.upstreams ?? []
  const upstreamTools = options.upstream_tools ?? []
  const allToolsSelected = grants.includes('*')

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

  /**
   * `*` 是互斥的全量授权：勾选时清空其他条目，取消勾选后从空清单重新开始；
   * 其余通配/精确名按普通多选处理。
   */
  const toggleGrant = (value: string, checked: boolean) => {
    if (value === '*') {
      setGrants(checked ? ['*'] : [])
      return
    }
    setGrants((current) => toggle(current, value, checked))
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
    setFormError(undefined)
    mutation.mutate({
      name: trimmedName,
      role,
      groups,
      builtin_tools: [...new Set([...builtinTools, ...grants])],
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

      <fieldset className="check-fieldset">
        <legend>上游工具</legend>
        {wildcards.length === 0 && upstreams.length === 0 && (
          <p className="hint">暂无可授予的上游工具；先在 MCP 接入页安装并刷新上游。</p>
        )}
        {wildcards.length > 0 && (
          <div className="check-grid">
            {wildcards.map((wildcard) => (
              <label
                key={wildcard.value}
                className="check-item"
                title={`${wildcard.label}（${wildcard.value}）`}
              >
                <input
                  type="checkbox"
                  checked={grants.includes(wildcard.value)}
                  onChange={(event) => toggleGrant(wildcard.value, event.target.checked)}
                />
                <span>{wildcard.label}</span>
                <code className="mono upstream-grant-value">{wildcard.value}</code>
              </label>
            ))}
          </div>
        )}
        {upstreams.length > 0 && (
          <div className="upstream-grants">
            {upstreams.map((upstream) => {
              const wildcard =
                upstream.wildcard === '' ? `${upstream.id}__*` : upstream.wildcard
              const tools = upstreamTools.filter((tool) => tool.upstream_id === upstream.id)
              return (
                <div className="upstream-grant" key={upstream.id} data-disabled={allToolsSelected}>
                  <label className="check-item upstream-grant-head" title={wildcard}>
                    <input
                      type="checkbox"
                      disabled={allToolsSelected}
                      checked={grants.includes(wildcard)}
                      onChange={(event) => toggleGrant(wildcard, event.target.checked)}
                    />
                    <span>{upstream.name} 全部工具</span>
                    <code className="mono upstream-grant-value">{wildcard}</code>
                    {!upstream.enabled && <Badge tone="neutral">已停用</Badge>}
                  </label>
                  {tools.length === 0 ? (
                    <p className="upstream-grant-hint">
                      {upstream.tool_count === 0
                        ? '尚未刷新，先授予通配或到上游页刷新后再按工具授予。'
                        : '该上游暂无暴露工具。'}
                    </p>
                  ) : (
                    <div className="check-grid">
                      {tools.map((tool) => (
                        <label key={tool.name} className="check-item check-item-stacked" title={tool.name}>
                          <input
                            type="checkbox"
                            disabled={allToolsSelected}
                            checked={grants.includes(tool.name)}
                            onChange={(event) => toggleGrant(tool.name, event.target.checked)}
                          />
                          <span className="cell-stack">
                            <span>{tool.label}</span>
                            <span className="cell-sub mono">
                              {tool.name} · {tool.group}
                            </span>
                          </span>
                        </label>
                      ))}
                    </div>
                  )}
                </div>
              )
            })}
          </div>
        )}
        <p className="hint">
          通配 `*` 与单台上游 `&lt;id&gt;__*` 会同时覆盖后续新安装/新刷新的工具；与上方勾选的内置工具合并提交。
        </p>
      </fieldset>

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
