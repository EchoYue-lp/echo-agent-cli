//! Tauri IPC commands for background tasks.

use crate::tauri::error::IpcError;
use crate::tauri::state::TauriState;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct TaskInfo {
    pub id: String,
    pub description: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<u8>,
    /// Task priority (0-10, higher = more urgent).
    pub priority: u8,
    /// Real-time progress percentage (0.0–100.0) from ProgressBridge.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress_pct: Option<f64>,
    /// Current phase name (e.g., "thinking", "web_search", "completed").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress_phase: Option<String>,
    /// Human-readable progress message (e.g., "Iteration 3", "Using: arxiv_search").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress_message: Option<String>,
    /// Estimated seconds remaining.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eta_secs: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct BackgroundRunLaunchReceipt {
    pub success: bool,
    pub workspace_id: String,
    pub run_id: String,
}

#[tauri::command]
pub async fn list_tasks(state: tauri::State<'_, TauriState>) -> Result<Vec<TaskInfo>, IpcError> {
    let service = state
        .app_state
        .tasks
        .service
        .as_ref()
        .ok_or_else(|| IpcError::Internal("Task service not initialized".to_string()))?;

    // Background-task APIs are compatibility projections over TaskRun files.
    let tasks = service.list_unified(None).await;
    let mut projected = Vec::with_capacity(tasks.len());
    for task in tasks {
        let progress = service.get_progress(&task.id).await;
        projected.push(task_to_info(task, progress));
    }
    Ok(projected)
}

#[tauri::command]
pub async fn submit_task(
    state: tauri::State<'_, TauriState>,
    kind: String,
    description: String,
    params: Option<serde_json::Value>,
    priority: Option<u8>,
) -> Result<BackgroundRunLaunchReceipt, IpcError> {
    let service = state
        .app_state
        .tasks
        .service
        .as_ref()
        .ok_or_else(|| IpcError::Internal("Task service not initialized".to_string()))?;

    use echo_agent_app_core::tasks::BackgroundTaskKind;
    let params = params.unwrap_or_default();

    // Every accepted kind creates a TaskRun; the kind only changes the prompt.
    let run_id = match kind.as_str() {
        "agent_chat" | "chat" => {
            let prompt = params
                .get("prompt")
                .and_then(|v| v.as_str())
                .unwrap_or(&description)
                .to_string();
            service
                .submit_run(&prompt, &description, "background", "ipc")
                .await
                .map_err(|e| IpcError::Internal(format!("Failed to submit task: {e}")))?
        }
        "cron" | "workflow" => {
            return Err(IpcError::Validation(format!(
                "{kind} tasks are not submitted via the background task service. Use the /cron command for scheduled tasks."
            )));
        }
        "research" => {
            let task_kind = BackgroundTaskKind::Research {
                topic: params
                    .get("topic")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&description)
                    .to_string(),
                max_papers: params
                    .get("max_papers")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(20) as usize,
                output_format: Default::default(),
            };
            service
                .submit_with_options(task_kind, &description, Some("ipc".to_string()), priority)
                .await
                .map_err(|e| IpcError::Internal(format!("Failed to submit task: {e}")))?
        }
        other => {
            return Err(IpcError::Validation(format!(
                "Unknown task kind: {other}. Valid: agent_chat, research"
            )));
        }
    };

    Ok(BackgroundRunLaunchReceipt {
        success: true,
        workspace_id: service.workspace_id(),
        run_id,
    })
}

#[tauri::command]
pub async fn get_task(
    state: tauri::State<'_, TauriState>,
    id: String,
) -> Result<TaskInfo, IpcError> {
    let service = state
        .app_state
        .tasks
        .service
        .as_ref()
        .ok_or_else(|| IpcError::Internal("Task service not initialized".to_string()))?;

    let task = service
        .get_unified(&id)
        .await
        .ok_or_else(|| IpcError::NotFound(format!("Task '{}' not found", id)))?;
    let progress = service.get_progress(&id).await;
    Ok(task_to_info(task, progress))
}

#[tauri::command]
pub async fn cancel_task(
    state: tauri::State<'_, TauriState>,
    id: String,
) -> Result<serde_json::Value, IpcError> {
    let service = state
        .app_state
        .tasks
        .service
        .as_ref()
        .ok_or_else(|| IpcError::Internal("Task service not initialized".to_string()))?;

    let cancelled = service.cancel(&id).await;
    Ok(serde_json::json!({
        "success": cancelled,
        "task_id": id,
    }))
}

fn task_to_info(
    task: echo_agent_app_core::tasks::UnifiedTaskInfo,
    progress: Option<echo_agent::tasks::progress::TaskProgress>,
) -> TaskInfo {
    // Progress is derived from the same TaskRuntime todo projection.
    let (progress_pct, progress_phase, progress_message, eta_secs) = match progress {
        Some(p) => (
            Some(p.percentage),
            Some(p.current_phase.clone()),
            p.message.clone(),
            p.eta_secs,
        ),
        None => (None, None, None, None),
    };

    TaskInfo {
        id: task.id,
        description: task.description,
        status: task.status,
        created_at: task.created_at,
        updated_at: task.updated_at,
        result: task.result,
        error: task.error,
        kind: task.kind,
        progress: None,
        priority: task.priority,
        progress_pct,
        progress_phase,
        progress_message,
        eta_secs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_launch_receipt_uses_workspace_and_run_identity()
    -> Result<(), Box<dyn std::error::Error>> {
        let value = serde_json::to_value(BackgroundRunLaunchReceipt {
            success: true,
            workspace_id: "workspace-a".to_string(),
            run_id: "run-a".to_string(),
        })?;
        assert_eq!(
            value
                .get("workspace_id")
                .and_then(serde_json::Value::as_str),
            Some("workspace-a")
        );
        assert_eq!(
            value.get("run_id").and_then(serde_json::Value::as_str),
            Some("run-a")
        );
        assert!(value.get("task_id").is_none());
        Ok(())
    }
}
