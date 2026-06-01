//! Tauri IPC commands for scheduled tasks (cron).

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use echo_agent::agent::Agent;
use echo_agent_app_core::scheduler::task::{CronTask, CronTaskStatus, TaskStore};

/// 获取与 SchedulerRunner 共享的 TaskStore（共享底层 Arc<dyn Store>）
fn get_shared_store(state: &TauriState) -> Result<TaskStore, IpcError> {
    state
        .app_state
        .scheduler
        .runner
        .as_ref()
        .map(|runner| runner.store())
        .ok_or_else(|| IpcError::Internal("Scheduler not initialized".to_string()))
}

#[tauri::command]
pub async fn list_scheduler_tasks(
    state: tauri::State<'_, TauriState>,
) -> Result<serde_json::Value, IpcError> {
    let store = get_shared_store(&state)?;
    let tasks = store
        .load_all()
        .map_err(|e| IpcError::Internal(e.to_string()))?;
    serde_json::to_value(tasks).map_err(|e| IpcError::Internal(e.to_string()))
}

#[tauri::command]
pub async fn add_scheduler_task(
    state: tauri::State<'_, TauriState>,
    name: String,
    cron_expr: String,
    prompt: String,
) -> Result<serde_json::Value, IpcError> {
    let store = get_shared_store(&state)?;
    let task = CronTask::new(&name, &cron_expr, &prompt);
    store
        .add(task)
        .map_err(|e| IpcError::Internal(e.to_string()))?;
    Ok(serde_json::json!({"success": true}))
}

#[tauri::command]
pub async fn remove_scheduler_task(
    state: tauri::State<'_, TauriState>,
    id: String,
) -> Result<serde_json::Value, IpcError> {
    let store = get_shared_store(&state)?;
    match store
        .remove(&id)
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
    let store = get_shared_store(&state)?;
    let s = match status.as_str() {
        "enabled" => CronTaskStatus::Enabled,
        "disabled" => CronTaskStatus::Disabled,
        _ => return Err(IpcError::Validation(format!("Invalid status: {}", status))),
    };
    match store
        .set_status(&id, s)
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
    let store = get_shared_store(&state)?;
    let task = store
        .get(&id)
        .map_err(|e| IpcError::Internal(e.to_string()))?
        .ok_or_else(|| IpcError::NotFound(format!("Task '{}' not found", id)))?;

    let prompt = task.prompt.clone();
    let result = state
        .app_state
        .connection
        .agent
        .read_async(|agent| {
            let prompt = prompt.clone();
            Box::pin(async move { agent.chat(&prompt).await })
        })
        .await
        .map_err(|e| IpcError::Internal(e.to_string()))?;

    store
        .update_last_run(&id, &result)
        .map_err(|e| IpcError::Internal(e.to_string()))?;

    Ok(serde_json::json!({
        "success": true,
        "result": result,
    }))
}
