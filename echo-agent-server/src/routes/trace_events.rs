//! 追踪事件 API
//!
//! 提供执行轨迹的查询和摘要端点。
//!
//! - `GET /api/trace-events/sessions` — 列出所有 session ID
//! - `GET /api/trace-events/:session_id` — 获取某个 session 的事件
//! - `GET /api/trace-events/:session_id/summary` — 获取某个 session 的统计摘要
//! - `DELETE /api/trace-events/:session_id` — 清除某个 session 的事件

use axum::{
    Json, Router,
    extract::{Path, State},
    routing::get,
};
use std::sync::Arc;

use echo_agent_app_core::observability::{TraceEvent, TraceSummary};
use echo_agent_app_core::state::AppState;

/// GET /api/trace-events/sessions — list all session IDs
async fn list_sessions(State(state): State<Arc<AppState>>) -> Json<Vec<String>> {
    let collector = &state.trace.collector;
    Json(collector.list_sessions().await)
}

/// GET /api/trace-events/:session_id — get events for a session
async fn get_events(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Json<Vec<TraceEvent>> {
    let collector = &state.trace.collector;
    Json(collector.get_events(&session_id).await)
}

/// GET /api/trace-events/:session_id/summary — get summary stats
async fn get_summary(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Result<Json<TraceSummary>, (axum::http::StatusCode, String)> {
    let collector = &state.trace.collector;
    match collector.get_summary(&session_id).await {
        Some(summary) => Ok(Json(summary)),
        None => Err((
            axum::http::StatusCode::NOT_FOUND,
            "Session not found".into(),
        )),
    }
}

/// DELETE /api/trace-events/:session_id — clear session events
async fn clear_session(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Json<serde_json::Value> {
    let collector = &state.trace.collector;
    collector.clear_session(&session_id).await;
    Json(serde_json::json!({ "cleared": session_id }))
}

pub fn trace_event_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/trace-events/sessions", get(list_sessions))
        .route(
            "/api/trace-events/:session_id",
            get(get_events).delete(clear_session),
        )
        .route("/api/trace-events/:session_id/summary", get(get_summary))
}
