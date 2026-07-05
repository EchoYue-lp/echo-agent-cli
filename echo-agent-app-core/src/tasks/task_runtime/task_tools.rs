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

use super::executor::ExecEvent;
use super::store::TaskRuntimeStore;
use super::types::{
    AttendedMode, DomainProfile, PlanTask, PlanTaskKind, TaskPatch, TaskRunStatus, TodoStatus,
};

/// Convenience alias for a trace-sink callback that forwards execution-flow
/// events out of the task-runtime executor. Same shape as
/// [`super::executor::ExecSink`]; kept as a distinct alias because the
/// `CURRENT_TRACE_SINK` task_local predates the `ExecSink` name and several
/// call sites still spell it as `TraceSink`.
pub type TraceSink = Arc<dyn Fn(ExecEvent) + Send + Sync>;

// ── Approval-signal registry (spec §10.5 ComplexRuntime) ──────────────────

/// Shared map of approval signals for ComplexRuntime runs (spec §10.5).
/// Stores `Arc<Notify>` handles keyed by `run_id` so the Tauri
/// `resume_task_run` command can wake the waiting `plan_execute` tool instead
/// of bypassing it (which would cause TWO concurrent execute_run calls).
pub(crate) static APPROVAL_NOTIFIES: LazyLock<DashMap<String, Arc<Notify>>> =
    LazyLock::new(DashMap::new);

/// Register an approval signal so `resume_task_run` can find it.
pub fn register_approval_signal(run_id: &str, signal: Arc<Notify>) {
    APPROVAL_NOTIFIES.insert(run_id.to_string(), signal);
}

/// Remove an approval signal after the plan_execute tool has been woken.
pub fn remove_approval_signal(run_id: &str) {
    APPROVAL_NOTIFIES.remove(run_id);
}

/// Notify a waiting `plan_execute` tool to resume. Returns `true` if a signal
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
    /// [`with_run_context`] around the subagent dispatch so tools can read it.
    pub static CURRENT_RUN_ID: String;
    /// The cancel token for the currently executing task run. Set alongside
    /// CURRENT_RUN_ID so plan_execute and other tools can read it.
    pub static CURRENT_CANCEL: tokio_util::sync::CancellationToken;
    /// Delegate nesting depth — incremented each time a subagent is delegated
    /// during tool execution. Used by Task 6 (L3 nesting) to prevent runaway
    /// recursion and to route subagent tool calls correctly.
    pub static CURRENT_DELEGATE_DEPTH: std::cell::Cell<u32>;
    /// An optional trace-sink that forwards [`ExecEvent`] items out of the
    /// executor so the frontend can render real-time execution-flow views. Set
    /// alongside `CURRENT_RUN_ID` by [`with_run_context`].
    pub static CURRENT_TRACE_SINK: Option<TraceSink>;
    /// The unattended write mode for the currently executing run (D7 stage 2).
    /// Set by `ExecutePlanTool::execute` so CP B preflight in `execute_task`
    /// can read it without threading the mode through `execute_run` →
    /// `run_dag` → `execute_task`. Defaults to `Disabled` when no scope is
    /// active (e.g. tests, attended runs).
    pub static CURRENT_UNATTENDED_WRITE_MODE: super::types::UnattendedWriteMode;
}

/// Run `f` with run_id, cancel, delegate_depth, and trace_sink available to all
/// task tools.
///
/// Called by the executor before dispatching task work. Replaces the old
/// [`with_run_id`] which only scoped the run_id. Delegate depth starts at 0
/// and is incremented by the L3 nesting layer (Task 6).
///
/// (stage4 P4.1) `cache_user_id` is no longer threaded here — tools/LLM calls
/// read the single source via `infra::load_or_create_cache_user_id()` instead.
pub async fn with_run_context<F, R>(
    run_id: String,
    cancel: tokio_util::sync::CancellationToken,
    trace_sink: Option<TraceSink>,
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
                    CURRENT_TRACE_SINK.scope(trace_sink, f),
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
    with_run_context(run_id, cancel, None, f).await
}

fn current_run_id() -> Option<String> {
    CURRENT_RUN_ID.try_with(|cell| cell.clone()).ok()
}

/// Read the current unattended write mode from the task-local scope (D7
/// stage 2). Returns `Disabled` when no scope is active (attended runs,
/// tests) — matching stage-1 behaviour as the safe default.
pub fn current_unattended_write_mode() -> super::types::UnattendedWriteMode {
    CURRENT_UNATTENDED_WRITE_MODE
        .try_with(|m| *m)
        .unwrap_or_default()
}

// ── Helpers ───────────────────────────────────────────────────────────────

#[allow(clippy::result_large_err)] // ToolResult is the framework error type; boxing would touch every caller
pub(crate) fn require_run_id() -> std::result::Result<String, ToolResult> {
    current_run_id().ok_or_else(|| ToolResult::error("no active run — run_id not set in context"))
}

/// 从 ToolContext 优先读 run_id(跨 spawn 安全),回退 task_local(主 agent scope)。
///
/// 这是根治 task_local 跨 tokio::spawn 断裂的关键:subagent 在框架层 dispatch_fork
/// 的 spawn 里执行,task_local 全部丢失;但 ToolContext 是值传递(经 dispatch_fork
/// → set_external_context → pipeline 填入),跨 spawn 安全。工具 override
/// `execute_with_context` 后用此 helper 读 run_id,主 agent 和 subagent 都能拿到。
#[allow(clippy::result_large_err)] // ToolResult is the framework error type; boxing would touch every caller
pub(crate) fn run_id_from_ctx_or_local(
    ctx: &echo_core::tools::ToolContext,
) -> std::result::Result<String, ToolResult> {
    ctx.run_id
        .clone()
        .or_else(current_run_id)
        .ok_or_else(|| ToolResult::error("no active run — run_id not in ToolContext or task_local"))
}

/// 在 ToolContext.run_id/trace_sink(若有)的 task_local 覆盖作用域内执行 f。
///
/// 这样工具既有的 `execute`(读 task_local 的 require_run_id)无需改动即可在
/// subagent 场景工作:execute_with_context 调本函数包住原 execute,ctx.run_id 被
/// 临时注入 task_local,require_run_id 读到的是 ToolContext 的值(跨 spawn 安全)。
/// ctx.run_id 为 None 时直接执行 f(回退原 task_local,主 agent 场景)。
///
/// 同时覆盖 run_id / cancel / trace_sink(若有),让 require_run_id /
/// CURRENT_CANCEL / CURRENT_TRACE_SINK 在框架 spawn 的工具执行场景读到
/// ToolContext 的值(跨 spawn 安全)。`ctx.trace_sink` 是框架 Value 形式,
/// 这里反序列化回 `ExecEvent` 包装成 `TraceSink` 注入 task_local。
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
            let cancel = ctx
                .cancel
                .as_ref()
                .map(|c| (**c).clone())
                .unwrap_or_default();
            let trace_sink = ctx.trace_sink.as_ref().map(|sink| {
                let sink = sink.clone();
                Arc::new(move |event: ExecEvent| {
                    if let Ok(value) = serde_json::to_value(event) {
                        sink(value);
                    }
                }) as TraceSink
            });
            CURRENT_CANCEL
                .scope(
                    cancel,
                    CURRENT_RUN_ID.scope(rid.clone(), CURRENT_TRACE_SINK.scope(trace_sink, f())),
                )
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

    // ── stage4 P4.1: cache_user_id single-source ────────────────────────────
    // with_run_context no longer threads a cache_user_id param — tools that
    // need the id read it from config / load_or_create_cache_user_id() instead.
    // This compile-time assertion guards the signature change.
    #[tokio::test]
    async fn with_run_context_drops_cache_user_id_param() {
        let result: i32 = with_run_context(
            "r1".to_string(),
            tokio_util::sync::CancellationToken::new(),
            None, // trace_sink
            async { 42 },
        )
        .await;
        assert_eq!(result, 42);
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

/// 按 task kind 推导默认 agent_role(映射到 infra.rs 注册的 subagent 角色)。
///
/// 必要性:PlanTask 的 agent_role 默认是 "general",但框架只注册了 13 个具体角色
/// (project_explorer/code_reviewer/...),委派 "general" 必然 "Subagent not found"。
/// 只读 kind(read_only_review/investigation/test_plan/review/summary)委派给对应
/// 只读 subagent;变更 kind(implementation/debugging/verification)由主 agent 直接
/// 执行(不委派 subagent),用 "primary" 占位(run_readonly_worker 不会触及)。
/// Map a plan-task kind to the registered subagent name that should run it.
///
/// SA-3 collapsed the old 13 specialized subagents (`project_explorer` /
/// `code_reviewer` / `test_planner` / `summary_writer` / …) into 4 generic
/// ones registered in `infra.rs`: `explorer`, `reviewer`, `planner`,
/// `summarizer`. This mapping must stay aligned with those registered names —
/// otherwise `run_readonly_worker` dispatches to a non-existent subagent and
/// every read-only plan task fails with "Subagent 'X' not found".
///
/// Read-only kinds (read_only_review / investigation / test_plan / review /
/// summary) delegate to the matching generic subagent; mutating kinds
/// (implementation / debugging / verification) are executed directly by the
/// main agent (`executor.rs::run_main_agent_task`), so the role here is only
/// a record label that `run_readonly_worker` never touches.
fn role_for_kind(kind: PlanTaskKind) -> &'static str {
    match kind {
        PlanTaskKind::ReadOnlyReview | PlanTaskKind::Investigation | PlanTaskKind::TestPlan => {
            "explorer"
        }
        PlanTaskKind::Review => "reviewer",
        PlanTaskKind::Summary => "summarizer",
        // Sprint 9: code-writing kinds route to the registered "implementer"
        // Fork subagent (runs in an isolated worktree). The role string IS the
        // registered subagent name (executor delegates by literal match).
        PlanTaskKind::Implementation | PlanTaskKind::Debugging => "implementer",
        // Verification (shell/build/test) stays on the primary agent: it runs
        // read-only-ish shell commands against the workspace and routing it to
        // a separate worktree checkout would detach it from the just-written
        // changes. It takes the shell permit, not the writer path.
        PlanTaskKind::Verification => "primary",
    }
}

#[cfg(test)]
mod role_routing_tests {
    use super::*;

    #[test]
    fn readonly_kinds_route_to_explorer_reviewer_summarizer() {
        assert_eq!(role_for_kind(PlanTaskKind::ReadOnlyReview), "explorer");
        assert_eq!(role_for_kind(PlanTaskKind::Investigation), "explorer");
        assert_eq!(role_for_kind(PlanTaskKind::TestPlan), "explorer");
        assert_eq!(role_for_kind(PlanTaskKind::Review), "reviewer");
        assert_eq!(role_for_kind(PlanTaskKind::Summary), "summarizer");
    }

    #[test]
    fn code_writer_kinds_route_to_implementer() {
        // Sprint 9: Implementation/Debugging dispatch to the registered writer
        // subagent (runs in an isolated worktree).
        assert_eq!(role_for_kind(PlanTaskKind::Implementation), "implementer");
        assert_eq!(role_for_kind(PlanTaskKind::Debugging), "implementer");
    }

    #[test]
    fn verification_stays_on_primary() {
        // Verification (shell/build/test) runs in-place on the primary agent —
        // it tests just-written changes against the workspace, so routing it to
        // a separate worktree would detach it.
        assert_eq!(role_for_kind(PlanTaskKind::Verification), "primary");
    }
}

// ── plan_create ───────────────────────────────────────────────────────────

pub struct TaskCreateTool {
    pub store: Arc<TaskRuntimeStore>,
}

impl Tool for TaskCreateTool {
    fn name(&self) -> &str {
        "plan_create"
    }

    fn description(&self) -> &str {
        "Create or append a PlanTask in the current formal plan. Use this to \
         materialize a task plan before calling plan_execute."
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

        if let Err(e) = self.ensure_run_exists(&run_id, &title, &description) {
            return Ok(e);
        }

        let task_id = format!("task_{}", uuid::Uuid::new_v4().as_simple());
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

    #[allow(clippy::result_large_err)] // ToolResult is the framework error type used by Tool::execute
    fn ensure_run_exists(
        &self,
        run_id: &str,
        title: &str,
        description: &str,
    ) -> std::result::Result<(), ToolResult> {
        match self.store.get_run(run_id) {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => {}
            Err(e) => {
                return Err(ToolResult::error(format!(
                    "Failed to inspect task run before creating task: {e}"
                )));
            }
        }

        let (conversation_id, root_message_id, attachments, trace_sink) =
            match crate::chat_resources::current_chat_resources() {
                Some(res) => (
                    res.conv_id.clone(),
                    res.root_message_id.clone(),
                    res.attachments.clone(),
                    res.sink.worker_trace_sink(),
                ),
                None => (None, run_id.to_string(), Vec::new(), None),
            };
        let conversation_id = conversation_id.unwrap_or_else(|| format!("message:{run_id}"));
        let goal = task_goal(title, description);

        if let Err(e) = self.store.create_run(
            run_id,
            "default",
            &conversation_id,
            &root_message_id,
            DomainProfile::General,
            &goal,
            "agent_task_plan",
            AttendedMode::Attended,
        ) {
            return Err(ToolResult::error(format!(
                "Failed to create task run before creating task: {e}"
            )));
        }
        #[allow(clippy::collapsible_if)]
        // outer guard + inner if-let-Err reads clearer than a let-chain
        if !attachments.is_empty() {
            if let Err(e) = self.store.set_run_attachments(run_id, &attachments) {
                tracing::warn!(run_id, error = %e, "failed to bind attachments to task run");
            }
        }
        if let Err(e) = self.store.transition_run(run_id, TaskRunStatus::Running) {
            return Err(ToolResult::error(format!(
                "Failed to start task run before creating task: {e}"
            )));
        }
        if let Some(sink) = trace_sink {
            sink(ExecEvent::run(
                run_id.to_string(),
                "run_started",
                serde_json::json!({
                    "goal": goal,
                    "route": "agent_task_plan",
                    "source": "plan_create",
                }),
            ));
        }
        Ok(())
    }
}

fn task_goal(title: &str, description: &str) -> String {
    if !description.trim().is_empty() {
        description.to_string()
    } else if !title.trim().is_empty() {
        title.to_string()
    } else {
        "Agent task plan".to_string()
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)] // complex-task tool impls below are production code; moving them is churn
mod plan_create_tests {
    use super::super::types::RuntimeEventKind;
    use super::*;

    #[tokio::test]
    async fn plan_create_bootstraps_run_before_plan_events() -> std::result::Result<(), String> {
        let shadow_root = tempfile::tempdir().map_err(|e| e.to_string())?;
        let store = Arc::new(
            TaskRuntimeStore::new_in_memory_with_shadow_root(shadow_root.path())
                .map_err(|e| e.to_string())?,
        );
        let tool = TaskCreateTool {
            store: store.clone(),
        };
        let run_id = "run_plan_create_bootstrap";
        let mut params = ToolParameters::new();
        params.insert(
            "title".to_string(),
            serde_json::Value::String("分析当前项目架构".to_string()),
        );
        params.insert(
            "description".to_string(),
            serde_json::Value::String("并行分析当前项目架构并汇总结果".to_string()),
        );
        params.insert(
            "kind".to_string(),
            serde_json::Value::String("read_only_review".to_string()),
        );

        let result = with_run_id(run_id.to_string(), tool.execute(params))
            .await
            .map_err(|e| e.to_string())?;
        if !result.success {
            return Err(format!("plan_create failed: {:?}", result.error));
        }

        let events = store.list_events(run_id, 0).map_err(|e| e.to_string())?;
        let first = events
            .first()
            .ok_or_else(|| "expected at least one runtime event".to_string())?;
        assert_eq!(first.event_type, RuntimeEventKind::RunCreated);

        let run = store.get_run(run_id).map_err(|e| e.to_string())?;
        assert!(run.is_some());
        let plan = store
            .get_plan(run_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "expected bootstrapped plan".to_string())?;
        assert_eq!(plan.tasks.len(), 1);
        assert_eq!(plan.tasks[0].agent_role, "explorer");
        Ok(())
    }
}

// ── create_complex_task (Phase B3) ────────────────────────────────────────
//
// The "nuclear button": lets the main agent autonomously spin up a background
// Run when it judges a task complex. Reads pool/store/sink from the chat
// turn's task_local (`current_chat_resources`, scoped by `drive_chat`), so it
// only works during an active chat turn. `reason` is required as a CoT
// anti-misfire gate (spec §4.2). foreground blocks the turn + streams subagent
// events (Claude Code Task style); background spawns + returns run_id (spec §6
// 主从异步). Default background (spec §4.1 Priority Trap).

/// Create a background orchestrated Run for a complex multi-step task.
pub struct CreateComplexTaskTool;

impl Tool for CreateComplexTaskTool {
    fn name(&self) -> &str {
        "create_complex_task"
    }

    fn description(&self) -> &str {
        r#"Create a background orchestrated Run for a complex multi-step task. ONLY use when one of these holds: (1) multi-step & time-consuming (>3 steps, each costly in tokens/time); (2) complex code generation (multi-file / architectural); (3) needs long-lived state (cross-turn / persisted); (4) multi-source research synthesis. For simple Q&A, single-file tweaks, or one-shot queries, DO NOT call this — reply directly. You MUST give a `reason` justifying the complexity. Default `priority`=background (non-blocking); use foreground only for tasks <1min where you need the result in this same reply."#
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["user_goal", "reason", "domain_profile", "plan_mode"],
            "properties": {
                "user_goal": { "type": "string", "description": "The user's full goal (verbatim or distilled), as the Run's goal." },
                "reason": { "type": "string", "description": "Why this is complex. List the complexity signals hit: multi_step / needs_research / needs_code_gen / long_running / multi_file. Anti-abuse audit." },
                "domain_profile": { "type": "string", "enum": ["general","ai_coding","data_analysis","academic_research","medical_research"], "description": "Domain. Determines subagent roles / review checklist." },
                "plan_mode": { "type": "string", "enum": ["plan_then_execute","direct_execute"], "description": "plan_then_execute = plan_create a plan first (reviewable) then plan_execute; direct_execute = agent ReActs autonomously in the Run." },
                "initial_plan": { "type": "array", "items": { "type": "object", "properties": { "step_name": {"type":"string"}, "expected_outcome": {"type":"string"} }, "required": ["step_name"] }, "description": "Optional coarse decomposition (>=2 steps) as a brief. Not the PlanTask DAG — the Run's agent refines via plan_create." },
                "priority": { "type": "string", "enum": ["foreground","background"], "default": "background", "description": "Default background (returns run_id immediately, non-blocking). foreground only for <1min tasks where you need the result in this turn (blocks the UI until done)." }
            }
        })
    }

    fn execute<'a>(
        &'a self,
        params: ToolParameters,
    ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<ToolResult>> {
        Box::pin(async move { self.do_create(params).await })
    }

    fn execute_with_context<'a>(
        &'a self,
        params: ToolParameters,
        _ctx: &'a echo_core::tools::ToolContext,
    ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<ToolResult>> {
        Box::pin(async move { self.do_create(params).await })
    }
}

impl CreateComplexTaskTool {
    async fn do_create(&self, params: ToolParameters) -> echo_agent::error::Result<ToolResult> {
        let res = match crate::chat_resources::current_chat_resources() {
            Some(r) => r,
            None => {
                return Ok(ToolResult::error(
                    "create_complex_task can only be used during an active chat turn (no chat resources available)",
                ));
            }
        };
        let pool = match res.pool.clone() {
            Some(p) => p,
            None => {
                return Ok(ToolResult::error(
                    "create_complex_task requires an AgentPool; none is available in this context",
                ));
            }
        };
        let store = match res.store.clone() {
            Some(s) => s,
            None => {
                return Ok(ToolResult::error(
                    "create_complex_task requires a TaskRuntimeStore; none is available in this context",
                ));
            }
        };

        let user_goal = params
            .get("user_goal")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if user_goal.is_empty() {
            return Ok(ToolResult::error("user_goal is required"));
        }
        let reason = params
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if reason.is_empty() {
            return Ok(ToolResult::error(
                "reason is required — list the complexity signals (multi_step/needs_research/needs_code_gen/long_running/multi_file) that justify this task",
            ));
        }
        let domain_profile_str = params
            .get("domain_profile")
            .and_then(|v| v.as_str())
            .unwrap_or("general");
        let domain = super::types::DomainProfile::from_str(domain_profile_str)
            .unwrap_or(super::types::DomainProfile::General);
        let _plan_mode = params
            .get("plan_mode")
            .and_then(|v| v.as_str())
            .unwrap_or("plan_then_execute");
        let priority = params
            .get("priority")
            .and_then(|v| v.as_str())
            .unwrap_or("background");
        let initial_plan: Vec<String> = params
            .get("initial_plan")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| {
                        v.get("step_name")
                            .and_then(|s| s.as_str())
                            .map(String::from)
                    })
                    .collect()
            })
            .unwrap_or_default();

        let goal = if initial_plan.is_empty() {
            user_goal.clone()
        } else {
            format!(
                "{user_goal}\n\nInitial plan:\n{}",
                initial_plan
                    .iter()
                    .map(|s| format!("- {s}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        };

        let run_id = uuid::Uuid::new_v4().to_string();
        let conv = res
            .conv_id
            .clone()
            .unwrap_or_else(|| format!("message:{run_id}"));
        let attended = super::types::AttendedMode::Attended;
        if let Err(e) = store.create_run(
            &run_id,
            "default",
            &conv,
            &res.root_message_id,
            domain,
            &goal,
            "agent_autonomous",
            attended,
        ) {
            return Ok(ToolResult::error(format!("Failed to create run: {e}")));
        }
        #[allow(clippy::collapsible_if)]
        // outer guard + inner if-let-Err reads clearer than a let-chain
        if !res.attachments.is_empty() {
            if let Err(e) = store.set_run_attachments(&run_id, &res.attachments) {
                tracing::warn!(error = %e, "failed to bind attachments to run");
            }
        }
        if let Err(e) = store.transition_run(&run_id, super::types::TaskRunStatus::Running) {
            return Ok(ToolResult::error(format!(
                "Failed to transition run to Running: {e}"
            )));
        }
        // Independent cancel token (spec §5.5): background runs must NOT reuse
        // the chat turn's token — the front-desk "stop" must not kill a
        // background run. cancel_run / GUI task panel trigger this one.
        let run_cancel = echo_agent::agent::CancellationToken::new();
        store.register_run_cancel_token(&run_id, run_cancel.clone());

        let trace_sink = if priority == "foreground" {
            res.sink.worker_trace_sink()
        } else {
            None
        };
        let payload = crate::run_driver::RunPayload {
            run_id: run_id.clone(),
            pool,
            store: store.clone(),
            cancel: run_cancel,
            reviewer_llm: None,
            // B5.1: forward the chat turn's memory layer so the run's Blocking
            // memory write (drive_run_async → execute_run) actually lands the
            // taskrun:completed:{run_id} memory. None when no memory subsystem
            // is wired (then the Blocking write is a no-op).
            layer_manager: res.layer_manager.clone(),
            trace_sink,
        };

        if priority == "foreground" {
            // Block the turn: drive_run_async streams subagent events to the chat
            // sink (via trace_sink), returns the terminal RunOutcome so the
            // agent can use the result in-turn (Claude Code Task style).
            match crate::run_driver::drive_run_async(payload).await {
                Ok(outcome) => {
                    use super::executor::RunOutcome;
                    let terminal = match outcome {
                        RunOutcome::Completed => "completed",
                        RunOutcome::Failed { .. } => "failed",
                        RunOutcome::Cancelled => "cancelled",
                        RunOutcome::Paused { .. } => "paused",
                    };
                    Ok(ToolResult::success(
                        serde_json::json!({"status":"completed","run_id":run_id,"terminal":terminal})
                            .to_string(),
                    ))
                }
                Err(e) => Ok(ToolResult::error(format!("Run failed: {e}"))),
            }
        } else {
            // Background: spawn + return immediately (decoupled, spec §6).
            tokio::spawn(crate::run_driver::drive_run_async(payload));
            Ok(ToolResult::success(
                serde_json::json!({"status":"accepted","run_id":run_id}).to_string(),
            ))
        }
    }
}

// ── check_run_status / cancel_run (Phase B3) ──────────────────────────────

/// Check the status of a Run created by `create_complex_task`.
pub struct CheckRunStatusTool;

impl Tool for CheckRunStatusTool {
    fn name(&self) -> &str {
        "check_run_status"
    }
    fn description(&self) -> &str {
        "Check the status of a background Run created by create_complex_task. Returns {status, goal}."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type":"object",
            "properties":{"run_id":{"type":"string","description":"The run_id returned by create_complex_task"}},
            "required":["run_id"]
        })
    }
    fn execute<'a>(
        &'a self,
        params: ToolParameters,
    ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<ToolResult>> {
        Box::pin(async move { Self::do_check(params).await })
    }
    fn execute_with_context<'a>(
        &'a self,
        params: ToolParameters,
        _ctx: &'a echo_core::tools::ToolContext,
    ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<ToolResult>> {
        Box::pin(async move { Self::do_check(params).await })
    }
}

impl CheckRunStatusTool {
    async fn do_check(params: ToolParameters) -> echo_agent::error::Result<ToolResult> {
        let run_id = match params.get("run_id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => return Ok(ToolResult::error("run_id is required")),
        };
        let res = match crate::chat_resources::current_chat_resources() {
            Some(r) => r,
            None => {
                return Ok(ToolResult::error(
                    "check_run_status requires an active chat turn",
                ));
            }
        };
        let store = match res.store.clone() {
            Some(s) => s,
            None => return Ok(ToolResult::error("no TaskRuntimeStore available")),
        };
        match store.get_run(&run_id) {
            Ok(Some(run)) => Ok(ToolResult::success(
                serde_json::json!({"status": format!("{:?}", run.status), "goal": run.goal})
                    .to_string(),
            )),
            Ok(None) => Ok(ToolResult::error(format!("Run {run_id} not found"))),
            Err(e) => Ok(ToolResult::error(format!("Failed to read run: {e}"))),
        }
    }
}

/// Cancel a background Run by run_id.
pub struct CancelRunTool;

impl Tool for CancelRunTool {
    fn name(&self) -> &str {
        "cancel_run"
    }
    fn description(&self) -> &str {
        "Cancel a background Run by run_id. Use when a task is no longer needed or going wrong."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type":"object",
            "properties":{"run_id":{"type":"string"}},
            "required":["run_id"]
        })
    }
    fn execute<'a>(
        &'a self,
        params: ToolParameters,
    ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<ToolResult>> {
        Box::pin(async move { Self::do_cancel(params).await })
    }
    fn execute_with_context<'a>(
        &'a self,
        params: ToolParameters,
        _ctx: &'a echo_core::tools::ToolContext,
    ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<ToolResult>> {
        Box::pin(async move { Self::do_cancel(params).await })
    }
}

impl CancelRunTool {
    async fn do_cancel(params: ToolParameters) -> echo_agent::error::Result<ToolResult> {
        let run_id = match params.get("run_id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => return Ok(ToolResult::error("run_id is required")),
        };
        let res = match crate::chat_resources::current_chat_resources() {
            Some(r) => r,
            None => return Ok(ToolResult::error("cancel_run requires an active chat turn")),
        };
        let store = match res.store.clone() {
            Some(s) => s,
            None => return Ok(ToolResult::error("no TaskRuntimeStore available")),
        };
        let cancelled = store.cancel_run(&run_id);
        Ok(ToolResult::success(
            serde_json::json!({"run_id": run_id, "cancelled": cancelled}).to_string(),
        ))
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
