#!/usr/bin/env node
/**
 * 传输加密夹具校验：用 @noble/ciphers 复算 smoke 镜像脚本里写死的 SMOKE_PASS_CIPHERTEXT，
 * 两边不一致说明前端实现、后端约定或 scripts/smoke-image.sh 夹具漂移了。
 *
 * 夹具参数（与 scripts/smoke-image.sh 注释一致）：
 *   key = 32 字节 0x44（即 64 个 4 的 hex），nonce = 000102030405060708090a0b，
 *   明文 "smoke-pass"，AAD 空，线格式 enc:v1: + hex(nonce ‖ ciphertext ‖ tag)。
 */
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { gcm } from '@noble/ciphers/aes.js'

const SMOKE_SCRIPT = fileURLToPath(new URL('../../scripts/smoke-image.sh', import.meta.url))
const FIXTURE_NAME = 'SMOKE_PASS_CIPHERTEXT'
const KEY = new Uint8Array(32).fill(0x44)
const NONCE = fromHex('000102030405060708090a0b')
const PLAINTEXT = 'smoke-pass'

function fromHex(hex) {
  const bytes = new Uint8Array(hex.length / 2)
  for (let index = 0; index < bytes.length; index += 1) {
    bytes[index] = Number.parseInt(hex.slice(index * 2, index * 2 + 2), 16)
  }
  return bytes
}

function toHex(bytes) {
  let hex = ''
  for (const byte of bytes) hex += byte.toString(16).padStart(2, '0')
  return hex
}

function fail(message) {
  console.error(`✗ ${message}`)
  process.exit(1)
}

const shell = readFileSync(SMOKE_SCRIPT, 'utf8')
const match = shell.match(/(?:^|\n)\s*SMOKE_PASS_CIPHERTEXT='([^']+)'/)
if (match === null) {
  fail(`在 ${SMOKE_SCRIPT} 里找不到 ${FIXTURE_NAME}='…' 常量，夹具缺失或被改名`)
}
const expected = match[1]

const sealed = gcm(KEY, NONCE).encrypt(new TextEncoder().encode(PLAINTEXT))
const concatenated = new Uint8Array(NONCE.length + sealed.length)
concatenated.set(NONCE, 0)
concatenated.set(sealed, NONCE.length)
const actual = `enc:v1:${toHex(concatenated)}`

if (actual !== expected) {
  fail(`${FIXTURE_NAME} 与前端加密实现不一致\n  夹具: ${expected}\n  复算: ${actual}`)
}
console.log(`✓ ${FIXTURE_NAME} 复算一致：${actual}`)
