/**
 * 唯一的对象类型守卫：所有 unknown JSON 的形状判定都从这里取，避免各文件重复实现。
 * 它只证明「是普通对象」，字段类型仍需按使用点单独校验。
 */
export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}
