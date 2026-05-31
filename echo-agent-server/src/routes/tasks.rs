//! Background tasks REST API
//!
//! | Method | Path | Description |
//! |--------|------|-------------|
//! | GET | /api/tasks | List all background tasks |
//! | POST | /api/tasks | Submit a new background task |
//! | GET | /api/tasks/:id | Get task detail |
//! | POST | /api/tasks/:id/cancel | Cancel a task |
//! | GET | /api/tasks/:id/events | SSE stream of task events |

use axum::{
    Json,
    extract::{Path, State},
    response::sse::{Event, Sse},
};
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::error::AppError;
use crate::state::AppState;

// ── Request/Response types ──

#[derive(Debug, Deserialize)]
pub struct SubmitTaskRequest {
    pub kind: String,
    pub description: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

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

// ── API handlers ──

/// GET /api/tasks — list all background tasks
pub async fn list_tasks(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<TaskInfo>>, AppError> {
    let service = state
        .tasks
        .service
        .as_ref()
        .ok_or_else(|| AppError::Internal("Task service not initialized".to_string()))?;

    let tasks = service.list(None);
    let infos: Vec<TaskInfo> = tasks.into_iter().map(task_to_info).collect();
    Ok(Json(infos))
}

/// POST /api/tasks — submit a new background task
pub async fn submit_task(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SubmitTaskRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let service = state
        .tasks
        .service
        .as_ref()
        .ok_or_else(|| AppError::Internal("Task service not initialized".to_string()))?;

    use echo_agent_app_core::tasks::BackgroundTaskKind;

    let kind = match req.kind.as_str() {
        "agent_chat" | "chat" => BackgroundTaskKind::AgentChat {
            prompt: req.params.get("prompt")
                .and_then(|v| v.as_str())
                .unwrap_or(&req.description)
                .to_string(),
            session_id: req.params.get("session_id")
                .and_then(|v| v.as_str())
                .map(String::from),
        },
        "cron" => BackgroundTaskKind::Cron {
            cron_expr: req.params.get("cron_expr")
                .and_then(|v| v.as_str())
                .unwrap_or("0 * * * *")
                .to_string(),
            prompt: req.params.get("prompt")
                .and_then(|v| v.as_str())
                .unwrap_or(&req.description)
                .to_string(),
        },
        "workflow" => BackgroundTaskKind::Workflow {
            workflow_id: req.params.get("workflow_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            input: req.params.get("input").cloned().unwrap_or_default(),
        },
        "research" => BackgroundTaskKind::Research {
            topic: req.params.get("topic")
                .and_then(|v| v.as_str())
                .unwrap_or(&req.description)
                .to_string(),
            max_papers: req.params.get("max_papers")
                .and_then(|v| v.as_u64())
                .unwrap_or(20) as usize,
            output_format: Default::default(),
        },
        other => {
            return Err(AppError::Validation(format!(
                "Unknown task kind: {other}. Valid: agent_chat, cron, workflow, research"
            )));
        }
    };

    let task_id = service
        .submit(kind, &req.description, Some("web".to_string()))
        .await
        .map_err(|e| AppError::Internal(format!("Failed to submit task: {e}")))?;

    Ok(Json(serde_json::json!({
        "success": true,
        "task_id": task_id,
    })))
}

/// GET /api/tasks/:id — get task detail
pub async fn get_task(
    Path(id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<TaskInfo>, AppError> {
    let service = state
        .tasks
        .service
        .as_ref()
        .ok_or_else(|| AppError::Internal("Task service not initialized".to_string()))?;

    let (task, _meta) = service
        .get(&id)
        .await
        .ok_or_else(|| AppError::NotFound(format!("Task '{}' not found", id)))?;

    Ok(Json(task_to_info(task)))
}

/// POST /api/tasks/:id/cancel — cancel a task
pub async fn cancel_task(
    Path(id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, AppError> {
    let service = state
        .tasks
        .service
        .as_ref()
        .ok_or_else(|| AppError::Internal("Task service not initialized".to_string()))?;

    let cancelled = service.cancel(&id).await;
    Ok(Json(serde_json::json!({
        "success": cancelled,
        "task_id": id,
    })))
}

/// GET /api/tasks/:id/events — SSE stream of task events
pub async fn task_events(
    Path(id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>>, AppError> {
    let service = state
        .tasks
        .service
        .as_ref()
        .ok_or_else(|| AppError::Internal("Task service not initialized".to_string()))?;

    let mut rx = service.subscribe_events();
    let task_id = id.clone();

    let stream = futures::stream::unfold(rx, move |mut rx| {
        let task_id = task_id.clone();
        async move {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        let event_task_id = event.task_id();
                        if event_task_id == task_id {
                            let data = serde_json::json!({
                                "task_id": event_task_id,
                                "event": format!("{:?}", event),
                            });
                            return Some((Ok(Event::default().data(data.to_string())), rx));
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                }
            }
        }
    });

    Ok(Sse::new(stream))
}

// ── Helpers ──

fn task_to_info(task: echo_agent_app_core::tasks::Task) -> TaskInfo {
    // Extract kind from tags (set during submit via with_tags)
    let kind = task
        .tags
        .iter()
        .find(|t| t.starts_with("bg:kind:"))
        .cloned();

    let description = task.description.clone();

    let (status_str, error) = match &task.status {
        echo_agent_app_core::tasks::TaskStatus::Pending => ("pending".to_string(), None),
        echo_agent_app_core::tasks::TaskStatus::InProgress => ("in_progress".to_string(), None),
        echo_agent_app_core::tasks::TaskStatus::Completed => ("completed".to_string(), None),
        echo_agent_app_core::tasks::TaskStatus::Cancelled => ("cancelled".to_string(), None),
        echo_agent_app_core::tasks::TaskStatus::Failed(e) => ("failed".to_string(), Some(e.clone())),
        echo_agent_app_core::tasks::TaskStatus::Blocked(e) => ("blocked".to_string(), Some(e.clone())),
        echo_agent_app_core::tasks::TaskStatus::TimedOut { error } => ("timed_out".to_string(), Some(error.clone())),
        echo_agent_app_core::tasks::TaskStatus::Retrying { attempt, last_error } => {
            ("retrying".to_string(), Some(format!("attempt {attempt}: {last_error}")))
        }
    };

    TaskInfo {
        id: task.id.clone(),
        description,
        status: status_str,
        created_at: task.created_at.to_string(),
        updated_at: task.updated_at.to_string(),
        result: task.result.clone(),
        error,
        kind,
        progress: None,
    }
}
