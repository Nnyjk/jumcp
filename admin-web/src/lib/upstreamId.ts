/**
 * 上游 id 的唯一校验规则：新建上游与目录安装共用，正则与文案只在这里维护。
 * 首字符小写字母，后续只允许小写字母/数字/_/-，长度 2–32；`__` 是工具名分隔符，不能出现。
 */
const UPSTREAM_ID_PATTERN = /^[a-z][a-z0-9_-]{1,31}$/

/** 校验上游 id（调用方先 trim）：返回错误文案，合法时返回 undefined。 */
export function validateUpstreamId(id: string): string | undefined {
  if (id === '') return '上游 id 不能为空。'
  if (!UPSTREAM_ID_PATTERN.test(id)) {
    return '首字符小写字母，后续只允许小写字母、数字、_、-，长度 2–32。'
  }
  if (id.includes('__')) return 'id 不能包含连续两个下划线（__ 是工具名分隔符）。'
  return undefined
}
