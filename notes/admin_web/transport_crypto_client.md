# 密码字段的浏览器传输加密（admin-web）

配套契约：`GET /admin/transport-key`、后端 `MCP_TRANSPORT_CIPHERTEXT_REQUIRED` / `MCP_TRANSPORT_KEY_MISSING`。
实现：`admin-web/src/lib/transport.ts` + `admin-web/src/api/admin.ts#fetchTransportKey`。

## 为什么不用 WebCrypto（`crypto.subtle`）

`crypto.subtle` 只在**安全上下文**可用：HTTPS、`localhost`、`127.0.0.1`。本功能要覆盖的恰恰是
「纯 HTTP 内网 IP」部署（管理面没上 TLS 的内网环境），那里 `crypto.subtle === undefined`，管理台会在
提交密码时直接抛错——不是降级，而是彻底不可用。因此改用 `@noble/ciphers`（2.4.0，纯 JS、零依赖、
已审计），并且**不自写 AES/GCM**。

代价值得记录：包体增加（gzip 后约十几 KB）、GCM 由 JS 实现（非恒定时间的纯 JS 路径，但输入是短密码，
且攻击者无法在浏览器本机测量——加密对象是管理员自己输入的密码）。安全性上，传输密钥由后端同鉴权
接口下发，浏览器只负责「别把明文放进请求体」，真正的信任边界仍在后端解密 + 落库加密。

## 线格式（与后端逐字节对齐）

```
enc:v1: + hex(nonce ‖ ciphertext ‖ tag)
```

- 算法 AES-256-GCM；密钥 = `GET /admin/transport-key` 返回的 64 位小写 hex 解出的 32 字节。
- nonce：12 字节随机（`randomBytes(12)`，每次加密重新取，绝不复用）。
- AAD：空（不传）。
- 明文：UTF-8（`TextEncoder`）。
- tag：16 字节，`gcm().encrypt()` 的尾部已包含，直接拼在 ciphertext 之后。
- hex：小写，两位一字节（`padStart(2,'0')`），后端据此解析。
- 空串**不加密也不提交**：安装路径空值不带该 key，编辑路径空值表示保持原值。
- 非密字段保持明文，不做任何加密。
- 密文字段只认 `isSecretField(field) === field.secret === true`：`kind`（`text` / `password`）只决定控件外观，
  不参与「是否需要加密」的判定，避免前后端对同一字段出现两套事实来源。

## 密钥生命周期

- 模块级 `keyPromise` 缓存：并发提交共享同一次 `GET /admin/transport-key`。
- 失败**不缓存 rejection**：`.catch` 里清空 `keyPromise` 再抛出，后端补配密钥后重试无需刷新页面。
- 只驻留内存，不写 `localStorage` / `sessionStorage`，不进日志；页面刷新后自然重新拉取。
- 响应校验：`algorithm` 必须是 `AES-256-GCM`，`key` 必须匹配 `/^[0-9a-f]{64}$/`，否则拒绝提交。
- 密钥轮换：后端在密钥更换后解不开旧密文，返回 `MCP_TRANSPORT_DECRYPT_FAILED`。两个表单的提交都由
  `withTransportRetry(build)` 包裹：命中该错误码时 `resetTransportKey()` 清缓存，重新取密钥、重新加密并
  **自动重试一次**；其他错误与第二次仍失败的情况原样上抛，不循环重试。轮换后的旧缓存只在当前页面
  生命周期内有效，刷新页面同样会重新拉取。
- 已落库密文的轮换属于后端 `JUMCP_SECRET_KEY` 语义（旧密文不可解，需重填密码），与传输密钥无关。

## 失败语义

`encryptSecret()` 抛出的错误信息带固定前缀「…，密码未提交」，表单沿用 `noticeFromError` 展示：

- 取不到密钥 → `无法获取传输密钥，密码未提交：<后端 message>`（如 503 `MCP_TRANSPORT_KEY_MISSING`）。
- 算法/key 格式不符 → `传输密钥格式不正确，密码未提交`。
- 加密异常 → `密码加密失败，密码未提交：<原因>`。

这些错误发生在 `mutationFn` 内、`installMcpCatalogEntry` / `updateMcpInstallConfig` 之前，因此
**不会发出任何安装/改配置请求**；按钮的 `mutation.isPending` 在 `await` 期间保持禁用。
