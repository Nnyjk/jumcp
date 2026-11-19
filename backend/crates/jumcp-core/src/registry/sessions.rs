//! 管理台会话：cookie 里只放不透明随机 id，身份由 `admin_user_id` 指回管理员账号。
//!
//! 为什么落 registry 库而不是进程内存：会话要跨重启存活（重启就把运维踢下线没有安全收益），
//! 过期与吊销也必须能被同一份审计看见。id 是 32 字节 CSPRNG 的 hex，由登录面生成后交给存储层——
//! 存储层不持有"凭据该多随机"的策略，调用方（含测试）才能固定/复现具体 cookie 值。
//! 按账号批量吊销（改密、禁用、删除账号）走 `admins` 的同一事务，不再经由本模块。

use super::CapabilityStore;
use crate::config::constant_time_eq;
use crate::errors::McpError;
use rusqlite::{OptionalExtension, params};

pub const ADMIN_SESSION_MAX_TTL_SECS: i64 = 30 * 24 * 3600;

/// 会话 id 的随机字节数（hex 后 64 字符）；登录面据此生成 cookie 值。
pub const ADMIN_SESSION_ID_BYTES: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminSession {
    pub id: String,
    pub admin_user_id: String,
    pub created_at: i64,
    pub expires_at: i64,
}

fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

impl CapabilityStore {
    /// 建立会话并顺手清理过期行：登录是低频写点，适合做会话表 GC。
    pub fn create_admin_session(
        &self,
        id: &str,
        admin_user_id: &str,
        ttl_secs: i64,
    ) -> Result<AdminSession, McpError> {
        if id.is_empty() || admin_user_id.trim().is_empty() {
            return Err(McpError::store(
                "会话缺少 id 或账号 id（管理面身份派生失败）".to_string(),
            ));
        }
        let ttl = ttl_secs.clamp(1, ADMIN_SESSION_MAX_TTL_SECS);
        let now = now_unix();
        let session = AdminSession {
            id: id.to_string(),
            admin_user_id: admin_user_id.to_string(),
            created_at: now,
            expires_at: now + ttl,
        };
        let conn = self.db().lock().map_err(|_| McpError::lock())?;
        conn.execute(
            "DELETE FROM admin_sessions WHERE expires_at <= ?1",
            params![now],
        )
        .map_err(|e| McpError::store(e.to_string()))?;
        conn.execute(
            "INSERT INTO admin_sessions (id, admin_user_id, created_at, expires_at, last_seen_at) VALUES (?1,?2,?3,?4,?5)",
            params![session.id, session.admin_user_id, session.created_at, session.expires_at, now],
        )
        .map_err(|e| McpError::store(e.to_string()))?;
        Ok(session)
    }

    /// 用 cookie 值换会话：过期即 `None`（不在这里删除，删除留给登录时的 GC 与本轮吊销）。
    ///
    /// 查询按主键取出后仍做一次常量时间比较：不把"是否命中"的时间差暴露给探测者。
    pub fn admin_session(&self, presented: &str) -> Result<Option<AdminSession>, McpError> {
        if presented.is_empty() {
            return Ok(None);
        }
        let conn = self.db().lock().map_err(|_| McpError::lock())?;
        let row = conn
            .query_row(
                "SELECT id, admin_user_id, created_at, expires_at FROM admin_sessions WHERE id = ?1",
                params![presented],
                |r| {
                    Ok(AdminSession {
                        id: r.get(0)?,
                        admin_user_id: r.get(1)?,
                        created_at: r.get(2)?,
                        expires_at: r.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(|e| McpError::store(e.to_string()))?;
        Ok(row.filter(|s| constant_time_eq(&s.id, presented) && s.expires_at > now_unix()))
    }

    /// 刷新 last_seen_at（会话有效期内每次管理面请求一次轻写）。
    pub fn touch_admin_session(&self, id: &str) -> Result<(), McpError> {
        let conn = self.db().lock().map_err(|_| McpError::lock())?;
        conn.execute(
            "UPDATE admin_sessions SET last_seen_at = ?1 WHERE id = ?2",
            params![now_unix(), id],
        )
        .map_err(|e| McpError::store(e.to_string()))?;
        Ok(())
    }

    /// 吊销会话（幂等：不存在也返回 Ok，无数可报）。
    pub fn delete_admin_session(&self, id: &str) -> Result<(), McpError> {
        if id.is_empty() {
            return Ok(());
        }
        let conn = self.db().lock().map_err(|_| McpError::lock())?;
        conn.execute("DELETE FROM admin_sessions WHERE id = ?1", params![id])
            .map_err(|e| McpError::store(e.to_string()))?;
        Ok(())
    }

    /// 会话数（测试与运维排查用：只暴露规模，不暴露 id）。
    pub fn admin_session_count(&self) -> Result<i64, McpError> {
        let conn = self.db().lock().map_err(|_| McpError::lock())?;
        conn.query_row("SELECT COUNT(*) FROM admin_sessions", [], |r| r.get(0))
            .map_err(|e| McpError::store(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> CapabilityStore {
        CapabilityStore::open(":memory:", None).expect("store")
    }

    #[test]
    fn create_lookup_and_delete_roundtrip() {
        let store = store();
        let session = store
            .create_admin_session("sess-1", "user-1", 60)
            .expect("create");
        assert_eq!(store.admin_session_count().unwrap(), 1);
        let found = store.admin_session(&session.id).unwrap().expect("命中");
        assert_eq!(found.admin_user_id, "user-1");
        store.touch_admin_session(&session.id).unwrap();
        store.delete_admin_session(&session.id).unwrap();
        assert!(store.admin_session(&session.id).unwrap().is_none());
        // 幂等：重复吊销不报错。
        store.delete_admin_session(&session.id).unwrap();
        assert_eq!(store.admin_session_count().unwrap(), 0);
    }

    #[test]
    fn expired_session_is_not_returned() {
        let store = store();
        let session = store
            .create_admin_session("sess-1", "user-1", 1)
            .expect("create");
        {
            let conn = store.db().lock().unwrap();
            conn.execute(
                "UPDATE admin_sessions SET expires_at = ?1 WHERE id = ?2",
                params![now_unix() - 1, session.id],
            )
            .unwrap();
        }
        assert!(store.admin_session(&session.id).unwrap().is_none());
    }

    #[test]
    fn unknown_or_tampered_id_is_rejected() {
        let store = store();
        let session = store
            .create_admin_session("sess-1", "user-1", 60)
            .expect("create");
        assert!(store.admin_session("").unwrap().is_none());
        assert!(store.admin_session("deadbeef").unwrap().is_none());
        let mut tampered = session.id.clone();
        tampered.push('0');
        assert!(store.admin_session(&tampered).unwrap().is_none());
    }

    #[test]
    fn login_purges_expired_rows() {
        let store = store();
        let stale = store
            .create_admin_session("sess-old", "user-1", 1)
            .expect("create");
        {
            let conn = store.db().lock().unwrap();
            conn.execute(
                "UPDATE admin_sessions SET expires_at = ?1",
                params![now_unix() - 5],
            )
            .unwrap();
        }
        store
            .create_admin_session("sess-new", "user-2", 60)
            .expect("create");
        assert_eq!(store.admin_session_count().unwrap(), 1);
        assert!(store.admin_session(&stale.id).unwrap().is_none());
    }

    #[test]
    fn ttl_is_clamped_and_empty_token_rejected() {
        let store = store();
        assert!(store.create_admin_session("", "user-1", 60).is_err());
        assert!(store.create_admin_session("sess-1", " ", 60).is_err());
        let session = store
            .create_admin_session("sess-1", "user-1", 0)
            .expect("create");
        assert!(session.expires_at > session.created_at);
    }
}
