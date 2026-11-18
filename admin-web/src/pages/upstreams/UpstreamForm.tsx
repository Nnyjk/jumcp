import { useMemo, useState } from 'react'
import { useMutation } from '@tanstack/react-query'
import { createUpstream, updateUpstream } from '../../api/admin'
import type {
  GroupItem,
  UpstreamCategory,
  UpstreamInput,
  UpstreamItem,
  UpstreamTransport,
} from '../../api/types'
import { Field } from '../../components/Field'
import { UPSTREAM_CATEGORY_LABEL, UPSTREAM_TRANSPORT_LABEL } from '../../labels'
import { validateUpstreamId } from '../../lib/upstreamId'
import { noticeFromError, type Notice } from '../../notice'
import {
  CATEGORY_OPTIONS,
  formatKeyValueLines,
  isHttpUrl,
  parseKeyValueLines,
  type EditorTarget,
} from './upstreamTools'

export function UpstreamForm({
  target,
  groups,
  groupsLoading,
  groupsError,
  onSaved,
  onError,
}: {
  target: EditorTarget
  groups: GroupItem[]
  groupsLoading: boolean
  groupsError: boolean
  onSaved: (item: UpstreamItem) => void
  onError: (notice: Notice) => void
}) {
  const existing = target.mode === 'edit' ? target.item : undefined
  const [id, setId] = useState(existing?.id ?? '')
  const [name, setName] = useState(existing?.name ?? '')
  const [transport, setTransport] = useState<UpstreamTransport>(existing?.transport ?? 'http')
  const [url, setUrl] = useState(existing?.url ?? '')
  const [command, setCommand] = useState(existing?.command ?? '')
  const [argsText, setArgsText] = useState((existing?.args ?? []).join('\n'))
  const [envText, setEnvText] = useState(formatKeyValueLines(existing?.env ?? {}))
  const [headersText, setHeadersText] = useState(formatKeyValueLines(existing?.headers ?? {}))
  const [group, setGroup] = useState(existing?.group ?? '')
  const [category, setCategory] = useState<UpstreamCategory | ''>(existing?.category ?? '')
  const [readOnly, setReadOnly] = useState(existing?.read_only ?? true)
  const [enabled, setEnabled] = useState(existing?.enabled ?? true)
  const [timeoutMs, setTimeoutMs] = useState(String(existing?.timeout_ms ?? 30000))
  const [formError, setFormError] = useState<string | undefined>(undefined)

  /**
   * 只允许选择可授予的组：builtin 合成组（basic）不是上游级组，不能作为授权组。
   * 编辑中的组若不在当前清单里（例如清单加载失败），以 code 兜底展示，避免保存时静默改组。
   */
  const groupChoices = useMemo(() => {
    const choices = groups
      .filter((item) => item.grantable !== false)
      .map((item) => ({ code: item.code, name: item.name }))
    if (existing !== undefined && !choices.some((choice) => choice.code === existing.group)) {
      choices.push({ code: existing.group, name: existing.group })
    }
    return choices
  }, [groups, existing])

  const mutation = useMutation({
    mutationFn: (input: UpstreamInput) => {
      if (existing === undefined) return createUpstream(input)
      return updateUpstream(existing.id, input)
    },
    onSuccess: onSaved,
    onError: (error) => {
      const notice = noticeFromError(error, existing === undefined ? '新建上游失败' : '更新上游失败')
      setFormError(notice.description)
      onError(notice)
    },
  })

  const submit = () => {
    const trimmedId = existing?.id ?? id.trim()
    const trimmedName = name.trim()

    if (existing === undefined) {
      const idError = validateUpstreamId(trimmedId)
      if (idError !== undefined) {
        setFormError(idError)
        return
      }
    }
    if (trimmedName === '') {
      setFormError('名称不能为空。')
      return
    }
    if (group === '') {
      setFormError('必须选择一个授权组。')
      return
    }
    if (category === '') {
      setFormError('必须选择一个分类。')
      return
    }

    const parsedTimeout = Number(timeoutMs)
    if (!Number.isInteger(parsedTimeout) || parsedTimeout < 1 || parsedTimeout > 120000) {
      setFormError('超时必须是 1–120000 之间的整数毫秒。')
      return
    }

    if (transport === 'http') {
      if (url.trim() === '') {
        setFormError('HTTP 传输必须填写 URL。')
        return
      }
      if (!isHttpUrl(url.trim())) {
        setFormError('URL 必须是合法的 http/https 地址。')
        return
      }
    } else if (command.trim() === '') {
      setFormError('stdio 传输必须填写 command。')
      return
    }

    const env = parseKeyValueLines(envText)
    if ('error' in env) {
      setFormError(`env：${env.error}`)
      return
    }
    const headers = parseKeyValueLines(headersText)
    if ('error' in headers) {
      setFormError(`headers：${headers.error}`)
      return
    }

    setFormError(undefined)
    mutation.mutate({
      id: trimmedId,
      name: trimmedName,
      transport,
      url: transport === 'http' ? url.trim() : null,
      command: transport === 'stdio' ? command.trim() : null,
      args:
        transport === 'stdio'
          ? argsText
              .split('\n')
              .map((line) => line.trim())
              .filter((line) => line !== '')
          : [],
      env: env.value,
      headers: headers.value,
      group,
      category,
      read_only: readOnly,
      enabled,
      timeout_ms: parsedTimeout,
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
        <Field label="id" title="id">
          <input
            className="input mono"
            value={existing?.id ?? id}
            readOnly={existing !== undefined}
            onChange={(event) => setId(event.target.value)}
            placeholder="reporting"
          />
        </Field>
        <Field label="名称" title="name">
          <input
            className="input"
            value={name}
            onChange={(event) => setName(event.target.value)}
            placeholder="例如 报表分析"
          />
        </Field>
      </div>

      <div className="form-row">
        <Field label="传输" title="transport">
          <select
            className="select"
            value={transport}
            onChange={(event) => setTransport(event.target.value as UpstreamTransport)}
          >
            <option value="http">{UPSTREAM_TRANSPORT_LABEL.http}</option>
            <option value="stdio">{UPSTREAM_TRANSPORT_LABEL.stdio}</option>
          </select>
        </Field>
        <Field label="授权组" title="group" required>
          <select
            className="select"
            value={group}
            onChange={(event) => setGroup(event.target.value)}
            disabled={groupsLoading}
            required
          >
            <option value="">
              {groupsLoading
                ? '正在加载组…'
                : groupsError
                  ? '组清单加载失败'
                  : '请选择组'}
            </option>
            {groupChoices.map((item) => (
              <option key={item.code} value={item.code} title={item.code}>
                {item.name}
              </option>
            ))}
          </select>
        </Field>
      </div>
      {groupsError && (
        <p className="hint">组清单加载失败：授权组下拉不可用，请修复后重试。</p>
      )}

      {transport === 'http' ? (
        <Field label="URL" title="url">
          <input
            className="input mono"
            value={url}
            onChange={(event) => setUrl(event.target.value)}
            placeholder="https://example.com/mcp"
          />
        </Field>
      ) : (
        <>
          <Field label="command" title="command">
            <input
              className="input mono"
              value={command}
              onChange={(event) => setCommand(event.target.value)}
              placeholder="例如 npx"
            />
          </Field>
          <Field label="args（每行一个参数）" title="args">
            <textarea
              className="input textarea"
              rows={3}
              value={argsText}
              onChange={(event) => setArgsText(event.target.value)}
              placeholder={'-y\n@modelcontextprotocol/server-filesystem\n/tmp'}
            />
          </Field>
        </>
      )}

      <div className="form-row">
        <Field label="分类" required>
          <select
            className="select"
            value={category}
            onChange={(event) => setCategory(event.target.value as UpstreamCategory | '')}
            required
          >
            <option value="">请选择分类</option>
            {CATEGORY_OPTIONS.map((value) => (
              <option key={value} value={value}>
                {UPSTREAM_CATEGORY_LABEL[value]}
              </option>
            ))}
          </select>
        </Field>
        <Field label="超时（ms）" title="timeout_ms">
          <input
            className="input"
            type="number"
            min={1}
            max={120000}
            step={1}
            value={timeoutMs}
            onChange={(event) => setTimeoutMs(event.target.value)}
          />
        </Field>
      </div>

      <Field label="env（每行 KEY=值；敏感值用 env:NAME 引用环境变量）" title="env">
        <textarea
          className="input textarea"
          rows={3}
          value={envText}
          onChange={(event) => setEnvText(event.target.value)}
          placeholder={'API_KEY=env:UPSTREAM_API_KEY\nDEBUG=true'}
        />
      </Field>
      <Field
        label="headers（每行 KEY=值；Authorization/Cookie 已脱敏为 ***，原样回传即保持原值）"
        title="headers"
      >
        <textarea
          className="input textarea"
          rows={3}
          value={headersText}
          onChange={(event) => setHeadersText(event.target.value)}
          placeholder={'Authorization=Bearer env:UPSTREAM_TOKEN\nX-Tenant=jumcp'}
        />
      </Field>
      <p className="hint">
        敏感值建议写成 <span className="mono">env:NAME</span>，避免明文落库；GET 响应中的敏感头显示为{' '}
        <span className="mono">***</span>，编辑时原样回传即保持原值。
      </p>

      <div className="form-row">
        <label className="check-item" title="只读上游不允许写操作；写上游还需部署开关">
          <input
            type="checkbox"
            checked={readOnly}
            onChange={(event) => setReadOnly(event.target.checked)}
          />
          <span>只读</span>
          <span className="hint">写上游还需部署开关</span>
        </label>
        <label className="check-item">
          <input
            type="checkbox"
            checked={enabled}
            onChange={(event) => setEnabled(event.target.checked)}
          />
          <span>启用</span>
        </label>
      </div>

      {formError !== undefined && (
        <p className="inline-error" role="alert">
          {formError}
        </p>
      )}
      <div className="form-actions">
        <button type="submit" className="btn btn-primary" disabled={mutation.isPending}>
          {mutation.isPending ? '保存中…' : existing === undefined ? '创建接入' : '保存修改'}
        </button>
      </div>
    </form>
  )
}

