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
    /// Task IDs this task depends on.
    pub dependencies: Vec<String>,
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

#[tauri::command]
pub async fn list_tasks(state: tauri::State<'_, TauriState>) -> Result<Vec<TaskInfo>, IpcError> {
    let service = state
        .app_state
        .tasks
        .service
        .as_ref()
        .ok_or_else(|| IpcError::Internal("Task service not initialized".to_string()))?;

    let tasks = service.list(None);
    Ok(tasks
        .into_iter()
        .map(|task| {
            let progress = service.get_progress(&task.id);
            task_to_info(task, progress)
        })
        .collect())
}

#[tauri::command]
pub async fn submit_task(
    state: tauri::State<'_, TauriState>,
    kind: String,
    description: String,
    params: Option<serde_json::Value>,
    priority: Option<u8>,
    depends_on: Option<Vec<String>>,
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
        "cron" | "workflow" => {
            return Err(IpcError::Validation(format!(
                "{kind} tasks are not submitted via the background task service. Use the /cron command for scheduled tasks."
            )));
        }
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
                "Unknown task kind: {other}. Valid: agent_chat, research"
            )));
        }
    };

    let task_id = service
        .submit_with_options(
            task_kind,
            &description,
            Some("ipc".to_string()),
            priority,
            depends_on.unwrap_or_default(),
        )
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
        .ok_or_else(|| IpcError::NotFound(format!("Task '{}' not found", id)))?;

    let progress = service.get_progress(&id);
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
    task: echo_agent_app_core::tasks::Task,
    progress: Option<echo_agent_app_core::tasks::progress::TaskProgress>,
) -> TaskInfo {
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
        echo_agent_app_core::tasks::TaskStatus::Skipped => ("skipped".to_string(), None),
        echo_agent_app_core::tasks::TaskStatus::Paused(reason) => {
            ("paused".to_string(), Some(reason.clone()))
        }
    };

    // Extract progress from the live progress cache (updated by ProgressBridge)
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
        id: task.id.clone(),
        description: task.description.clone(),
        status: status_str,
        created_at: task.created_at.to_string(),
        updated_at: task.updated_at.to_string(),
        result: task.result.clone(),
        error,
        kind,
        progress: None,
        priority: task.priority,
        dependencies: task.dependencies.clone(),
        progress_pct,
        progress_phase,
        progress_message,
        eta_secs,
    }
}

/// Response for get_task_dag command
#[derive(Debug, Serialize)]
pub struct TaskDagInfo {
    /// Mermaid format DAG visualization
    pub mermaid: String,
    /// Task details with dependencies
    pub tasks: Vec<TaskDagNode>,
}

#[derive(Debug, Serialize)]
pub struct TaskDagNode {
    pub id: String,
    pub description: String,
    pub status: String,
    pub priority: u8,
    pub dependencies: Vec<String>,
}

#[tauri::command]
pub async fn get_task_dag(state: tauri::State<'_, TauriState>) -> Result<TaskDagInfo, IpcError> {
    let service = state
        .app_state
        .tasks
        .service
        .as_ref()
        .ok_or_else(|| IpcError::Internal("Task service not initialized".to_string()))?;

    let manager = service.manager();
    let tasks = manager.get_all_tasks();

    let mermaid = manager.visualize_dependencies();

    let task_nodes: Vec<TaskDagNode> = tasks
        .into_iter()
        .map(|task| {
            let status = match &task.status {
                echo_agent_app_core::tasks::TaskStatus::Pending => "pending".to_string(),
                echo_agent_app_core::tasks::TaskStatus::InProgress => "in_progress".to_string(),
                echo_agent_app_core::tasks::TaskStatus::Completed => "completed".to_string(),
                echo_agent_app_core::tasks::TaskStatus::Cancelled => "cancelled".to_string(),
                echo_agent_app_core::tasks::TaskStatus::Failed(_) => "failed".to_string(),
                echo_agent_app_core::tasks::TaskStatus::Blocked(_) => "blocked".to_string(),
                echo_agent_app_core::tasks::TaskStatus::TimedOut { .. } => "timed_out".to_string(),
                echo_agent_app_core::tasks::TaskStatus::Retrying { .. } => "retrying".to_string(),
                echo_agent_app_core::tasks::TaskStatus::Skipped => "skipped".to_string(),
                echo_agent_app_core::tasks::TaskStatus::Paused(_) => "paused".to_string(),
            };
            TaskDagNode {
                id: task.id,
                description: task.description,
                status,
                priority: task.priority,
                dependencies: task.dependencies,
            }
        })
        .collect();

    Ok(TaskDagInfo {
        mermaid,
        tasks: task_nodes,
    })
}
