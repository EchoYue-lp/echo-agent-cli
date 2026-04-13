//! 审计日志 API

use axum::{extract::State, Json};
use serde::Serialize;
use std::sync::Arc;

use crate::error::AppError;
use crate::state::{AuditLogEntry, AppState};

// ── 类型定义 ─────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct AuditStats {
    pub total_entries: usize,
    pub allow_count: usize,
    pub deny_count: usize,
    pub ask_count: usize,
}

// ── API 处理器 ───────────────────────────────────────────────────

/// GET /api/audit/logs
pub async fn get_audit_logs(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<AuditLogEntry>>, AppError> {
    let logs = state.audit_logs.read().unwrap();
    Ok(Json(logs.clone()))
}

/// GET /api/audit/stats
pub async fn get_audit_stats(
    State(state): State<Arc<AppState>>,
) -> Result<Json<AuditStats>, AppError> {
    let logs = state.audit_logs.read().unwrap();
    let total_entries = logs.len();
    let allow_count = logs.iter().filter(|l| l.decision == "allow").count();
    let deny_count = logs.iter().filter(|l| l.decision == "deny").count();
    let ask_count = logs.iter().filter(|l| l.decision == "ask").count();

    Ok(Json(AuditStats {
        total_entries,
        allow_count,
        deny_count,
        ask_count,
    }))
}

/// DELETE /api/audit/logs
pub async fn clear_audit_logs(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, AppError> {
    tracing::info!("清空审计日志");
    let mut logs = state.audit_logs.write().unwrap();
    let count = logs.len();
    logs.clear();
    Ok(Json(serde_json::json!({
        "success": true,
        "cleared": count
    })))
}
