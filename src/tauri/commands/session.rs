//! Tauri IPC commands for session management.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use echo_agent::agent::Agent;
use echo_agent::memory::ConversationFilter;
use echo_agent_app_core::api::types::SessionInfo;
use serde::Serialize;

async fn session_agent(
    state: &TauriState,
    workspace_id: &str,
    conversation_id: &str,
) -> Result<echo_agent_app_core::api::agent_pool::AgentPoolExecutionLease, IpcError> {
    if conversation_id.trim().is_empty() {
        return Err(IpcError::Validation(
            "conversation_id must not be empty".to_string(),
        ));
    }
    let runtime = state
        .app_state
        .chat_runtime_for_scope(workspace_id)
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    runtime
        .agent_for(conversation_id)
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))
}

fn ensure_session_mutation_idle(
    state: &TauriState,
    workspace_id: &str,
    conversation_id: &str,
) -> Result<(), IpcError> {
    let active = state
        .app_state
        .session
        .foreground_turns
        .snapshots_for_conversation_scoped(workspace_id, conversation_id)
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    if active.is_empty() {
        Ok(())
    } else {
        Err(IpcError::Validation(format!(
            "conversation '{conversation_id}' has an active foreground turn"
        )))
    }
}

#[derive(Debug, Serialize)]
pub struct SnapshotInfo {
    pub id: String,
    pub iteration: usize,
    pub created_at: u64,
}

#[tauri::command]
pub async fn get_session(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    conversation_id: String,
) -> Result<SessionInfo, IpcError> {
    let execution = session_agent(&state, &workspace_id, &conversation_id).await?;
    Ok(execution
        .agent()
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
pub async fn reset_session(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    conversation_id: String,
) -> Result<SessionInfo, IpcError> {
    ensure_session_mutation_idle(&state, &workspace_id, &conversation_id)?;
    let execution = session_agent(&state, &workspace_id, &conversation_id).await?;
    Ok(execution
        .agent()
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
    workspace_id: String,
    conversation_id: String,
) -> Result<serde_json::Value, IpcError> {
    ensure_session_mutation_idle(&state, &workspace_id, &conversation_id)?;
    let execution = session_agent(&state, &workspace_id, &conversation_id).await?;
    let snapshot_id = execution
        .agent()
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
    workspace_id: String,
    conversation_id: String,
) -> Result<Vec<SnapshotInfo>, IpcError> {
    let execution = session_agent(&state, &workspace_id, &conversation_id).await?;
    Ok(execution
        .agent()
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
    workspace_id: String,
    conversation_id: String,
    snapshot_id: String,
) -> Result<serde_json::Value, IpcError> {
    ensure_session_mutation_idle(&state, &workspace_id, &conversation_id)?;
    let execution = session_agent(&state, &workspace_id, &conversation_id).await?;
    let sid = snapshot_id.clone();
    let result = execution
        .agent()
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
    workspace_id: String,
) -> Result<serde_json::Value, IpcError> {
    let runtime = state
        .app_state
        .chat_runtime_for_scope(&workspace_id)
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))?;
    let store = match runtime.conversation_store() {
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
