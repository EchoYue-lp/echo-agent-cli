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
}

#[tauri::command]
pub async fn list_tasks(state: tauri::State<'_, TauriState>) -> Result<Vec<TaskInfo>, IpcError> {
    let service = state
        .app_state
        .tasks
        .service
        .as_ref()
        .ok_or_else(|| IpcError::Internal("Task service not initialized".to_string()))?;

    let tasks = service.list(None);
    Ok(tasks.into_iter().map(task_to_info).collect())
}

#[tauri::command]
pub async fn submit_task(
    state: tauri::State<'_, TauriState>,
    kind: String,
    description: String,
    params: Option<serde_json::Value>,
) -> Result<serde_json::Value, IpcError> {
    let service = state
        .app_state
        .tasks
        .service
        .as_ref()
        .ok_or_else(|| IpcError::Internal("Task service not initialized".to_string()))?;

    use echo_agent_app_core::tasks::BackgroundTaskKind;
    let params = params.unwrap_or_default();

    let task_kind = match kind.as_str() {
        "agent_chat" | "chat" => BackgroundTaskKind::AgentChat {
            prompt: params
                .get("prompt")
                .and_then(|v| v.as_str())
                .unwrap_or(&description)
                .to_string(),
            session_id: params
                .get("session_id")
                .and_then(|v| v.as_str())
                .map(String::from),
        },
        "cron" => BackgroundTaskKind::Cron {
            cron_expr: params
                .get("cron_expr")
                .and_then(|v| v.as_str())
                .unwrap_or("0 * * * *")
                .to_string(),
            prompt: params
                .get("prompt")
                .and_then(|v| v.as_str())
                .unwrap_or(&description)
                .to_string(),
        },
        "workflow" => BackgroundTaskKind::Workflow {
            workflow_id: params
                .get("workflow_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            input: params.get("input").cloned().unwrap_or_default(),
        },
        "research" => BackgroundTaskKind::Research {
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
        },
        other => {
            return Err(IpcError::Validation(format!(
                "Unknown task kind: {other}. Valid: agent_chat, cron, workflow, research"
            )));
        }
    };

    let task_id = service
        .submit(task_kind, &description, Some("ipc".to_string()))
        .await
        .map_err(|e| IpcError::Internal(format!("Failed to submit task: {e}")))?;

    Ok(serde_json::json!({
        "success": true,
        "task_id": task_id,
    }))
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

    let (task, _meta) = service
        .get(&id)
        .await
        .ok_or_else(|| IpcError::NotFound(format!("Task '{}' not found", id)))?;

    Ok(task_to_info(task))
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

fn task_to_info(task: echo_agent_app_core::tasks::Task) -> TaskInfo {
    let kind = task
        .tags
        .iter()
        .find(|t| t.starts_with("bg:kind:"))
        .cloned();

    let (status_str, error) = match &task.status {
        echo_agent_app_core::tasks::TaskStatus::Pending => ("pending".to_string(), None),
        echo_agent_app_core::tasks::TaskStatus::InProgress => ("in_progress".to_string(), None),
        echo_agent_app_core::tasks::TaskStatus::Completed => ("completed".to_string(), None),
        echo_agent_app_core::tasks::TaskStatus::Cancelled => ("cancelled".to_string(), None),
        echo_agent_app_core::tasks::TaskStatus::Failed(e) => {
            ("failed".to_string(), Some(e.clone()))
        }
        echo_agent_app_core::tasks::TaskStatus::Blocked(e) => {
            ("blocked".to_string(), Some(e.clone()))
        }
        echo_agent_app_core::tasks::TaskStatus::TimedOut { error } => {
            ("timed_out".to_string(), Some(error.clone()))
        }
        echo_agent_app_core::tasks::TaskStatus::Retrying {
            attempt,
            last_error,
        } => (
            "retrying".to_string(),
            Some(format!("attempt {attempt}: {last_error}")),
        ),
    };

    TaskInfo {
        id: task.id.clone(),
        description: task.description.clone(),
        status: status_str,
        created_at: task.created_at.to_string(),
        updated_at: task.updated_at.to_string(),
        result: task.result.clone(),
        error,
        kind,
        progress: None,
    }
}
