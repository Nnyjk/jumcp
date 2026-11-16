/** 行内展示用 JSON：紧凑单行，无法序列化时退回 String。 */
export function inlineJson(value: unknown): string {
  try {
    const text = JSON.stringify(value)
    return text === undefined ? String(value) : text
  } catch {
    return String(value)
  }
}

/** 详情/定义展示用 JSON 文本：无法序列化时退回 String，避免面板整体崩溃。 */
export function formatJson(value: unknown): string {
  try {
    const text = JSON.stringify(value, null, 2)
    return text === undefined ? String(value) : text
  } catch {
    return String(value)
  }
}
