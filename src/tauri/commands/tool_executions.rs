use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use echo_agent_app_core::tool_execution::{
    DEFAULT_DETAIL_PAGE_BYTES, ToolExecutionDetailManifest, ToolExecutionDetailPage,
    ToolExecutionError, ToolExecutionSummary,
};

fn ipc_error(error: ToolExecutionError) -> IpcError {
    match error {
        ToolExecutionError::NotFound(value) => IpcError::NotFound(value),
        other => IpcError::Internal(other.to_string()),
    }
}

#[tauri::command]
pub async fn get_tool_execution_detail(
    state: tauri::State<'_, TauriState>,
    detail_ref: String,
) -> Result<ToolExecutionDetailManifest, IpcError> {
    state
        .app_state
        .storage
        .tool_executions
        .detail_manifest(&detail_ref)
        .map_err(ipc_error)
}

#[tauri::command]
pub async fn read_tool_execution_output(
    state: tauri::State<'_, TauriState>,
    detail_ref: String,
    cursor: Option<String>,
    limit: Option<usize>,
) -> Result<ToolExecutionDetailPage, IpcError> {
    state
        .app_state
        .storage
        .tool_executions
        .read_output(
            &detail_ref,
            cursor.as_deref(),
            limit.unwrap_or(DEFAULT_DETAIL_PAGE_BYTES),
        )
        .map_err(ipc_error)
}

#[tauri::command]
pub async fn list_tool_executions(
    state: tauri::State<'_, TauriState>,
    conversation_id: String,
) -> Result<Vec<ToolExecutionSummary>, IpcError> {
    Ok(state
        .app_state
        .storage
        .tool_executions
        .summaries_for_conversation(&conversation_id))
}
