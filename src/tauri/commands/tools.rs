//! Tauri IPC commands for tool management.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;

#[tauri::command]
pub async fn list_tools(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
) -> Result<Vec<echo_agent_app_core::api::types::ToolInfo>, IpcError> {
    let (_publication, runtime) = state
        .app_state
        .tool_control_runtime_for_scope(&workspace_id, &workspace_generation)
        .await
        .map_err(IpcError::Internal)?;
    let agent = runtime.primary_agent();
    state
        .app_state
        .get_tool_infos(&agent)
        .await
        .map_err(tool_control_error)
}

#[tauri::command]
pub async fn get_tool(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
    name: String,
) -> Result<echo_agent_app_core::api::types::ToolInfo, IpcError> {
    let (_publication, runtime) = state
        .app_state
        .tool_control_runtime_for_scope(&workspace_id, &workspace_generation)
        .await
        .map_err(IpcError::Internal)?;
    let agent = runtime.primary_agent();
    let infos = state
        .app_state
        .get_tool_infos(&agent)
        .await
        .map_err(tool_control_error)?;
    match infos.into_iter().find(|tool| tool.name == name) {
        Some(tool) => Ok(tool),
        None => Err(IpcError::NotFound(format!("Tool '{}' not found", name))),
    }
}

#[tauri::command]
pub async fn enable_tool(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
    name: String,
) -> Result<echo_agent_app_core::api::tool_control::ToolControlReceipt, IpcError> {
    let (_publication, runtime) = state
        .app_state
        .tool_control_runtime_for_scope(&workspace_id, &workspace_generation)
        .await
        .map_err(IpcError::Internal)?;
    let agent = runtime.primary_agent();
    state
        .app_state
        .set_tool_enabled(&agent, &name, true)
        .await
        .map_err(tool_control_error)
}

#[tauri::command]
pub async fn disable_tool(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
    name: String,
) -> Result<echo_agent_app_core::api::tool_control::ToolControlReceipt, IpcError> {
    let (_publication, runtime) = state
        .app_state
        .tool_control_runtime_for_scope(&workspace_id, &workspace_generation)
        .await
        .map_err(IpcError::Internal)?;
    let agent = runtime.primary_agent();
    state
        .app_state
        .set_tool_enabled(&agent, &name, false)
        .await
        .map_err(tool_control_error)
}

fn tool_control_error(error: echo_agent_app_core::api::tool_control::ToolControlError) -> IpcError {
    match error {
        echo_agent_app_core::api::tool_control::ToolControlError::NotRegistered { name } => {
            IpcError::NotFound(format!("Tool '{name}' not found"))
        }
        error => IpcError::Internal(error.to_string()),
    }
}
