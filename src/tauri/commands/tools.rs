//! Tauri IPC commands for tool management.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;

#[tauri::command]
pub async fn list_tools(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let infos = state
        .app_state
        .get_tool_infos(&state.app_state.connection.primary_agent())
        .await;
    serde_json::to_value(infos).map_err(|e| IpcError::Internal(e.to_string()))
}

#[tauri::command]
pub async fn get_tool(
    state: tauri::State<'_, TauriState>,
    name: String,
) -> Result<serde_json::Value, IpcError> {
    let infos = state
        .app_state
        .get_tool_infos(&state.app_state.connection.primary_agent())
        .await;
    match infos.iter().find(|t| t.name == name) {
        Some(tool) => serde_json::to_value(tool).map_err(|e| IpcError::Internal(e.to_string())),
        None => Err(IpcError::NotFound(format!("Tool '{}' not found", name))),
    }
}

#[tauri::command]
pub async fn enable_tool(
    state: tauri::State<'_, TauriState>,
    name: String,
) -> Result<serde_json::Value, IpcError> {
    {
        let mut states = state.app_state.session.tool_states.write().await;
        if let Some(s) = states.get_mut(&name) {
            s.enabled = true;
        }
    }
    let infos = state
        .app_state
        .get_tool_infos(&state.app_state.connection.primary_agent())
        .await;
    match infos.iter().find(|t| t.name == name) {
        Some(tool) => serde_json::to_value(tool).map_err(|e| IpcError::Internal(e.to_string())),
        None => Err(IpcError::NotFound(format!("Tool '{}' not found", name))),
    }
}

#[tauri::command]
pub async fn disable_tool(
    state: tauri::State<'_, TauriState>,
    name: String,
) -> Result<serde_json::Value, IpcError> {
    {
        let mut states = state.app_state.session.tool_states.write().await;
        if let Some(s) = states.get_mut(&name) {
            s.enabled = false;
        }
    }
    let infos = state
        .app_state
        .get_tool_infos(&state.app_state.connection.primary_agent())
        .await;
    match infos.iter().find(|t| t.name == name) {
        Some(tool) => serde_json::to_value(tool).map_err(|e| IpcError::Internal(e.to_string())),
        None => Err(IpcError::NotFound(format!("Tool '{}' not found", name))),
    }
}
