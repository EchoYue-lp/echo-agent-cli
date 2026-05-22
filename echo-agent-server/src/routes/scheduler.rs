//! 定时任务调度 API

use axum::{
    Json, debug_handler,
    extract::State,
    response::{IntoResponse, Response},
};
use echo_agent::agent::Agent;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use std::sync::Arc;

use crate::scheduler::task::{CronTask, CronTaskStatus, TaskStore};
use crate::state::AppState;

// ── Request/Response types ─────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct AddTaskRequest {
    pub name: String,
    pub cron_expr: String,
    pub prompt: String,
}

#[derive(Debug, Serialize)]
pub struct CronTaskResponse {
    pub id: String,
    pub name: String,
    pub cron_expr: String,
    pub prompt: String,
    pub status: String,
    pub last_run_at: Option<String>,
    pub last_result: Option<String>,
    pub created_at: String,
    pub next_run: Option<String>,
}

impl From<CronTask> for CronTaskResponse {
    fn from(t: CronTask) -> Self {
        let status = match t.status {
            CronTaskStatus::Enabled => "enabled",
            CronTaskStatus::Disabled => "disabled",
        };
        let next_run = t.next_run().ok().map(|dt| dt.to_rfc3339());
        Self {
            id: t.id,
            name: t.name,
            cron_expr: t.cron_expr,
            prompt: t.prompt,
            status: status.to_string(),
            last_run_at: t.last_run_at,
            last_result: t.last_result,
            created_at: t.created_at,
            next_run,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct SetStatusRequest {
    pub status: String,
}

// ── Handlers ───────────────────────────────────────────────────────

/// GET /api/scheduler/tasks — 列出所有定时任务
#[debug_handler]
pub async fn list_tasks(State(_state): State<Arc<AppState>>) -> Response {
    let store = TaskStore::new();
    match store.load_all() {
        Ok(tasks) => {
            let items: Vec<CronTaskResponse> =
                tasks.into_iter().map(CronTaskResponse::from).collect();
            Json(items).into_response()
        }
        Err(e) => {
            Json(serde_json::json!({ "error": format!("加载任务失败: {e}") })).into_response()
        }
    }
}

/// POST /api/scheduler/tasks — 添加定时任务
#[debug_handler]
pub async fn add_task(
    State(_state): State<Arc<AppState>>,
    Json(req): Json<AddTaskRequest>,
) -> Response {
    // Validate cron expression
    match cron::Schedule::from_str(&req.cron_expr) {
        Ok(_) => {}
        Err(e) => {
            return Json(serde_json::json!({
                "error": format!("Invalid cron expression: {e}")
            }))
            .into_response();
        }
    }

    let task = CronTask::new(&req.name, &req.cron_expr, &req.prompt);
    let store = TaskStore::new();
    match store.add(task) {
        Ok(()) => {
            // Reload to get the full task
            Json(serde_json::json!({ "success": true })).into_response()
        }
        Err(e) => Json(serde_json::json!({ "error": format!("{e}") })).into_response(),
    }
}

/// DELETE /api/scheduler/tasks/:id — 删除定时任务
#[debug_handler]
pub async fn remove_task(
    State(_state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    let store = TaskStore::new();
    match store.remove(&id) {
        Ok(true) => Json(serde_json::json!({ "success": true })).into_response(),
        Ok(false) => Json(serde_json::json!({ "error": "Task not found" })).into_response(),
        Err(e) => Json(serde_json::json!({ "error": format!("{e}") })).into_response(),
    }
}

/// PUT /api/scheduler/tasks/:id/status — 启用/禁用任务
#[debug_handler]
pub async fn set_task_status(
    State(_state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(req): Json<SetStatusRequest>,
) -> Response {
    let status = match req.status.as_str() {
        "enabled" => CronTaskStatus::Enabled,
        "disabled" => CronTaskStatus::Disabled,
        _ => {
            return Json(serde_json::json!({
                "error": "status must be 'enabled' or 'disabled'"
            }))
            .into_response();
        }
    };

    let store = TaskStore::new();
    match store.set_status(&id, status) {
        Ok(true) => Json(serde_json::json!({ "success": true })).into_response(),
        Ok(false) => Json(serde_json::json!({ "error": "Task not found" })).into_response(),
        Err(e) => Json(serde_json::json!({ "error": format!("{e}") })).into_response(),
    }
}

/// POST /api/scheduler/tasks/:id/run — 手动触发一次任务
#[debug_handler]
pub async fn run_task(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    let store = TaskStore::new();
    let task = match store.get(&id) {
        Ok(Some(t)) => t,
        Ok(None) => {
            return Json(serde_json::json!({ "error": "Task not found" })).into_response();
        }
        Err(e) => {
            return Json(serde_json::json!({ "error": format!("{e}") })).into_response();
        }
    };

    let prompt = task.prompt.clone();
    let task_id = task.id.clone();
    let guard = state.connection.agent.inner().read().await;
    let result = guard.chat(&prompt).await;

    match result {
        Ok(answer) => {
            let summary: String = answer.chars().take(500).collect();
            let _ = store.update_last_run(&task_id, &summary);
            Json(serde_json::json!({
                "success": true,
                "result": answer
            }))
            .into_response()
        }
        Err(e) => Json(serde_json::json!({
            "error": format!("Task execution failed: {e}")
        }))
        .into_response(),
    }
}
