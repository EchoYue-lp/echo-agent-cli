//! Tauri IPC commands for scheduled tasks (cron).
//!
//! All commands operate on the framework's `SchedulerRunner` directly —
//! there is no separate `store()` accessor; the runner wraps the store
//! and exposes high-level management methods.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use echo_agent_app_core::scheduler::{CronTask, CronTaskStatus};

/// Helper: borrow the scheduler runner from Tauri state, returning an IPC
/// error if the scheduler was never initialized.
fn get_runner(
    state: &TauriState,
) -> Result<&std::sync::Arc<echo_agent_app_core::scheduler::SchedulerRunner>, IpcError> {
    state
        .app_state
        .scheduler
        .runner
        .as_ref()
        .ok_or_else(|| IpcError::Internal("Scheduler not initialized".to_string()))
}

#[tauri::command]
pub async fn list_scheduler_tasks(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let runner = get_runner(&state)?;
    let tasks = runner.list_tasks().await;
    serde_json::to_value(tasks).map_err(|e| IpcError::Internal(e.to_string()))
}

#[tauri::command]
pub async fn add_scheduler_task(
    state: tauri::State<'_, TauriState>,
    name: String,
    cron_expr: String,
    prompt: String,
) -> Result<serde_json::Value, IpcError> {
    let runner = get_runner(&state)?;
    let task = CronTask::new(&name, &cron_expr, &prompt);
    runner
        .add_task(task)
        .await
        .map_err(|e| IpcError::Internal(e.to_string()))?;
    Ok(serde_json::json!({"success": true}))
}

#[tauri::command]
pub async fn remove_scheduler_task(
    state: tauri::State<'_, TauriState>,
    id: String,
) -> Result<serde_json::Value, IpcError> {
    let runner = get_runner(&state)?;
    match runner
        .remove_task(&id)
        .await
        .map_err(|e| IpcError::Internal(e.to_string()))?
    {
        true => Ok(serde_json::json!({"success": true})),
        false => Err(IpcError::NotFound(format!("Task '{}' not found", id))),
    }
}

#[tauri::command]
pub async fn set_scheduler_task_status(
    state: tauri::State<'_, TauriState>,
    id: String,
    status: String,
) -> Result<serde_json::Value, IpcError> {
    let runner = get_runner(&state)?;
    let s = match status.as_str() {
        "enabled" => CronTaskStatus::Enabled,
        "disabled" => CronTaskStatus::Disabled,
        _ => return Err(IpcError::Validation(format!("Invalid status: {}", status))),
    };
    match runner
        .set_status(&id, s)
        .await
        .map_err(|e| IpcError::Internal(e.to_string()))?
    {
        true => Ok(serde_json::json!({"success": true})),
        false => Err(IpcError::NotFound(format!("Task '{}' not found", id))),
    }
}

#[tauri::command]
pub async fn run_scheduler_task(
    state: tauri::State<'_, TauriState>,
    id: String,
) -> Result<serde_json::Value, IpcError> {
    let runner = get_runner(&state)?;
    let result = runner
        .run_once(&id)
        .await
        .map_err(|e| IpcError::Internal(e.to_string()))?;
    Ok(serde_json::json!({
        "success": true,
        "result": result,
    }))
}
