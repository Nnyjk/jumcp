//! 配置密钥的加密存取：AES-256-GCM，落库格式 `enc:v1:<hex(nonce12 || ciphertext+tag)>`。
//!
//! 为什么带格式前缀：迁移前的明文 env 仍在库里，读侧必须能区分"已加密"与"历史明文"；
//! 前缀让 `is_encrypted` 无需密钥即可判断，将来换算法可加 v2 而不动调用方。
//! 解密失败必须显式报错：静默退化成空串会把"密钥轮换过"伪装成"密码为空"。

use aes_gcm::aead::rand_core::RngCore;
use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Nonce};
use std::fmt;
use thiserror::Error;

/// 密文前缀；版本号属于落库格式的一部分。
pub const SECRET_PREFIX: &str = "enc:v1:";
const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum SecretError {
    #[error("JUMCP_SECRET_KEY 必须是 64 位十六进制（32 字节）")]
    InvalidKey,
    #[error("密文格式不合法")]
    InvalidCiphertext,
    #[error("密文解密失败（密钥已更换或数据被篡改）")]
    DecryptFailed,
    #[error("系统随机数不可用")]
    RandomFailed,
}

/// 进程级配置密钥。Debug 不输出密钥字节，避免它随 `Settings` 的日志泄漏。
#[derive(Clone)]
pub struct SecretKey([u8; KEY_LEN]);

impl SecretKey {
    /// 解析 64 位 hex 密钥（首尾空白忽略）。
    pub fn parse(raw: &str) -> Result<Self, SecretError> {
        let decoded = decode_hex(raw.trim()).ok_or(SecretError::InvalidKey)?;
        let bytes: [u8; KEY_LEN] = decoded.try_into().map_err(|_| SecretError::InvalidKey)?;
        Ok(Self(bytes))
    }

    /// 密钥的 64 位小写 hex。
    ///
    /// 只给"鉴权后下发传输密钥"这一个用途（`GET /admin/transport-key`）：浏览器必须拿到
    /// 同一把密钥才能加密 secret 字段。**不得**写进日志、审计或错误响应——那等于把密钥
    /// 本身泄露给任何能读日志的人。
    pub fn to_hex(&self) -> String {
        encode_hex(&self.0)
    }

    /// 加密一段明文，返回可直接落库的 `enc:v1:` 字符串。
    pub fn encrypt(&self, plaintext: &str) -> Result<String, SecretError> {
        let cipher = self.cipher();
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let sealed = cipher
            .encrypt(&nonce, plaintext.as_bytes())
            .map_err(|_| SecretError::DecryptFailed)?;
        let mut bytes = Vec::with_capacity(NONCE_LEN + sealed.len());
        bytes.extend_from_slice(nonce.as_slice());
        bytes.extend_from_slice(&sealed);
        Ok(format!("{SECRET_PREFIX}{}", encode_hex(&bytes)))
    }

    /// 解密 `enc:v1:` 值；历史明文原样返回（迁移前的 env 仍然可读）。
    pub fn decrypt(&self, value: &str) -> Result<String, SecretError> {
        let Some(encoded) = value.strip_prefix(SECRET_PREFIX) else {
            return Ok(value.to_string());
        };
        let bytes = decode_hex(encoded).ok_or(SecretError::InvalidCiphertext)?;
        if bytes.len() < NONCE_LEN + TAG_LEN {
            return Err(SecretError::InvalidCiphertext);
        }
        let (nonce, ciphertext) = bytes.split_at(NONCE_LEN);
        let plaintext = self
            .cipher()
            .decrypt(Nonce::from_slice(nonce), ciphertext)
            .map_err(|_| SecretError::DecryptFailed)?;
        String::from_utf8(plaintext).map_err(|_| SecretError::DecryptFailed)
    }

    fn cipher(&self) -> Aes256Gcm {
        // KEY_LEN 固定 32，new_from_slice 不会失败。
        Aes256Gcm::new_from_slice(&self.0).expect("AES-256 密钥长度固定合法")
    }
}

impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretKey(<redacted>)")
    }
}

/// 取 `len` 字节系统随机数（会话 id、密码盐等凭据材料共用同一随机源）。
///
/// 与 nonce 共用 `OsRng`，不额外引入 RNG 依赖。
pub fn random_bytes(len: usize) -> Result<Vec<u8>, SecretError> {
    let mut bytes = vec![0u8; len];
    OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|_| SecretError::RandomFailed)?;
    Ok(bytes)
}

/// 取 `len` 字节系统随机数的 hex（内置端点的 Bearer token 用）。
pub fn random_hex(len: usize) -> Result<String, SecretError> {
    Ok(encode_hex(&random_bytes(len)?))
}

/// 值是否已是密文（无需密钥）。
pub fn is_encrypted(value: &str) -> bool {
    value.starts_with(SECRET_PREFIX)
}

fn encode_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 0x0f) as usize] as char);
    }
    out
}

fn decode_hex(raw: &str) -> Option<Vec<u8>> {
    if !raw.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(raw.len() / 2);
    let bytes = raw.as_bytes();
    for pair in bytes.chunks(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

    fn key() -> SecretKey {
        SecretKey::parse(KEY).expect("fixed key")
    }

    #[test]
    fn roundtrip_and_prefix() {
        let key = key();
        let sealed = key.encrypt("p@ss").expect("encrypt");
        assert!(sealed.starts_with(SECRET_PREFIX));
        assert!(is_encrypted(&sealed));
        assert_eq!(key.decrypt(&sealed).expect("decrypt"), "p@ss");
        // 随机 nonce：同一明文两次密文不同。
        assert_ne!(sealed, key.encrypt("p@ss").expect("encrypt"));
    }

    #[test]
    fn plaintext_passthrough() {
        let key = key();
        assert!(!is_encrypted("plain"));
        assert_eq!(key.decrypt("plain").expect("passthrough"), "plain");
    }

    #[test]
    fn wrong_key_fails_loudly() {
        let sealed = key().encrypt("p@ss").expect("encrypt");
        let other =
            SecretKey::parse("ff0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f")
                .expect("fixed key");
        assert!(matches!(
            other.decrypt(&sealed),
            Err(SecretError::DecryptFailed)
        ));
    }

    #[test]
    fn rejects_bad_keys_and_bad_ciphertext() {
        assert!(matches!(
            SecretKey::parse("abc"),
            Err(SecretError::InvalidKey)
        ));
        assert!(matches!(
            SecretKey::parse(&"zz".repeat(32)),
            Err(SecretError::InvalidKey)
        ));
        assert!(matches!(
            key().decrypt("enc:v1:zz"),
            Err(SecretError::InvalidCiphertext)
        ));
        assert!(matches!(
            key().decrypt("enc:v1:0011"),
            Err(SecretError::InvalidCiphertext)
        ));
    }
}
