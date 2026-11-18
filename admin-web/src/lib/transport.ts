/**
 * 密码字段的传输加密：后端 GET /admin/transport-key 下发 32 字节密钥（64 位 hex），
 * 浏览器用 AES-256-GCM 加密后按 `enc:v1:<hex(nonce‖ciphertext‖tag)>` 提交。
 *
 * 为什么用 @noble/ciphers 而不是 WebCrypto：crypto.subtle 只在安全上下文（HTTPS / localhost）
 * 可用，而本功能恰恰要覆盖纯 HTTP 内网 IP 的部署；noble 是纯 JS、零依赖、已审计的实现，
 * 不为兼容性自写 AES。取舍与线格式细节见 notes/admin_web/transport_crypto_client.md。
 */
import { gcm } from '@noble/ciphers/aes.js'
import { randomBytes } from '@noble/ciphers/utils.js'
import { fetchTransportKey } from '../api/admin'
import { ApiError } from '../api/client'
import type { TransportKey } from '../api/types'

const ALGORITHM = 'AES-256-GCM'
const CIPHERTEXT_PREFIX = 'enc:v1:'
const KEY_BYTES = 32
const NONCE_BYTES = 12
const TAG_BYTES = 16

/** 模块级缓存：密钥只驻留内存，不写 localStorage/sessionStorage，也不进任何日志。 */
let keyPromise: Promise<Uint8Array> | null = null

/** 密钥格式异常 / 取不到密钥时的统一失败文案：调用方据此提示「密码未提交」。 */
function transportError(reason: string, detail?: string): Error {
  return new Error(detail === undefined ? reason : `${reason}：${detail}`)
}

/** 校验并解析 64 位小写 hex 密钥；不合规直接失败，不把可疑密钥喂给 AES。 */
function parseKey(hex: string): Uint8Array {
  if (!/^[0-9a-f]{64}$/.test(hex)) {
    throw transportError('传输密钥格式不正确，密码未提交')
  }
  const key = new Uint8Array(KEY_BYTES)
  for (let index = 0; index < KEY_BYTES; index += 1) {
    key[index] = Number.parseInt(hex.slice(index * 2, index * 2 + 2), 16)
  }
  return key
}

async function requestKey(): Promise<Uint8Array> {
  let payload: TransportKey
  try {
    payload = await fetchTransportKey()
  } catch (error) {
    // 保留后端错误码/消息（如 503 MCP_TRANSPORT_KEY_MISSING），否则管理员不知道该配什么。
    throw transportError(
      '无法获取传输密钥，密码未提交',
      error instanceof Error ? error.message : String(error),
    )
  }
  if (payload.algorithm !== ALGORITHM) {
    throw transportError(`不支持传输加密算法 ${payload.algorithm}，密码未提交`)
  }
  return parseKey(payload.key)
}

/**
 * 取传输密钥：并发调用共享同一个 Promise。
 * 失败不缓存 rejection —— 清空缓存，下次提交可以重新尝试（后端补配密钥后无需刷新页面）。
 */
export function loadTransportKey(): Promise<Uint8Array> {
  if (keyPromise === null) {
    keyPromise = requestKey().catch((error: unknown) => {
      keyPromise = null
      throw error
    })
  }
  return keyPromise
}

/** 清空密钥缓存：后端报解密失败（密钥轮换）或需要强制重取时调用。 */
export function resetTransportKey(): void {
  keyPromise = null
}

/** 后端在密钥轮换后无法解开旧密钥加密的密码时返回的错误码。 */
const DECRYPT_FAILED_CODE = 'MCP_TRANSPORT_DECRYPT_FAILED'

/** 判断错误是否意味着「浏览器持有旧传输密钥」：只有这种错误值得清缓存重试一次。 */
export function isTransportKeyRotation(error: unknown): boolean {
  return error instanceof ApiError && error.code === DECRYPT_FAILED_CODE
}

/**
 * 传输密钥轮换的自动重试：后端返回 MCP_TRANSPORT_DECRYPT_FAILED 时清掉密钥缓存，
 * 让 build 重新取密钥、重新加密并重发；其余错误、以及第二次仍失败的情况原样抛出。
 * 安装与改配置共用这一份实现，页面里不再各写一份重试。
 */
export async function withTransportRetry<T>(build: () => Promise<T>): Promise<T> {
  try {
    return await build()
  } catch (error) {
    if (!isTransportKeyRotation(error)) throw error
    resetTransportKey()
    return build()
  }
}

/** 小写 hex：后端只接受小写，`Uint8Array#toString` 之类不保证格式。 */
function toHex(bytes: Uint8Array): string {
  let hex = ''
  for (const byte of bytes) {
    hex += byte.toString(16).padStart(2, '0')
  }
  return hex
}

/**
 * 加密一个密码字段：输出 `enc:v1:<hex>`，其中 hex 是 nonce(12B) ‖ ciphertext ‖ tag(16B)。
 * AAD 为空；明文按 UTF-8 编码。密钥取不到或校验失败时抛出带「密码未提交」的错误。
 */
export async function encryptSecret(plaintext: string): Promise<string> {
  const key = await loadTransportKey()
  const nonce = randomBytes(NONCE_BYTES)
  const plaintextBytes = new TextEncoder().encode(plaintext)
  let sealed: Uint8Array
  try {
    const encrypted = gcm(key, nonce).encrypt(plaintextBytes)
    sealed = new Uint8Array(nonce.length + encrypted.length)
    sealed.set(nonce, 0)
    sealed.set(encrypted, nonce.length)
  } catch (error) {
    throw transportError(
      '密码加密失败，密码未提交',
      error instanceof Error ? error.message : String(error),
    )
  }
  // gcm 输出的尾部固定 16 字节 tag，长度断言只防实现变更，不参与线格式。
  if (sealed.length !== NONCE_BYTES + TAG_BYTES + plaintextBytes.length) {
    throw transportError('密码加密结果长度异常，密码未提交')
  }
  return `${CIPHERTEXT_PREFIX}${toHex(sealed)}`
}
