use echo_agent_app_core::api::product_data_io::ScopedProductData;

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;

pub(super) async fn scoped_control(
    state: &TauriState,
    workspace_id: &str,
    workspace_generation: &str,
) -> Result<ScopedProductData, IpcError> {
    state
        .app_state
        .product_data_for_scope(workspace_id, workspace_generation)
        .await
        .map_err(|error| IpcError::Validation(error.to_string()))
}

pub(super) fn blocking_error(
    error: echo_agent_app_core::api::product_data_io::ProductDataIoError,
) -> IpcError {
    IpcError::Internal(error.to_string())
}
