use super::*;
use crate::config::TenantPolicy;
use crate::entry::Actor;
use crate::registry::CapabilityStore;
use chrono::Utc;
use serde_json::json;

fn store() -> CapabilityStore {
    CapabilityStore::open(
        ":memory:",
        Some(std::sync::Arc::new(
            crate::audit::AuditLog::open(":memory:").unwrap(),
        )),
    )
    .unwrap()
}

fn actor() -> Actor {
    let mut actor = Actor::new("default", "admin");
    actor.actor_id = "alice".into();
    actor.source = "admin-api".into();
    actor
}

fn new_token(name: &str) -> NewAccessToken {
    NewAccessToken {
        name: name.into(),
        role: "analyst".into(),
        groups: vec!["db.read.core".into()],
        source: "agent".into(),
        caller: "agent-analyst".into(),
        expires_at: None,
        tenant: "default".into(),
        created_by: "alice".into(),
    }
}

#[test]
fn create_authenticate_list_revoke_roundtrip() {
    let s = store();
    let issued = s.create_access_token(&new_token("v1"), &actor()).unwrap();
    assert!(issued.token.starts_with("jumcp_"));
    assert_eq!(issued.token.len(), 6 + 64);
    assert_eq!(issued.record.id.len(), 20);
    assert!(issued.record.id.starts_with("tok_"));
    assert_eq!(issued.record.status(), "active");

    let authed = s.authenticate_access_token(&issued.token).unwrap().unwrap();
    assert_eq!(authed.id, issued.record.id);
    assert_eq!(authed.caller, "agent-analyst");
    assert!(
        !authed.last_used_at.is_empty(),
        "鉴权成功必须刷新 last_used_at"
    );

    let list = s.list_access_tokens().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(s.token_count().unwrap(), 1);
    assert_eq!(s.token_counts_by_group().unwrap()["db.read.core"], 1);

    let revoked = s.revoke_access_token(&issued.record.id, &actor()).unwrap();
    assert_eq!(revoked.status(), "revoked");
    assert!(!revoked.enabled);
    assert!(
        s.authenticate_access_token(&issued.token)
            .unwrap()
            .is_none()
    );
    assert_eq!(s.token_count().unwrap(), 0, "已撤销 token 不再计数");

    // 重复撤销：409 语义且不重复写审计。
    let e = s
        .revoke_access_token(&issued.record.id, &actor())
        .unwrap_err();
    assert_eq!(e.code, "MCP_TOKEN_ALREADY_REVOKED");
    let events = s
        .audit_log()
        .unwrap()
        .query(Some(&json!({"event_type": "admin_change"})))
        .unwrap();
    let revokes = events
        .iter()
        .filter(|e| e.payload["action"] == json!("token_revoke"))
        .count();
    assert_eq!(revokes, 1);
    let create = events
        .iter()
        .find(|e| e.payload["action"] == json!("token_create"))
        .expect("token_create");
    assert_eq!(create.actor["source"], json!("admin-api"));
    assert!(create.actor.get("token_id").is_none());
    // 审计载荷绝不含明文/hash。
    let raw = create.payload.to_string();
    assert!(!raw.contains(&issued.token));
    assert!(!raw.contains(&issued.record.token_hash));
    assert_eq!(
        create.payload["data"]["token_prefix"],
        json!(issued.record.token_prefix)
    );
}

#[test]
fn reissue_rotates_secret_and_keeps_metadata() {
    let s = store();
    let mut spec = new_token("rotate-me");
    spec.expires_at = Some((Utc::now() + chrono::Duration::hours(1)).to_rfc3339());
    let issued = s.create_access_token(&spec, &actor()).unwrap();
    assert!(
        s.authenticate_access_token(&issued.token)
            .unwrap()
            .is_some()
    );

    let reissued = s.reissue_access_token(&issued.record.id, &actor()).unwrap();
    assert_ne!(reissued.token, issued.token);
    assert!(reissued.token.starts_with(TOKEN_PLAINTEXT_PREFIX));
    assert_eq!(reissued.record.id, issued.record.id);
    assert_eq!(reissued.record.name, "rotate-me");
    assert_eq!(reissued.record.role, "analyst");
    assert_eq!(reissued.record.groups, vec!["db.read.core".to_string()]);
    assert_eq!(reissued.record.source, "agent");
    assert_eq!(reissued.record.caller, "agent-analyst");
    assert_eq!(reissued.record.expires_at, issued.record.expires_at);
    assert!(
        reissued.record.last_used_at.is_empty(),
        "重签发清空使用时间"
    );
    assert_ne!(reissued.record.token_hash, issued.record.token_hash);
    assert_ne!(reissued.record.token_prefix, issued.record.token_prefix);
    assert_eq!(s.access_token_total().unwrap(), 1, "重签发不新增行");

    assert!(
        s.authenticate_access_token(&issued.token)
            .unwrap()
            .is_none(),
        "旧明文立即失效"
    );
    assert!(
        s.authenticate_access_token(&reissued.token)
            .unwrap()
            .is_some(),
        "新明文立即可用"
    );

    let events = s
        .audit_log()
        .unwrap()
        .query(Some(&json!({"event_type": "admin_change"})))
        .unwrap();
    let reissue = events
        .iter()
        .find(|e| e.payload["action"] == json!("token_reissue"))
        .expect("token_reissue 审计");
    assert_eq!(reissue.actor["source"], json!("admin-api"));
    let raw = reissue.payload.to_string();
    assert!(!raw.contains(&issued.token));
    assert!(!raw.contains(&reissued.token));
    assert!(!raw.contains(&reissued.record.token_hash));
    assert_eq!(
        reissue.payload["data"]["token_prefix"],
        json!(reissued.record.token_prefix)
    );

    // 撤销后不可重签：409 且哈希/前缀不变。
    let revoked = s.revoke_access_token(&issued.record.id, &actor()).unwrap();
    let before = (revoked.token_hash.clone(), revoked.token_prefix.clone());
    let e = s
        .reissue_access_token(&issued.record.id, &actor())
        .unwrap_err();
    assert_eq!(e.code, "MCP_STATE_TRANSITION_INVALID");
    let after = s.get_access_token(&issued.record.id).unwrap().unwrap();
    assert_eq!((after.token_hash, after.token_prefix), before);
}

#[test]
fn expired_token_is_rejected() {
    let s = store();
    let mut spec = new_token("expiring");
    spec.expires_at = Some((Utc::now() + chrono::Duration::seconds(2)).to_rfc3339());
    let issued = s.create_access_token(&spec, &actor()).unwrap();
    assert_eq!(issued.record.status(), "active");
    // 直接改库模拟时间流逝（store 不提供改过期时间的 API）。
    {
        let conn = s.db().lock().unwrap();
        conn.execute(
            "UPDATE access_tokens SET expires_at=? WHERE id=?",
            rusqlite::params![
                (Utc::now() - chrono::Duration::seconds(1)).to_rfc3339(),
                issued.record.id
            ],
        )
        .unwrap();
    }
    assert!(
        s.authenticate_access_token(&issued.token)
            .unwrap()
            .is_none()
    );
    let rec = s.get_access_token(&issued.record.id).unwrap().unwrap();
    assert_eq!(rec.status(), "expired");
    assert_eq!(s.token_count().unwrap(), 1, "过期未撤销仍计入组引用");
}

#[test]
fn bootstrap_imports_only_once_and_tokens_still_work() {
    let s = store();
    let principals = crate::config::AgentTokens::parse(
        "agent-analyst:analyst:tok_agent_analyst_0001,agent-admin:admin:tok_agent_admin_000001",
    )
    .unwrap();
    let mut roles = std::collections::HashMap::new();
    roles.insert(
        "analyst".into(),
        crate::config::RoleDef {
            name: "analyst".into(),
            default_groups: vec!["db.read.core".into()],
            default_budget: 2000,
        },
    );
    roles.insert(
        "admin".into(),
        crate::config::RoleDef {
            name: "admin".into(),
            default_groups: vec!["admin".into()],
            default_budget: 2000,
        },
    );
    let policy = TenantPolicy {
        roles,
        ..Default::default()
    };
    assert_eq!(
        s.import_bootstrap_tokens(principals.principals(), &policy)
            .unwrap(),
        2
    );
    assert_eq!(
        s.import_bootstrap_tokens(principals.principals(), &policy)
            .unwrap(),
        0,
        "重复启动不得重复导入"
    );
    let rec = s
        .authenticate_access_token("tok_agent_admin_000001")
        .unwrap()
        .unwrap();
    assert_eq!(rec.role, "admin");
    assert_eq!(rec.source, "bootstrap");
    assert_eq!(
        rec.groups,
        vec!["admin".to_string()],
        "引导令牌只拿角色的 default_groups，不隐式补 basic"
    );
}

#[test]
fn validation_rejects_bad_fields() {
    type Mutation = Box<dyn Fn(&mut NewAccessToken)>;
    let s = store();
    let cases: Vec<Mutation> = vec![
        Box::new(|t| t.name = "".into()),
        Box::new(|t| t.name = "x".repeat(65)),
        Box::new(|t| t.caller = "BAD".into()),
        Box::new(|t| t.caller = "a".into()),
        Box::new(|t| t.source = "".into()),
        Box::new(|t| t.source = "Agent".into()),
        Box::new(|t| t.groups.clear()),
        Box::new(|t| t.expires_at = Some("not-a-time".into())),
        Box::new(|t| t.expires_at = Some("2020-01-01T00:00:00Z".into())),
    ];
    for mutate in cases {
        let mut spec = new_token("bad");
        mutate(&mut spec);
        let e = spec.validate().unwrap_err();
        assert_eq!(e.code, "MCP_VALIDATION_ERROR", "{spec:?}");
    }
    // 存储层也会再判一次（不依赖 HTTP 层）。
    let e = s
        .create_access_token(
            &{
                let mut bad = new_token("bad");
                bad.source = "Agent".into();
                bad
            },
            &actor(),
        )
        .unwrap_err();
    assert_eq!(e.code, "MCP_VALIDATION_ERROR");
}

/// 授权面只剩 `groups`：改名、换组、改有效期都在同一条记录上完成，密钥与角色不动。
#[test]
fn update_changes_groups_name_and_expiry_without_rotating_secret() {
    let audit = std::sync::Arc::new(crate::audit::AuditLog::open(":memory:").unwrap());
    let s = CapabilityStore::open(":memory:", Some(audit.clone())).unwrap();
    let issued = s
        .create_access_token(&new_token("edit-me"), &actor())
        .unwrap();
    let expires = (Utc::now() + chrono::Duration::hours(2)).to_rfc3339();
    let updated = s
        .update_access_token(
            &issued.record.id,
            Some("renamed"),
            Some(&["basic".to_string(), "admin".to_string()]),
            Some(Some(expires.clone())),
            &actor(),
        )
        .unwrap();
    assert_eq!(updated.name, "renamed");
    assert_eq!(
        updated.groups,
        vec!["basic".to_string(), "admin".to_string()]
    );
    assert_eq!(updated.role, "analyst", "角色不随组编辑改变");
    assert_eq!(updated.token_hash, issued.record.token_hash, "换组不动密钥");
    assert_eq!(updated.expires_at, expires);
    assert!(
        s.authenticate_access_token(&issued.token)
            .unwrap()
            .is_some(),
        "旧密钥在改授权后仍然有效"
    );

    let events = audit
        .query(Some(&json!({"event_type": "admin_change"})))
        .unwrap();
    let update = events
        .iter()
        .find(|event| event.payload["action"] == json!("token_update"))
        .expect("token_update 审计");
    let fields: Vec<&str> = update.payload["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|change| change["field"].as_str().unwrap())
        .collect();
    assert_eq!(fields, vec!["name", "groups", "expires_at"]);

    let e = s
        .update_access_token(&issued.record.id, None, Some(&[]), None, &actor())
        .unwrap_err();
    assert_eq!(e.code, "MCP_VALIDATION_ERROR", "不允许把组清空");

    s.revoke_access_token(&issued.record.id, &actor()).unwrap();
    let e = s
        .update_access_token(&issued.record.id, Some("late"), None, None, &actor())
        .unwrap_err();
    assert_eq!(e.code, "MCP_STATE_TRANSITION_INVALID", "已撤销不可编辑");
}

#[test]
fn create_without_audit_is_rejected_and_persists_nothing() {
    let s = CapabilityStore::open(":memory:", None).unwrap();
    let e = s
        .create_access_token(&new_token("noaudit"), &actor())
        .unwrap_err();
    assert_eq!(e.code, "MCP_AUDIT_ERROR");
    assert_eq!(s.access_token_total().unwrap(), 0);
}
