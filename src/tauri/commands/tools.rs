//! Tauri IPC commands for tool management.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;

#[tauri::command]
pub async fn list_tools(
    state: tauri::State<'_, TauriState>,
) -> Result<Vec<echo_agent_app_core::api::types::ToolInfo>, IpcError> {
    let runtime = current_tool_runtime(&state).await?;
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
    name: String,
) -> Result<echo_agent_app_core::api::types::ToolInfo, IpcError> {
    let runtime = current_tool_runtime(&state).await?;
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
    name: String,
) -> Result<echo_agent_app_core::api::tool_control::ToolControlReceipt, IpcError> {
    let runtime = current_tool_runtime(&state).await?;
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
    name: String,
) -> Result<echo_agent_app_core::api::tool_control::ToolControlReceipt, IpcError> {
    let runtime = current_tool_runtime(&state).await?;
    let agent = runtime.primary_agent();
    state
        .app_state
        .set_tool_enabled(&agent, &name, false)
        .await
        .map_err(tool_control_error)
}

async fn current_tool_runtime(
    state: &TauriState,
) -> Result<echo_agent_app_core::api::state::ScopedChatRuntime, IpcError> {
    state
        .app_state
        .current_control_runtime()
        .await
        .map_err(|error| IpcError::Internal(error.to_string()))
}

fn tool_control_error(error: echo_agent_app_core::api::tool_control::ToolControlError) -> IpcError {
    match error {
        echo_agent_app_core::api::tool_control::ToolControlError::NotRegistered { name } => {
            IpcError::NotFound(format!("Tool '{name}' not found"))
        }
        error => IpcError::Internal(error.to_string()),
    }
}
