//! Tauri IPC commands for session management.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use echo_agent::agent::Agent;
use echo_agent::memory::{ConversationFilter, ConversationStore};
use echo_agent_app_core::types::SessionInfo;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct SnapshotInfo {
    pub id: String,
    pub iteration: usize,
    pub created_at: u64,
}

#[tauri::command]
pub async fn get_session(state: tauri::State<'_, TauriState>) -> Result<SessionInfo, IpcError> {
    Ok(state
        .app_state
        .connection
        .agent
        .read_async(|agent| {
            Box::pin(async move {
                let (message_count, _) = agent.context_stats().await;
                SessionInfo {
                    session_id: agent.config().get_session_id().map(|s| s.to_string()),
                    message_count,
                    tool_count: agent.tool_names().len(),
                    skill_count: agent.skill_names().len(),
                    mcp_server_count: agent.mcp_server_names().len(),
                }
            })
        })
        .await)
}

#[tauri::command]
pub async fn reset_session(state: tauri::State<'_, TauriState>) -> Result<SessionInfo, IpcError> {
    Ok(state
        .app_state
        .connection
        .agent
        .write_async(|agent| {
            Box::pin(async move {
                agent.reset().await;
                SessionInfo {
                    session_id: agent.config().get_session_id().map(|s| s.to_string()),
                    message_count: 0,
                    tool_count: agent.tool_names().len(),
                    skill_count: agent.skill_names().len(),
                    mcp_server_count: agent.mcp_server_names().len(),
                }
            })
        })
        .await)
}

#[tauri::command]
pub async fn create_checkpoint(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let snapshot_id = state
        .app_state
        .connection
        .agent
        .write_async(|agent| Box::pin(async move { agent.snapshot().await }))
        .await;

    match snapshot_id {
        Some(id) => Ok(serde_json::json!({
            "success": true,
            "snapshot_id": id,
        })),
        None => Err(IpcError::Internal("创建快照失败".to_string())),
    }
}

#[tauri::command]
pub async fn list_checkpoints(
    state: tauri::State<'_, TauriState>,
) -> Result<Vec<SnapshotInfo>, IpcError> {
    Ok(state
        .app_state
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
        .await)
}

#[tauri::command]
pub async fn restore_checkpoint(
    state: tauri::State<'_, TauriState>,
    snapshot_id: String,
) -> Result<serde_json::Value, IpcError> {
    let sid = snapshot_id.clone();
    let result = state
        .app_state
        .connection
        .agent
        .write_async(|agent| Box::pin(async move { agent.rollback_to(&sid).await }))
        .await;

    match result {
        Some(snapshot) => Ok(serde_json::json!({
            "success": true,
            "restored_to": snapshot.id,
        })),
        None => Err(IpcError::NotFound(format!("快照 '{}' 未找到", snapshot_id))),
    }
}

#[tauri::command]
pub async fn get_latest_session(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let store_guard = state.app_state.storage.conversation_store.read().await;
    let store = match store_guard.as_ref() {
        Some(s) => s,
        None => {
            return Ok(serde_json::json!({
                "found": false,
                "error": "Conversation persistence is disabled",
            }));
        }
    };

    let filter = ConversationFilter {
        limit: Some(1),
        ..Default::default()
    };

    match store.list_conversations(filter).await {
        Ok(metas) if !metas.is_empty() => {
            let latest = &metas[0];
            Ok(serde_json::json!({
                "found": true,
                "id": latest.conversation_id,
                "title": latest.title,
                "updated_at": latest.updated_at,
                "message_count": latest.message_count,
            }))
        }
        Ok(_) => Ok(serde_json::json!({ "found": false })),
        Err(e) => Ok(serde_json::json!({
            "found": false,
            "error": format!("Failed to query latest session: {e}"),
        })),
    }
}
