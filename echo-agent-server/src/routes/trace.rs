//! Trace observability API
//!
//! REST endpoints for session-level trace analytics:
//! - `GET /api/trace/sessions` — list all sessions with trace data
//! - `GET /api/trace/session/:id` — detailed summary for a session
//! - `GET /api/trace/stats` — aggregate tool/token/error stats

use axum::{
    Json,
    extract::{Path, Query, State},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::error::AppError;
use crate::state::AppState;
use echo_agent::trace::analyzer::{
    ErrorPattern, SessionSummary, TokenBreakdown, ToolUsageStats, TraceAnalyzer,
};

// ── Query parameters ───────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct TraceQuery {
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct SlowToolQuery {
    pub threshold_ms: Option<u64>,
    pub limit: Option<usize>,
}

// ── Response types ─────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct TraceStatsResponse {
    pub tool_usage: Vec<ToolUsageStats>,
    pub token_breakdown: TokenBreakdown,
    pub error_patterns: Vec<ErrorPattern>,
}

// ── API handlers ───────────────────────────────────────────────────────

/// GET /api/trace/sessions — list all sessions that have trace data
pub async fn list_trace_sessions(
    Query(query): Query<TraceQuery>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<String>>, AppError> {
    let limit = query.limit.unwrap_or(100);
    let analyzer = state.trace.analyzer.read().await;
    let a = analyzer
        .as_ref()
        .ok_or_else(|| AppError::Internal("Trace analyzer not initialized".to_string()))?;
    let sessions = a
        .list_sessions(limit)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(Json(sessions))
}

/// GET /api/trace/session/:id — detailed summary for a specific session
pub async fn get_trace_session(
    Path(session_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Response {
    let analyzer = state.trace.analyzer.read().await;
    match analyzer.as_ref() {
        Some(a) => match a.summarize_session(&session_id).await {
            Ok(summary) => Json(summary).into_response(),
            Err(e) => Json(serde_json::json!({
                "error": e.to_string()
            }))
                .into_response(),
        },
        None => Json(serde_json::json!({
            "error": "Trace analyzer not initialized"
        }))
            .into_response(),
    }
}

/// GET /api/trace/stats — aggregate tool usage, token breakdown, and error
/// patterns
pub async fn get_trace_stats(
    Query(query): Query<TraceQuery>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<TraceStatsResponse>, AppError> {
    let limit = query.limit.unwrap_or(100);
    let analyzer = state.trace.analyzer.read().await;
    let a = analyzer
        .as_ref()
        .ok_or_else(|| AppError::Internal("Trace analyzer not initialized".to_string()))?;

    let tool_usage = a
        .tool_usage_stats(limit)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let token_breakdown = a
        .token_usage_breakdown(limit)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let error_patterns = a
        .error_pattern_analysis(limit)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    Ok(Json(TraceStatsResponse {
        tool_usage,
        token_breakdown,
        error_patterns,
    }))
}