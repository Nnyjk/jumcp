import { useMemo, useState } from 'react'
import { useMutation } from '@tanstack/react-query'
import { createCapability } from '../../api/admin'
import {
  indexRoles,
  roleLabel,
  useGroupsQuery,
  useRolesQuery,
  useUpstreamsQuery,
} from '../../api/queries'
import { Field } from '../../components/Field'
import { ErrorState } from '../../components/PageShell'
import { noticeFromError } from '../../notice'

/** 新建能力草案：模板 ID、组与执行目标必填，params 以 JSON 文本录入并在此校验。 */
export function CreateCapabilityForm({
  onCreated,
  onError,
}: {
  onCreated: (created: { id: string; version: number; definition_hash: string }) => void
  onError: (error: unknown) => void
}) {
  const [templateId, setTemplateId] = useState('')
  const [group, setGroup] = useState('')
  const [upstream, setUpstream] = useState('')
  const [tool, setTool] = useState('query')
  const [database, setDatabase] = useState('')
  const [description, setDescription] = useState('')
  const [sql, setSql] = useState('')
  const [paramsText, setParamsText] = useState('')
  const [formError, setFormError] = useState<string | undefined>(undefined)

  const groupsQuery = useGroupsQuery()
  const rolesQuery = useRolesQuery()
  const upstreamsQuery = useUpstreamsQuery()
  const roleIndex = useMemo(() => indexRoles(rolesQuery.data), [rolesQuery.data])

  const mutation = useMutation({
    mutationFn: (definition: Record<string, unknown>) => createCapability(definition),
    onSuccess: (created) => {
      setTemplateId('')
      setGroup('')
      setUpstream('')
      setTool('query')
      setDatabase('')
      setDescription('')
      setSql('')
      setParamsText('')
      setFormError(undefined)
      onCreated(created)
    },
    // 弹层内的服务端错误必须留在表单里：页面顶部的 notice 会被弹层挡住。
    onError: (error) => {
      setFormError(noticeFromError(error, '新建草案失败').description)
      onError(error)
    },
  })

  const groups = groupsQuery.data?.items ?? []
  const upstreams = upstreamsQuery.data?.items ?? []
  const selectedGroup = groups.find((item) => item.code === group)

  return (
    <form
      className="create-form"
      onSubmit={(event) => {
        event.preventDefault()
        if (templateId.trim() === '') {
          setFormError('模板 ID 不能为空。')
          return
        }
        if (group === '') {
          setFormError('必须选择一个已存在的组：组决定哪些调用方能看到该能力。')
          return
        }
        if (upstream === '') {
          setFormError('必须选择一个已接入的上游：模板 SQL 只能交给上游 MCP 的工具执行。')
          return
        }
        if (tool.trim() === '') {
          setFormError('工具名不能为空：填上游暴露的原始工具名，通常是 query。')
          return
        }
        const definition: Record<string, unknown> = { template_id: templateId.trim(), group }
        const target: Record<string, string> = { upstream, tool: tool.trim() }
        if (database.trim() !== '') target.database = database.trim()
        definition.target = target
        if (description.trim() !== '') definition.description = description.trim()
        if (sql.trim() !== '') definition.sql = sql.trim()
        if (paramsText.trim() !== '') {
          try {
            definition.params = JSON.parse(paramsText) as unknown
          } catch {
            setFormError('参数不是合法 JSON，请修正后再提交。')
            return
          }
        }
        setFormError(undefined)
        mutation.mutate(definition)
      }}
    >
      <p className="hint">
        模板 SQL 会发给所选上游 MCP 的工具执行，jumcp 不再直连数据库；SQL 会先过前置拦截
        （拒绝无 LIMIT 且无 WHERE 的宽查询）。
      </p>
      <div className="form-row">
        <Field label="模板 ID" title="template_id">
          <input
            className="input"
            value={templateId}
            onChange={(event) => setTemplateId(event.target.value)}
            placeholder="例如 orders.daily_summary"
          />
        </Field>
        <Field label="组（必选）" title="group" required>
          <select
            className="select"
            value={group}
            onChange={(event) => setGroup(event.target.value)}
            disabled={groupsQuery.isPending}
            required
          >
            <option value="">
              {groupsQuery.isPending
                ? '正在加载组…'
                : groupsQuery.isError
                  ? '组清单加载失败'
                  : '请选择组'}
            </option>
            {groups.map((item) => (
              <option key={item.code} value={item.code} title={item.code}>
                {`${item.name}（${
                  item.roles.length > 0
                    ? item.roles.map((code) => roleLabel(roleIndex, code)).join(' / ')
                    : '未绑定角色'
                }）`}
              </option>
            ))}
          </select>
        </Field>
      </div>
      {groupsQuery.isError && <ErrorState error={groupsQuery.error} />}
      {selectedGroup !== undefined && selectedGroup.roles.length === 0 && (
        <p className="inline-error" role="alert">
          该组未绑定任何角色：能力创建后也不会出现在任何调用方的 tools/list 中。
        </p>
      )}
      <div className="form-row">
        <Field
          label="执行上游（必选）"
          title="target.upstream"
          required
          hint={
            !upstreamsQuery.isPending && !upstreamsQuery.isError && upstreams.length === 0
              ? '请先在「上游」页接入 MCP，否则模板无法执行。'
              : undefined
          }
        >
          <select
            className="select"
            value={upstream}
            onChange={(event) => setUpstream(event.target.value)}
            disabled={upstreamsQuery.isPending}
          >
            <option value="">
              {upstreamsQuery.isPending
                ? '正在加载上游…'
                : upstreamsQuery.isError
                  ? '上游清单加载失败'
                  : '请选择上游'}
            </option>
            {upstreams.map((item) => (
              <option key={item.id} value={item.id} title={item.id}>
                {`${item.id}（${item.transport}）`}
              </option>
            ))}
          </select>
        </Field>
        <Field label="工具名（必填）" title="target.tool" required>
          <input
            className="input mono"
            value={tool}
            onChange={(event) => setTool(event.target.value)}
            placeholder="query"
          />
        </Field>
        <Field label="库（可选）" title="target.database">
          <input
            className="input mono"
            value={database}
            onChange={(event) => setDatabase(event.target.value)}
            placeholder="例如 env_air"
          />
        </Field>
      </div>
      {upstreamsQuery.isError && <ErrorState error={upstreamsQuery.error} />}
      <Field label="描述" title="description">
        <input
          className="input"
          value={description}
          onChange={(event) => setDescription(event.target.value)}
          placeholder="给调用方看的一句话说明"
        />
      </Field>
      <Field label="模板 SQL" title="sql">
        <textarea
          className="input textarea"
          rows={4}
          value={sql}
          onChange={(event) => setSql(event.target.value)}
          placeholder="SELECT ..."
        />
      </Field>
      <Field label="参数（JSON）" title="params">
        <textarea
          className="input textarea"
          rows={3}
          value={paramsText}
          onChange={(event) => setParamsText(event.target.value)}
          placeholder='例如 {"order_id":{"type":"string","required":true}}'
        />
      </Field>
      {formError !== undefined && (
        <p className="inline-error" role="alert">
          {formError}
        </p>
      )}
      <div className="form-actions">
        <button type="submit" className="btn btn-primary" disabled={mutation.isPending || groupsQuery.isPending}>
          {mutation.isPending ? '正在创建…' : '创建草案'}
        </button>
      </div>
    </form>
  )
}
