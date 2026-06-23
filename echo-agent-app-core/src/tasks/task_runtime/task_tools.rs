//! Agent tools for managing the task plan during execution.
//!
//! These let the main agent autonomously create / update / complete / skip /
//! list tasks, mirroring Claude Code's TaskCreate / TaskUpdate model.
//!
//! Each tool reads `run_id` from a thread-local set by the executor before
//! dispatching task work, and operates on the [`TaskRuntimeStore`] injected
//! at construction time.

use std::sync::Arc;

use echo_agent::prelude::*;
use echo_agent::tools::{Tool, ToolResult};

use super::store::TaskRuntimeStore;
use super::types::{PlanTask, PlanTaskKind, TaskPatch, TodoStatus};

// ── Thread-local run_id injection ─────────────────────────────────────────

std::thread_local! {
    static CURRENT_RUN_ID: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Set the current run_id for the calling thread. Called by the executor
/// before dispatching task work so tools can read it.
pub fn set_current_run_id(run_id: Option<String>) {
    CURRENT_RUN_ID.with(|cell| {
        *cell.borrow_mut() = run_id;
    });
}

/// Clear the current run_id after task work completes.
pub fn clear_current_run_id() {
    CURRENT_RUN_ID.with(|cell| {
        *cell.borrow_mut() = None;
    });
}

fn current_run_id() -> Option<String> {
    CURRENT_RUN_ID.with(|cell| cell.borrow().clone())
}

// ── Helpers ───────────────────────────────────────────────────────────────

fn require_run_id() -> std::result::Result<String, ToolResult> {
    current_run_id().ok_or_else(|| ToolResult::error("no active run — run_id not set in context"))
}

fn parse_kind(s: &str) -> PlanTaskKind {
    match s {
        "read_only_review" => PlanTaskKind::ReadOnlyReview,
        "investigation" => PlanTaskKind::Investigation,
        "test_plan" => PlanTaskKind::TestPlan,
        "implementation" => PlanTaskKind::Implementation,
        "debugging" => PlanTaskKind::Debugging,
        "review" => PlanTaskKind::Review,
        "summary" => PlanTaskKind::Summary,
        "verification" => PlanTaskKind::Verification,
        _ => PlanTaskKind::Implementation,
    }
}

// ── task_create ───────────────────────────────────────────────────────────

pub struct TaskCreateTool {
    pub store: Arc<TaskRuntimeStore>,
}

impl Tool for TaskCreateTool {
    fn name(&self) -> &str {
        "task_create"
    }

    fn description(&self) -> &str {
        "Create a new task in the current plan. Use when you discover \
         additional work is needed during execution."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "title": { "type": "string", "description": "Short task title" },
                "description": { "type": "string", "description": "What this task should accomplish" },
                "kind": {
                    "type": "string",
                    "enum": ["implementation","debugging","verification","review","investigation","test_plan","summary","read_only_review"],
                    "description": "Task kind"
                },
                "depends_on": { "type": "array", "items": { "type": "string" }, "description": "Task ids this depends on" },
                "after_task_id": { "type": "string", "description": "Insert after this task id (optional)" }
            },
            "required": ["title","description","kind"]
        })
    }

    fn execute<'a>(
        &'a self,
        params: ToolParameters,
    ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<ToolResult>> {
        Box::pin(async move {
            let run_id = match require_run_id() {
                Ok(id) => id,
                Err(e) => return Ok(e),
            };
            let title = params
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let description = params
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let kind_str = params
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("implementation");
            let depends_on: Vec<String> = params
                .get("depends_on")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let after_task_id = params
                .get("after_task_id")
                .and_then(|v| v.as_str())
                .map(String::from);

            let task_id = format!("task_{}", chrono::Utc::now().timestamp_millis());
            let task = PlanTask {
                id: task_id.clone(),
                title: title.clone(),
                description,
                kind: parse_kind(kind_str),
                depends_on,
                status: TodoStatus::Pending,
                ..Default::default()
            };
            match self.store.insert_task(&run_id, after_task_id, task) {
                Ok(()) => Ok(ToolResult::success(format!(
                    "Created task '{title}' (id: {task_id})"
                ))),
                Err(e) => Ok(ToolResult::error(format!("Failed to create task: {e}"))),
            }
        })
    }
}

// ── task_update ───────────────────────────────────────────────────────────

pub struct TaskUpdateTool {
    pub store: Arc<TaskRuntimeStore>,
}

impl Tool for TaskUpdateTool {
    fn name(&self) -> &str {
        "task_update"
    }
    fn description(&self) -> &str {
        "Update an existing task's fields. Only pending/blocked tasks can be fully updated; \
         running tasks can only change title/description."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "description": "Task to update" },
                "title": { "type": "string", "description": "New title (optional)" },
                "description": { "type": "string", "description": "New description (optional)" },
                "kind": { "type": "string", "description": "New kind (optional)" },
                "depends_on": { "type": "array", "items": { "type": "string" }, "description": "New deps (optional)" }
            },
            "required": ["task_id"]
        })
    }
    fn execute<'a>(
        &'a self,
        params: ToolParameters,
    ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<ToolResult>> {
        Box::pin(async move {
            let run_id = match require_run_id() {
                Ok(id) => id,
                Err(e) => return Ok(e),
            };
            let task_id = params
                .get("task_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let patch = TaskPatch {
                title: params
                    .get("title")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                description: params
                    .get("description")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                kind: params.get("kind").and_then(|v| v.as_str()).map(parse_kind),
                depends_on: params
                    .get("depends_on")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    }),
                ..Default::default()
            };
            match self.store.update_task(&run_id, &task_id, patch) {
                Ok(()) => Ok(ToolResult::success(format!("Updated task '{task_id}'"))),
                Err(e) => Ok(ToolResult::error(format!("Failed to update task: {e}"))),
            }
        })
    }
}

// ── task_complete ─────────────────────────────────────────────────────────

pub struct TaskCompleteTool {
    pub store: Arc<TaskRuntimeStore>,
}

impl Tool for TaskCompleteTool {
    fn name(&self) -> &str {
        "task_complete"
    }
    fn description(&self) -> &str {
        "Mark a task as completed."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "description": "Task to complete" },
                "summary": { "type": "string", "description": "Brief summary (optional)" }
            },
            "required": ["task_id"]
        })
    }
    fn execute<'a>(
        &'a self,
        params: ToolParameters,
    ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<ToolResult>> {
        Box::pin(async move {
            let run_id = match require_run_id() {
                Ok(id) => id,
                Err(e) => return Ok(e),
            };
            let task_id = params.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
            let summary = params.get("summary").and_then(|v| v.as_str());
            match self
                .store
                .set_task_status(&run_id, task_id, TodoStatus::Completed, None, summary)
            {
                Ok(()) => Ok(ToolResult::success(format!("Completed task '{task_id}'"))),
                Err(e) => Ok(ToolResult::error(format!("Failed: {e}"))),
            }
        })
    }
}

// ── task_skip ─────────────────────────────────────────────────────────────

pub struct TaskSkipTool {
    pub store: Arc<TaskRuntimeStore>,
}

impl Tool for TaskSkipTool {
    fn name(&self) -> &str {
        "task_skip"
    }
    fn description(&self) -> &str {
        "Skip a task (soft-delete). Use when a task is no longer relevant."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "description": "Task to skip" },
                "reason": { "type": "string", "description": "Why this task is being skipped" }
            },
            "required": ["task_id","reason"]
        })
    }
    fn execute<'a>(
        &'a self,
        params: ToolParameters,
    ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<ToolResult>> {
        Box::pin(async move {
            let run_id = match require_run_id() {
                Ok(id) => id,
                Err(e) => return Ok(e),
            };
            let task_id = params.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
            let reason = params
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("skipped by agent");
            match self.store.remove_task(&run_id, task_id) {
                Ok(()) => Ok(ToolResult::success(format!(
                    "Skipped task '{task_id}': {reason}"
                ))),
                Err(e) => Ok(ToolResult::error(format!("Failed: {e}"))),
            }
        })
    }
}

// ── task_list ─────────────────────────────────────────────────────────────

pub struct TaskListTool {
    pub store: Arc<TaskRuntimeStore>,
}

impl Tool for TaskListTool {
    fn name(&self) -> &str {
        "task_list"
    }
    fn description(&self) -> &str {
        "List all tasks in the current plan with their status."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }
    fn execute<'a>(
        &'a self,
        _params: ToolParameters,
    ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<ToolResult>> {
        Box::pin(async move {
            let run_id = match require_run_id() {
                Ok(id) => id,
                Err(e) => return Ok(e),
            };
            match self.store.list_todos(&run_id) {
                Ok(todos) => {
                    let lines: Vec<String> = todos
                        .iter()
                        .map(|t| format!("[{}] {} — {}", t.status.as_str(), t.task_id, t.title))
                        .collect();
                    Ok(ToolResult::success(format!(
                        "Tasks ({}):\n{}",
                        todos.len(),
                        lines.join("\n")
                    )))
                }
                Err(e) => Ok(ToolResult::error(format!("Failed: {e}"))),
            }
        })
    }
}
