//! File-backed analysis workbench IPC.

use echo_agent_app_core::api::analysis::{
    AnalysisDocument, AnalysisError, AnalysisLanguage, AnalysisSummary, SaveAnalysisRequest,
};
use serde_json::Value;

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;

async fn with_analysis_io<T, F>(
    state: &TauriState,
    workspace_id: &str,
    workspace_generation: &str,
    operation: &'static str,
    function: F,
) -> Result<T, IpcError>
where
    T: Send + 'static,
    F: FnOnce(&std::path::Path) -> Result<T, AnalysisError> + Send + 'static,
{
    let control =
        super::product_data::scoped_control(state, workspace_id, workspace_generation).await?;
    control
        .data(operation, function)
        .await
        .map_err(super::product_data::blocking_error)?
        .map_err(ipc_error)
}

fn ipc_error(error: AnalysisError) -> IpcError {
    match error {
        AnalysisError::NotFound(message) => IpcError::NotFound(message),
        AnalysisError::Invalid(message) => IpcError::Validation(message),
        AnalysisError::Conflict => {
            IpcError::Validation("analysis changed on disk; reload before saving".to_string())
        }
        AnalysisError::Io(_)
        | AnalysisError::Json(_)
        | AnalysisError::Execution(_)
        | AnalysisError::RuntimeUnavailable(_) => IpcError::Internal(error.to_string()),
    }
}

fn control_error(
    error: echo_agent_app_core::api::product_data_io::AnalysisRunControlError,
) -> IpcError {
    use echo_agent_app_core::api::product_data_io::AnalysisRunControlError;
    match &error {
        AnalysisRunControlError::SupervisorClosed
        | AnalysisRunControlError::AlreadyRunning { .. }
        | AnalysisRunControlError::NotFound { .. }
        | AnalysisRunControlError::ReceiptMismatch { .. }
        | AnalysisRunControlError::Busy { .. } => IpcError::Validation(error.to_string()),
        AnalysisRunControlError::CleanupFailed { .. } | AnalysisRunControlError::Execution(_) => {
            IpcError::Internal(error.to_string())
        }
    }
}

#[tauri::command]
pub async fn list_analyses(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
) -> Result<Vec<AnalysisSummary>, IpcError> {
    with_analysis_io(
        &state,
        &workspace_id,
        &workspace_generation,
        "list analyses",
        echo_agent_app_core::api::analysis::list_analyses,
    )
    .await
}

#[tauri::command]
pub async fn create_analysis(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
    title: String,
    language: AnalysisLanguage,
) -> Result<AnalysisDocument, IpcError> {
    with_analysis_io(
        &state,
        &workspace_id,
        &workspace_generation,
        "create analysis",
        move |root| echo_agent_app_core::api::analysis::create_analysis(root, &title, language),
    )
    .await
}

#[tauri::command]
pub async fn get_analysis(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
    analysis_id: String,
) -> Result<AnalysisDocument, IpcError> {
    with_analysis_io(
        &state,
        &workspace_id,
        &workspace_generation,
        "load analysis",
        move |root| echo_agent_app_core::api::analysis::load_analysis(root, &analysis_id),
    )
    .await
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn save_analysis(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
    analysis_id: String,
    title: String,
    script: String,
    expected_script_revision: String,
    input_paths: Vec<String>,
    parameters: Value,
    random_seed: Option<u64>,
) -> Result<AnalysisDocument, IpcError> {
    let control =
        super::product_data::scoped_control(&state, &workspace_id, &workspace_generation).await?;
    control
        .save_analysis(
            &analysis_id,
            SaveAnalysisRequest {
                title,
                script,
                expected_script_revision,
                input_paths,
                parameters,
                random_seed,
            },
        )
        .await
        .map_err(control_error)
}

#[tauri::command]
pub async fn run_analysis(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
    analysis_id: String,
) -> Result<AnalysisDocument, IpcError> {
    let control =
        super::product_data::scoped_control(&state, &workspace_id, &workspace_generation).await?;
    let receipt = control
        .start_analysis(&analysis_id)
        .map_err(control_error)?;
    let result = control.wait_analysis(&receipt).await.map_err(control_error);
    drop(control);
    result
}

#[tauri::command]
pub async fn cancel_analysis(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
    analysis_id: String,
) -> Result<echo_agent_app_core::api::product_data_io::AnalysisCancelReceipt, IpcError> {
    let control =
        super::product_data::scoped_control(&state, &workspace_id, &workspace_generation).await?;
    control
        .cancel_analysis(&analysis_id)
        .await
        .map_err(control_error)
}

#[tauri::command]
pub async fn delete_analysis(
    state: tauri::State<'_, TauriState>,
    workspace_id: String,
    workspace_generation: String,
    analysis_id: String,
) -> Result<bool, IpcError> {
    let control =
        super::product_data::scoped_control(&state, &workspace_id, &workspace_generation).await?;
    control
        .delete_analysis(&analysis_id)
        .await
        .map_err(control_error)?;
    Ok(true)
}
