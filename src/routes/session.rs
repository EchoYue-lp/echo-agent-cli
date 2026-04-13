//! 会话管理 API

use axum::{
    debug_handler,
    extract::State,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use std::sync::Arc;
use echo_agent::agent::Agent;

use crate::state::AppState;
use crate::types::SessionInfo;

// ── 类型定义 ─────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct CheckpointInfo {
    pub id: String,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
pub struct SnapshotInfo {
    pub id: String,
    pub iteration: usize,
    pub created_at: u64,
}

// ── API 处理器 ───────────────────────────────────────────────────

/// GET /api/session - 获取当前会话状态
#[debug_handler]
pub async fn get_session(
    State(state): State<Arc<AppState>>,
) -> Response {
    let agent = state.agent.lock().await;
    let (message_count, _) = agent.context_stats();

    Json(SessionInfo {
        session_id: agent.config().get_session_id().map(|s| s.to_string()),
        message_count,
        tool_count: agent.tool_names().len(),
        skill_count: agent.skill_names().len(),
        mcp_server_count: agent.mcp_server_names().len(),
    }).into_response()
}

/// POST /api/session/reset - 重置会话
#[debug_handler]
pub async fn reset_session(
    State(state): State<Arc<AppState>>,
) -> Response {
    let mut agent = state.agent.lock().await;
    agent.reset();

    Json(SessionInfo {
        session_id: agent.config().get_session_id().map(|s| s.to_string()),
        message_count: 0,
        tool_count: agent.tool_names().len(),
        skill_count: agent.skill_names().len(),
        mcp_server_count: agent.mcp_server_names().len(),
    }).into_response()
}

/// POST /api/session/checkpoint - 创建快照
pub async fn create_checkpoint(
    State(state): State<Arc<AppState>>,
) -> Response {
    let mut agent = state.agent.lock().await;
    let snapshot_id = agent.snapshot();

    match snapshot_id {
        Some(id) => Json(serde_json::json!({
            "success": true,
            "snapshot_id": id
        })).into_response(),
        None => Json(serde_json::json!({
            "success": false,
            "error": "创建快照失败"
        })).into_response(),
    }
}

/// GET /api/session/checkpoints - 列出所有快照
pub async fn list_checkpoints(
    State(state): State<Arc<AppState>>,
) -> Response {
    let agent = state.agent.lock().await;
    let snapshots: Vec<SnapshotInfo> = agent.snapshots().iter().map(|s| {
        SnapshotInfo {
            id: s.id.clone(),
            iteration: s.iteration,
            created_at: s.created_at,
        }
    }).collect();

    Json(snapshots).into_response()
}

/// POST /api/session/restore/:snapshot_id - 恢复到指定快照
pub async fn restore_checkpoint(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(snapshot_id): axum::extract::Path<String>,
) -> Response {
    let mut agent = state.agent.lock().await;
    let result = agent.rollback_to(&snapshot_id);

    match result {
        Some(snapshot) => Json(serde_json::json!({
            "success": true,
            "restored_to": snapshot.id
        })).into_response(),
        None => Json(serde_json::json!({
            "success": false,
            "error": format!("快照 '{}' 未找到", snapshot_id)
        })).into_response(),
    }
}