//! 进程内定时静态审计：把"每次会话现扫审计库、现算排名"提前成后台周期任务。
//!
//! 审计 agent 的工作链路因此缩短为一次 `admin_tool_candidates`：默认读到的是已经排好序、
//! 带最近一次调用证据的快照，不必再调 `admin_audit_queries` 回放入参，也不必等窗口扫描。
//! 排名口径与 HTTP `GET /admin/tools/candidates` 完全一致（同一个 `static_audit_report`），
//! 差别只在扫描时刻。
//!
//! 只读、不改审计：快照写进 `Service` 的内存槽位，进程重启后由首次 tick 重新生成；
//! 审计库 itself 仍是 append-only 的唯一事实源。

use crate::service::Service;
use jumcp_core::errors::McpError;
use std::sync::Arc;
use std::time::Duration;

/// 静态审计固定窗口（天）：与统计台账/候选接口的默认值一致，agent 无需传参。
pub const WINDOW_DAYS: i64 = 7;

/// 快照保留的候选条数：只取前若干名，避免把整份候选表塞进会话。
pub const LIMIT: i64 = 20;

/// 跑一轮静态审计并整体替换快照。
///
/// 扫描审计库是阻塞的 SQLite 读取，因此放到 `spawn_blocking`，不占 runtime 工作线程。
/// 调度器与集成测试共用这一入口：测试可以手动刷新，不必等真实定时器。
pub async fn refresh(service: &Arc<Service>) -> Result<(), McpError> {
    let Some(log) = service.audit.clone() else {
        return Ok(());
    };
    let report = tokio::task::spawn_blocking(move || {
        crate::admin::static_audit_report(&log, WINDOW_DAYS, LIMIT)
    })
    .await
    .map_err(|e| McpError::audit(format!("静态审计任务失败: {e}")))??;
    service.set_static_audit(report)?;
    Ok(())
}

/// 启动定时刷新任务；`JUMCP_STATIC_AUDIT_INTERVAL_SECS=0` 时不启动。
///
/// 首轮在第一次 tick 立即执行，因此服务开始接流量后不久快照就绪；刷新失败只记日志并
/// 保留上一份快照——实时接口仍在，不会因为没有快照而失去候选能力。
pub fn spawn(service: Arc<Service>) {
    let secs = service.settings.static_audit_interval_secs;
    if secs <= 0 {
        tracing::info!(
            "JUMCP_STATIC_AUDIT_INTERVAL_SECS=0：静态审计定时任务关闭，候选由调用时实时计算"
        );
        return;
    }
    if service.audit.is_none() {
        tracing::warn!("审计未启用：静态审计定时任务不启动");
        return;
    }
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(secs as u64));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            match refresh(&service).await {
                Ok(()) => tracing::debug!(interval_secs = secs, "静态审计快照已刷新"),
                Err(e) => tracing::warn!(
                    error = %e.message,
                    "静态审计刷新失败，保留上一份快照"
                ),
            }
        }
    });
}
