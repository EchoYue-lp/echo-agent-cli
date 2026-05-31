//! 会话管理 API

use axum::{
    Json, debug_handler,
    extract::State,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use std::sync::Arc;

use crate::state::AppState;
use crate::types::SessionInfo;
use echo_agent::agent::Agent;
use echo_agent::memory::{ConversationFilter, ConversationStore};

// ── 类型定义 ─────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct SnapshotInfo {
    pub id: String,
    pub iteration: usize,
    pub created_at: u64,
}

// ── API 处理器 ───────────────────────────────────────────────────

/// GET /api/session - 获取当前会话状态
#[cfg_attr(debug_assertions, debug_handler)]
pub async fn get_session(State(state): State<Arc<AppState>>) -> Response {
    state
        .connection
        .agent
        .read_async(|agent| {
            Box::pin(async move {
                let (message_count, _) = agent.context_stats().await;

                Json(SessionInfo {
                    session_id: agent.config().get_session_id().map(|s| s.to_string()),
                    message_count,
                    tool_count: agent.tool_names().len(),
                    skill_count: agent.skill_names().len(),
                    mcp_server_count: agent.mcp_server_names().len(),
                })
                .into_response()
            })
        })
        .await
}

/// POST /api/session/reset - 重置会话
#[cfg_attr(debug_assertions, debug_handler)]
pub async fn reset_session(State(state): State<Arc<AppState>>) -> Response {
    state
        .connection
        .agent
        .write_async(|agent| {
            Box::pin(async move {
                agent.reset().await;

                Json(SessionInfo {
                    session_id: agent.config().get_session_id().map(|s| s.to_string()),
                    message_count: 0,
                    tool_count: agent.tool_names().len(),
                    skill_count: agent.skill_names().len(),
                    mcp_server_count: agent.mcp_server_names().len(),
                })
                .into_response()
            })
        })
        .await
}

/// POST /api/session/checkpoint - 创建快照
pub async fn create_checkpoint(State(state): State<Arc<AppState>>) -> Response {
    let snapshot_id = state
        .connection
        .agent
        .write_async(|agent| Box::pin(async move { agent.snapshot().await }))
        .await;

    match snapshot_id {
        Some(id) => Json(serde_json::json!({
            "success": true,
            "snapshot_id": id
        }))
        .into_response(),
        None => Json(serde_json::json!({
            "success": false,
            "error": "创建快照失败"
        }))
        .into_response(),
    }
}

/// GET /api/session/checkpoints - 列出所有快照
pub async fn list_checkpoints(State(state): State<Arc<AppState>>) -> Response {
    let snapshots: Vec<SnapshotInfo> = state
        .connection
        .agent
        .read(|agent| {
            agent
                .snapshots()
                .iter()
                .map(|s| SnapshotInfo {
                    id: s.id.clone(),
                    iteration: s.iteration,
                    created_at: s.created_at,
                })
                .collect()
        })
        .await;

    Json(snapshots).into_response()
}

/// POST /api/session/restore/:snapshot_id - 恢复到指定快照
pub async fn restore_checkpoint(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(snapshot_id): axum::extract::Path<String>,
) -> Response {
    let sid = snapshot_id.clone();
    let result = state
        .connection
        .agent
        .write_async(|agent| Box::pin(async move { agent.rollback_to(&sid).await }))
        .await;

    match result {
        Some(snapshot) => Json(serde_json::json!({
            "success": true,
            "restored_to": snapshot.id
        }))
        .into_response(),
        None => Json(serde_json::json!({
            "success": false,
            "error": format!("快照 '{}' 未找到", snapshot_id)
        }))
        .into_response(),
    }
}

/// GET /api/session/latest - 获取最近的会话信息
///
/// Returns the most recent conversation from the store so the GUI can
/// offer a "Continue last session" affordance without loading the full list.
pub async fn get_latest_session(
    State(state): State<Arc<AppState>>,
) -> Response {
    let store = match state.storage.conversation_store.as_ref() {
        Some(s) => s,
        None => {
            return Json(serde_json::json!({
                "found": false,
                "error": "Conversation persistence is disabled",
            }))
            .into_response();
        }
    };

    let filter = ConversationFilter {
        limit: Some(1),
        ..Default::default()
    };

    match store.list_conversations(filter).await {
        Ok(metas) if !metas.is_empty() => {
            let latest = &metas[0];
            Json(serde_json::json!({
                "found": true,
                "id": latest.conversation_id,
                "title": latest.title,
                "updated_at": latest.updated_at,
                "message_count": latest.message_count,
            }))
            .into_response()
        }
        Ok(_) => Json(serde_json::json!({
            "found": false,
        }))
        .into_response(),
        Err(e) => Json(serde_json::json!({
            "found": false,
            "error": format!("Failed to query latest session: {e}"),
        }))
        .into_response(),
    }
}
