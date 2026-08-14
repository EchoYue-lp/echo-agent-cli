//! File-backed analysis workbench IPC.

use std::path::PathBuf;
use std::sync::Arc;

use echo_agent_app_core::analysis::{
    AnalysisDocument, AnalysisError, AnalysisLanguage, AnalysisSummary, SaveAnalysisRequest,
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;

async fn workspace_root(state: &TauriState) -> PathBuf {
    match state.app_state.workspace.current.read().await.as_ref() {
        Some(workspace) => workspace.root.clone(),
        None => {
            let agent = state.app_state.connection.primary_agent();
            echo_agent_app_core::analysis::workspace_root_for_agent(&agent).await
        }
    }
}

fn ipc_error(error: AnalysisError) -> IpcError {
    match error {
        AnalysisError::NotFound(message) => IpcError::NotFound(message),
        AnalysisError::Invalid(message) => IpcError::Validation(message),
        AnalysisError::Conflict => {
            IpcError::Validation("analysis changed on disk; reload before saving".to_string())
        }
        AnalysisError::Io(_) | AnalysisError::Json(_) | AnalysisError::Execution(_) => {
            IpcError::Internal(error.to_string())
        }
    }
}

#[tauri::command]
pub async fn list_analyses(
    state: tauri::State<'_, TauriState>,
) -> Result<Vec<AnalysisSummary>, IpcError> {
    echo_agent_app_core::analysis::list_analyses(&workspace_root(&state).await).map_err(ipc_error)
}

#[tauri::command]
pub async fn create_analysis(
    state: tauri::State<'_, TauriState>,
    title: String,
    language: AnalysisLanguage,
) -> Result<AnalysisDocument, IpcError> {
    echo_agent_app_core::analysis::create_analysis(&workspace_root(&state).await, &title, language)
        .map_err(ipc_error)
}

#[tauri::command]
pub async fn get_analysis(
    state: tauri::State<'_, TauriState>,
    analysis_id: String,
) -> Result<AnalysisDocument, IpcError> {
    echo_agent_app_core::analysis::load_analysis(&workspace_root(&state).await, &analysis_id)
        .map_err(ipc_error)
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn save_analysis(
    state: tauri::State<'_, TauriState>,
    analysis_id: String,
    title: String,
    script: String,
    expected_script_revision: String,
    input_paths: Vec<String>,
    parameters: Value,
    random_seed: Option<u64>,
) -> Result<AnalysisDocument, IpcError> {
    echo_agent_app_core::analysis::save_analysis(
        &workspace_root(&state).await,
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
    .map_err(ipc_error)
}

#[tauri::command]
pub async fn run_analysis(
    state: tauri::State<'_, TauriState>,
    analysis_id: String,
) -> Result<AnalysisDocument, IpcError> {
    let cancel_key = format!("analysis:{analysis_id}");
    let cancel = CancellationToken::new();
    match state
        .app_state
        .session
        .operation_cancel_tokens
        .entry(cancel_key.clone())
    {
        dashmap::mapref::entry::Entry::Occupied(_) => {
            return Err(IpcError::Validation(
                "analysis is already running".to_string(),
            ));
        }
        dashmap::mapref::entry::Entry::Vacant(entry) => {
            entry.insert(cancel.clone());
        }
    }

    let root = workspace_root(&state).await;
    let agent = state.app_state.connection.primary_agent();
    let result = echo_agent_app_core::analysis::run_analysis_with_agent(
        &agent,
        &root,
        &analysis_id,
        Some(Arc::new(cancel)),
    )
    .await
    .map_err(ipc_error);
    state
        .app_state
        .session
        .operation_cancel_tokens
        .remove(&cancel_key);
    result
}

#[tauri::command]
pub async fn cancel_analysis(
    state: tauri::State<'_, TauriState>,
    analysis_id: String,
) -> Result<bool, IpcError> {
    let key = format!("analysis:{analysis_id}");
    let cancelled = state
        .app_state
        .session
        .operation_cancel_tokens
        .get(&key)
        .map(|token| {
            token.cancel();
            true
        })
        .unwrap_or(false);
    Ok(cancelled)
}
