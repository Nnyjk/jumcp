import { CodeBlock, JsonBlock } from './JsonBlock'
import { isRecord } from '../objects'
import { DASH } from '../format'
import { inlineJson } from '../json'

function firstString(...values: unknown[]): string | undefined {
  for (const value of values) {
    if (typeof value === 'string' && value.trim() !== '') return value
  }
  return undefined
}

function valueText(value: unknown): string {
  if (value === null || value === undefined || value === '') return DASH
  if (typeof value === 'string') return value
  if (typeof value === 'number' || typeof value === 'boolean') return String(value)
  return inlineJson(value)
}

interface ParamRow {
  name: string
  type: string
  required: string
  fallback: string
  description: string
}

/** params 既可能是 [{name,type,...}] 也可能是 {name: {type,...}}，两种形态都摊平成表格。 */
function paramRows(source: unknown): ParamRow[] {
  if (Array.isArray(source)) {
    return source.map((entry, index) => {
      if (!isRecord(entry)) {
        return {
          name: `#${index + 1}`,
          type: valueText(entry),
          required: DASH,
          fallback: DASH,
          description: DASH,
        }
      }
      return {
        name: valueText(entry.name ?? entry.id ?? `#${index + 1}`),
        type: valueText(entry.type ?? entry.kind),
        required:
          typeof entry.required === 'boolean' ? (entry.required ? '是' : '否') : valueText(entry.required),
        fallback: valueText(entry.default ?? entry.fallback),
        description: valueText(entry.description ?? entry.doc),
      }
    })
  }
  if (isRecord(source)) {
    return Object.entries(source).map(([name, schema]) => {
      if (!isRecord(schema)) {
        return { name, type: valueText(schema), required: DASH, fallback: DASH, description: DASH }
      }
      return {
        name,
        type: valueText(schema.type ?? schema.kind),
        required:
          typeof schema.required === 'boolean'
            ? schema.required
              ? '是'
              : '否'
            : valueText(schema.required),
        fallback: valueText(schema.default ?? schema.fallback),
        description: valueText(schema.description ?? schema.doc),
      }
    })
  }
  return []
}

function ConstraintList({ source }: { source: unknown }) {
  if (typeof source === 'string') {
    return <CodeBlock>{source}</CodeBlock>
  }
  if (Array.isArray(source)) {
    return (
      <ul className="constraint-list">
        {source.map((entry, index) => (
          <li key={index} className="mono">
            {valueText(entry)}
          </li>
        ))}
      </ul>
    )
  }
  if (isRecord(source)) {
    return (
      <dl className="kv-list">
        {Object.entries(source).map(([key, value]) => (
          <div key={key} className="kv-row">
            <dt className="mono">{key}</dt>
            <dd className="mono">{valueText(value)}</dd>
          </div>
        ))}
      </dl>
    )
  }
  return <span className="muted">{DASH}</span>
}

/**
 * 能力定义的结构化只读视图：SQL / 参数表 / 约束 / 缓存 / 方言 / 绑定顺序 / 变体。
 * 完整 JSON 收在「高级」折叠里，避免管理员把原始定义当成唯一入口。
 */
export function DefinitionView({ definition }: { definition: Record<string, unknown> }) {
  const template = isRecord(definition.template) ? definition.template : undefined
  const sql = firstString(definition.base_sql, definition.sql, template?.base_sql, template?.sql)
  const params = paramRows(definition.params ?? definition.parameters ?? template?.params)
  const constraints = definition.constraints ?? template?.constraints
  const cacheTtl = definition.result_cache_ttl_s ?? template?.result_cache_ttl_s
  const cache =
    definition.cache ??
    template?.cache ??
    (typeof cacheTtl === 'number'
      ? { 结果缓存TTL: cacheTtl === 0 ? '0 秒（未启用）' : `${cacheTtl} 秒` }
      : undefined)
  const bindOrder = definition.bind_order ?? template?.bind_order
  const dialect = firstString(definition.dialect, template?.dialect)
  const variants = definition.variants ?? template?.variants
  const description = firstString(definition.description, template?.description)

  return (
    <div className="definition-view">
      {description !== undefined && <p className="definition-desc">{description}</p>}

      <div className="definition-meta">
        <span>
          方言：<span className="mono">{dialect ?? DASH}</span>
        </span>
        <span>
          组：<span className="mono">{firstString(definition.group, template?.group) ?? DASH}</span>
        </span>
        <span>
          绑定顺序：
          <span className="mono">
            {Array.isArray(bindOrder)
              ? bindOrder.length === 0
                ? '（无）'
                : bindOrder.map((item) => valueText(item)).join(' → ')
              : valueText(bindOrder)}
          </span>
        </span>
      </div>

      <section>
        <h4 className="detail-heading">模板 SQL</h4>
        {sql === undefined ? <p className="muted">定义中没有可识别的 SQL 字段。</p> : <CodeBlock tone="sql">{sql}</CodeBlock>}
      </section>

      <section>
        <h4 className="detail-heading">参数</h4>
        {params.length === 0 ? (
          <p className="muted">该定义没有声明参数。</p>
        ) : (
          <div className="table-scroll">
            <table className="kv-table">
              <thead>
                <tr>
                  <th scope="col">参数</th>
                  <th scope="col">类型</th>
                  <th scope="col">必填</th>
                  <th scope="col">默认值</th>
                  <th scope="col">说明</th>
                </tr>
              </thead>
              <tbody>
                {params.map((param) => (
                  <tr key={param.name}>
                    <td className="mono">{param.name}</td>
                    <td className="mono">{param.type}</td>
                    <td>{param.required}</td>
                    <td className="mono">{param.fallback}</td>
                    <td>{param.description}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>

      <section>
        <h4 className="detail-heading">约束</h4>
        {constraints === undefined ? <p className="muted">未声明约束。</p> : <ConstraintList source={constraints} />}
      </section>

      <section>
        <h4 className="detail-heading">缓存</h4>
        {cache === undefined ? (
          <p className="muted">未配置缓存（每次真实执行）。</p>
        ) : (
          <ConstraintList source={cache} />
        )}
      </section>

      {variants !== undefined && (
        <section>
          <h4 className="detail-heading">变体</h4>
          <JsonBlock value={variants} />
        </section>
      )}

      <details className="raw-payload">
        <summary>高级：完整定义 JSON</summary>
        <JsonBlock value={definition} />
      </details>
    </div>
  )
}
