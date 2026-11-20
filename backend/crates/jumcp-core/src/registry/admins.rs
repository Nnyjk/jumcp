//! 平台管理员账号：管理台账号密码登录的唯一身份来源。
//!
//! 为什么是表而不是配置：账号要能在管理台增删改，且改密/禁用必须能连锁吊销该账号的会话；
//! 配置文件里的共享令牌既分不清"谁是谁"，也无法逐个吊销。
//!
//! 口令只以 Argon2id 哈希（`crate::password`）落库，本模块的输入是明文口令、
//! 输出永不含明文——哈希动作放在存储层，避免调用方遗漏。

use rusqlite::{OptionalExtension, Result as SqlResult, Row, params};

use super::{CapabilityStore, now_iso};
use crate::errors::McpError;
use crate::password::{hash_password, verify_password};
use crate::secret::random_hex;

pub const ADMIN_USERNAME_MIN: usize = 3;
pub const ADMIN_USERNAME_MAX: usize = 32;
pub const ADMIN_PASSWORD_MIN_CHARS: usize = 12;
pub const ADMIN_STATE_ACTIVE: &str = "active";
pub const ADMIN_STATE_DISABLED: &str = "disabled";

/// 首次启动必须提供初始管理员；缺失或不合规共用这一句，运维只需知道"去设两个环境变量"。
pub const BOOTSTRAP_ADMIN_REQUIRED: &str = "JUMCP_ADMIN_USERNAME/JUMCP_ADMIN_PASSWORD 未设置：首次启动必须提供初始管理员账号（密码至少 12 位）";

/// 账号 id 的随机字节数（hex 后 32 字符，与令牌 id 同风格）。
const ADMIN_USER_ID_BYTES: usize = 16;

#[derive(Clone, PartialEq, Eq)]
pub struct AdminUser {
    pub id: String,
    pub username: String,
    pub password_hash: String,
    pub state: String,
    pub created_at: String,
    pub updated_at: String,
    pub last_login_at: Option<String>,
}

/// 哈希是凭据：`Debug` 里脱敏，避免随错误日志落盘。
impl std::fmt::Debug for AdminUser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdminUser")
            .field("id", &self.id)
            .field("username", &self.username)
            .field("state", &self.state)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .field("last_login_at", &self.last_login_at)
            .finish_non_exhaustive()
    }
}

impl AdminUser {
    pub fn is_active(&self) -> bool {
        self.state == ADMIN_STATE_ACTIVE
    }

    /// 对外视图：不含密码哈希（管理面所有响应都从这里出）。
    pub fn view(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "username": self.username,
            "state": self.state,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
            "last_login_at": self.last_login_at,
        })
    }

    /// 校验口令；禁用账号一律不通过（不区分"密码错"与"已禁用"给调用方）。
    pub fn authenticate(&self, password: &str) -> bool {
        self.is_active() && verify_password(&self.password_hash, password)
    }
}

/// 用户名规范：3–32 位 ASCII 小写字母/数字/`._-`，存储前统一转小写。
pub fn normalize_admin_username(raw: &str) -> Result<String, McpError> {
    let name = raw.trim().to_ascii_lowercase();
    let ok_len = (ADMIN_USERNAME_MIN..=ADMIN_USERNAME_MAX).contains(&name.len());
    let ok_chars = name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'));
    if !ok_len || !ok_chars {
        return Err(McpError::validation(format!(
            "username 需 {ADMIN_USERNAME_MIN}–{ADMIN_USERNAME_MAX} 位 [a-z0-9._-]：{raw}"
        )));
    }
    Ok(name)
}

pub fn validate_admin_password(password: &str) -> Result<(), McpError> {
    if password.chars().count() < ADMIN_PASSWORD_MIN_CHARS {
        return Err(McpError::validation(format!(
            "password 至少 {ADMIN_PASSWORD_MIN_CHARS} 位"
        )));
    }
    Ok(())
}

fn normalize_state(raw: &str) -> Result<String, McpError> {
    match raw {
        ADMIN_STATE_ACTIVE | ADMIN_STATE_DISABLED => Ok(raw.to_string()),
        other => Err(McpError::validation(format!(
            "state 只能是 active 或 disabled：{other}"
        ))),
    }
}

fn map_user(row: &Row<'_>) -> SqlResult<AdminUser> {
    Ok(AdminUser {
        id: row.get("id")?,
        username: row.get("username")?,
        password_hash: row.get("password_hash")?,
        state: row.get("state")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        last_login_at: row.get("last_login_at")?,
    })
}

const SELECT_USER: &str =
    "SELECT id, username, password_hash, state, created_at, updated_at, last_login_at";

impl CapabilityStore {
    /// 首次启动播种：`admin_users` 为空时必须提供合规的初始管理员。
    ///
    /// 已有账号时返回 `None` 且不看环境变量：重启不该再要求配置一次口令。
    pub fn seed_initial_admin(
        &self,
        username: &str,
        password: &str,
    ) -> Result<Option<AdminUser>, String> {
        if self.admin_user_count().map_err(|e| e.message)? > 0 {
            return Ok(None);
        }
        // 缺失、用户名非法、口令过短是同一件事：首个启动没有可用的初始管理员。
        if normalize_admin_username(username).is_err() || validate_admin_password(password).is_err()
        {
            return Err(BOOTSTRAP_ADMIN_REQUIRED.to_string());
        }
        let user = self
            .create_admin_user(username, password)
            .map_err(|e| e.message)?;
        Ok(Some(user))
    }

    pub fn create_admin_user(&self, username: &str, password: &str) -> Result<AdminUser, McpError> {
        let username = normalize_admin_username(username)?;
        validate_admin_password(password)?;
        let now = now_iso();
        let user = AdminUser {
            id: random_hex(ADMIN_USER_ID_BYTES).map_err(|e| McpError::store(e.to_string()))?,
            username,
            password_hash: hash_password(password)?,
            state: ADMIN_STATE_ACTIVE.to_string(),
            created_at: now.clone(),
            updated_at: now,
            last_login_at: None,
        };
        let conn = self.db().lock().map_err(|_| McpError::lock())?;
        let inserted = conn.execute(
            "INSERT INTO admin_users (id, username, password_hash, state, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                user.id,
                user.username,
                user.password_hash,
                user.state,
                user.created_at,
                user.updated_at
            ],
        );
        match inserted {
            Ok(_) => Ok(user),
            // username 有 UNIQUE 约束；其余约束（主键）在 128 位随机 id 下视为存储错误。
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                Err(McpError::new(
                    "MCP_ADMIN_USER_EXISTS",
                    serde_json::json!({"username": user.username}),
                ))
            }
            Err(e) => Err(McpError::store(e.to_string())),
        }
    }

    pub fn list_admin_users(&self) -> Result<Vec<AdminUser>, McpError> {
        let conn = self.db().lock().map_err(|_| McpError::lock())?;
        let mut stmt = conn
            .prepare(&format!(
                "{SELECT_USER} FROM admin_users ORDER BY created_at, username"
            ))
            .map_err(|e| McpError::store(e.to_string()))?;
        let rows = stmt
            .query_map([], map_user)
            .map_err(|e| McpError::store(e.to_string()))?;
        rows.collect::<SqlResult<Vec<_>>>()
            .map_err(|e| McpError::store(e.to_string()))
    }

    pub fn admin_user(&self, id: &str) -> Result<Option<AdminUser>, McpError> {
        let conn = self.db().lock().map_err(|_| McpError::lock())?;
        conn.query_row(
            &format!("{SELECT_USER} FROM admin_users WHERE id = ?1"),
            params![id],
            map_user,
        )
        .optional()
        .map_err(|e| McpError::store(e.to_string()))
    }

    pub fn admin_user_by_username(&self, username: &str) -> Result<Option<AdminUser>, McpError> {
        let conn = self.db().lock().map_err(|_| McpError::lock())?;
        conn.query_row(
            &format!("{SELECT_USER} FROM admin_users WHERE username = ?1"),
            params![username.trim().to_ascii_lowercase()],
            map_user,
        )
        .optional()
        .map_err(|e| McpError::store(e.to_string()))
    }

    pub fn admin_user_count(&self) -> Result<i64, McpError> {
        let conn = self.db().lock().map_err(|_| McpError::lock())?;
        conn.query_row("SELECT COUNT(*) FROM admin_users", [], |r| r.get(0))
            .map_err(|e| McpError::store(e.to_string()))
    }

    pub fn active_admin_count(&self) -> Result<i64, McpError> {
        let conn = self.db().lock().map_err(|_| McpError::lock())?;
        conn.query_row(
            "SELECT COUNT(*) FROM admin_users WHERE state = ?1",
            params![ADMIN_STATE_ACTIVE],
            |r| r.get(0),
        )
        .map_err(|e| McpError::store(e.to_string()))
    }

    /// 局部更新账号。改密或禁用会在同一事务里吊销该账号的全部会话——
    /// 否则被改掉的旧口令仍能凭已签发的 cookie 继续用，改密等于没改。
    pub fn update_admin_user(
        &self,
        id: &str,
        username: Option<&str>,
        password: Option<&str>,
        state: Option<&str>,
    ) -> Result<AdminUser, McpError> {
        let username = username.map(normalize_admin_username).transpose()?;
        if let Some(password) = password {
            validate_admin_password(password)?;
        }
        let state = state.map(normalize_state).transpose()?;
        let conn = self.db().lock().map_err(|_| McpError::lock())?;
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| McpError::store(e.to_string()))?;
        let current = tx
            .query_row(
                &format!("{SELECT_USER} FROM admin_users WHERE id = ?1"),
                params![id],
                map_user,
            )
            .optional()
            .map_err(|e| McpError::store(e.to_string()))?
            .ok_or_else(|| {
                McpError::new("MCP_ADMIN_USER_NOT_FOUND", serde_json::json!({"id": id}))
            })?;

        let disabled = state.as_deref() == Some(ADMIN_STATE_DISABLED);
        if disabled && current.is_active() {
            let active: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM admin_users WHERE state = ?1 AND id <> ?2",
                    params![ADMIN_STATE_ACTIVE, id],
                    |r| r.get(0),
                )
                .map_err(|e| McpError::store(e.to_string()))?;
            if active == 0 {
                return Err(McpError::new(
                    "MCP_ADMIN_LAST_ACTIVE",
                    serde_json::json!({"id": id}),
                ));
            }
        }

        let next_password_hash = password
            .map(hash_password)
            .transpose()?
            .unwrap_or_else(|| current.password_hash.clone());
        let revoke_sessions = password.is_some() || disabled;
        let updated = AdminUser {
            username: username.unwrap_or_else(|| current.username.clone()),
            password_hash: next_password_hash,
            state: state.unwrap_or_else(|| current.state.clone()),
            updated_at: now_iso(),
            ..current
        };
        let written = tx.execute(
            "UPDATE admin_users SET username = ?1, password_hash = ?2, state = ?3, updated_at = ?4
             WHERE id = ?5",
            params![
                updated.username,
                updated.password_hash,
                updated.state,
                updated.updated_at,
                updated.id
            ],
        );
        match written {
            Ok(_) => {}
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                return Err(McpError::new(
                    "MCP_ADMIN_USER_EXISTS",
                    serde_json::json!({"username": updated.username}),
                ));
            }
            Err(e) => return Err(McpError::store(e.to_string())),
        }
        if revoke_sessions {
            tx.execute(
                "DELETE FROM admin_sessions WHERE admin_user_id = ?1",
                params![id],
            )
            .map_err(|e| McpError::store(e.to_string()))?;
        }
        tx.commit().map_err(|e| McpError::store(e.to_string()))?;
        Ok(updated)
    }

    /// 删除账号并吊销其会话（"最后一名启用管理员"由调用方在同一次判定里拦截）。
    pub fn delete_admin_user(&self, id: &str) -> Result<(), McpError> {
        let conn = self.db().lock().map_err(|_| McpError::lock())?;
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| McpError::store(e.to_string()))?;
        let current = tx
            .query_row(
                &format!("{SELECT_USER} FROM admin_users WHERE id = ?1"),
                params![id],
                map_user,
            )
            .optional()
            .map_err(|e| McpError::store(e.to_string()))?
            .ok_or_else(|| {
                McpError::new("MCP_ADMIN_USER_NOT_FOUND", serde_json::json!({"id": id}))
            })?;
        if current.is_active() {
            let other_active: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM admin_users WHERE state = ?1 AND id <> ?2",
                    params![ADMIN_STATE_ACTIVE, id],
                    |r| r.get(0),
                )
                .map_err(|e| McpError::store(e.to_string()))?;
            if other_active == 0 {
                return Err(McpError::new(
                    "MCP_ADMIN_LAST_ACTIVE",
                    serde_json::json!({"id": id}),
                ));
            }
        }
        tx.execute("DELETE FROM admin_users WHERE id = ?1", params![id])
            .map_err(|e| McpError::store(e.to_string()))?;
        tx.execute(
            "DELETE FROM admin_sessions WHERE admin_user_id = ?1",
            params![id],
        )
        .map_err(|e| McpError::store(e.to_string()))?;
        tx.commit().map_err(|e| McpError::store(e.to_string()))?;
        Ok(())
    }

    /// 登录成功后记录时间戳（只动 `last_login_at`，不污染 `updated_at` 的"账号配置变更"语义）。
    pub fn mark_admin_login(&self, id: &str) -> Result<(), McpError> {
        let conn = self.db().lock().map_err(|_| McpError::lock())?;
        conn.execute(
            "UPDATE admin_users SET last_login_at = ?1 WHERE id = ?2",
            params![now_iso(), id],
        )
        .map_err(|e| McpError::store(e.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> CapabilityStore {
        CapabilityStore::open(":memory:", None).expect("store")
    }

    #[test]
    fn create_stores_only_hashes_and_normalizes_username() {
        let store = store();
        let user = store
            .create_admin_user("  Alice.Admin ", "twelve-chars-x")
            .expect("create");
        assert_eq!(user.username, "alice.admin");
        assert_eq!(user.state, ADMIN_STATE_ACTIVE);
        assert!(user.password_hash.starts_with("$argon2id$"));
        assert!(!user.password_hash.contains("twelve-chars-x"));
        assert!(user.authenticate("twelve-chars-x"));
        assert!(!user.authenticate("twelve-chars-Y"));
        assert_eq!(store.admin_user_count().unwrap(), 1);
        let found = store
            .admin_user_by_username("ALICE.ADMIN")
            .unwrap()
            .expect("NOCASE 查找");
        assert_eq!(found.id, user.id);
        store.mark_admin_login(&user.id).unwrap();
        assert!(
            store
                .admin_user(&user.id)
                .unwrap()
                .unwrap()
                .last_login_at
                .is_some()
        );
    }

    #[test]
    fn rejects_bad_username_and_weak_password() {
        let store = store();
        assert!(store.create_admin_user("ab", "twelve-chars-x").is_err());
        assert!(
            store
                .create_admin_user("has space", "twelve-chars-x")
                .is_err()
        );
        assert!(store.create_admin_user("alice", "short").is_err());
        assert_eq!(store.admin_user_count().unwrap(), 0);
    }

    #[test]
    fn duplicate_username_is_conflict_under_nocase() {
        let store = store();
        store.create_admin_user("alice", "twelve-chars-x").unwrap();
        let err = store
            .create_admin_user("ALICE", "twelve-chars-y")
            .expect_err("重名");
        assert_eq!(err.code, "MCP_ADMIN_USER_EXISTS");
    }

    #[test]
    fn seed_only_when_empty_and_requires_credentials() {
        let store = store();
        assert_eq!(
            store.seed_initial_admin("", "").unwrap_err(),
            BOOTSTRAP_ADMIN_REQUIRED
        );
        assert_eq!(
            store.seed_initial_admin("alice", "short").unwrap_err(),
            BOOTSTRAP_ADMIN_REQUIRED
        );
        let seeded = store
            .seed_initial_admin("alice", "twelve-chars-x")
            .unwrap()
            .expect("首次播种");
        assert_eq!(seeded.username, "alice");
        // 表非空后环境变量不再参与：返回 None，不重建。
        assert!(
            store
                .seed_initial_admin("bob", "twelve-chars-y")
                .unwrap()
                .is_none()
        );
        assert_eq!(store.admin_user_count().unwrap(), 1);
    }

    /// 落库的只能是 Argon2id 哈希：口令明文连文件字节里都找不到
    /// （降级成"某处还能 diff 出明文"的话，这个测试会挂）。
    #[test]
    fn persisted_database_contains_no_plaintext_password() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("registry.sqlite3");
        let path_str = path.to_string_lossy().to_string();
        let store = CapabilityStore::open(&path_str, None).expect("store");
        store
            .seed_initial_admin("alice", "alice-correct-horse")
            .expect("seed")
            .expect("首次播种");
        let hash: String = {
            let conn = store.db().lock().expect("lock");
            conn.query_row("SELECT password_hash FROM admin_users", [], |r| r.get(0))
                .expect("hash")
        };
        assert!(hash.starts_with("$argon2id$"), "{hash}");
        drop(store);
        let bytes = std::fs::read(&path).expect("db bytes");
        let needle = b"alice-correct-horse";
        assert!(
            !bytes.windows(needle.len()).any(|w| w == needle),
            "库里不得出现口令明文"
        );
    }

    #[test]
    fn last_active_admin_cannot_be_disabled_or_deleted() {
        let store = store();
        let solo = store.create_admin_user("solo", "twelve-chars-x").unwrap();
        assert_eq!(
            store
                .update_admin_user(&solo.id, None, None, Some(ADMIN_STATE_DISABLED))
                .unwrap_err()
                .code,
            "MCP_ADMIN_LAST_ACTIVE"
        );
        assert_eq!(
            store.delete_admin_user(&solo.id).unwrap_err().code,
            "MCP_ADMIN_LAST_ACTIVE"
        );
        let second = store.create_admin_user("second", "twelve-chars-y").unwrap();
        store
            .update_admin_user(&solo.id, None, None, Some(ADMIN_STATE_DISABLED))
            .unwrap();
        // 现在 second 是唯一启用的账号，不能删。
        assert_eq!(
            store.delete_admin_user(&second.id).unwrap_err().code,
            "MCP_ADMIN_LAST_ACTIVE"
        );
        store.delete_admin_user(&solo.id).unwrap();
        assert_eq!(store.active_admin_count().unwrap(), 1);
    }

    #[test]
    fn password_change_revokes_sessions_and_rehashes() {
        let store = store();
        let user = store.create_admin_user("alice", "twelve-chars-x").unwrap();
        let session = store
            .create_admin_session("sess-alice", &user.id, 60)
            .expect("session");
        let updated = store
            .update_admin_user(&user.id, None, Some("twelve-chars-y"), None)
            .expect("改密");
        assert!(updated.authenticate("twelve-chars-y"));
        assert!(!updated.authenticate("twelve-chars-x"));
        assert_ne!(updated.password_hash, user.password_hash);
        assert!(store.admin_session(&session.id).unwrap().is_none());
    }

    #[test]
    fn rename_to_taken_username_is_conflict() {
        let store = store();
        store.create_admin_user("alice", "twelve-chars-x").unwrap();
        let bob = store.create_admin_user("bob", "twelve-chars-x").unwrap();
        assert_eq!(
            store
                .update_admin_user(&bob.id, Some("alice"), None, None)
                .unwrap_err()
                .code,
            "MCP_ADMIN_USER_EXISTS"
        );
        assert_eq!(
            store
                .update_admin_user("missing", Some("carol"), None, None)
                .unwrap_err()
                .code,
            "MCP_ADMIN_USER_NOT_FOUND"
        );
    }
}
