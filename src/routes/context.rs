//! 上下文状态 API

use axum::{
    debug_handler,
    extract::State,
    response::{IntoResponse, Response},
    Json,
};
use std::sync::Arc;

use crate::state::AppState;
use crate::types::ContextStats;

/// GET /api/context - 获取上下文统计
#[debug_handler]
pub async fn get_context(
    State(state): State<Arc<AppState>>,
) -> Response {
    let agent = state.agent.lock().await;

    // 从 agent 获取实际统计信息
    let (message_count, estimated_tokens) = agent.context_stats();

    Json(ContextStats {
        message_count,
        estimated_tokens,
    }).into_response()
}