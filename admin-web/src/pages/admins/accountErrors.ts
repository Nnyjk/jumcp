/**
 * 管理账户操作错误码转中文提示：
 * 后端错误体的 message 主要为英文/技术描述，管理端前台严格按业务错误码（MCP_ADMIN_*）转译；
 * 未收录的错误码退回原始 message 并保留错误码便于排查定位。
 */
import { ApiError } from '../../api/client'

/** 契约中管理账户接口可能返回的业务错误码（以 backend 实际定义为准）。 */
const CODE_TEXT: Record<string, string> = {
  MCP_ADMIN_USER_EXISTS: '该用户名已被占用，请使用其他用户名。',
  MCP_ADMIN_USER_NOT_FOUND: '管理账户不存在（可能已被其他管理员删除）。',
  MCP_ADMIN_LAST_ACTIVE: '该账户是系统中最后一个启用中的管理账户：请先新建或启用其他账户，再停用或删除该账户。',
  MCP_ADMIN_SELF_DELETE: '禁止删除当前已登录的管理账户。',
  MCP_VALIDATION_ERROR: '输入不合法：用户名需 3–32 位小写字母、数字与 ._-，密码长度至少 12 位。',
}

/** 错误呈现约束：账户表单模态弹层位于 top layer 会遮挡页面 InfoBar，操作异常必须保留在表单内。 */
export function accountErrorMessage(error: unknown): string {
  if (error instanceof ApiError) {
    const text = CODE_TEXT[error.code]
    if (text !== undefined) return text
    return error.message
  }
  return error instanceof Error ? error.message : String(error)
}
