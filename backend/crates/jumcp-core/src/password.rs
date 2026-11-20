//! 管理员口令哈希：Argon2id（`argon2` 默认参数 m=19456KiB / t=2 / p=1）。
//!
//! 为什么算法与参数只出现在这一个文件：哈希串是落库格式的一部分，播种、改密、登录校验
//! 必须用同一套参数才能互相验证；参数一旦改动，旧哈希仍需按自带参数（哈希串编码了参数）校验，
//! 因此调用方只该看到 hash/verify 两个动作。
//!
//! 盐取自 `secret::random_bytes`（与 nonce/会话 id 同一 CSPRNG），不额外引入 RNG 依赖。

use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};

use crate::errors::McpError;
use crate::secret::random_bytes;

/// 盐长度（16 字节：Argon2 推荐下限，盐本身不需要保密，只需按口令唯一）。
const SALT_BYTES: usize = 16;

pub fn hash_password(password: &str) -> Result<String, McpError> {
    let salt_bytes = random_bytes(SALT_BYTES).map_err(|e| McpError::store(e.to_string()))?;
    let salt = SaltString::encode_b64(&salt_bytes)
        .map_err(|e| McpError::store(format!("密码盐编码失败: {e}")))?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|e| McpError::store(format!("密码哈希失败: {e}")))
}

/// 校验口令。哈希串损坏按"不匹配"处理：调用方无法据错误类型做出任何有用的分支，
/// 而区分"库坏了"与"密码错"会给登录口一个探测信号。
///
/// 比较由 `password-hash` 用常量时间完成，不使用 `==`。
pub fn verify_password(hash: &str, password: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_roundtrip_and_rejects_wrong_password() {
        let hash = hash_password("correct horse battery").expect("hash");
        assert!(hash.starts_with("$argon2id$"), "{hash}");
        assert!(!hash.contains("correct horse battery"));
        assert!(verify_password(&hash, "correct horse battery"));
        assert!(!verify_password(&hash, "correct horse batterY"));
        assert!(!verify_password(&hash, ""));
    }

    #[test]
    fn salt_is_per_password() {
        let a = hash_password("same-password-123").expect("hash");
        let b = hash_password("same-password-123").expect("hash");
        assert_ne!(a, b, "同一口令两次哈希必须不同（随机盐）");
    }

    #[test]
    fn malformed_hash_is_a_mismatch() {
        assert!(!verify_password("", "x"));
        assert!(!verify_password("$argon2id$broken", "x"));
    }
}
