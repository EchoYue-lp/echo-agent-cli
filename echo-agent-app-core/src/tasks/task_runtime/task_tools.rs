//! Agent tools for managing the task plan during execution.
//!
//! These let the main agent autonomously create / update / complete / skip /
//! list tasks, mirroring Claude Code's TaskCreate / TaskUpdate model.
//!
//! Each tool reads `run_id` from a `tokio::task_local!` scoped by the executor
//! around task execution, and operates on the [`TaskRuntimeStore`] injected at
//! construction time.
//!
//! Why task_local (not thread_local): tokio uses a work-stealing scheduler that
//! may move a task across OS threads at any `.await` point. A `thread_local!`
//! value would be lost or silently swapped with another run's value after a
//! thread hop. `task_local!` is bound to the logical async task and survives
//! `.await` across threads — correct for this use case.

use std::sync::{Arc, LazyLock};

use dashmap::DashMap;
use echo_agent::prelude::*;
use echo_agent::tools::{Tool, ToolResult};
use tokio::sync::Notify;

use super::store::TaskRuntimeStore;
use super::types::{PlanTask, PlanTaskKind, TaskPatch, TodoStatus, WorkerTraceEvent};

/// Convenience alias for a trace-sink callback that forwards worker trace
/// events out of the task-runtime executor.
pub type TraceSink = Arc<dyn Fn(WorkerTraceEvent) + Send + Sync>;

// ── Approval-signal registry (spec §10.5 ComplexRuntime) ──────────────────

/// Shared map of approval signals for ComplexRuntime runs (spec §10.5).
/// Stores `Arc<Notify>` handles keyed by `run_id` so the Tauri
/// `resume_task_run` command can wake the waiting `execute_plan` tool instead
/// of bypassing it (which would cause TWO concurrent execute_run calls).
pub(crate) static APPROVAL_NOTIFIES: LazyLock<DashMap<String, Arc<Notify>>> =
    LazyLock::new(DashMap::new);

/// Register an approval signal so `resume_task_run` can find it.
pub fn register_approval_signal(run_id: &str, signal: Arc<Notify>) {
    APPROVAL_NOTIFIES.insert(run_id.to_string(), signal);
}

/// Remove an approval signal after the execute_plan tool has been woken.
pub fn remove_approval_signal(run_id: &str) {
    APPROVAL_NOTIFIES.remove(run_id);
}

/// Notify a waiting `execute_plan` tool to resume. Returns `true` if a signal
/// was found and notified, `false` if the run_id has no registered signal
/// (the caller should fall back to a direct execution path).
pub fn notify_approval_signal(run_id: &str) -> bool {
    if let Some(signal) = APPROVAL_NOTIFIES.get(run_id) {
        signal.notify_one();
        true
    } else {
        false
    }
}

// ── Task-local run_id injection (async-safe) ──────────────────────────────

tokio::task_local! {
    /// The run_id of the currently executing task run. Set by the executor via
    /// [`with_run_context`] around the worker dispatch so tools can read it.
    pub static CURRENT_RUN_ID: String;
    /// The cancel token for the currently executing task run. Set alongside
    /// CURRENT_RUN_ID so delegate_readonly and other tools can read it.
    pub static CURRENT_CANCEL: tokio_util::sync::CancellationToken;
    /// Delegate nesting depth — incremented each time a subagent is delegated
    /// during tool execution. Used by Task 6 (L3 nesting) to prevent runaway
    /// recursion and to route subagent tool calls correctly.
    pub static CURRENT_DELEGATE_DEPTH: std::cell::Cell<u32>;
    /// An optional trace-sink that forwards `WorkerTraceEvent` items out of the
    /// executor so the frontend can render real-time worker trace views. Set
    /// alongside `CURRENT_RUN_ID` by [`with_run_context`].
    pub static CURRENT_TRACE_SINK: Option<TraceSink>;
    /// The cache user id for the currently executing run. Used when dispatching
    /// LLM calls so that cache keys are scoped per user. Falls back to the empty
    /// string when no user context is available.
    pub static CURRENT_CACHE_USER_ID: String;
}

/// Run `f` with run_id, cancel, delegate_depth, trace_sink, and cache_user_id
/// available to all task tools.
///
/// Called by the executor before dispatching task work. Replaces the old
/// [`with_run_id`] which only scoped the run_id. Delegate depth starts at 0
/// and is incremented by the L3 nesting layer (Task 6).
pub async fn with_run_context<F, R>(
    run_id: String,
    cancel: tokio_util::sync::CancellationToken,
    trace_sink: Option<TraceSink>,
    cache_user_id: String,
    f: F,
) -> R
where
    F: std::future::Future<Output = R>,
{
    let cell_cancel = cancel.clone();
    CURRENT_RUN_ID
        .scope(
            run_id,
            CURRENT_CANCEL.scope(
                cell_cancel,
                CURRENT_DELEGATE_DEPTH.scope(
                    std::cell::Cell::new(0),
                    CURRENT_TRACE_SINK
                        .scope(trace_sink, CURRENT_CACHE_USER_ID.scope(cache_user_id, f)),
                ),
            ),
        )
        .await
}

/// Legacy wrapper — keeps old callers compiling. Prefer [`with_run_context`].
pub async fn with_run_id<F, R>(run_id: String, f: F) -> R
where
    F: std::future::Future<Output = R>,
{
    let cancel = tokio_util::sync::CancellationToken::new();
    with_run_context(run_id, cancel, None, String::new(), f).await
}

fn current_run_id() -> Option<String> {
    CURRENT_RUN_ID.try_with(|cell| cell.clone()).ok()
}

// ── Helpers ───────────────────────────────────────────────────────────────

#[allow(clippy::result_large_err)] // ToolResult is the framework error type; boxing would touch every caller
pub(crate) fn require_run_id() -> std::result::Result<String, ToolResult> {
    current_run_id().ok_or_else(|| ToolResult::error("no active run — run_id not set in context"))
}

/// 从 ToolContext 优先读 run_id(跨 spawn 安全),回退 task_local(主 agent scope)。
///
/// 这是根治 task_local 跨 tokio::spawn 断裂的关键:worker 在框架层 dispatch_fork
/// 的 spawn 里执行,task_local 全部丢失;但 ToolContext 是值传递(经 dispatch_fork
/// → set_external_context → pipeline 填入),跨 spawn 安全。工具 override
/// `execute_with_context` 后用此 helper 读 run_id,主 agent 和 worker 都能拿到。
#[allow(clippy::result_large_err)] // ToolResult is the framework error type; boxing would touch every caller
pub(crate) fn run_id_from_ctx_or_local(
    ctx: &echo_core::tools::ToolContext,
) -> std::result::Result<String, ToolResult> {
    ctx.run_id
        .clone()
        .or_else(current_run_id)
        .ok_or_else(|| ToolResult::error("no active run — run_id not in ToolContext or task_local"))
}

/// 在 ToolContext.run_id(若有)的 task_local 覆盖作用域内执行 f。
///
/// 这样工具既有的 `execute`(读 task_local 的 require_run_id)无需改动即可在
/// worker 场景工作:execute_with_context 调本函数包住原 execute,ctx.run_id 被
/// 临时注入 task_local,require_run_id 读到的是 ToolContext 的值(跨 spawn 安全)。
/// ctx.run_id 为 None 时直接执行 f(回退原 task_local,主 agent 场景)。
pub(crate) async fn scoped_with_ctx_run_id<F, Fut, R>(
    ctx: &echo_core::tools::ToolContext,
    f: F,
) -> R
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = R>,
{
    match &ctx.run_id {
        Some(rid) => {
            // 同时覆盖 run_id 和 cancel(若有),让 require_run_id / CURRENT_CANCEL
            // 在 worker 场景读到 ToolContext 的值(跨 spawn 安全)。
            let cancel = ctx
                .cancel
                .as_ref()
                .map(|c| (**c).clone())
                .unwrap_or_default();
            CURRENT_CANCEL
                .scope(cancel, CURRENT_RUN_ID.scope(rid.clone(), f()))
                .await
        }
        None => f().await,
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)] // tool impls below are production code; reordering is pure churn
mod tests {
    use super::*;

    /// task_local must survive `.await` across tokio thread hops.
    /// (thread_local would fail this: a work-stealing scheduler can move the
    /// task to another OS thread after `yield_now`, dropping the thread_local
    /// value or returning another run's.)
    #[tokio::test]
    async fn run_id_survives_await_across_threads() {
        // Force a yield point so the scheduler has the opportunity to move us
        // to a different runtime worker thread.
        let captured = with_run_id("run-xyz".to_string(), async {
            tokio::task::yield_now().await;
            // After yield, we may be on a different thread — task_local must
            // still hold the value.
            current_run_id()
        })
        .await;
        assert_eq!(captured.as_deref(), Some("run-xyz"));
    }

    /// Outside a `with_run_id` scope, `current_run_id` must be None.
    #[tokio::test]
    async fn run_id_absent_outside_scope() {
        // Use a multi-thread runtime to make any latent thread_local bug
        // surface; task_local must still correctly return None.
        assert_eq!(current_run_id(), None);
    }

    /// Nested scopes must shadow correctly (inner wins, outer restored).
    #[tokio::test]
    async fn nested_scopes_shadow_and_restore() {
        let inner = with_run_id("outer".to_string(), async {
            with_run_id("inner".to_string(), async { current_run_id() }).await
        })
        .await;
        assert_eq!(inner.as_deref(), Some("inner"));
    }
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

/// 按 task kind 推导默认 agent_role(映射到 infra.rs 注册的 worker 角色)。
///
/// 必要性:PlanTask 的 agent_role 默认是 "general",但框架只注册了 13 个具体角色
/// (project_explorer/code_reviewer/...),委派 "general" 必然 "Subagent not found"。
/// 只读 kind(read_only_review/investigation/test_plan/review/summary)委派给对应
/// 只读 worker;变更 kind(implementation/debugging/verification)由主 agent 直接
/// 执行(不委派 worker),用 "primary" 占位(run_readonly_worker 不会触及)。
fn role_for_kind(kind: PlanTaskKind) -> &'static str {
    match kind {
        PlanTaskKind::ReadOnlyReview | PlanTaskKind::Investigation => "project_explorer",
        PlanTaskKind::TestPlan => "test_planner",
        PlanTaskKind::Review => "code_reviewer",
        PlanTaskKind::Summary => "summary_writer",
        // 变更类:主 agent 直接执行(executor.rs run_main_agent_task),不委派 worker。
        // agent_role 仅作记录,不会被 run_readonly_worker 用到。
        PlanTaskKind::Implementation | PlanTaskKind::Debugging | PlanTaskKind::Verification => {
            "primary"
        }
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
            self.create_task(run_id, params).await
        })
    }

    fn execute_with_context<'a>(
        &'a self,
        params: ToolParameters,
        ctx: &'a echo_core::tools::ToolContext,
    ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<ToolResult>> {
        Box::pin(async move {
            let run_id = match run_id_from_ctx_or_local(ctx) {
                Ok(id) => id,
                Err(e) => return Ok(e),
            };
            self.create_task(run_id, params).await
        })
    }
}

impl TaskCreateTool {
    async fn create_task(
        &self,
        run_id: String,
        params: ToolParameters,
    ) -> echo_agent::error::Result<ToolResult> {
        let description = params
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        // title 兜底:LLM 偶尔漏传 title,用 description 首行(前 60 字符)代替,
        // 避免右侧栏显示空标题(日志里 "Created task ''")。
        let title = params
            .get("title")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from)
            .unwrap_or_else(|| {
                if description.is_empty() {
                    "未命名任务".to_string()
                } else {
                    description.chars().take(60).collect()
                }
            });
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
        let kind = parse_kind(kind_str);
        let task = PlanTask {
            id: task_id.clone(),
            title: title.clone(),
            description,
            kind,
            agent_role: role_for_kind(kind).to_string(),
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

    fn execute_with_context<'a>(
        &'a self,
        params: ToolParameters,
        ctx: &'a echo_core::tools::ToolContext,
    ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<ToolResult>> {
        Box::pin(async move { scoped_with_ctx_run_id(ctx, || self.execute(params)).await })
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

    fn execute_with_context<'a>(
        &'a self,
        params: ToolParameters,
        ctx: &'a echo_core::tools::ToolContext,
    ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<ToolResult>> {
        Box::pin(async move { scoped_with_ctx_run_id(ctx, || self.execute(params)).await })
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

    fn execute_with_context<'a>(
        &'a self,
        params: ToolParameters,
        ctx: &'a echo_core::tools::ToolContext,
    ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<ToolResult>> {
        Box::pin(async move { scoped_with_ctx_run_id(ctx, || self.execute(params)).await })
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

    fn execute_with_context<'a>(
        &'a self,
        params: ToolParameters,
        ctx: &'a echo_core::tools::ToolContext,
    ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<ToolResult>> {
        Box::pin(async move { scoped_with_ctx_run_id(ctx, || self.execute(params)).await })
    }
}
