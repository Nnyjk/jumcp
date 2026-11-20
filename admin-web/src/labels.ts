import type { BadgeTone } from './components/Badge'
import type {
  AccessTokenStatus,
  AuditAction,
  AuditChainState,
  AuditObjectType,
  AuditOutcome,
  CapabilityRisk,
  CapabilityStatus,
  GroupSource,
  ToolHealth,
  ToolSource,
  UpstreamCategory,
  UpstreamStatus,
  UpstreamTransport,
} from './api/types'

/**
 * 展示用中文名。后端原值仍是唯一契约：徽标/单元格的 title 保留英文原值，
 * 过滤参数与错误码一律使用原值，避免界面翻译与状态机脱节。
 */
export const STATUS_LABEL: Record<CapabilityStatus, string> = {
  draft: '草稿',
  review: '待审',
  published: '已发布',
  deprecated: '已下线',
}

/** 台账的生命周期列：内置能力没有流程状态，显示「内置」并不代表已发布。 */
export const LIFECYCLE_LABEL: Record<string, string> = {
  ...STATUS_LABEL,
  builtin: '内置',
  unknown: '未知',
}

export const RISK_LABEL: Record<CapabilityRisk, string> = {
  read: '只读',
  write: '写入',
  sensitive: '敏感',
  admin: '管理',
}

/** 审计事件类型：闭集里没有 session_init（该事件从不写入）。 */
export const EVENT_TYPE_LABEL: Record<string, string> = {
  tools_list_served: '工具列表下发',
  capability_search: '能力检索',
  capability_describe: '能力描述',
  tool_call: '工具调用',
  template_run: '模板执行',
  policy_decision: '策略判定',
  admin_change: '管理变更',
}

export const OBJECT_TYPE_LABEL: Record<AuditObjectType, string> = {
  capability: '能力',
  group: '组',
  tool: '工具',
  template: '模板',
  catalog: '目录',
  request: '请求',
  token: '访问令牌',
  upstream: '上游接入',
}

export const ACTION_LABEL: Record<AuditAction, string> = {
  create: '创建',
  update_definition: '修改定义',
  submit: '提交审核',
  publish: '发布',
  reject: '退回',
  visibility: '调整可见性',
  deprecate: '下架',
  group_create: '创建组',
  group_update: '修改组',
  group_delete: '删除组',
  token_create: '创建令牌',
  token_revoke: '撤销令牌',
  token_reissue: '重新签发令牌',
  token_delete: '删除令牌',
  invoke: '调用',
  search: '检索',
  describe: '查看描述',
  list_tools: '下发工具列表',
  deny: '拒绝',
  refresh: '刷新上游',
  update_tool_groups: '调整基础工具',
}

export const OUTCOME_LABEL: Record<AuditOutcome, string> = {
  ok: '成功',
  error: '失败',
  denied: '拒绝',
  summary: '摘要',
}

export const APPROVAL_SOURCE_LABEL: Record<string, string> = {
  external: '外部审批单',
  none: '无审批单',
}

/** 后端业务错误码 → 人话。码表外的错误码原样展示，不隐藏。 */
export const ERROR_CODE_LABEL: Record<string, string> = {
  MCP_UNAUTHORIZED: '未认证调用方',
  MCP_FORBIDDEN: '无访问权限',
  MCP_NOT_FOUND: '对象不存在',
  MCP_CONFLICT: '状态冲突',
  MCP_VALIDATION_ERROR: '参数校验失败',
  MCP_STORE_ERROR: '存储层错误',
  MCP_BUDGET_EXCEEDED: '超出调用预算',
  MCP_GROUP_NOT_FOUND: '组不存在或未授权',
  MCP_RATE_LIMITED: '触发频率限制',
  MCP_INTERNAL_ERROR: '服务内部错误',
  MCP_UNSUPPORTED: '不支持的操作',
  MCP_APPROVAL_REQUIRED: '缺少有效审批',
  MCP_AUDIT_ERROR: '审计写入失败',
  MCP_BAD_REQUEST: '请求不合法',
  MCP_BUILTIN_IMMUTABLE: '内置对象不可修改',
  MCP_CAPABILITY_NOT_FOUND: '能力不存在',
  MCP_CONFIG_ERROR: '配置错误',
  MCP_CONTEXT_BUDGET_EXHAUSTED: '上下文预算耗尽',
  MCP_EXEC_TIMEOUT: '执行超时',
  MCP_FILTER_UNSUPPORTED: '不支持的过滤条件',
  MCP_GROUP_CONFLICT: '组冲突',
  MCP_GROUP_FORBIDDEN: '组访问被拒绝',
  MCP_GROUP_IN_USE: '组仍被引用',
  MCP_TOKEN_NOT_FOUND: '令牌不存在',
  MCP_TOKEN_ALREADY_REVOKED: '令牌已撤销',
  MCP_GROUP_RESERVED: '组名保留',
  MCP_IMMUTABLE_PUBLISHED: '已发布不可原地修改',
  MCP_MULTI_STATEMENT: '只允许单条语句',
  MCP_NOT_IMPLEMENTED: '未实现的操作',
  MCP_NULL_PARAM_NEEDS_CAST: '空参数需显式类型',
  MCP_PARAM_COUNT_MISMATCH: '参数数量不匹配',
  MCP_PARAM_STYLE_UNSUPPORTED: '不支持的参数样式',
  MCP_PREFLIGHT_REQUIRED: '需要预检',
  MCP_SCHEMA_OUT_OF_SCOPE: '超出允许的 schema',
  MCP_SQL_REJECTED: 'SQL 被拒绝',
  MCP_SQL_BLOCKED: 'SQL 被前置拦截',
  MCP_STATE_TRANSITION_INVALID: '非法状态迁移',
  MCP_TEMPLATE_INVALID: '模板定义非法',
  MCP_UNSUPPORTED_COLUMN_TYPE: '不支持的列类型',
  MCP_VERSION_CONFLICT: '版本冲突',
  MCP_UPSTREAM_TOOL_UNKNOWN: '上游工具不存在或已变更',
  MCP_UPSTREAM_TOOL_NOT_READ_ONLY: '写工具不能归入基础组',
  MCP_UPSTREAM_TOOL_NOT_ELIGIBLE: '工具不满足基础组条件',
}

/** 定义/可见性变更的字段名 → 中文。未知字段原样展示。 */
export const CHANGE_FIELD_LABEL: Record<string, string> = {
  base_sql: '基础 SQL',
  sql: '模板 SQL',
  params: '参数',
  bind_order: '绑定顺序',
  dialect: '方言',
  template: '模板正文',
  variants: '变体',
  description: '描述',
  group: '组',
  risk: '风险',
  discoverable: '可被发现',
  callable: '可被调用',
  status: '生命周期',
  successor_id: '替代能力',
}

/** /health 的 status 是后端自由字符串：已知值翻译，未知值原样展示。 */
export const HEALTH_STATUS_LABEL: Record<string, string> = {
  ok: '正常',
  degraded: '降级',
  error: '故障',
}

export const TOOL_KIND_LABEL: Record<string, string> = {
  template: '模板',
  tool: '工具',
  endpoint: '端点',
  unknown: '未知',
}

export const TOOL_SOURCE_LABEL: Record<ToolSource, string> = {
  builtin: '内置',
  managed: '受管',
}

export const TOOL_HEALTH_LABEL: Record<ToolHealth, string> = {
  ok: '正常',
  degraded: '降级',
  failing: '故障',
  unknown: '未知',
}

/** 调用结果：ok / summary 是成功路径，denied 是策略拦截。 */
export const TOOL_OUTCOME_LABEL: Record<string, string> = {
  ok: '成功',
  summary: '摘要',
  error: '失败',
  denied: '拒绝',
  budget_rejected: '预算拒绝',
  unknown: '未知',
}

export const TOOL_CACHE_LABEL: Record<string, string> = {
  hit: '命中',
  miss: '未命中',
}

/** 组来源：policy 由 config/policy.toml 提供，managed 由管理台维护，builtin 是后端合成组。 */
export const GROUP_SOURCE_LABEL: Record<GroupSource, string> = {
  policy: '策略组',
  managed: '受管组',
  builtin: '内置',
}

/** 访问令牌状态：expired 由到期时间派生，revoked 为手工撤销。 */
export const TOKEN_STATUS_LABEL: Record<AccessTokenStatus, string> = {
  active: '有效',
  revoked: '已撤销',
  expired: '已过期',
}

/** MCP 上游传输方式：契约值仍是 http / stdio。 */
export const UPSTREAM_TRANSPORT_LABEL: Record<UpstreamTransport, string> = {
  http: 'HTTP',
  stdio: 'stdio',
}

/** 上游分类：admin 指维护/管理类上游（docs/mcp-router.md §2），不是「审计」。 */
export const UPSTREAM_CATEGORY_LABEL: Record<UpstreamCategory, string> = {
  basic: '基础',
  analysis: '分析',
  admin: '管理',
  custom: '自定义',
}

/** 工具台账与上游共用同一套分类中文名；旧数据缺 category 时显示破折号。 */
export const TOOL_CATEGORY_LABEL: Record<string, string> = {
  ...UPSTREAM_CATEGORY_LABEL,
}

/**
 * 工具的生效组展示名：backend 的常驻内置工具现在挂在 basic（基础）组；
 * 旧数据里可能残留 builtin 分组，仍兼容显示“内置”；其余 code 回退到组清单 name。
 */
export function toolGroupLabel(code: string, fallback?: string): string {
  if (code === 'basic') return '基础'
  if (code === 'builtin') return '内置'
  return fallback ?? code
}

/** 上游探测状态：unknown 表示尚未探测，不冒充正常。 */
export const UPSTREAM_STATUS_LABEL: Record<UpstreamStatus, string> = {
  ok: '正常',
  degraded: '降级',
  error: '错误',
  unknown: '未知',
}

/**
 * 可见性组合的中文短句，避免两个裸布尔被误读：
 * - 可发现：出现在调用方的 tools/list 与 search 结果中；
 * - 可调用：允许 tools/call 直接调用。
 */
export function visibilityText(discoverable: boolean, callable: boolean): string {
  if (discoverable && callable) return '可发现 · 可调用'
  if (discoverable) return '可发现 · 不可调用'
  if (callable) return '不可发现 · 系统直调'
  return '不可见 · 不可调用'
}

export function errorCodeText(code: string | null | undefined): string {
  if (code === null || code === undefined || code === '') return '—'
  return ERROR_CODE_LABEL[code] ?? code
}

/** 审计链校验状态（看板 KPI 与健康页共用；两个页面的文案长短不同，色板相同）。 */
export const CHAIN_TONE: Record<AuditChainState, BadgeTone> = {
  valid: 'success',
  tampered: 'danger',
  error: 'danger',
}

/**
 * 状态色板：上游探测状态与访问令牌状态共用一组语义值，含义一致才同色
 * （ok/active 成功，degraded/expired 警示，error/revoked 危险，unknown 中性）。
 */
export const STATUS_TONE: Record<string, BadgeTone> = {
  active: 'success',
  ok: 'success',
  degraded: 'warning',
  expired: 'warning',
  error: 'danger',
  revoked: 'danger',
  unknown: 'neutral',
}
