use crate::support::*;
use axum::http::StatusCode;
use jumcp_core::audit::AuditLog;
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn missing_token_is_401_and_audited() {
    let h = harness();
    let (status, body) = call(&h.app, Req::get("/admin/review-queue").without_token()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], json!("MCP_UNAUTHORIZED"));
    let events = h
        .audit
        .query(Some(&json!({"event_type": "policy_decision"})))
        .expect("query");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].payload["error_code"], json!("MCP_UNAUTHORIZED"));
}

#[tokio::test]
async fn wrong_token_is_401() {
    let h = harness();
    let (status, body) = call(&h.app, Req::get("/admin/review-queue").with_token("nope")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], json!("MCP_UNAUTHORIZED"));
}

#[tokio::test]
async fn health_does_not_require_token() {
    let h = harness();
    let (status, body) = call(&h.app, Req::get("/health").without_token()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], json!("ok"));
    assert_eq!(body["audit_chain"], json!("valid"));
    assert_eq!(body["datasources"][0]["name"], json!("main-pg"));
    assert_eq!(body["datasources"][0]["readonly"], json!(true));
}

#[tokio::test]
async fn health_reports_tampered_chain_but_stays_200() {
    let path = std::env::temp_dir().join("jumcp-admin-tampered.sqlite3");
    let _ = std::fs::remove_file(&path);
    let log = Arc::new(AuditLog::open(path.to_str().expect("path")).expect("audit"));
    log.append(audit_event("tool_call")).expect("append");
    log.append(audit_event("tool_call")).expect("append");
    // 模拟字节级篡改：绕过 trigger 改一行 payload（真实攻击者同样只能这样改）。
    {
        let conn = rusqlite::Connection::open(&path).expect("raw");
        conn.execute("DROP TRIGGER trg_audit_no_update", [])
            .expect("drop");
        conn.execute(
            "UPDATE audit_events SET payload='{\"forged\":1}' WHERE rowid=1",
            [],
        )
        .expect("tamper");
    }
    let h = harness_with_audit(log);
    let (status, body) = call(&h.app, Req::get("/health").without_token()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], json!("ok"));
    assert_eq!(body["audit_chain"], json!("tampered"));
    assert!(body["audit_chain_detail"].as_str().is_some());
    let _ = std::fs::remove_file(&path);
}
