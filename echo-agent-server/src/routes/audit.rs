//! 审计日志 API

use axum::{Json, extract::{Query, State}};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::error::AppError;
use crate::state::{AppState, AuditDecision, AuditLogEntry};

// ── 类型定义 ─────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct AuditStats {
    pub total_entries: usize,
    pub allow_count: usize,
    pub deny_count: usize,
    pub ask_count: usize,
}

#[derive(Debug, Deserialize)]
pub struct AuditQuery {
    pub offset: Option<usize>,
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct AuditLogsResponse {
    pub logs: Vec<AuditLogEntry>,
    pub total: usize,
    pub offset: usize,
    pub limit: usize,
}

// ── API 处理器 ───────────────────────────────────────────────────

/// GET /api/audit/logs?offset=0&limit=100
pub async fn get_audit_logs(
    Query(query): Query<AuditQuery>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<AuditLogsResponse>, AppError> {
    let offset = query.offset.unwrap_or(0);
    let limit = query.limit.unwrap_or(100).min(1000);
    let total = state.audit_log_count().await;
    let logs = state.get_audit_logs_paged(offset, limit).await;
    Ok(Json(AuditLogsResponse {
        logs,
        total,
        offset,
        limit,
    }))
}

/// GET /api/audit/stats
pub async fn get_audit_stats(
    State(state): State<Arc<AppState>>,
) -> Result<Json<AuditStats>, AppError> {
    let logs = state.get_audit_logs().await;
    let total_entries = logs.len();
    let allow_count = logs
        .iter()
        .filter(|l| l.decision == AuditDecision::Allow)
        .count();
    let deny_count = logs
        .iter()
        .filter(|l| l.decision == AuditDecision::Deny)
        .count();
    let ask_count = logs
        .iter()
        .filter(|l| l.decision == AuditDecision::Ask)
        .count();

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
    let count = state.clear_audit_entries().await;
    Ok(Json(serde_json::json!({
        "success": true,
        "cleared": count
    })))
}
