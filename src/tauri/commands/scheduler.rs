//! Tauri IPC commands for scheduled tasks (cron).
//!
//! All commands operate on the framework's `SchedulerRunner` directly —
//! there is no separate `store()` accessor; the runner wraps the store
//! and exposes high-level management methods.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use echo_agent_app_core::api::scheduler::{CronTask, CronTaskStatus};

/// Helper: borrow the scheduler runner from Tauri state, returning an IPC
/// error if the scheduler was never initialized.
fn get_runner(
    state: &TauriState,
) -> Result<&std::sync::Arc<echo_agent_app_core::api::scheduler::SchedulerRunner>, IpcError> {
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
    let runner = get_runner(&state)?.clone();
    let flow = state
        .app_state
        .session
        .product_data_io
        .begin_owned_flow("manual scheduler occurrence")
        .map_err(|error| IpcError::Internal(error.to_string()))?;
    let result = flow
        .run_async("execute original manual occurrence", async move {
            runner.run_once_with_receipt(&id).await
        })
        .await
        .map_err(|error| IpcError::Internal(error.to_string()))?;
    let occurrence = match &result {
        Ok(receipt) => Some(receipt.occurrence.clone()),
        Err(error) => error.occurrence.as_deref().cloned(),
    };
    let store = state.app_state.tasks.runtime.clone();
    let response = flow
        .run("project original manual receipt", move || {
            let mut value =
                echo_agent_app_core::api::scheduler::management::manual_receipt_view(result);
            let run = occurrence.as_ref().map(|occurrence| {
                echo_agent_app_core::api::scheduler::management::occurrence_run_view(
                    store.as_deref(),
                    occurrence,
                )
            });
            if let Some(object) = value.as_object_mut() {
                object.insert("run_lookup".to_string(), serde_json::json!(run));
            }
            value
        })
        .await;
    flow.settle(response.as_ref().err().map(ToString::to_string));
    response.map_err(|error| IpcError::Internal(error.to_string()))
}

#[tauri::command]
pub async fn edit_scheduler_task(
    state: tauri::State<'_, TauriState>,
    expected: CronTask,
    edit: echo_agent::scheduler::CronTaskEdit,
) -> Result<serde_json::Value, IpcError> {
    let runner = get_runner(&state)?;
    Ok(match runner.edit_task(expected, edit).await {
        Ok(task) => serde_json::json!({"success":true,"task":task}),
        Err(error) => echo_agent_app_core::api::scheduler::management::edit_error_view(error),
    })
}

#[tauri::command]
pub async fn read_scheduler_history(
    state: tauri::State<'_, TauriState>,
    cursor: Option<String>,
    limit: Option<usize>,
) -> Result<serde_json::Value, IpcError> {
    let runner = get_runner(&state)?.clone();
    let store = state.app_state.tasks.runtime.clone();
    let cursor = match cursor
        .map(|cursor| {
            serde_json::from_str::<echo_agent::scheduler::SchedulerHistoryCursor>(&cursor)
        })
        .transpose()
    {
        Ok(cursor) => cursor,
        Err(_) => {
            return Ok(
                echo_agent_app_core::api::scheduler::management::history_error_view(
                    echo_agent::scheduler::SchedulerHistoryReadError::InvalidCursor,
                ),
            );
        }
    };
    state.app_state.session.product_data_io.run("read original scheduler history", move || {
        match runner.read_occurrence_history(cursor, limit) {
            Ok(page) => {
                // Only Persisted facts provide a complete captured occurrence.
                // Later events remain original typed events, never inferred completions.
                let runs = page.events.iter().filter_map(|record| {
                    if let echo_agent::scheduler::SchedulerHistoryEvent::Persisted { envelope, .. } = &record.event {
                        let occurrence = echo_agent::scheduler::SchedulerOccurrenceRef {
                            task_id: envelope.payload.task.id.clone(),
                            definition_id: envelope.payload.task.definition_id.clone(),
                            occurrence_id: envelope.message_id.clone(),
                            scheduled_at: envelope.payload.scheduled_at,
                            trigger: envelope.payload.trigger,
                        };
                        Some((envelope.message_id.clone(), echo_agent_app_core::api::scheduler::management::occurrence_run_view(store.as_deref(), &occurrence)))
                    } else { None }
                }).collect::<std::collections::BTreeMap<_, _>>();
                echo_agent_app_core::api::scheduler::management::history_page_view(page, runs)
            }
            Err(error) => echo_agent_app_core::api::scheduler::management::history_error_view(error),
        }
    }).await.map_err(|error| IpcError::Internal(error.to_string()))
}
