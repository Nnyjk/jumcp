/**
 * 账户写操作的错误码 → 中文提示。
 * 后端错误体的 message 是给脚本看的英文/开发措辞，界面只按 code 翻译，
 * 未收录的 code 才退回原始 message（并保留 code 便于定位）。
 */
import { ApiError } from '../../api/client'

/** 契约里账户接口可能返回的业务码（以后端实际 `MCP_ADMIN_*` 为准）。 */
const CODE_TEXT: Record<string, string> = {
  MCP_ADMIN_USER_EXISTS: '该用户名已被占用，请换一个。',
  MCP_ADMIN_USER_NOT_FOUND: '账号不存在（可能已被其他管理员删除）。',
  MCP_ADMIN_LAST_ACTIVE: '这是最后一个启用中的账户：先启用或新建另一个账户，再停用/删除它。',
  MCP_ADMIN_SELF_DELETE: '不能删除当前登录的账户。',
  MCP_VALIDATION_ERROR: '输入不合法：用户名需 3–32 位小写字母/数字/._-，密码至少 12 位。',
}

/** 表单内联错误：账户弹层会盖住页面顶部 InfoBar，错误必须留在原地。 */
export function accountErrorMessage(error: unknown): string {
  if (error instanceof ApiError) {
    const text = CODE_TEXT[error.code]
    if (text !== undefined) return text
    return error.message
  }
  return error instanceof Error ? error.message : String(error)
}
