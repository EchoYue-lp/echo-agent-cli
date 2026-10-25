//! Thin desktop controls for the single application channel runtime.
use crate::tauri::{error::IpcError, state::TauriState};
use echo_agent_app_core::api::channel_runtime::{
    ChannelConfigurationPatch, ChannelControlAction, ManagedChannel,
};

#[tauri::command]
pub async fn get_channel_configuration(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    state
        .channel_runtime
        .configuration()
        .await
        .map_err(IpcError::Internal)
}
#[tauri::command]
pub async fn save_channel_configuration(
    state: tauri::State<'_, TauriState>,
    patch: ChannelConfigurationPatch,
) -> Result<serde_json::Value, IpcError> {
    state
        .channel_runtime
        .save_configuration(patch)
        .await
        .map_err(IpcError::Internal)
}
#[tauri::command]
pub async fn list_channel_status(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    state
        .channel_runtime
        .status()
        .await
        .map_err(IpcError::Internal)
}
#[tauri::command]
pub async fn control_channel(
    state: tauri::State<'_, TauriState>,
    channel: ManagedChannel,
    expected_revision: String,
    expected_mutation_epoch: Option<String>,
    expected_applied_revision: Option<String>,
    action: ChannelControlAction,
) -> Result<serde_json::Value, IpcError> {
    state
        .channel_runtime
        .control(
            channel,
            expected_revision,
            expected_mutation_epoch,
            expected_applied_revision,
            action,
        )
        .await
        .map_err(IpcError::Internal)
}
