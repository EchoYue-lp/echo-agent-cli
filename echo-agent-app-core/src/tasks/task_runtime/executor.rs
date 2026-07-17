//! DAG scheduler for TaskRuntime runs.
//!
//! Converts an approved `TaskPlan` into a dependency graph and executes it,
//! honoring the plan's parallelism rules:
//!
//! - read-only tasks (read_only_review, investigation, test_plan, review,
//!   summary) run concurrently up to `max_concurrent_workers`, each delegated
//!   to a registered subagent role via `delegate_to_agent_with_cancel` (fork
//!   mode → isolated instance under the executor's semaphore, NOT the primary
//!   agent's execution_mutex, so they parallelize);
//! - implementation / debugging / verification tasks serialize (acquire the
//!   write lock) and run on the PRIMARY agent directly (never delegated to a
//!   read-only worker);
//! - the overall worker count is capped by `ConcurrencyLimits`.
//!
//! Cancellation: each dispatched task gets a child of the parent run's
//! CancellationToken. Read-only delegation propagates cancel through
//! `delegate_to_agent_with_cancel`; mutating tasks race `Agent::execute`
//! against the cancel token. Cancelling the run therefore cancels every
//! in-flight task.
//!
//! Guarantees:
//! - the run transitions Running → (Completed | Failed | Cancelled | Paused);
//! - every task boundary writes a RuntimeTaskEvent + updates the todo projection;
//! - implementation/debugging tasks pass a review gate before being marked
//!   Completed; a failing review either re-queues a fix task or trips the
//!   circuit breaker (Paused);
//! - cancellation propagates to all in-flight tasks;
//! - a failed task marks itself Failed but lets already-running siblings
//!   finish (the run ends Failed); downstream tasks are skipped.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use echo_agent::agent::{Agent, AgentEvent, CancellationToken};
use futures::StreamExt;
use tokio::sync::{Mutex as TokioMutex, OwnedMutexGuard, Semaphore};

use super::store::{StoreError, TaskRuntimeStore};
use super::types::*;

pub use echo_agent::tasks::ConcurrencyLimits;

/// A lightweight execution-flow event emitted to the frontend via the unified
/// `execution://event` Tauri channel (kind="subagent" for the main agent's
/// thinking/tool/token stream, kind="run" for run lifecycle).
///
/// Replaces the old `WorkerTraceEvent`/`WorkerTraceEventKind` pair (Phase 4c of
/// the Subagent unification). The `event` field is a string (e.g.
/// `"tool_started"`, `"run_completed"`) matching the frontend's
/// `SubagentRunEventKind`; `payload` carries event-specific fields
/// (`content`/`name`/`args`/...) as a flat JSON object.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExecEvent {
    pub run_id: String,
    /// `None` for run-level events (RunStarted/Completed/...), `Some(task_id)`
    /// for task-scoped events (the main agent's thinking/tool/token stream).
    pub task_id: Option<String>,
    pub event: String,
    pub agent: Option<String>,
    pub payload: serde_json::Value,
}

impl ExecEvent {
    /// Construct a run-level event (no task_id).
    pub fn run(run_id: impl Into<String>, event: &'static str, payload: serde_json::Value) -> Self {
        Self {
            run_id: run_id.into(),
            task_id: None,
            event: event.to_string(),
            agent: None,
            payload,
        }
    }

    /// Construct a task-scoped event (carries task_id as the synthetic
    /// subagent_run_id for the main agent's execution flow).
    pub fn for_task(
        run_id: impl Into<String>,
        task_id: impl Into<String>,
        event: &'static str,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            run_id: run_id.into(),
            task_id: Some(task_id.into()),
            event: event.to_string(),
            agent: None,
            payload,
        }
    }

    /// Attach the agent/role name. Builder-style for call-site readability.
    pub fn with_agent(mut self, agent: impl Into<String>) -> Self {
        self.agent = Some(agent.into());
        self
    }

    /// No-op kept for call-site compatibility with the old
    /// `WorkerTraceEvent::with_title` chain. The frontend derives a display
    /// label from `agent` (falling back to subagent_run_id), so a separate
    /// title field is no longer needed on the wire.
    #[allow(clippy::unused_self)]
    pub fn with_title(self, _title: impl Into<String>) -> Self {
        self
    }

    /// No-op kept for call-site compatibility (the old `WorkerTraceEvent`
    /// carried a separate `task` field; the frontend now reads the task brief
    /// from the run's plan, so this field is dropped on the wire).
    #[allow(clippy::unused_self)]
    pub fn with_task(self, _task: impl Into<String>) -> Self {
        self
    }
}

/// Sink closure that receives [`ExecEvent`]s. The GUI's `TauriChatSink` provides
/// one that re-emits each event onto `execution://event`; non-GUI modes return
/// `None` (events dropped, functionality unaffected).
pub type ExecSink = Arc<dyn Fn(ExecEvent) + Send + Sync>;

/// Emit `ev` to `sink` if present. Single chokepoint so every emit site is
/// uniform and grep-friendly.
fn emit_exec(sink: Option<&ExecSink>, ev: ExecEvent) {
    if let Some(sink) = sink {
        sink(ev);
    }
}

/// Outcome of executing a whole run.
#[derive(Debug, Clone)]
pub enum RunOutcome {
    Completed,
    Failed {
        failed_task_id: String,
        error: String,
    },
    Cancelled,
    /// A task failed and the run is paused for user/agent decision. The
    /// failed task is marked `Failed`; downstream dependents are `Blocked`.
    /// The run is transitioned to `Paused`. The user can retry, skip, or
    /// edit the plan before resuming.
    Paused {
        failed_task_id: String,
        error: String,
    },
}

/// Error returned by the executor.
#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    #[error("run {0} not found")]
    RunNotFound(String),
    #[error("run {0} has no plan")]
    NoPlan(String),
    #[error("run {0} is in state {1:?}, expected Running")]
    NotRunning(String, TaskRunStatus),
    #[error("primary agent required to dispatch subagents")]
    NoAgent,
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("subagent dispatch failed: {0}")]
    Delegate(String),
    #[error("subagent execution failed: {0}")]
    Worker(String),
    #[error("{0}")]
    Other(String),
}

/// Execute a planned run to completion.
///
/// The caller (a Tauri command) holds the `AppState`, the store, and the
/// optional `AgentPool`. Execution is driven on the provided runtime; the
/// caller typically `tokio::spawn`s this and lets it run independently of the
/// chat stream (so a long run does not block the GUI, per plan §4).
#[allow(clippy::too_many_arguments)] // many typed handles + concurrency primitives; grouping would fragment the read path
pub async fn execute_run(
    store: Arc<TaskRuntimeStore>,
    primary_agent: Option<crate::agent_handle::AgentHandle>,
    reviewer_llm: Option<Arc<dyn echo_agent::llm::LlmClient>>,
    layer_manager: Option<Arc<echo_agent::evolution::MemoryLayerManager>>,
    run_store: Option<Arc<dyn echo_agent::trace::RunStore>>,
    trace_sink: Option<ExecSink>,
    run_id: &str,
    parent_cancel: CancellationToken,
    memory_policy: super::memory_bridge::MemoryPolicy,
) -> Result<RunOutcome, ExecError> {
    let run = store
        .get_run(run_id)?
        .ok_or(ExecError::RunNotFound(run_id.to_string()))?;
    // The caller must have transitioned Pending → Running before spawning
    // the executor. Here we only accept Running.
    if run.status != TaskRunStatus::Running {
        return Err(ExecError::NotRunning(run_id.to_string(), run.status));
    }
    let initial_plan = store
        .get_plan(run_id)?
        .ok_or(ExecError::NoPlan(run_id.to_string()))?;
    tracing::info!(
        run_id = %run_id,
        task_count = initial_plan.tasks.len(),
        status = %run.status.as_str(),
        route = %run.route,
        "task_runtime: execute_run start"
    );
    emit_exec(
        trace_sink.as_ref(),
        ExecEvent::run(
            run_id.to_string(),
            "run_started",
            serde_json::json!({
                "goal": &run.goal,
                "conversation_id": &run.conversation_id,
                "mode": "task_runtime",
            }),
        ),
    );

    let primary_agent = primary_agent.ok_or(ExecError::NoAgent)?;
    let limits = ConcurrencyLimits::default();

    let mut drain_cycle = 0usize;
    let outcome = loop {
        let plan = store
            .get_plan(run_id)?
            .ok_or(ExecError::NoPlan(run_id.to_string()))?;
        let unresolved_count = plan
            .tasks
            .iter()
            .filter(|task| {
                !matches!(
                    task.status,
                    TodoStatus::Completed | TodoStatus::Failed | TodoStatus::Skipped
                )
            })
            .count();
        if unresolved_count == 0 {
            break Ok(RunOutcome::Completed);
        }
        tracing::info!(
            run_id = %run_id,
            drain_cycle,
            task_count = plan.tasks.len(),
            unresolved_count,
            "task_runtime: drain plan snapshot"
        );

        let outcome = run_dag(
            store.clone(),
            RealTaskDispatcher {
                primary_agent: primary_agent.clone(),
            },
            reviewer_llm.clone(),
            run_id,
            plan.tasks,
            limits,
            parent_cancel.clone(),
            trace_sink.clone(),
        )
        .await;

        if matches!(outcome, Ok(RunOutcome::Completed)) && has_unresolved_tasks(&store, run_id) {
            // Inline plan_execute calls in the same LLM tool batch can append
            // tasks while this executor is already running. The holder of the
            // per-run execution lock is the authoritative drainer, so it must
            // re-read the plan and keep going instead of handing the tail to a
            // later plan_execute call.
            drain_cycle = drain_cycle.saturating_add(1);
            tracing::info!(
                run_id = %run_id,
                drain_cycle,
                "task_runtime: appended tasks detected after completed snapshot; continuing drain"
            );
            continue;
        }

        break outcome;
    };
    let outcome = match outcome {
        Ok(RunOutcome::Completed) => {
            let blockers = run_completion_blockers(&store, run_id);
            if blockers.is_empty() {
                Ok(RunOutcome::Completed)
            } else {
                Ok(RunOutcome::Failed {
                    failed_task_id: "<completion_gate>".to_string(),
                    error: blockers.join("; "),
                })
            }
        }
        other => other,
    };

    // Reflect the outcome on the run state. Each branch also writes a trace
    // Run record when a RunStore is available.
    match &outcome {
        Ok(RunOutcome::Completed) => {
            emit_exec(
                trace_sink.as_ref(),
                ExecEvent::run(
                    run_id.to_string(),
                    "run_completed",
                    serde_json::json!({ "status": "completed" }),
                ),
            );
            let _ = store.transition_run(run_id, TaskRunStatus::Completed);
            save_trace(
                run_store.as_ref(),
                run_id,
                &run.goal,
                &run.conversation_id,
                "completed",
            );
            super::memory_bridge::write_memory_candidate_dispatch(
                memory_policy,
                layer_manager.as_ref(),
                &store,
                super::memory_bridge::MemoryEvent::RunCompleted {
                    run_id: run_id.to_string(),
                    goal: run.goal.clone(),
                },
            )
            .await;
        }
        Ok(RunOutcome::Failed {
            failed_task_id,
            error,
        }) => {
            emit_exec(
                trace_sink.as_ref(),
                ExecEvent::run(
                    run_id.to_string(),
                    "run_failed",
                    serde_json::json!({
                        "failed_task_id": failed_task_id,
                        "error": error,
                    }),
                ),
            );
            // Running → Failed is legal. Use None for synthetic task ids
            // (<none>/<join>) to avoid orphan task_id events.
            let tid = if failed_task_id.starts_with('<') {
                None
            } else {
                Some(failed_task_id.as_str())
            };
            let _ = store.note(run_id, tid, &format!("run failed: {error}"));
            let _ = store.transition_run(run_id, TaskRunStatus::Failed);
            save_trace(
                run_store.as_ref(),
                run_id,
                &run.goal,
                &run.conversation_id,
                "failed",
            );
        }
        Ok(RunOutcome::Cancelled) => {
            emit_exec(
                trace_sink.as_ref(),
                ExecEvent::run(
                    run_id.to_string(),
                    "run_cancelled",
                    serde_json::json!({ "status": "cancelled" }),
                ),
            );
            if let Ok(todos) = store.list_todos(run_id) {
                for todo in todos
                    .into_iter()
                    .filter(|todo| todo.status == TodoStatus::Running)
                {
                    let _ = store.set_task_status(
                        run_id,
                        &todo.task_id,
                        TodoStatus::Skipped,
                        None,
                        Some("cancelled with parent run"),
                    );
                }
            }
            let _ = store.transition_run(run_id, TaskRunStatus::Cancelled);
            save_trace(
                run_store.as_ref(),
                run_id,
                &run.goal,
                &run.conversation_id,
                "cancelled",
            );
            super::memory_bridge::write_memory_candidate_dispatch(
                memory_policy,
                layer_manager.as_ref(),
                &store,
                super::memory_bridge::MemoryEvent::RunCancelledByUser {
                    run_id: run_id.to_string(),
                    goal: run.goal.clone(),
                },
            )
            .await;
        }
        Ok(RunOutcome::Paused {
            failed_task_id,
            error,
        }) => {
            emit_exec(
                trace_sink.as_ref(),
                ExecEvent::run(
                    run_id.to_string(),
                    "run_status_changed",
                    serde_json::json!({
                        "status": "paused",
                        "failed_task_id": failed_task_id,
                        "error": error,
                    }),
                ),
            );
            // run_dag or the control path already transitioned Running → Paused.
            // Any worker that was in flight no longer exists after cancellation,
            // so make it pending again for the resume drain.
            if let Ok(todos) = store.list_todos(run_id) {
                for todo in todos
                    .into_iter()
                    .filter(|todo| todo.status == TodoStatus::Running)
                {
                    let _ = store.set_task_status(
                        run_id,
                        &todo.task_id,
                        TodoStatus::Pending,
                        None,
                        Some("paused; pending resume"),
                    );
                }
            }
            let task_id = (!failed_task_id.starts_with('<')).then_some(failed_task_id.as_str());
            let _ = store.note(run_id, task_id, &format!("run paused: {error}"));
            save_trace(
                run_store.as_ref(),
                run_id,
                &run.goal,
                &run.conversation_id,
                "paused",
            );
        }
        Err(e) => {
            emit_exec(
                trace_sink.as_ref(),
                ExecEvent::run(
                    run_id.to_string(),
                    "run_failed",
                    serde_json::json!({ "error": e.to_string() }),
                ),
            );
            let _ = store.note(run_id, None, &format!("executor error: {e}"));
            // Running → Failed is legal even if some tasks were mid-flight.
            let _ = store.transition_run(run_id, TaskRunStatus::Failed);
        }
    }
    outcome
}

fn has_unresolved_tasks(store: &TaskRuntimeStore, run_id: &str) -> bool {
    store
        .get_plan(run_id)
        .ok()
        .flatten()
        .map(|plan| {
            plan.tasks.iter().any(|task| {
                !matches!(
                    task.status,
                    TodoStatus::Completed | TodoStatus::Failed | TodoStatus::Skipped
                )
            })
        })
        .unwrap_or(false)
}

fn run_completion_blockers(store: &TaskRuntimeStore, run_id: &str) -> Vec<String> {
    let mut blockers = Vec::new();
    let Some(plan) = store.get_plan(run_id).ok().flatten() else {
        blockers.push("run has no plan".to_string());
        return blockers;
    };
    for task in &plan.tasks {
        match task.status {
            TodoStatus::Completed => match store.get_summary(run_id, &task.id) {
                Ok(Some(summary)) => {
                    if let Err(issues) = validate_task_result(task, &summary.result) {
                        blockers.push(format!("task '{}': {}", task.title, issues.join("; ")));
                    }
                }
                Ok(None) => blockers.push(format!(
                    "task '{}' completed without a structured result",
                    task.title
                )),
                Err(error) => blockers.push(format!(
                    "task '{}' result could not be read: {error}",
                    task.title
                )),
            },
            TodoStatus::Skipped => {}
            status => blockers.push(format!("task '{}' is {}", task.title, status.as_str())),
        }
    }
    match store.list_recovery_blockers(run_id) {
        Ok(recovery) if !recovery.is_empty() => {
            blockers.push(format!("{} unresolved recovery blocker(s)", recovery.len()))
        }
        Err(error) => blockers.push(format!("recovery blockers could not be read: {error}")),
        _ => {}
    }
    blockers
}

fn validate_task_result(task: &PlanTask, result: &SubagentTaskResult) -> Result<(), Vec<String>> {
    let mut issues = Vec::new();
    if result.contract_version != 1 {
        issues.push("missing versioned Subagent result contract".to_string());
    }
    if result.status != SubagentRunStatus::Completed {
        issues.push(format!("terminal status is {}", result.status.as_str()));
    }
    if result.summary.trim().is_empty() {
        issues.push("summary is empty".to_string());
    }
    if !result.remaining_work.is_empty() {
        issues.push(format!(
            "remaining work: {}",
            result.remaining_work.join("; ")
        ));
    }
    for verification in &result.verification {
        if verification.status != SubagentVerificationStatus::Passed {
            issues.push(format!(
                "verification '{}' is {:?}",
                verification.check, verification.status
            ));
        }
    }
    for required in &task.verification {
        let matched = result.verification.iter().any(|verification| {
            verification.source == SubagentVerificationSource::Observed
                && verification.status == SubagentVerificationStatus::Passed
                && verification_matches(required, &verification.check)
        });
        if !matched {
            issues.push(format!(
                "required verification has no observed pass: {required}"
            ));
        }
    }
    for required in &task.required_artifacts {
        let matched = result.artifacts.iter().any(|artifact| {
            artifact_matches(required, &artifact.path)
                && artifact.available
                && artifact
                    .sha256
                    .as_deref()
                    .is_some_and(|hash| hash.chars().count() == 64)
                && artifact
                    .producer_execution_id
                    .as_deref()
                    .is_some_and(|id| !id.trim().is_empty())
        });
        if !matched {
            issues.push(format!(
                "required artifact is missing or lacks integrity metadata: {required}"
            ));
        }
    }
    if issues.is_empty() {
        Ok(())
    } else {
        Err(issues)
    }
}

fn verification_matches(required: &str, observed: &str) -> bool {
    let required = required.split_whitespace().collect::<Vec<_>>().join(" ");
    let observed = observed.split_whitespace().collect::<Vec<_>>().join(" ");
    !required.is_empty() && required.eq_ignore_ascii_case(&observed)
}

fn artifact_matches(required: &str, actual: &str) -> bool {
    let required = required.trim().replace('\\', "/");
    let actual = actual.trim().replace('\\', "/");
    !required.is_empty()
        && (actual == required
            || actual.ends_with(&format!("/{required}"))
            || std::path::Path::new(&actual)
                .file_name()
                .is_some_and(|name| name.to_string_lossy() == required))
}

fn build_contract_fix_task(task: &PlanTask, issues: &[String]) -> PlanTask {
    let mut fix = task.clone();
    fix.title = format!(
        "{} (result fix #{})",
        task.title,
        task.retry_count.saturating_add(1)
    );
    fix.description = format!(
        "The previous execution ended but its structured result did not satisfy the task contract. \
         Resolve every issue and return a complete versioned result:\n- {}\n\nOriginal task: {}",
        issues.join("\n- "),
        task.description
    );
    fix.parallel_group = None;
    fix.retry_count = task.retry_count.saturating_add(1);
    fix.status = TodoStatus::Pending;
    fix
}

/// Abstraction over how a single ready task is dispatched in the EKO runtime.
///
/// `run_dag` depends on this trait (not on `execute_task` directly) so the
/// scheduling core — frontier computation, dependency resolution, failure
/// propagation, cancellation, stall detection — can be unit-tested with a
/// deterministic mock dispatcher instead of a real LLM-backed agent. The
/// production implementation ([`RealTaskDispatcher`]) wraps `execute_task`.
///
/// The dispatcher is given the semaphores + file locks so it can honor the same
/// concurrency limits as the real path; mocks usually ignore them.
pub trait TaskDispatcher: Send + Sync {
    /// Execute `task` for `run_id`. Returns `(task_id, structured result)` on success or
    /// `(task_id, error)` on failure (matching `execute_task`'s contract).
    #[allow(clippy::too_many_arguments, clippy::type_complexity)] // semaphores/locks passed so mocks honor the same limits; boxed-future return is the worker contract
    fn dispatch(
        &self,
        store: Arc<TaskRuntimeStore>,
        context: echo_agent::tasks::TaskWorkerContext,
        task: PlanTask,
        worker_sem: Arc<Semaphore>,
        write_sem: Arc<Semaphore>,
        shell_sem: Arc<Semaphore>,
        llm_sem: Arc<Semaphore>,
        file_write_locks: Arc<std::sync::Mutex<HashMap<String, Arc<TokioMutex<()>>>>>,
        trace_sink: Option<ExecSink>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = TaskDispatchResult> + Send>>;

    /// Integrate a reviewed writer result into the authoritative workspace.
    /// Non-writer dispatchers use the default no-op implementation.
    #[allow(clippy::too_many_arguments)]
    fn integrate(
        &self,
        _store: Arc<TaskRuntimeStore>,
        _run_id: String,
        _task: PlanTask,
        _execution_id: String,
        _cancel: CancellationToken,
        _trace_sink: Option<ExecSink>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<Option<super::worktree::WorktreeIntegrationOutcome>, String>,
                > + Send,
        >,
    > {
        Box::pin(async { Ok(None) })
    }
}

/// Production worker: delegates to [`execute_task`] against the primary agent.
///
/// Note: the reviewer LLM is NOT held here — it is owned by `run_dag` itself
/// (the review gate runs at the `run_dag` level, after a worker returns). The
/// dispatcher only needs the agent + concurrency primitives.
pub struct RealTaskDispatcher {
    pub primary_agent: crate::agent_handle::AgentHandle,
}

impl TaskDispatcher for RealTaskDispatcher {
    fn dispatch(
        &self,
        store: Arc<TaskRuntimeStore>,
        context: echo_agent::tasks::TaskWorkerContext,
        task: PlanTask,
        worker_sem: Arc<Semaphore>,
        write_sem: Arc<Semaphore>,
        shell_sem: Arc<Semaphore>,
        llm_sem: Arc<Semaphore>,
        file_write_locks: Arc<std::sync::Mutex<HashMap<String, Arc<TokioMutex<()>>>>>,
        trace_sink: Option<ExecSink>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = TaskDispatchResult> + Send>> {
        let primary_agent = self.primary_agent.clone();
        Box::pin(async move {
            let run_id = context.run_id;
            let cancel = context.cancel;
            let delegation_policy = context.delegation_policy;
            // Scope run_id + cancel + trace_sink into task-local so worker-internal
            // tools (task_*/plan_execute, and their execute_with_context
            // fallback path) and L3 nested sub-workers can read them.
            // NOTE: trace_sink/cancel are also passed as explicit params to
            // execute_task (which uses them directly, not via task_local) — but
            // scoping them here keeps the task_local consistent for any code
            // path that reads CURRENT_TRACE_SINK/CURRENT_CANCEL directly.
            let sink_clone = trace_sink.clone();
            let cancel_clone = cancel.clone();
            super::task_tools::with_run_context(run_id.clone(), cancel_clone, sink_clone, async {
                execute_task(
                    store,
                    primary_agent,
                    worker_sem,
                    write_sem,
                    shell_sem,
                    llm_sem,
                    file_write_locks,
                    trace_sink,
                    run_id,
                    task,
                    cancel,
                    delegation_policy,
                )
                .await
            })
            .await
        })
    }

    fn integrate(
        &self,
        store: Arc<TaskRuntimeStore>,
        run_id: String,
        task: PlanTask,
        execution_id: String,
        cancel: CancellationToken,
        trace_sink: Option<ExecSink>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<Option<super::worktree::WorktreeIntegrationOutcome>, String>,
                > + Send,
        >,
    > {
        let primary_agent = self.primary_agent.clone();
        Box::pin(async move {
            if !matches!(
                task.kind,
                PlanTaskKind::Implementation | PlanTaskKind::Debugging
            ) {
                return Ok(None);
            }
            if cancel.is_cancelled() {
                return Err("cancelled before worktree integration".to_string());
            }

            let working_dir = primary_agent
                .read(|agent| agent.working_dir())
                .await
                .ok_or_else(|| "writer integration requires a Git working directory".to_string())?;
            let repo_root =
                tokio::task::spawn_blocking(move || super::worktree::git_repo_root(&working_dir))
                    .await
                    .map_err(|error| format!("failed to join repo-root lookup: {error}"))?
                    .map_err(|error| error.to_string())?;
            let merge_lock = super::worktree::repo_merge_lock(&repo_root);
            let _merge_guard = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err("cancelled while waiting for worktree integration".to_string()),
                guard = merge_lock.lock_owned() => guard,
            };
            if cancel.is_cancelled() {
                return Err("cancelled before worktree integration started".to_string());
            }

            let label = format!("{}-{}", task.agent_role, execution_id);
            let ownership = super::planner::file_ownership(&task);
            let branch = super::worktree::fork_branch_name(&label);
            let _ = store.note(
                &run_id,
                Some(&task.id),
                &format!("worktree integration started: execution={execution_id}, branch={branch}"),
            );
            emit_exec(
                trace_sink.as_ref(),
                ExecEvent::for_task(
                    run_id.clone(),
                    task.id.clone(),
                    "merge_started",
                    serde_json::json!({
                        "execution_id": execution_id,
                        "branch": branch,
                    }),
                )
                .with_agent(task.agent_role.clone())
                .with_title(task.title.clone()),
            );

            let task_id = task.id.clone();
            let execution_for_merge = execution_id.clone();
            let repo_for_merge = repo_root.clone();
            let label_for_merge = label.clone();
            let outcome = tokio::task::spawn_blocking(move || {
                super::worktree::integrate_fork_worktree(
                    &repo_for_merge,
                    &label_for_merge,
                    &task_id,
                    &execution_for_merge,
                    &ownership,
                )
            })
            .await
            .map_err(|error| format!("failed to join worktree integration: {error}"))?;

            match outcome {
                Ok(outcome) => {
                    let summary = outcome.summary();
                    let _ = store.note(&run_id, Some(&task.id), &summary);
                    if let Some(warning) = &outcome.cleanup_warning {
                        let _ = store.note(
                            &run_id,
                            Some(&task.id),
                            &format!("worktree cleanup warning: {warning}"),
                        );
                    }
                    emit_exec(
                        trace_sink.as_ref(),
                        ExecEvent::for_task(
                            run_id,
                            task.id.clone(),
                            "merge_completed",
                            serde_json::json!({
                                "execution_id": execution_id,
                                "integration_status": outcome.status.as_str(),
                                "branch": outcome.branch,
                                "path": outcome.path,
                                "changed_files": outcome.changed_files,
                                "merge_commit": outcome.merge_commit,
                                "cleanup_warning": outcome.cleanup_warning,
                            }),
                        )
                        .with_agent(task.agent_role)
                        .with_title(task.title),
                    );
                    Ok(Some(outcome))
                }
                Err(error) => {
                    let message = error.to_string();
                    let _ = store.note(
                        &run_id,
                        Some(&task.id),
                        &format!("worktree integration failed: {message}"),
                    );
                    emit_exec(
                        trace_sink.as_ref(),
                        ExecEvent::for_task(
                            run_id,
                            task.id.clone(),
                            "merge_failed",
                            serde_json::json!({
                                "execution_id": execution_id,
                                "branch": branch,
                                "error": message,
                            }),
                        )
                        .with_agent(task.agent_role)
                        .with_title(task.title),
                    );
                    Err(message)
                }
            }
        })
    }
}

type TaskDispatchResult = Result<(String, SubagentTaskResult), (String, String)>;

/// Pick the largest deterministic subset of the ready frontier that has no
/// writer ownership conflicts. Read-only tasks never consume ownership.
fn select_ownership_safe_wave(ready: Vec<PlanTask>) -> Vec<PlanTask> {
    let mut selected = Vec::new();
    let mut selected_writers: Vec<super::planner::FileOwnership> = Vec::new();
    for task in ready {
        let ownership = super::planner::file_ownership(&task);
        if matches!(ownership, super::planner::FileOwnership::ReadOnly) {
            selected.push(task);
            continue;
        }
        if selected_writers
            .iter()
            .all(|selected| !ownership.conflicts_with(selected))
        {
            selected_writers.push(ownership);
            selected.push(task);
        }
    }
    selected
}

/// Core DAG loop. Maintains a frontier of ready tasks and dispatches them
/// under the concurrency semaphores until all are done, the run is cancelled,
/// or a task fails.
#[allow(clippy::too_many_arguments)] // semaphores + stores + sinks all thread through; matches framework TaskExecutor style
async fn run_dag<W: TaskDispatcher + 'static>(
    store: Arc<TaskRuntimeStore>,
    worker: W,
    reviewer_llm: Option<Arc<dyn echo_agent::llm::LlmClient>>,
    run_id: &str,
    tasks: Vec<PlanTask>,
    limits: ConcurrencyLimits,
    parent_cancel: CancellationToken,
    trace_sink: Option<ExecSink>,
) -> Result<RunOutcome, ExecError> {
    // Wrap the worker in an Arc so each spawned task can clone the handle.
    let worker = Arc::new(worker);
    // Index tasks by id.
    let mut by_id: HashMap<String, PlanTask> =
        tasks.iter().map(|t| (t.id.clone(), t.clone())).collect();
    let runtime_tasks: Vec<echo_agent::tasks::RuntimeTask> =
        tasks.iter().map(PlanTask::to_runtime_task).collect();

    // Generic DAG bookkeeping lives in the framework. App-core still owns
    // store writes, review gates, event emission, and worker dispatch.
    let mut dag_state = echo_agent::tasks::DagExecutionState::from_tasks(&runtime_tasks);
    let mut failed_id: Option<String> = None;
    // Fix-task overrides produced by review gates, keyed by task id. A task
    // that fails review gets re-queued here with a bumped retry_count; the
    // next wave picks it up and re-runs it (possibly with a richer brief).
    let mut tasks_with_fixes: HashMap<String, PlanTask> = HashMap::new();

    let worker_sem = Arc::new(Semaphore::new(limits.max_concurrent_workers));
    let write_sem = Arc::new(Semaphore::new(limits.max_concurrent_writes));
    let shell_sem = Arc::new(Semaphore::new(limits.max_concurrent_shells));
    let llm_sem = Arc::new(Semaphore::new(limits.max_parallel_llm_calls));
    // G5: Per-file async mutex map. Non-overlapping files run in parallel
    // (max_concurrent_writes=4), overlapping files serialize on the same
    // per-file TokioMutex. Files are sorted before locking (see execute_task)
    // to prevent lock-ordering deadlocks.
    let file_write_locks: Arc<std::sync::Mutex<HashMap<String, Arc<TokioMutex<()>>>>> =
        Arc::new(std::sync::Mutex::new(HashMap::new()));

    // Loop until every task is resolved or the run aborts.
    loop {
        if parent_cancel.is_cancelled() {
            return Ok(interrupted_outcome(&store, run_id));
        }
        if let Some(id) = &failed_id {
            // A task failed: propagate Blocked to downstream dependents
            // (but NEVER overwrite a task that's already Failed).
            for task_id in dag_state.blocked_by_failures(&runtime_tasks) {
                if let Some(t) = by_id.get(&task_id) {
                    let _ = store.set_task_status(
                        run_id,
                        &t.id,
                        TodoStatus::Blocked,
                        None,
                        Some("blocked: upstream task failed"),
                    );
                }
            }
            // Check if ALL non-terminal tasks are Failed or Blocked — if so,
            // the run is unrecoverable and should fail outright. Otherwise,
            // pause for user/agent decision.
            let all_dead = dag_state.all_unfinished_failed_or_blocked(&runtime_tasks);
            let failed = by_id.get(id).cloned();
            let error = failed
                .map(|t| format!("task '{}' failed", t.title))
                .unwrap_or_else(|| "task failed".into());
            let unattended = store
                .get_run(run_id)
                .ok()
                .flatten()
                .is_some_and(|run| run.attended_mode == AttendedMode::Unattended);
            if all_dead || unattended {
                return Ok(RunOutcome::Failed {
                    failed_task_id: id.clone(),
                    error,
                });
            }
            // Pause the run for decision rather than failing outright.
            let _ = store.transition_run(run_id, TaskRunStatus::Paused);
            return Ok(RunOutcome::Paused {
                failed_task_id: id.clone(),
                error,
            });
        }
        if dag_state.all_completed(&runtime_tasks) {
            return Ok(RunOutcome::Completed);
        }

        // Refresh in_flight tasks from the store: any that reached a terminal
        // state (Completed/Failed/Skipped) while we were away are no longer
        // in-flight. Completed ones move into `completed`; Failed/Skipped are
        // treated as resolved (count toward the all_ids check). This is what
        // lets a sibling run_dag instance finish a task and this instance
        // observe it without re-dispatching.
        if !dag_state.in_flight.is_empty() {
            let live_plan = store.get_plan(run_id).ok().flatten();
            if let Some(plan) = live_plan {
                let live_runtime_tasks: Vec<echo_agent::tasks::RuntimeTask> =
                    plan.tasks.iter().map(PlanTask::to_runtime_task).collect();
                let refresh = dag_state.refresh_in_flight(&live_runtime_tasks);
                for id in refresh.failed {
                    if failed_id.is_none() {
                        failed_id = Some(id);
                    }
                }
            }
            if dag_state.all_completed(&runtime_tasks) {
                return Ok(RunOutcome::Completed);
            }
        }

        // Find ready tasks: not completed, not in_flight, all deps completed.
        // in_flight tasks are excluded so they aren't re-dispatched (they are
        // being driven by a sibling run_dag instance).
        let ready: Vec<PlanTask> = dag_state
            .ready_task_ids(&runtime_tasks)
            .into_iter()
            .filter_map(|id| {
                tasks_with_fixes
                    .get(&id)
                    .cloned()
                    .or_else(|| by_id.get(&id).cloned())
            })
            .collect();

        if ready.is_empty() {
            if !dag_state.in_flight.is_empty() {
                // Nothing ready but sibling run_dag instances are still
                // driving tasks. Wait briefly for them to make progress, then
                // loop back to re-check the store. Without this wait the loop
                // would spin hot.
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                continue;
            }
            // Nothing ready, nothing in-flight, and not all done → deadlock
            // (cycle or all remaining are blocked by the failed one).
            if dag_state.completed.len() + dag_state.failed.len() >= runtime_tasks.len() {
                continue;
            }
            // Genuine stall: record and fail.
            let _ = store.note(run_id, None, "DAG stalled: no ready tasks");
            return Ok(RunOutcome::Failed {
                failed_task_id: "<none>".into(),
                error: "DAG stalled with unfinished tasks (cycle or blocked)".into(),
            });
        }
        let ready_count = ready.len();
        let ready = select_ownership_safe_wave(ready);
        let ready_ids: Vec<String> = ready.iter().map(|t| t.id.clone()).collect();
        tracing::info!(
            run_id = %run_id,
            ready_count = ready_ids.len(),
            deferred_for_ownership = ready_count.saturating_sub(ready_ids.len()),
            ready_tasks = ?ready_ids,
            completed_count = dag_state.completed.len(),
            total_count = runtime_tasks.len(),
            "task_runtime: dispatching DAG wave"
        );

        // Dispatch each ready task. We run them concurrently up to the
        // semaphores; join all before recomputing the frontier.
        //
        // Cancellation: each task gets parent_cancel.clone() (NOT child_token —
        // child_token creates a separate subtree that parent cancellation does
        // NOT propagate into). With clone, parent_cancel.cancel() immediately
        // fires every worker's select! guard. If we detect cancellation
        // mid-wave, we abort remaining handles before returning Cancelled so
        // no orphan tasks keep writing files.
        let mut handles: Vec<_> = Vec::new();
        let mut wave_results: Vec<TaskDispatchResult> = Vec::new();
        for task in ready {
            let execution_id = format!("{}:{}", task.id, task.retry_count.saturating_add(1));
            match store.recoverable_worker_result(run_id, &task.id, &execution_id) {
                Ok(Some(result)) => {
                    tracing::info!(
                        run_id = %run_id,
                        task_id = %task.id,
                        execution_id,
                        "task_runtime: reusing durable worker result after restart"
                    );
                    let _ = store.note(
                        run_id,
                        Some(&task.id),
                        "reused completed worker result; continuing at review boundary",
                    );
                    wave_results.push(Ok((task.id.clone(), result)));
                    continue;
                }
                Ok(None) => {}
                Err(error) => tracing::warn!(
                    run_id = %run_id,
                    task_id = %task.id,
                    %error,
                    "failed to inspect durable worker result; dispatching normally"
                ),
            }
            let store = store.clone();
            let worker = worker.clone();
            let worker_sem = worker_sem.clone();
            let write_sem = write_sem.clone();
            let shell_sem = shell_sem.clone();
            let llm_sem = llm_sem.clone();
            let file_write_locks = file_write_locks.clone();
            let trace_sink = trace_sink.clone();
            // clone shares the same cancellation tree — parent cancel fires here.
            let context = echo_agent::tasks::TaskWorkerContext::new(run_id.to_string())
                .with_cancel(parent_cancel.clone())
                .with_concurrency_limits(limits)
                .with_delegation_policy(echo_agent::tasks::NestedDelegationPolicy {
                    can_spawn_subagents: true,
                    delegate_depth: 0,
                    max_delegate_depth: 2,
                });
            handles.push(tokio::spawn(async move {
                worker
                    .dispatch(
                        store,
                        context,
                        task,
                        worker_sem,
                        write_sem,
                        shell_sem,
                        llm_sem,
                        file_write_locks,
                        trace_sink,
                    )
                    .await
            }));
        }

        // Await the wave. Collect results; on cancellation, abort stragglers.
        let mut cancelled_mid_wave = false;
        for handle in &mut handles {
            if parent_cancel.is_cancelled() {
                cancelled_mid_wave = true;
                break;
            }
            match handle.await {
                Ok(r) => wave_results.push(r),
                Err(join_err) => {
                    wave_results.push(Err((
                        "<join>".to_string(),
                        format!("subagent task panicked: {join_err}"),
                    )));
                }
            }
        }
        if parent_cancel.is_cancelled() {
            cancelled_mid_wave = true;
        }
        if cancelled_mid_wave {
            // Abort any handles we didn't await so their workers stop ASAP.
            for handle in &mut handles {
                handle.abort();
            }
            return Ok(interrupted_outcome(&store, run_id));
        }

        // Process wave results: first failure wins for the error message, but
        // ALL failed tasks are marked Failed (not later overwritten to Skipped
        // by the skip logic, which now excludes the failed set).
        let mut wave_failed: Vec<String> = Vec::new();
        for result in wave_results {
            match result {
                Ok((id, result)) => {
                    // Review gate: implementation/debugging tasks must pass
                    // review before being marked Completed (plan §776-831).
                    // Read-only kinds are their own review → auto-pass.
                    let Some(task) = by_id.get(&id).cloned() else {
                        continue;
                    };
                    if let Err(issues) = validate_task_result(&task, &result) {
                        let reason = issues.join("; ");
                        if task.retry_count < task.max_retries {
                            let fix_task = build_contract_fix_task(&task, &issues);
                            if let Err(error) = store.update_plan_task(run_id, &fix_task) {
                                tracing::warn!(
                                    task_id = %fix_task.id,
                                    %error,
                                    "failed to persist result-contract retry task"
                                );
                            }
                            tasks_with_fixes.insert(id.clone(), fix_task.clone());
                            let _ = store.set_task_status(
                                run_id,
                                &id,
                                TodoStatus::Pending,
                                Some(&task.agent_role),
                                Some(&format!("result contract incomplete: {reason}")),
                            );
                            by_id.insert(id.clone(), fix_task);
                        } else {
                            let _ = store.set_task_status(
                                run_id,
                                &id,
                                TodoStatus::Failed,
                                Some(&task.agent_role),
                                Some(&format!("result contract rejected: {reason}")),
                            );
                            wave_failed.push(id.clone());
                            dag_state.failed.insert(id.clone());
                            if failed_id.is_none() {
                                failed_id = Some(id);
                            }
                        }
                        continue;
                    }
                    let summary = result.summary.clone();
                    let passed = run_review_gate(
                        store.clone(),
                        reviewer_llm.clone(),
                        run_id,
                        &task,
                        &summary,
                    )
                    .await;
                    let approved = match passed {
                        ReviewGateOutcome::Pass => true,
                        ReviewGateOutcome::NeedsFix(fix_task) => {
                            if let Err(e) = store.update_plan_task(run_id, &fix_task) {
                                tracing::warn!(
                                    task_id = %fix_task.id,
                                    error = %e,
                                    "failed to persist fix task; in-memory only"
                                );
                            }
                            tasks_with_fixes.insert(fix_task.id.clone(), fix_task.clone());
                            let _ = store.set_task_status(
                                run_id,
                                &id,
                                TodoStatus::Pending,
                                Some(
                                    by_id
                                        .get(&id)
                                        .map(|t| t.agent_role.as_str())
                                        .unwrap_or("unknown"),
                                ),
                                Some("re-queued after review"),
                            );
                            by_id.insert(id.clone(), fix_task);
                            false
                        }
                        ReviewGateOutcome::Suspend(reason) => {
                            let _ = store.note(
                                run_id,
                                Some(&id),
                                &format!("circuit breaker: {reason}"),
                            );
                            let _ = store.transition_run(run_id, TaskRunStatus::Paused);
                            return Ok(RunOutcome::Paused {
                                failed_task_id: id.clone(),
                                error: reason,
                            });
                        }
                        ReviewGateOutcome::Skipped => {
                            let _ = store.note(
                                run_id,
                                Some(&id),
                                "no reviewer LLM; auto-passing review gate",
                            );
                            true
                        }
                    };
                    if !approved {
                        continue;
                    }

                    let execution_id =
                        format!("{}:{}", task.id, task.retry_count.saturating_add(1));
                    match integrate_reviewed_task(
                        worker.clone(),
                        store.clone(),
                        run_id,
                        &task,
                        &execution_id,
                        &summary,
                        parent_cancel.clone(),
                        trace_sink.clone(),
                    )
                    .await
                    {
                        Ok(completion_summary) => {
                            let _ = store.set_task_status(
                                run_id,
                                &id,
                                TodoStatus::Completed,
                                Some(&task.agent_role),
                                Some(&completion_summary),
                            );
                            dag_state.completed.insert(id);
                        }
                        Err(error) => {
                            let _ = store.set_task_status(
                                run_id,
                                &id,
                                TodoStatus::Failed,
                                Some(&task.agent_role),
                                Some(&format!("worktree integration failed: {error}")),
                            );
                            wave_failed.push(id.clone());
                            dag_state.failed.insert(id.clone());
                            if failed_id.is_none() {
                                failed_id = Some(id);
                            }
                        }
                    }
                }
                Err((id, err)) => {
                    // Mark this task Failed and record it in wave_failed so
                    // the skip logic (top of loop) does NOT overwrite it to
                    // Skipped. failed_id keeps the FIRST failure for the
                    // error message.
                    let _ = store.set_task_status(
                        run_id,
                        &id,
                        TodoStatus::Failed,
                        None,
                        Some(&format!("error: {err}")),
                    );
                    wave_failed.push(id.clone());
                    dag_state.failed.insert(id.clone());
                    if failed_id.is_none() {
                        failed_id = Some(id);
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn integrate_reviewed_task<W: TaskDispatcher + 'static>(
    worker: Arc<W>,
    store: Arc<TaskRuntimeStore>,
    run_id: &str,
    task: &PlanTask,
    execution_id: &str,
    summary: &str,
    cancel: CancellationToken,
    trace_sink: Option<ExecSink>,
) -> Result<String, String> {
    let integration = match worker
        .integrate(
            store.clone(),
            run_id.to_string(),
            task.clone(),
            execution_id.to_string(),
            cancel,
            trace_sink,
        )
        .await
    {
        Ok(integration) => integration,
        Err(error) => {
            if let Ok(Some(mut persisted)) = store.get_summary(run_id, &task.id) {
                persisted.result.status = SubagentRunStatus::Failed;
                let remaining = format!("worktree integration failed: {error}");
                if !persisted.result.remaining_work.contains(&remaining) {
                    persisted.result.remaining_work.push(remaining.clone());
                }
                if !persisted.decisions.contains(&remaining) {
                    persisted.decisions.push(remaining);
                }
                if let Err(persist_error) = store.put_summary(&persisted) {
                    tracing::warn!(
                        run_id,
                        task_id = %task.id,
                        error = %persist_error,
                        "failed to persist worktree integration failure"
                    );
                }
            }
            return Err(error);
        }
    };
    let Some(integration) = integration else {
        return Ok(summary.to_string());
    };

    let integration_summary = integration.summary();
    if let Ok(Some(mut persisted)) = store.get_summary(run_id, &task.id) {
        if !integration.changed_files.is_empty() {
            persisted.result.touched_files.written = integration.changed_files.clone();
        }
        if !persisted.decisions.contains(&integration_summary) {
            persisted.decisions.push(integration_summary.clone());
        }
        if let Err(error) = store.put_summary(&persisted) {
            tracing::warn!(
                run_id,
                task_id = %task.id,
                %error,
                "failed to persist worktree integration summary"
            );
        }
    }
    Ok(format!("{summary} | {integration_summary}"))
}

fn interrupted_outcome(store: &TaskRuntimeStore, run_id: &str) -> RunOutcome {
    let paused = store
        .get_run(run_id)
        .ok()
        .flatten()
        .is_some_and(|run| run.status == TaskRunStatus::Paused);
    if paused {
        RunOutcome::Paused {
            failed_task_id: "<pause>".to_string(),
            error: "paused by user".to_string(),
        }
    } else {
        RunOutcome::Cancelled
    }
}

/// Outcome of the review gate over a freshly-completed task.
#[allow(clippy::large_enum_variant)] // PlanTask is Clone and short-lived in the review path; Box would add indirection with no win
enum ReviewGateOutcome {
    /// Task passed review (or is read-only and self-reviewing). Mark Completed.
    Pass,
    /// Review found fixable issues → re-queue this fix task (same id, bumped
    /// retry_count, review-informed brief) for the next wave.
    NeedsFix(PlanTask),
    /// Circuit breaker tripped (retry budget exhausted or repeated fingerprint).
    /// The run should be Suspended.
    Suspend(String),
    /// No reviewer LLM configured → auto-trust the task. Logged.
    Skipped,
}

/// Run the review gate for a task that just finished executing. Read-only
/// kinds auto-pass; implementation/debugging kinds are reviewed by the LLM
/// (when available) against the domain checklist. Applies the circuit
/// breaker on NeedsFix/Blocked outcomes.
async fn run_review_gate(
    store: Arc<TaskRuntimeStore>,
    reviewer_llm: Option<Arc<dyn echo_agent::llm::LlmClient>>,
    run_id: &str,
    task: &PlanTask,
    worker_output: &str,
) -> ReviewGateOutcome {
    // Read-only kinds are their own review — no gate.
    if !super::review::requires_review(task.kind) {
        return ReviewGateOutcome::Pass;
    }
    let Some(llm) = reviewer_llm else {
        return ReviewGateOutcome::Skipped;
    };

    // Retry transient review errors (LLM 5xx/timeout, JSON parse failures) up to
    // 2 times before suspending. Transient failures are expected in production
    // and should not block the run on the first hiccup.
    const MAX_REVIEW_RETRIES: u32 = 2;
    let mut retries: u32 = 0;
    let review = loop {
        match super::review::review_task(&llm, &store, run_id, task, worker_output).await {
            Ok(r) => break r,
            Err(e) => {
                retries += 1;
                if retries <= MAX_REVIEW_RETRIES {
                    tracing::warn!(
                        task_id = %task.id,
                        attempt = retries,
                        error = %e,
                        "review gate transient error, retrying"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
                    continue;
                }
                // Exhausted retries. Do NOT auto-pass — that would let
                // unreviewed mutating work through. Surface the error so the
                // user can retry, lower the standard, or intervene.
                let reason = format!(
                    "review gate failed after {MAX_REVIEW_RETRIES} retries ({e}); run suspended pending user input"
                );
                let _ = store.note(run_id, Some(&task.id), &reason);
                return ReviewGateOutcome::Suspend(reason);
            }
        }
    };

    match review.outcome {
        ReviewOutcome::Pass => ReviewGateOutcome::Pass,
        ReviewOutcome::NeedsFix => {
            match super::review::circuit_breaker_action(&store, task, &review, 2) {
                super::review::BreakerAction::CreateFix => {
                    ReviewGateOutcome::NeedsFix(super::review::build_fix_task(task, &review))
                }
                super::review::BreakerAction::Suspend { reason } => {
                    ReviewGateOutcome::Suspend(reason)
                }
            }
        }
        ReviewOutcome::Blocked => ReviewGateOutcome::Suspend("review returned blocked".to_string()),
    }
}

/// Execute a single task on a pooled worker. Returns `(task_id, structured result)` on
/// success or `(task_id, error)` on failure. Honors read vs write concurrency
/// via the two semaphores.
#[allow(clippy::too_many_arguments)] // store + semaphores + locks + sinks all thread through
async fn execute_task(
    store: Arc<TaskRuntimeStore>,
    primary_agent: crate::agent_handle::AgentHandle,
    worker_sem: Arc<Semaphore>,
    write_sem: Arc<Semaphore>,
    shell_sem: Arc<Semaphore>,
    llm_sem: Arc<Semaphore>,
    file_write_locks: Arc<std::sync::Mutex<HashMap<String, Arc<TokioMutex<()>>>>>,
    trace_sink: Option<ExecSink>,
    run_id: String,
    task: PlanTask,
    cancel: CancellationToken,
    delegation_policy: echo_agent::tasks::NestedDelegationPolicy,
) -> TaskDispatchResult {
    let task_id = task.id.clone();
    let is_write = !task.kind.is_read_only();

    // ── U1c phase-1 CP B: per-task unattended preflight ──
    // Re-check the task (kind + tools + shell) before acquiring permits.
    // Chat runs (Attended) skip this; only Unattended runs are checked.
    // Terminal fail on violation — never Paused, never awaits a human.
    {
        let attended_mode = store
            .get_run(&run_id)
            .ok()
            .flatten()
            .map(|r| r.attended_mode)
            .unwrap_or_default();
        if attended_mode == AttendedMode::Unattended
            && let Err(rejection) =
                preflight_unattended_task(&task, super::task_tools::current_unattended_write_mode())
        {
            let msg = format!(
                "CP B preflight rejected task '{}': {}",
                task_id, rejection.reason
            );
            let _ = store.set_task_status(
                &run_id,
                &task_id,
                TodoStatus::Failed,
                Some(&task.agent_role),
                Some(&msg),
            );
            return Err((task_id.clone(), msg));
        }
    }

    // Create a child cancellation token for THIS task and register it with the
    // store. remove_task / update_task can cancel it to stop this worker
    // promptly without cancelling sibling tasks. child_token() means run-level
    // cancel still propagates here (child fires when parent fires).
    let task_cancel = cancel.child_token();
    store.register_task_cancel_token(&run_id, &task_id, task_cancel.clone());
    // RAII guard: always unregister on exit (success/fail/cancel), so the
    // token map doesn't leak finished tasks. Owns its key strings to avoid
    // borrowing task_id/run_id (which may be moved later in this function).
    struct TokenGuard {
        store: std::sync::Arc<TaskRuntimeStore>,
        run_id: String,
        task_id: String,
    }
    impl Drop for TokenGuard {
        fn drop(&mut self) {
            self.store
                .unregister_task_cancel_token(&self.run_id, &self.task_id);
        }
    }
    let _token_guard = TokenGuard {
        store: store.clone(),
        run_id: run_id.clone(),
        task_id: task_id.clone(),
    };

    let worker_trace_id = task_id.clone();
    let contract = subagent_runtime_contract(&primary_agent, &task.agent_role, &task.kind).await;
    tracing::info!(
        run_id = %run_id,
        task_id = %task_id,
        kind = %task.kind.as_str(),
        agent_role = %task.agent_role,
        read_only = task.kind.is_read_only(),
        prompt_chars = task.description.chars().count(),
        "task_runtime: task dispatch start"
    );

    // Mark the task running + emit TaskStarted (transactional with the todo
    // projection update).
    if let Err(e) = store.set_task_status(
        &run_id,
        &task_id,
        TodoStatus::Running,
        Some(&task.agent_role),
        None,
    ) {
        tracing::warn!(task_id = %task_id, error = %e, "failed to mark task running");
    }
    emit_task_started(
        trace_sink.as_ref(),
        &run_id,
        &worker_trace_id,
        &task,
        &contract,
    );

    // Acquire concurrency permits with cancel awareness:
    // - Read-only tasks take a worker permit (fan-out up to max_concurrent_workers).
    // - Write tasks (implementation/debugging) take ONLY the write permit.
    // - Verification tasks (shell/build/test) take the write permit + the shell
    //   permit (default 1, plan §678-680 shell_concurrency = 1).
    let is_shell = matches!(task.kind, PlanTaskKind::Verification);
    let (_worker_permit, _write_permit, _shell_permit) = if is_shell {
        tracing::info!(
            run_id = %run_id,
            task_id = %task_id,
            available = write_sem.available_permits(),
            "task_runtime: waiting for write permit"
        );
        let wp = tokio::select! {
            biased;
            _ = task_cancel.cancelled() => return Err((task_id.clone(), "cancelled while waiting for write permit".to_string())),
            p = write_sem.acquire() => p.map_err(|e| (task_id.clone(), e.to_string()))?,
        };
        tracing::info!(
            run_id = %run_id,
            task_id = %task_id,
            "task_runtime: acquired write permit"
        );
        tracing::info!(
            run_id = %run_id,
            task_id = %task_id,
            available = shell_sem.available_permits(),
            "task_runtime: waiting for shell permit"
        );
        let sp = tokio::select! {
            biased;
            _ = task_cancel.cancelled() => return Err((task_id.clone(), "cancelled while waiting for shell permit".to_string())),
            p = shell_sem.acquire() => p.map_err(|e| (task_id.clone(), e.to_string()))?,
        };
        tracing::info!(
            run_id = %run_id,
            task_id = %task_id,
            "task_runtime: acquired shell permit"
        );
        (None, Some(wp), Some(sp))
    } else if is_write {
        tracing::info!(
            run_id = %run_id,
            task_id = %task_id,
            available = write_sem.available_permits(),
            "task_runtime: waiting for write permit"
        );
        let wp = tokio::select! {
            biased;
            _ = task_cancel.cancelled() => return Err((task_id.clone(), "cancelled while waiting for write permit".to_string())),
            p = write_sem.acquire() => p.map_err(|e| (task_id.clone(), e.to_string()))?,
        };
        tracing::info!(
            run_id = %run_id,
            task_id = %task_id,
            "task_runtime: acquired write permit"
        );
        (None, Some(wp), None)
    } else {
        tracing::info!(
            run_id = %run_id,
            task_id = %task_id,
            available = worker_sem.available_permits(),
            "task_runtime: waiting for subagent permit"
        );
        let wp = tokio::select! {
            biased;
            _ = task_cancel.cancelled() => return Err((task_id.clone(), "cancelled while waiting for subagent permit".to_string())),
            p = worker_sem.acquire() => p.map_err(|e| (task_id.clone(), e.to_string()))?,
        };
        tracing::info!(
            run_id = %run_id,
            task_id = %task_id,
            "task_runtime: acquired subagent permit"
        );
        (Some(wp), None, None)
    };

    // Physical safety net below the ownership-safe DAG wave: exact file owners
    // take the same normalized mutex keys. Unknown owners were already kept out
    // of mixed writer waves and remain isolated in their own worktree.
    //
    // Two-layer concurrency:
    // - write_sem: global writer count cap (max_concurrent_writes=4)
    // - per-file TokioMutex: file-level mutual exclusion (1 permit per file)
    let ownership = super::planner::file_ownership(&task);
    let _file_lock_guard = if is_write {
        // CRITICAL: sort files before acquiring locks to prevent classic
        // lock-ordering deadlock. Without this, two tasks declaring the same
        // files in different orders (e.g. [A,B] vs [B,A]) would deadlock when
        // both reach Step 2 concurrently (A waits for B while B waits for A).
        // Sorting guarantees all tasks acquire per-file locks in the same
        // canonical order, breaking any potential wait-for cycle.
        let sorted_files: Vec<String> = ownership
            .known_files()
            .map(|files| files.iter().cloned().collect())
            .unwrap_or_default();

        if sorted_files.is_empty() {
            None
        } else {
            // Step 1: get-or-create per-file mutexes (outer lock held briefly).
            let per_file_mutexes: Vec<Arc<TokioMutex<()>>> = {
                let mut locks = file_write_locks.lock().unwrap_or_else(|e| e.into_inner());
                sorted_files
                    .iter()
                    .map(|f| {
                        locks
                            .entry(f.clone())
                            .or_insert_with(|| Arc::new(TokioMutex::new(())))
                            .clone()
                    })
                    .collect()
            }; // outer lock released here — brief, never held across awaits.

            // Step 2: acquire all per-file locks asynchronously. Overlapping files
            // block here until the previous writer releases its guard.
            let mut guards: Vec<OwnedMutexGuard<()>> = Vec::with_capacity(per_file_mutexes.len());
            for mtx in per_file_mutexes {
                let guard = tokio::select! {
                    biased;
                    _ = task_cancel.cancelled() => {
                        return Err((
                            task_id.clone(),
                            "cancelled while waiting for file write lock".to_string(),
                        ));
                    }
                    guard = mtx.lock_owned() => guard,
                };
                guards.push(guard);
            }
            Some(FileLockGuard { _guards: guards })
        }
    } else {
        None
    };

    // G4: LLM rate-limit permit — caps concurrent LLM calls to prevent
    // provider rate-limit hits and cost spikes (plan §704).
    tracing::info!(
        run_id = %run_id,
        task_id = %task_id,
        available = llm_sem.available_permits(),
        "task_runtime: waiting for llm permit"
    );
    let _llm_permit = tokio::select! {
        biased;
        _ = task_cancel.cancelled() => return Err((task_id.clone(), "cancelled while waiting for LLM permit".to_string())),
        p = llm_sem.acquire() => p.map_err(|e| (task_id.clone(), e.to_string()))?,
    };
    tracing::info!(
        run_id = %run_id,
        task_id = %task_id,
        "task_runtime: acquired llm permit"
    );

    // Summary Chain: gather the summaries of this task's completed
    // dependencies, so the worker gets compact upstream context instead of
    // (or in addition to) re-reading everything from scratch (plan §1039).
    let dep_summaries = collect_dependency_summaries(&store, &run_id, &task);
    let parent_goal = store.get_run(&run_id).ok().flatten().map(|run| run.goal);

    // Stable workspace context for the worker — prepended to the task prompt
    // so workers know where to operate without needing CWD in their system prompt.
    let ws_prefix = {
        let wd = primary_agent.read(|a| a.working_dir()).await;
        if let Some(ref wd) = wd {
            format!("[workspace]\n- root: {}\n[/workspace]\n\n", wd.display())
        } else {
            String::new()
        }
    };
    let prompt = build_task_prompt(
        &task,
        &dep_summaries,
        delegation_policy,
        parent_goal.as_deref(),
    );
    let prompt = format!("{ws_prefix}{prompt}");

    // Dispatch the task. Three paths, by kind:
    // - Read-only kinds (read_only_review, investigation, test_plan, review,
    //   summary) → delegate to the registered readonly subagent role via Fork.
    //   The child cancel token propagates parent-run cancellation.
    // - Code-writer kinds (implementation, debugging) → Sprint 9: delegate to
    //   the registered "implementer" Fork worker, which runs inside an isolated
    //   git worktree. Disjoint exact owners may run concurrently; overlap and
    //   unknown ownership are split into later DAG waves. Dispatch failure is
    //   terminal — there is no in-place fallback that could duplicate writes.
    // - Verification (shell/build/test) → MAIN agent executes directly. These
    //   run against the workspace (testing just-written changes), so routing
    //   them to a separate worktree checkout would detach them from the work.
    let is_read_only_task = task.kind.is_read_only();
    let is_code_writer_task = matches!(
        task.kind,
        PlanTaskKind::Implementation | PlanTaskKind::Debugging
    );
    // Stable execution id for this dispatch: "{task_id}:{attempt}". Aligns
    // with SubagentRun.subagent_run_id and the framework's
    // SubagentEvent.execution_id (via ExternalRunContext). Including the
    // attempt ordinal (= retry_count + 1) keeps retries of the same task
    // distinguishable, so the bridge/frontend never has to temp-allocate ids.
    let attempt = task.retry_count.saturating_add(1);
    let execution_id = format!("{task_id}:{attempt}");
    // Resolve the run's root_message_id so the framework can carry it on
    // SubagentEvent::DispatchStarted → execution://event, letting the frontend
    // pin the subagent stream to the right chat message block.
    let root_message_id = store
        .get_run(&run_id)
        .ok()
        .flatten()
        .map(|r| r.root_message_id);
    if let Err(error) = store.record_worker_assigned(
        &run_id,
        &task_id,
        &execution_id,
        &task.agent_role,
        attempt,
        task.kind.is_read_only(),
    ) {
        return Err((
            task_id,
            format!("failed to persist worker start boundary: {error}"),
        ));
    }
    let (result, readonly_usage) = if is_read_only_task {
        tracing::info!(
            run_id = %run_id,
            task_id = %task_id,
            agent_role = %task.agent_role,
            prompt_chars = prompt.chars().count(),
            "task_runtime: delegating read-only task to subagent"
        );
        let dispatch_result = run_readonly_worker(
            &primary_agent,
            &run_id,
            &execution_id,
            root_message_id.as_deref(),
            &task.agent_role,
            &prompt,
            task_cancel.clone(),
            delegation_policy,
            trace_sink.clone(),
        )
        .await;
        match dispatch_result {
            Ok(sub_result)
                if sub_result.outcome.status
                    == echo_agent::agent::subagent::SubagentStatus::Cancelled =>
            {
                tracing::info!(
                    run_id = %run_id,
                    task_id = %task_id,
                    agent_role = %task.agent_role,
                    "task_runtime: read-only subagent cancelled"
                );
                (Err("task cancelled".to_string()), sub_result.usage)
            }
            Ok(sub_result) => {
                tracing::info!(
                    run_id = %run_id,
                    task_id = %task_id,
                    agent_role = %task.agent_role,
                    output_chars = sub_result.output.chars().count(),
                    iterations = sub_result.iterations,
                    usage_reported = sub_result.usage.is_some(),
                    "task_runtime: read-only subagent completed"
                );
                (
                    Ok((
                        SubagentTaskResult::from_framework(&sub_result),
                        sub_result.output.clone(),
                    )),
                    sub_result.usage,
                )
            }
            Err(e) => {
                tracing::warn!(
                    run_id = %run_id,
                    task_id = %task_id,
                    agent_role = %task.agent_role,
                    error = %e,
                    "task_runtime: read-only subagent failed"
                );
                (Err(e), None)
            }
        }
    } else if is_code_writer_task {
        // Sprint 9: route to the worktree-isolated writer worker.
        tracing::info!(
            run_id = %run_id,
            task_id = %task_id,
            agent_role = %task.agent_role,
            prompt_chars = prompt.chars().count(),
            "task_runtime: delegating writer task to subagent"
        );
        let dispatch_result = run_writer_worker(
            &primary_agent,
            store.clone(),
            &run_id,
            &execution_id,
            &task.agent_role,
            &prompt,
            task_cancel.clone(),
            delegation_policy,
            trace_sink.clone(),
        )
        .await;
        match dispatch_result {
            Ok(sub_result)
                if sub_result.outcome.status
                    == echo_agent::agent::subagent::SubagentStatus::Cancelled =>
            {
                tracing::info!(
                    run_id = %run_id,
                    task_id = %task_id,
                    agent_role = %task.agent_role,
                    "task_runtime: writer subagent cancelled"
                );
                (Err("task cancelled".to_string()), sub_result.usage)
            }
            Ok(sub_result) => {
                tracing::info!(
                    run_id = %run_id,
                    task_id = %task_id,
                    agent_role = %task.agent_role,
                    output_chars = sub_result.output.chars().count(),
                    summary_chars = sub_result.outcome.summary.chars().count(),
                    iterations = sub_result.iterations,
                    usage_reported = sub_result.usage.is_some(),
                    "task_runtime: writer subagent completed"
                );
                (
                    Ok((
                        SubagentTaskResult::from_framework(&sub_result),
                        sub_result.output.clone(),
                    )),
                    sub_result.usage,
                )
            }
            Err(e) => (
                Err(if task_cancel.is_cancelled() {
                    "task cancelled".to_string()
                } else {
                    e
                }),
                None,
            ),
        }
    } else {
        emit_task_isolation_observed(
            trace_sink.as_ref(),
            &run_id,
            &worker_trace_id,
            &task,
            &contract,
            "primary",
        );
        (
            run_main_agent_task(
                &primary_agent,
                store.clone(),
                &run_id,
                &task,
                &prompt,
                task_cancel.clone(),
                trace_sink.clone(),
            )
            .await,
            None,
        )
    };

    if is_read_only_task && result.is_ok() {
        let usage_payload = match &readonly_usage {
            Some(stats) => stats.to_payload(&run_id),
            None => {
                unavailable_llm_usage_payload("provider_returned_no_usage_for_readonly_subagent")
            }
        };
        if let Err(error) = store.record_worker_llm_usage(
            &run_id,
            &task_id,
            &worker_trace_id,
            &task.agent_role,
            &task.title,
            usage_payload.clone(),
        ) {
            tracing::warn!(
                run_id = %run_id,
                task_id = %task_id,
                error = %error,
                "failed to persist read-only subagent LLM usage"
            );
        }
        emit_exec(
            trace_sink.as_ref(),
            ExecEvent::for_task(
                run_id.clone(),
                worker_trace_id.clone(),
                "usage",
                usage_payload,
            )
            .with_agent(task.agent_role.clone())
            .with_title(task.title.clone()),
        );
    }

    match result {
        Ok((task_result, full_output)) => {
            // The parent consumes the bounded structured summary; extract
            // suggested_tasks from the full output because that optional block
            // appears before the terminal ## Result contract.
            let suggested_tasks = extract_suggested_tasks_from_worker_output(&full_output);
            let parent_facing = task_result.summary.trim().to_string();
            tracing::info!(
                run_id = %run_id,
                task_id = %task_id,
                agent_role = %task.agent_role,
                summary_chars = parent_facing.chars().count(),
                output_chars = full_output.chars().count(),
                "task_runtime: task completed"
            );
            super::ledger::archive_trace(&run_id, &task_id, &full_output, None);
            let _ = super::ledger::write_progress(&store, &run_id, None);
            if let Err(e) = store.put_summary(&TaskExecutionSummary {
                run_id: run_id.clone(),
                task_id: task_id.clone(),
                worker_agent: task.agent_role.clone(),
                result: task_result.clone(),
                decisions: vec![],
                next_implications: vec![],
                suggested_tasks: suggested_tasks.clone(),
                created_at: chrono::Utc::now(),
            }) {
                tracing::warn!(task_id = %task_id, error = %e, "failed to persist TaskExecutionSummary");
            }
            if let Err(error) = store.record_worker_released(
                &run_id,
                &task_id,
                &execution_id,
                task_result.status.as_str(),
                Some(&task_result),
            ) {
                return Err((
                    task_id,
                    format!("worker completed but terminal boundary was not persisted: {error}"),
                ));
            }
            append_suggested_tasks_to_plan(&store, &run_id, &task, &suggested_tasks);
            emit_exec(
                trace_sink.as_ref(),
                ExecEvent::for_task(
                    run_id.clone(),
                    worker_trace_id.clone(),
                    "completed",
                    serde_json::json!({
                        "output": &parent_facing,
                        "terminal_status": task_result.status.as_str(),
                        "contract_version": task_result.contract_version,
                        "summary": task_result.summary,
                        "artifacts": task_result.artifacts,
                        "verification": task_result.verification,
                        "remaining_work": task_result.remaining_work,
                        "touched_files": task_result.touched_files,
                    }),
                )
                .with_agent(task.agent_role.clone())
                .with_title(task.title.clone()),
            );
            Ok((task_id, task_result))
        }
        Err(e) => {
            let cancelled = task_cancel.is_cancelled() || e.contains("cancelled");
            let status = if cancelled {
                SubagentRunStatus::Cancelled
            } else if e.to_ascii_lowercase().contains("timed out")
                || e.to_ascii_lowercase().contains("timeout")
            {
                SubagentRunStatus::TimedOut
            } else {
                SubagentRunStatus::Failed
            };
            let task_result = SubagentTaskResult::terminal(status, e.clone(), vec![e.clone()]);
            if let Err(error) = store.put_summary(&TaskExecutionSummary {
                run_id: run_id.clone(),
                task_id: task_id.clone(),
                worker_agent: task.agent_role.clone(),
                result: task_result.clone(),
                decisions: Vec::new(),
                next_implications: Vec::new(),
                suggested_tasks: Vec::new(),
                created_at: chrono::Utc::now(),
            }) {
                tracing::warn!(task_id = %task_id, %error, "failed to persist failed TaskExecutionSummary");
            }
            if let Err(error) = store.record_worker_released(
                &run_id,
                &task_id,
                &execution_id,
                status.as_str(),
                Some(&task_result),
            ) {
                tracing::warn!(
                    run_id = %run_id,
                    task_id = %task_id,
                    %error,
                    "failed to persist worker terminal boundary"
                );
            }
            tracing::warn!(
                run_id = %run_id,
                task_id = %task_id,
                agent_role = %task.agent_role,
                error = %e,
                "task_runtime: task failed"
            );
            emit_exec(
                trace_sink.as_ref(),
                ExecEvent::for_task(
                    run_id,
                    worker_trace_id,
                    status.as_str(),
                    serde_json::json!({
                        "error": &e,
                        "terminal_status": status.as_str(),
                        "contract_version": task_result.contract_version,
                        "summary": task_result.summary,
                        "artifacts": task_result.artifacts,
                        "verification": task_result.verification,
                        "remaining_work": task_result.remaining_work,
                        "touched_files": task_result.touched_files,
                    }),
                )
                .with_agent(task.agent_role.clone())
                .with_title(task.title.clone()),
            );
            Err((task_id, e))
        }
    }
}

const MAX_SUGGESTED_TASKS_PER_WORKER: usize = 5;

#[derive(Debug, serde::Deserialize)]
struct SuggestedTaskEnvelope {
    #[serde(default)]
    suggested_tasks: Vec<RawSuggestedTask>,
}

#[derive(Debug, serde::Deserialize)]
struct RawSuggestedTask {
    title: Option<String>,
    description: Option<String>,
    kind: Option<PlanTaskKind>,
    agent_role: Option<String>,
    #[serde(default)]
    dependencies: Vec<String>,
    why_needed: Option<String>,
    risk: Option<String>,
}

fn extract_suggested_tasks_from_worker_output(text: &str) -> Vec<SuggestedTask> {
    let mut out = Vec::new();
    for candidate in suggested_task_json_candidates(text) {
        let Ok(envelope) = serde_json::from_str::<SuggestedTaskEnvelope>(&candidate) else {
            continue;
        };
        for raw in envelope.suggested_tasks {
            if out.len() >= MAX_SUGGESTED_TASKS_PER_WORKER {
                return out;
            }
            let Some(task) = normalize_suggested_task(raw) else {
                continue;
            };
            out.push(task);
        }
        if !out.is_empty() {
            break;
        }
    }
    out
}

fn suggested_task_json_candidates(text: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    for block in text.split("```json").skip(1) {
        if let Some(json) = block.split("```").next() {
            candidates.push(json.trim().to_string());
        }
    }
    let trimmed = text.trim();
    if trimmed.starts_with('{') {
        candidates.push(trimmed.to_string());
    }
    candidates
}

fn normalize_suggested_task(raw: RawSuggestedTask) -> Option<SuggestedTask> {
    let title = raw.title.unwrap_or_default().trim().to_string();
    let description = raw.description.unwrap_or_default().trim().to_string();
    if title.is_empty() || description.is_empty() {
        return None;
    }
    Some(SuggestedTask {
        title: title.chars().take(120).collect(),
        description,
        kind: raw.kind.unwrap_or(PlanTaskKind::Investigation),
        agent_role: raw
            .agent_role
            .filter(|role| !role.trim().is_empty())
            .unwrap_or_else(|| "explorer".to_string()),
        dependencies: raw
            .dependencies
            .into_iter()
            .map(|dep| dep.trim().to_string())
            .filter(|dep| !dep.is_empty())
            .take(8)
            .collect(),
        why_needed: raw.why_needed.unwrap_or_default().trim().to_string(),
        risk: raw
            .risk
            .filter(|risk| !risk.trim().is_empty())
            .unwrap_or_else(|| "medium".to_string()),
    })
}

fn normalized_task_title_tokens(title: &str) -> HashSet<String> {
    const STOP_WORDS: &[&str] = &[
        "the",
        "and",
        "for",
        "with",
        "this",
        "that",
        "current",
        "project",
        "echo",
        "agent",
        "analyze",
        "analysis",
        "focus",
        "architecture",
    ];

    let mut normalized = String::new();
    let mut last_was_space = false;
    for ch in title.chars() {
        for lower in ch.to_lowercase() {
            if lower.is_alphanumeric() {
                normalized.push(lower);
                last_was_space = false;
            } else if !last_was_space {
                normalized.push(' ');
                last_was_space = true;
            }
        }
    }

    normalized
        .split_whitespace()
        .filter(|token| token.chars().count() >= 3)
        .filter(|token| !STOP_WORDS.contains(token))
        .map(ToString::to_string)
        .collect()
}

fn normalized_task_title_text(title: &str) -> String {
    let mut normalized = String::new();
    let mut last_was_space = false;
    for ch in title.chars() {
        for lower in ch.to_lowercase() {
            if lower.is_alphanumeric() {
                normalized.push(lower);
                last_was_space = false;
            } else if !last_was_space {
                normalized.push(' ');
                last_was_space = true;
            }
        }
    }
    normalized.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn task_titles_look_duplicate(candidate: &str, existing: &str) -> bool {
    let candidate_text = normalized_task_title_text(candidate);
    let existing_text = normalized_task_title_text(existing);
    if candidate_text.is_empty() || existing_text.is_empty() {
        return false;
    }
    if candidate_text == existing_text
        || candidate_text.contains(&existing_text)
        || existing_text.contains(&candidate_text)
    {
        return true;
    }

    let candidate_tokens = normalized_task_title_tokens(candidate);
    let existing_tokens = normalized_task_title_tokens(existing);
    let min_tokens = candidate_tokens.len().min(existing_tokens.len());
    if min_tokens < 2 {
        return false;
    }
    let overlap = candidate_tokens.intersection(&existing_tokens).count();
    overlap.saturating_mul(100) >= min_tokens.saturating_mul(60)
}

fn append_suggested_tasks_to_plan(
    store: &Arc<TaskRuntimeStore>,
    run_id: &str,
    parent: &PlanTask,
    suggestions: &[SuggestedTask],
) {
    if suggestions.is_empty() {
        return;
    }
    let existing_tasks = store
        .get_plan(run_id)
        .ok()
        .flatten()
        .map(|plan| plan.tasks)
        .unwrap_or_default();
    let existing_ids: HashSet<String> = existing_tasks.iter().map(|task| task.id.clone()).collect();
    let mut seen_titles: Vec<String> = existing_tasks
        .iter()
        .map(|task| task.title.clone())
        .collect();
    let mut after_task_id = Some(parent.id.clone());
    for suggestion in suggestions.iter().take(MAX_SUGGESTED_TASKS_PER_WORKER) {
        if seen_titles
            .iter()
            .any(|title| task_titles_look_duplicate(&suggestion.title, title))
        {
            tracing::info!(
                run_id = %run_id,
                parent_task_id = %parent.id,
                suggested_title = %suggestion.title,
                "task_runtime: skipped duplicate subagent-suggested task"
            );
            continue;
        }

        let mut depends_on: Vec<String> = suggestion
            .dependencies
            .iter()
            .filter(|dep| existing_ids.contains(dep.as_str()))
            .cloned()
            .collect();
        if !depends_on.iter().any(|dep| dep == &parent.id) {
            depends_on.push(parent.id.clone());
        }
        let new_task_id = format!("suggested_{}", uuid::Uuid::new_v4().as_simple());
        let task = PlanTask {
            id: new_task_id.clone(),
            title: suggestion.title.clone(),
            description: format!(
                "{}\n\nWhy needed: {}\nRisk: {}",
                suggestion.description, suggestion.why_needed, suggestion.risk
            ),
            kind: suggestion.kind,
            agent_role: suggestion.agent_role.clone(),
            depends_on,
            ..Default::default()
        };
        match store.insert_task(run_id, after_task_id.clone(), task) {
            Ok(()) => {
                tracing::info!(
                    run_id = %run_id,
                    parent_task_id = %parent.id,
                    suggested_task_id = %new_task_id,
                    "task_runtime: appended subagent-suggested task"
                );
                after_task_id = Some(new_task_id);
                seen_titles.push(suggestion.title.clone());
            }
            Err(e) => {
                tracing::warn!(
                    run_id = %run_id,
                    parent_task_id = %parent.id,
                    error = %e,
                    "task_runtime: failed to append subagent-suggested task"
                );
            }
        }
    }
}

/// Prefers the structured TaskExecutionSummary (persisted by put_summary at
/// task boundary) over the truncated todo.summary text, so downstream workers
/// get full context: summary, touched files, decisions, and remaining work.
fn collect_dependency_summaries(
    store: &Arc<TaskRuntimeStore>,
    run_id: &str,
    task: &PlanTask,
) -> Vec<(String, String)> {
    if task.depends_on.is_empty() {
        return Vec::new();
    }
    let Ok(todos) = store.list_todos(run_id) else {
        return Vec::new();
    };
    task.depends_on
        .iter()
        .filter_map(|dep_id| {
            todos.iter().find(|t| &t.task_id == dep_id).and_then(|t| {
                if t.status != TodoStatus::Completed {
                    return None;
                }
                // Prefer the structured summary when available; fall back to
                // the truncated todo text for tasks that predate put_summary.
                let structured = store
                    .get_summary(run_id, &t.task_id)
                    .ok()
                    .flatten()
                    .map(|s| {
                        let mut parts: Vec<String> = Vec::new();
                        if !s.result.summary.trim().is_empty() {
                            parts.push(format!("完成: {}", s.result.summary));
                        }
                        if !s.result.touched_files.written.is_empty() {
                            parts.push(format!(
                                "修改文件: {}",
                                s.result.touched_files.written.join(", ")
                            ));
                        }
                        if !s.decisions.is_empty() {
                            parts.push(format!("决策: {}", s.decisions.join("; ")));
                        }
                        (t.title.clone(), parts.join(" | "))
                    });
                structured.or_else(|| {
                    t.summary
                        .as_deref()
                        .map(|s| (t.title.clone(), s.to_string()))
                })
            })
        })
        .collect()
}

struct SubagentRuntimeContract {
    prompt_source: String,
    isolation_requested: String,
    context_in: String,
    returns: String,
}

fn runtime_contract_started_payload(contract: &SubagentRuntimeContract) -> serde_json::Value {
    serde_json::json!({
        "prompt_source": contract.prompt_source,
        "isolation_requested": contract.isolation_requested,
        "context_in": contract.context_in,
        "returns": contract.returns,
    })
}

fn runtime_isolation_observed_payload(
    contract: &SubagentRuntimeContract,
    isolation_observed: &str,
) -> serde_json::Value {
    serde_json::json!({
        "isolation_requested": contract.isolation_requested,
        "isolation_observed": isolation_observed,
    })
}

fn emit_task_started(
    sink: Option<&ExecSink>,
    run_id: &str,
    worker_trace_id: &str,
    task: &PlanTask,
    contract: &SubagentRuntimeContract,
) {
    let mut payload = runtime_contract_started_payload(contract);
    if let Some(payload) = payload.as_object_mut() {
        payload.insert(
            "kind".to_string(),
            serde_json::Value::String(task.kind.as_str().to_string()),
        );
        payload.insert(
            "agent_role".to_string(),
            serde_json::Value::String(task.agent_role.clone()),
        );
    }
    emit_exec(
        sink,
        ExecEvent::for_task(run_id, worker_trace_id, "started", payload)
            .with_agent(task.agent_role.clone())
            .with_title(task.title.clone())
            .with_task(task.description.clone()),
    );
}

fn emit_task_isolation_observed(
    sink: Option<&ExecSink>,
    run_id: &str,
    worker_trace_id: &str,
    task: &PlanTask,
    contract: &SubagentRuntimeContract,
    isolation_observed: &str,
) {
    emit_exec(
        sink,
        ExecEvent::for_task(
            run_id,
            worker_trace_id,
            "isolation_observed",
            runtime_isolation_observed_payload(contract, isolation_observed),
        )
        .with_agent(task.agent_role.clone())
        .with_title(task.title.clone()),
    );
}

async fn subagent_runtime_contract(
    primary_agent: &crate::agent_handle::AgentHandle,
    agent_role: &str,
    kind: &PlanTaskKind,
) -> SubagentRuntimeContract {
    let registry = primary_agent
        .read(|agent| agent.subagent_registry().clone())
        .await;
    let definitions = registry.list_available().await;
    let definition = definitions.iter().find(|def| def.name == agent_role);

    let prompt_source = definition
        .and_then(|def| {
            def.tags
                .iter()
                .find_map(|tag| tag.strip_prefix("prompt_source:").map(str::to_string))
        })
        .unwrap_or_else(|| "unknown".to_string());

    let isolation_requested = definition
        .and_then(|def| {
            def.tags
                .iter()
                .find_map(|tag| tag.strip_prefix("isolation:").map(str::to_string))
        })
        .unwrap_or_else(|| {
            if matches!(kind, PlanTaskKind::Implementation | PlanTaskKind::Debugging) {
                "worktree".to_string()
            } else if kind.is_read_only() {
                "context".to_string()
            } else {
                "primary".to_string()
            }
        });

    SubagentRuntimeContract {
        prompt_source,
        isolation_requested,
        context_in: "task_context + dependency summaries + workspace root".to_string(),
        returns: "TaskExecutionSummary + execution://event trace".to_string(),
    }
}

/// Build the prompt handed to a task's worker. Combines the task brief with
/// its verification criteria, the Summary Chain from completed dependencies,
/// and a read-only reminder for non-mutating kinds.
fn build_task_prompt(
    task: &PlanTask,
    dep_summaries: &[(String, String)],
    delegation_policy: echo_agent::tasks::NestedDelegationPolicy,
    parent_goal: Option<&str>,
) -> String {
    let mut s = String::new();
    // [task_context] marker: all content below is dynamic per-task information.
    // Worker system prompts are fixed templates — dynamic task descriptions,
    // target files, verification steps, and dependency summaries go HERE
    // (in the user message), keeping the system prefix cache-stable.
    s.push_str("[task_context]\n");
    if let Some(goal) = parent_goal.filter(|goal| !goal.trim().is_empty()) {
        s.push_str(&format!("Parent goal: {goal}\n\n"));
    }
    s.push_str(&format!("Task: {}\n\n{}\n\n", task.title, task.description));
    // Summary Chain: compact context from completed upstream tasks. Replaces
    // the raw upstream worker conversations (plan §1039-1062).
    if !dep_summaries.is_empty() {
        s.push_str("Context from completed upstream tasks:\n");
        for (title, summary) in dep_summaries {
            s.push_str(&format!("- {title}: {summary}\n"));
        }
        s.push('\n');
    }
    let ownership = super::planner::file_ownership(task);
    if !task.files.is_empty() {
        if task.kind.is_read_only() {
            s.push_str("Read targets:\n");
        } else {
            s.push_str("Declared exclusive write ownership:\n");
        }
        for f in &task.files {
            s.push_str(&format!("- {f}\n"));
        }
        s.push('\n');
    }
    if !task.verification.is_empty() {
        s.push_str("Verification (you must address each):\n");
        for v in &task.verification {
            s.push_str(&format!("- {v}\n"));
        }
        s.push('\n');
    }
    if !task.required_artifacts.is_empty() {
        s.push_str("Required artifacts (each must exist and be reported):\n");
        for artifact in &task.required_artifacts {
            s.push_str(&format!("- {artifact}\n"));
        }
        s.push('\n');
    }
    if task.kind.is_read_only() {
        s.push_str(
            "Execution boundary: READ-ONLY. You may inspect files, metadata, logs, and other \
             available evidence, including non-mutating commands. Do not edit files, install \
             dependencies, change repository state, or run commands with side effects.\n",
        );
    } else {
        match ownership {
            super::planner::FileOwnership::Known(_) => s.push_str(
                "Execution boundary: EXCLUSIVE SCOPED WRITE. Change only the declared ownership \
                 files. Runtime validates the actual Git diff and rejects undeclared writes. \
                 Preserve unrelated user work and run every listed verification that is available.\n",
            ),
            super::planner::FileOwnership::Unknown { reason } => s.push_str(&format!(
                "Execution boundary: ISOLATED UNKNOWN-SCOPE WRITE ({reason}). This task is \
                 serialized from other writers and runs in a worktree. Keep changes as narrow as \
                 possible, preserve unrelated user work, and report every actual changed file.\n"
            )),
            super::planner::FileOwnership::ReadOnly => {}
        }
    }
    s.push_str(
        "\nWork to the stated outcome and success evidence. Inspect before concluding, keep fact and \
         inference distinct, and do not modify the global plan. ",
    );
    if delegation_policy.can_delegate() {
        s.push_str(
            "This role may use agent_tool for tightly scoped child subagent help within this \
             PlanTask only. Child results must be summarized back into your answer; do not let \
             child subagents create or execute the global plan. ",
        );
    } else {
        s.push_str("Do not delegate this task to other agents. ");
    }
    s.push_str(
        "If the evidence reveals genuinely required follow-up work that is outside this task, \
         include an optional fenced JSON block exactly like:\n\
         ```json\n\
         {\"suggested_tasks\":[{\"title\":\"short title\",\"description\":\"specific follow-up\",\
         \"kind\":\"investigation\",\"agent_role\":\"explorer\",\"dependencies\":[],\
         \"why_needed\":\"why this is needed\",\"risk\":\"low|medium|high\"}]}\n\
         ```\n\
         Suggest only independently executable work necessary for the parent goal; do not use \
         suggestions as a substitute for completing the assigned task.\n",
    );
    s.push_str(
        "\nReturn contract: end with `## Result` followed by exactly one fenced JSON object:\n\
         ```json\n\
         {\"contract_version\":1,\"status\":\"completed\",\"summary\":\"at most 1200 characters\",\
         \"artifacts\":[{\"path\":\"actual path\",\"kind\":\"file|report|chart|other\"}],\
         \"verification\":[{\"check\":\"exact command or check\",\"status\":\"passed|failed|not_run\",\
         \"details\":\"bounded evidence\",\"source\":\"reported\"}],\
         \"remaining_work\":[],\"touched_files\":{\"read\":[],\"written\":[]}}\n\
         ```\n\
         Runtime owns the final status and artifact hash. Report exact checks and paths; never \
         claim an artifact, changed file, or verification that does not exist. Put any incomplete \
         or blocked work in remaining_work. Optional detailed notes and suggested_tasks may appear \
         before `## Result`.\n",
    );
    s
}

/// Run a READ-ONLY task by delegating to a registered subagent role via the
/// primary agent's `delegate_to_agent_with_cancel`. Fork mode runs the worker
/// on an isolated agent instance under the executor's own semaphore (not the
/// primary agent's execution_mutex), so multiple read-only workers run in
/// parallel. The child cancel token propagates parent-run cancellation.
#[allow(clippy::too_many_arguments)] // handles + cancel + sink thread through; matches framework dispatch style
async fn run_readonly_worker(
    primary_agent: &crate::agent_handle::AgentHandle,
    run_id: &str,
    execution_id: &str,
    message_id: Option<&str>,
    role: &str,
    prompt: &str,
    cancel: CancellationToken,
    delegation_policy: echo_agent::tasks::NestedDelegationPolicy,
    trace_sink: Option<ExecSink>,
) -> Result<echo_agent::agent::subagent::SubagentResult, String> {
    primary_agent
        .read_async(|agent| {
            let prompt = prompt.to_string();
            let role = role.to_string();
            let run_id = run_id.to_string();
            let execution_id = execution_id.to_string();
            let message_id = message_id.map(|s| s.to_string());
            let core_trace_sink = worker_trace_sink_to_core(trace_sink);
            Box::pin(async move {
                let runtime_context = Some(echo_core::tools::ExternalRunContext {
                    conversation_id: None,
                    run_id: Some(run_id.clone()),
                    turn_id: message_id.clone(),
                    execution_id: Some(execution_id),
                    message_id,
                    cancel: Some(Arc::new(cancel.clone())),
                    trace_sink: core_trace_sink,
                    delegation_policy: Some(delegation_policy),
                });
                agent
                    .delegate_to_agent_with_parent_context_and_cancel(
                        &role,
                        &prompt,
                        &run_id,
                        cancel,
                        0,
                        runtime_context,
                    )
                    .await
                    .map_err(|e| format!("subagent dispatch failed: {e}"))
            })
        })
        .await
}

fn worker_trace_sink_to_core(
    trace_sink: Option<ExecSink>,
) -> Option<echo_core::tools::TraceSinkFn> {
    // Wrap an app-layer `ExecSink` into the framework's `TraceSinkFn`
    // (Value-based) so it can be carried across `tokio::spawn` boundaries via
    // `ExternalRunContext.trace_sink`. The app's `scoped_with_ctx_run_id`
    // (task_tools.rs) reads `ctx.trace_sink` back and re-scopes it into
    // `CURRENT_TRACE_SINK` so tools running inside a spawned task (e.g.
    // `plan_execute`) can emit execution-flow events.
    //
    // Subagent dispatch itself does NOT use this path — it goes through
    // `SubagentEventBus`. This conversion is only for the main-agent tool path
    // (plan_execute / plan_create) that runs inside the framework's spawned
    // tool executor and needs to reach the trace_sink.
    trace_sink.map(|sink| {
        Arc::new(move |value: serde_json::Value| {
            if let Ok(ev) = serde_json::from_value::<ExecEvent>(value) {
                sink(ev);
            }
        }) as echo_core::tools::TraceSinkFn
    })
}

/// Run a CODE-WRITER task (implementation / debugging) by delegating to the
/// registered writer subagent role via Fork dispatch (Sprint 9).
///
/// Mirrors [`run_readonly_worker`] but with attachment-aware delegation: when
/// the run carries user attachments (images/files), the multimodal variant
/// `delegate_to_agent_with_parent_cancel_and_message` is used so the writer
/// worker sees them (parity with the old in-place `run_main_agent_task` path).
///
/// The registered writer worker (built by `build_writer_worker_agent`) carries
/// the full write tool set and declares `isolate_worktree: true`, so the
/// framework's `dispatch_fork` runs it inside an isolated git worktree
/// (eko-fork-<label>) — writes land in the worktree, not the main workspace.
/// If no WorktreeFactory is configured, dispatch **hard-fails** (Phase 2 Task 13)
/// rather than silently sharing the main tree.
/// Disjoint exact owners may run concurrently; the DAG scheduler separates
/// overlapping and unknown ownership before dispatch.
#[allow(clippy::too_many_arguments)] // handles + cancel + sink thread through; matches framework dispatch style
async fn run_writer_worker(
    primary_agent: &crate::agent_handle::AgentHandle,
    store: Arc<TaskRuntimeStore>,
    run_id: &str,
    execution_id: &str,
    role: &str,
    prompt: &str,
    cancel: CancellationToken,
    delegation_policy: echo_agent::tasks::NestedDelegationPolicy,
    trace_sink: Option<ExecSink>,
) -> Result<echo_agent::agent::subagent::SubagentResult, String> {
    // Rebuild a multimodal Message when the run carries user attachments, so
    // the writer worker sees the same images/files as the primary agent would
    // (parity with run_main_agent_task, executor.rs:1373-1380).
    let run_record = store.get_run(run_id).ok().flatten();
    let root_message_id = run_record.as_ref().map(|r| r.root_message_id.clone());
    let conversation_id = run_record.as_ref().map(|r| r.conversation_id.clone());
    let run_message: Option<echo_core::llm::types::Message> = run_record.as_ref().and_then(|r| {
        if r.attachments.is_empty() {
            None
        } else {
            crate::attachments::build_message_from_refs(prompt, &r.attachments).ok()
        }
    });

    primary_agent
        .read_async(|agent| {
            let prompt = prompt.to_string();
            let role = role.to_string();
            let run_id = run_id.to_string();
            let execution_id = execution_id.to_string();
            let run_message = run_message.clone();
            let core_trace_sink = worker_trace_sink_to_core(trace_sink);
            Box::pin(async move {
                let runtime_context = Some(echo_core::tools::ExternalRunContext {
                    conversation_id: conversation_id.clone(),
                    run_id: Some(run_id.clone()),
                    turn_id: root_message_id.clone(),
                    execution_id: Some(execution_id),
                    message_id: root_message_id,
                    cancel: Some(Arc::new(cancel.clone())),
                    trace_sink: core_trace_sink,
                    delegation_policy: Some(delegation_policy),
                });
                if let Some(msg) = run_message {
                    agent
                        .delegate_to_agent_with_parent_context_cancel_and_message(
                            &role,
                            &prompt,
                            msg,
                            &run_id,
                            cancel,
                            0,
                            runtime_context,
                        )
                        .await
                } else {
                    agent
                        .delegate_to_agent_with_parent_context_and_cancel(
                            &role,
                            &prompt,
                            &run_id,
                            cancel,
                            0,
                            runtime_context,
                        )
                        .await
                }
                .map_err(|e| format!("writer subagent dispatch failed: {e}"))
            })
        })
        .await
    // The SubagentResult returned by delegation carries the writer's accumulated
    // output, which already includes the appended worktree diff from dispatch_fork's
    // finalize step (Sprint 8). trace_sink is accepted for signature parity with
    // run_main_agent_task but unused here — subagent token/thinking events are
    // emitted by the framework's executor event bus, not this caller.
}

/// Run a MUTATING task (verification) directly on the PRIMARY agent via its
/// versioned streaming contract. These tasks are never delegated to a read-only subagent
/// (workers can't write). The write_sem acquired by the caller serializes them,
/// and the primary agent's execution_mutex serializes them further — correct,
/// because mutating work must not race.
///
/// Cancellation: `Agent::execute` is not cancel-aware, so we race it against
/// the cancel token. If the run is cancelled mid-task, we return an error and
/// the task is marked Failed (the run then goes Cancelled/Failed).
fn tool_call_is_replay_safe(agent: &echo_agent::agent::ReactAgent, tool_name: &str) -> bool {
    let Some(tool) = agent.tool_manager().get_tool(tool_name) else {
        return false;
    };
    let permissions = tool.permissions();
    !permissions.iter().any(|permission| {
        matches!(
            permission,
            echo_agent::prelude::ToolPermission::Write
                | echo_agent::prelude::ToolPermission::Execute
                | echo_agent::prelude::ToolPermission::Network
                | echo_agent::prelude::ToolPermission::Sensitive
        )
    })
}

fn verification_check_from_agent_tool(name: &str, args: &serde_json::Value) -> Option<String> {
    let normalized = name.to_ascii_lowercase().replace('-', "_");
    if !matches!(
        normalized.as_str(),
        "shell" | "bash" | "terminal" | "run_code" | "execute_command"
    ) {
        return None;
    }
    ["command", "cmd", "code", "script"]
        .iter()
        .find_map(|key| args.get(*key).and_then(serde_json::Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn file_access_from_agent_tool(name: &str, args: &serde_json::Value) -> Option<(bool, String)> {
    let normalized = name.to_ascii_lowercase().replace('-', "_");
    let write = normalized.contains("write")
        || normalized.contains("edit")
        || normalized.contains("delete")
        || normalized.contains("patch");
    let read = normalized.contains("read")
        || normalized.contains("search")
        || normalized.contains("glob")
        || normalized.contains("grep");
    if !write && !read {
        return None;
    }
    ["path", "file_path", "target", "directory"]
        .iter()
        .find_map(|key| args.get(*key).and_then(serde_json::Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|path| (write, path.to_string()))
}

fn push_unique_path(paths: &mut Vec<String>, path: String) {
    if !path.is_empty() && !paths.iter().any(|existing| existing == &path) {
        paths.push(path);
    }
}

async fn run_main_agent_task(
    primary_agent: &crate::agent_handle::AgentHandle,
    store: Arc<TaskRuntimeStore>,
    run_id: &str,
    task: &PlanTask,
    prompt: &str,
    cancel: CancellationToken,
    trace_sink: Option<ExecSink>,
) -> Result<(SubagentTaskResult, String), String> {
    let run_id = run_id.to_string();
    let task_id = task.id.clone();
    let agent_role = task.agent_role.clone();
    let title = task.title.clone();
    let execution_id = format!("{}:{}", task.id, task.retry_count.saturating_add(1));

    // Rebuild a multimodal Message when the run carries user attachments, so
    // write-task workers see the same images/files as the main agent (#1b).
    let run_record = store.get_run(&run_id).ok().flatten();
    let conversation_id = run_record.as_ref().map(|run| run.conversation_id.clone());
    let root_message_id = run_record.as_ref().map(|run| run.root_message_id.clone());
    let run_message: Option<echo_core::llm::types::Message> = run_record.as_ref().and_then(|r| {
        if r.attachments.is_empty() {
            None
        } else {
            crate::attachments::build_message_from_refs(prompt, &r.attachments).ok()
        }
    });

    primary_agent
        .read_async(|agent| {
            let prompt = prompt.to_string();
            let run_message = run_message.clone();
            let execution_id = execution_id.clone();
            Box::pin(async move {
                let event_cancel = cancel.clone();
                let invocation = echo_core::agent::AgentInvocationContext {
                    runtime: Some(echo_core::tools::ExternalRunContext {
                        conversation_id,
                        run_id: Some(run_id.clone()),
                        turn_id: root_message_id.clone(),
                        execution_id: Some(execution_id.clone()),
                        message_id: root_message_id,
                        cancel: Some(Arc::new(cancel.clone())),
                        trace_sink: worker_trace_sink_to_core(trace_sink.clone()),
                        delegation_policy: None,
                    }),
                    working_dir: None,
                    cancel: None,
                    disabled_tools: None,
                    run_budget: None,
                };
                let event_identity = echo_core::agent::EventIdentity::from_invocation(&invocation);
                // Multimodal path when the run has attachments; plain text otherwise.
                let raw_stream = if let Some(msg) = run_message {
                    agent
                        .execute_stream_message_with_invocation_context(
                            msg,
                            cancel,
                            invocation,
                        )
                        .await
                        .map_err(|e| format!("main agent stream failed: {e}"))?
                } else {
                    agent
                        .execute_stream_with_invocation_context(&prompt, cancel, invocation)
                        .await
                        .map_err(|e| format!("main agent stream failed: {e}"))?
                };
                let mut stream = echo_core::agent::envelope_event_stream(
                    raw_stream,
                    event_identity,
                );
                let mut output = String::new();
                let mut in_thinking = false;
                let mut pending_verification = HashMap::<String, String>::new();
                let mut pending_file_access = HashMap::<String, (bool, String)>::new();
                let mut observed_verification = Vec::new();
                let mut observed_artifacts = Vec::new();
                let mut touched_files = echo_agent::agent::subagent::SubagentTouchedFiles::default();

                while let Some(event_result) = stream.next().await {
                    let event = event_result
                        .map_err(|e| format!("main agent stream failed: {e}"))?
                        .payload;
                    match event {
                        AgentEvent::Token(content) => {
                            if in_thinking {
                                emit_exec(
                                    trace_sink.as_ref(),
                                    ExecEvent::for_task(
                                        run_id.clone(),
                                        task_id.clone(),
                                        "thinking_delta",
                                        serde_json::json!({ "content": content }),
                                    )
                                    .with_agent(agent_role.clone())
                                    .with_title(title.clone()),
                                );
                            } else {
                                output.push_str(&content);
                                emit_exec(
                                    trace_sink.as_ref(),
                                    ExecEvent::for_task(
                                        run_id.clone(),
                                        task_id.clone(),
                                        "token_delta",
                                        serde_json::json!({ "content": content }),
                                    )
                                    .with_agent(agent_role.clone())
                                    .with_title(title.clone()),
                                );
                            }
                        }
                        AgentEvent::ThinkStart => {
                            in_thinking = true;
                            emit_exec(
                                trace_sink.as_ref(),
                                ExecEvent::for_task(
                                    run_id.clone(),
                                    task_id.clone(),
                                    "thinking_started",
                                    serde_json::json!({}),
                                )
                                .with_agent(agent_role.clone())
                                .with_title(title.clone()),
                            );
                        }
                        AgentEvent::ThinkEnd {
                            prompt_tokens,
                            completion_tokens,
                        } => {
                            in_thinking = false;
                            emit_exec(
                                trace_sink.as_ref(),
                                ExecEvent::for_task(
                                    run_id.clone(),
                                    task_id.clone(),
                                    "thinking_ended",
                                    serde_json::json!({
                                        "prompt_tokens": prompt_tokens,
                                        "completion_tokens": completion_tokens,
                                    }),
                                )
                                .with_agent(agent_role.clone())
                                .with_title(title.clone()),
                            );
                        }
                        AgentEvent::LlmUsage {
                            model,
                            prompt_tokens,
                            completion_tokens,
                            total_tokens,
                            cached_prompt_tokens,
                            cache_creation_prompt_tokens,
                            usage_reported,
                        } => {
                            let usage_payload = serde_json::json!({
                                "model": model,
                                "prompt_tokens": prompt_tokens,
                                "completion_tokens": completion_tokens,
                                "total_tokens": total_tokens,
                                "cached_prompt_tokens": cached_prompt_tokens,
                                "cache_creation_prompt_tokens": cache_creation_prompt_tokens,
                                "usage_reported": usage_reported,
                                "usage_event_id": uuid::Uuid::new_v4().to_string(),
                            });
                            if let Err(error) = store.record_worker_llm_usage(
                                &run_id,
                                &task_id,
                                &task_id,
                                &agent_role,
                                &title,
                                usage_payload.clone(),
                            ) {
                                tracing::warn!(
                                    run_id = %run_id,
                                    task_id = %task_id,
                                    error = %error,
                                    "failed to persist subagent LLM usage"
                                );
                            }
                            emit_exec(
                                trace_sink.as_ref(),
                                ExecEvent::for_task(
                                    run_id.clone(),
                                    task_id.clone(),
                                    "usage",
                                    usage_payload,
                                )
                                .with_agent(agent_role.clone())
                                .with_title(title.clone()),
                            );
                        }
                        AgentEvent::ToolCall {
                            call_id,
                            name,
                            args,
                        } => {
                            if let Some(check) = verification_check_from_agent_tool(&name, &args) {
                                pending_verification.insert(call_id.clone(), check);
                            }
                            if let Some(access) = file_access_from_agent_tool(&name, &args) {
                                pending_file_access.insert(call_id.clone(), access);
                            }
                            let replay_safe = tool_call_is_replay_safe(agent, &name);
                            if let Err(error) = store.record_tool_started(
                                &run_id,
                                &task_id,
                                &execution_id,
                                &call_id,
                                &name,
                                replay_safe,
                            ) {
                                event_cancel.cancel();
                                return Err(format!(
                                    "failed to persist tool start boundary for {name}: {error}"
                                ));
                            }
                            emit_exec(
                                trace_sink.as_ref(),
                                ExecEvent::for_task(
                                    run_id.clone(),
                                    task_id.clone(),
                                    "tool_started",
                                    serde_json::json!({
                                        "call_id": call_id,
                                        "name": name,
                                        "args": args,
                                    }),
                                )
                                .with_agent(agent_role.clone())
                                .with_title(title.clone()),
                            );
                        }
                        AgentEvent::ToolResult {
                            call_id,
                            name,
                            output: result,
                        } => {
                            if let Some(check) = pending_verification.remove(&call_id) {
                                observed_verification.push(
                                    echo_agent::agent::subagent::SubagentVerification {
                                        check,
                                        status: echo_agent::agent::subagent::SubagentVerificationStatus::Passed,
                                        details: result.chars().take(500).collect(),
                                        source: echo_agent::agent::subagent::SubagentVerificationSource::Observed,
                                    },
                                );
                            }
                            if let Some((write, path)) = pending_file_access.remove(&call_id) {
                                if write {
                                    push_unique_path(&mut touched_files.written, path);
                                } else {
                                    push_unique_path(&mut touched_files.read, path);
                                }
                            }
                            if let Err(error) = store.record_tool_finished(
                                &run_id,
                                &task_id,
                                &execution_id,
                                &call_id,
                                &name,
                                true,
                                &result,
                                None,
                            ) {
                                event_cancel.cancel();
                                return Err(format!(
                                    "tool {name} completed but its terminal boundary was not persisted: {error}"
                                ));
                            }
                            emit_exec(
                                trace_sink.as_ref(),
                                ExecEvent::for_task(
                                    run_id.clone(),
                                    task_id.clone(),
                                    "tool_completed",
                                    serde_json::json!({
                                        "call_id": call_id,
                                        "name": name,
                                        "result": result,
                                        "success": true,
                                    }),
                                )
                                .with_agent(agent_role.clone())
                                .with_title(title.clone()),
                            );
                        }
                        AgentEvent::ToolError {
                            call_id,
                            name,
                            error,
                            failure,
                        } => {
                            if let Some(check) = pending_verification.remove(&call_id) {
                                observed_verification.push(
                                    echo_agent::agent::subagent::SubagentVerification {
                                        check,
                                        status: echo_agent::agent::subagent::SubagentVerificationStatus::Failed,
                                        details: error.chars().take(500).collect(),
                                        source: echo_agent::agent::subagent::SubagentVerificationSource::Observed,
                                    },
                                );
                            }
                            pending_file_access.remove(&call_id);
                            if let Err(store_error) = store.record_tool_finished(
                                &run_id,
                                &task_id,
                                &execution_id,
                                &call_id,
                                &name,
                                false,
                                &error,
                                Some(&failure),
                            ) {
                                event_cancel.cancel();
                                return Err(format!(
                                    "tool {name} failed but its terminal boundary was not persisted: {store_error}"
                                ));
                            }
                            emit_exec(
                                trace_sink.as_ref(),
                                ExecEvent::for_task(
                                    run_id.clone(),
                                    task_id.clone(),
                                    "tool_completed",
                                    serde_json::json!({
                                        "call_id": call_id,
                                        "name": name,
                                        "result": error,
                                        "success": false,
                                        "failure": failure,
                                    }),
                                )
                                .with_agent(agent_role.clone())
                                .with_title(title.clone()),
                            );
                        }
                        AgentEvent::ToolStream {
                            call_id,
                            name,
                            event,
                        } => {
                            let (event_type, payload) = match event {
                                echo_agent::tools::ToolStreamEvent::Progress {
                                    message,
                                    percent,
                                } => ("tool_output", serde_json::json!({
                                    "call_id": call_id,
                                    "name": name,
                                    "message": message,
                                    "percent": percent,
                                })),
                                echo_agent::tools::ToolStreamEvent::Output { channel, chunk } => {
                                    ("tool_output", serde_json::json!({
                                        "call_id": call_id,
                                        "name": name,
                                        "channel": match channel {
                                            echo_agent::tools::ToolOutputChannel::Stdout => "stdout",
                                            echo_agent::tools::ToolOutputChannel::Stderr => "stderr",
                                            echo_agent::tools::ToolOutputChannel::Log => "log",
                                        },
                                        "chunk": chunk,
                                    }))
                                }
                                echo_agent::tools::ToolStreamEvent::Complete(result) => {
                                    if let Some(artifact) = echo_core::tools::artifact::ToolOutputArtifactRef::from_metadata(&result.metadata) {
                                        observed_artifacts.push(
                                            echo_agent::agent::subagent::SubagentArtifact {
                                                path: artifact.path.to_string_lossy().to_string(),
                                                kind: "tool_log".to_string(),
                                                bytes: Some(artifact.artifact_bytes),
                                                sha256: Some(artifact.sha256),
                                                producer_execution_id: Some(execution_id.clone()),
                                                available: artifact.path.is_file(),
                                            },
                                        );
                                    }
                                    (
                                        "tool_completed",
                                        serde_json::json!({
                                        "call_id": call_id,
                                        "name": name,
                                        "success": result.success,
                                        "metadata": result.metadata,
                                        "truncated": result.truncated,
                                        }),
                                    )
                                }
                            };
                            emit_exec(
                                trace_sink.as_ref(),
                                ExecEvent::for_task(
                                    run_id.clone(),
                                    task_id.clone(),
                                    event_type,
                                    payload,
                                )
                                .with_agent(agent_role.clone())
                                .with_title(title.clone()),
                            );
                        }
                        AgentEvent::FinalAnswer(answer) => {
                            #[allow(clippy::collapsible_match)]
                            // guard is a method call on the bound value, not a pattern; collapsing obscures it
                            if !answer.is_empty() {
                                output = answer;
                            }
                        }
                        AgentEvent::Cancelled => {
                            return Err("task cancelled".to_string());
                        }
                        AgentEvent::Error { source, message } => {
                            return Err(format!("{source}: {message}"));
                        }
                        _ => {}
                    }
                }

                let working_dir = agent.working_dir();
                let mut outcome = echo_agent::agent::subagent::parse_subagent_outcome(
                    &output,
                    echo_agent::agent::subagent::SubagentStatus::Completed,
                    Some(&execution_id),
                    working_dir.as_deref(),
                );
                echo_agent::agent::subagent::merge_observed_evidence(
                    &mut outcome,
                    observed_verification,
                    touched_files,
                    observed_artifacts,
                );
                Ok((
                    SubagentTaskResult::from_framework_outcome(&outcome),
                    output,
                ))
            })
        })
        .await
}

fn unavailable_llm_usage_payload(reason: &'static str) -> serde_json::Value {
    serde_json::json!({
        "model": "unknown",
        "prompt_tokens": 0,
        "completion_tokens": 0,
        "total_tokens": 0,
        "cached_prompt_tokens": 0,
        "cache_creation_prompt_tokens": 0,
        "usage_reported": false,
        "reason": reason,
    })
}

/// RAII guard that releases file write locks when dropped (G5).
struct FileLockGuard {
    /// Per-file async mutex guards. Dropping releases all per-file locks.
    _guards: Vec<OwnedMutexGuard<()>>,
}

/// Write a terminal Run record to the trace store when available.
/// Best-effort: trace failures are logged but never fail the run.
fn save_trace(
    run_store: Option<&Arc<dyn echo_agent::trace::RunStore>>,
    run_id: &str,
    goal: &str,
    conversation_id: &str,
    status: &str,
) {
    let Some(rs) = run_store else { return };
    let run = echo_agent::trace::Run {
        run_id: run_id.to_string(),
        parent_run_id: None,
        session_id: conversation_id.to_string(),
        status: match status {
            "completed" => echo_agent::trace::RunStatus::Completed,
            "failed" => echo_agent::trace::RunStatus::Failed,
            "cancelled" => echo_agent::trace::RunStatus::Cancelled,
            _ => echo_agent::trace::RunStatus::Completed,
        },
        input: goal.to_string(),
        events: vec![],
        final_output: None,
        error: if status == "failed" {
            Some("run failed".to_string())
        } else {
            None
        },
        token_usage: Default::default(),
        timings: Default::default(),
        started_at: chrono::Utc::now(),
        finished_at: Some(chrono::Utc::now()),
    };
    let rs = rs.clone();
    let log_id = run_id.to_string();
    tokio::spawn(async move {
        if let Err(e) = rs.save(run).await {
            tracing::warn!(run_id = %log_id, error = %e, "trace Run save failed (non-fatal)");
        } else {
            tracing::debug!(run_id = %log_id, "trace Run saved");
        }
    });
}

// ── Unattended run adapter (cron / background AgentChat) ────────────────

/// Launch an unattended run through the unified TaskRuntime executor,
/// bypassing the chat routing path. Generic over the source kind (cron /
/// background AgentChat) and route.
///
/// Creates a run, then drives the agent's ReAct loop in the run's context so
/// the agent itself calls `plan_create` (to materialise the plan) and
/// `plan_execute` (which internally calls `execute_run`). Simple prompts that
/// the agent answers directly (without `plan_execute`) auto-Complete.
///
/// **Why not call `execute_run` directly?** `execute_run` requires a plan to
/// already exist (`store.get_plan → NoPlan` if absent). The plan is created
/// by the agent during its ReAct loop via the `plan_create` tool. Skipping
/// the agent loop would leave the plan empty and the run would fail
/// immediately. This mirrors how `launch_unified_run` (chat path) works.
///
/// The run is created with `attended_mode = Unattended` so the configured
/// write preflight applies inside `plan_execute` / `execute_task`.
#[allow(clippy::too_many_arguments)] // run identity + agent + cancel + write policy all thread through; matches run_dag style
pub async fn launch_unattended_run(
    store: Arc<TaskRuntimeStore>,
    primary_agent: crate::agent_handle::AgentHandle,
    source_kind: &str,
    source_id: &str,
    fire_id: &str,
    prompt: &str,
    parent_cancel: CancellationToken,
    write_mode: UnattendedWriteMode,
    repo_root: Option<std::path::PathBuf>,
) -> Result<String, ExecError> {
    let run_id = uuid::Uuid::new_v4().to_string();
    let conversation_id = format!("{source_kind}:{source_id}:{fire_id}");

    // 1. Create the run in Pending, attended_mode = Unattended.
    store.create_run(
        &run_id,
        "default",
        &conversation_id,
        "", // root_message_id — no chat message for unattended run
        DomainProfile::General,
        prompt,
        "parallel_readonly_delegation",
        AttendedMode::Unattended,
    )?;

    // 2. Transition Pending → Running.
    store.transition_run(&run_id, TaskRunStatus::Running)?;

    // 3. Drive the agent's ReAct loop + finalize status. Extracted so callers
    //    that own the run_id (e.g. submit_run in Phase 3.4) can drive a run
    //    they created themselves without re-generating the id.
    drive_unattended_run(
        store,
        primary_agent,
        &run_id,
        source_id,
        fire_id,
        prompt,
        parent_cancel,
        write_mode,
        repo_root,
    )
    .await
}

/// Drive an already-created Run to completion via the agent ReAct loop, then
/// finalize its status from the store. Phase 3.4: extracted from
/// `launch_unattended_run` so the caller can own the run_id (create_run +
/// transition_run happen in the caller). This fn only drives the agent's
/// ReAct loop (which may call plan_create + plan_execute) and finalizes the
/// run status (auto-Complete a direct answer, auto-Fail an unexpected Paused).
///
/// U1c stage 2 (D7): when `write_mode == Worktree` and `repo_root` is given,
/// this function creates an isolated git worktree branched from `repo_root`,
/// sets the agent's `working_dir` to the worktree path so every shell/file/
/// git tool runs inside the isolated checkout, and after the run finishes
/// records the worktree diff as a run artifact and keeps the worktree for
/// later human review (no automatic merge — Q1).
#[allow(clippy::too_many_arguments)] // run identity (run_id/source_id/fire_id) + agent + prompt + cancel + mode + repo_root
pub async fn drive_unattended_run(
    store: Arc<TaskRuntimeStore>,
    primary_agent: crate::agent_handle::AgentHandle,
    run_id: &str,
    source_id: &str,
    fire_id: &str,
    prompt: &str,
    parent_cancel: CancellationToken,
    write_mode: UnattendedWriteMode,
    repo_root: Option<std::path::PathBuf>,
) -> Result<String, ExecError> {
    let child_cancel = parent_cancel.child_token();
    let _cancel_registration = store
        .register_run_cancellation(run_id, child_cancel.clone())
        .map_err(|error| ExecError::Other(format!("register run cancellation: {error}")))?;
    let conversation_id_for_scope = store
        .get_run(run_id)
        .ok()
        .flatten()
        .map(|run| run.conversation_id);

    // D7 stage 2: attempt to provision an isolated git worktree for write
    // operations. Lazy: only when mode is Worktree AND a repo_root is given
    // AND the worktree can be created. Failure is a soft fallback to Disabled
    // (logged as warn) so we never fail the whole run just because worktree
    // setup failed — the user can still review why via the warn.
    let worktree: Option<super::worktree::RunWorktree> = if write_mode
        == UnattendedWriteMode::Worktree
    {
        if let Some(ref root) = repo_root {
            // P1-14: RunWorktree::create 内部用 std::process::Command 同步执行
            // `git worktree add`(worktree.rs:84), 在 async 上下文里直接调用会阻塞
            // tokio worker 线程。包进 spawn_blocking 把它丢到阻塞线程池。
            let wt_run_id = run_id.to_string();
            let wt_root = root.clone();
            match tokio::task::spawn_blocking(move || {
                super::worktree::RunWorktree::create(&wt_run_id, &wt_root)
            })
            .await
            {
                Ok(create_result) => match create_result {
                    Ok(wt) => {
                        tracing::info!(
                            run_id = %run_id,
                            branch = %wt.branch,
                            path = %wt.path.display(),
                            "U1c stage 2: unattended worktree provisioned"
                        );
                        Some(wt)
                    }
                    Err(e) => {
                        // SAFETY (D7 stage 2): worktree creation failed under
                        // Worktree mode. We must NOT silently continue — that
                        // would allow writes to land in the main workspace
                        // without isolation (the preflight loosens its write
                        // ban under Worktree mode, expecting isolation to
                        // provide safety). Fail the run hard so the user sees
                        // the problem, rather than silently risking their
                        // uncommitted work.
                        let msg = format!(
                            "Unattended worktree creation failed (mode=Worktree): {}. \
                             Refusing to run without isolation — fix the git state \
                             or set unattended_write_mode=disabled/in_place.",
                            e.message
                        );
                        tracing::error!(
                            run_id = %run_id,
                            error = %e.message,
                            "U1c stage 2: worktree creation failed — failing run (no silent fallback)"
                        );
                        let _ = store.transition_run(run_id, TaskRunStatus::Failed);
                        let _ = store.note(run_id, None, &msg);
                        return Err(ExecError::Other(msg));
                    }
                },
                Err(join_err) => {
                    // spawn_blocking 任务 panic (JoinError)。同等 fail-hard 处理。
                    let msg = format!(
                        "Unattended worktree creation panicked: {join_err}. \
                         Refusing to run without isolation."
                    );
                    tracing::error!(
                        run_id = %run_id,
                        error = %join_err,
                        "U1c stage 2: spawn_blocking join error"
                    );
                    let _ = store.transition_run(run_id, TaskRunStatus::Failed);
                    let _ = store.note(run_id, None, &msg);
                    return Err(ExecError::Other(msg));
                }
            }
        } else {
            // repo_root not available — same safety argument: don't run
            // writes without isolation under Worktree mode.
            let msg = "Unattended run requested Worktree mode but repo_root \
                       could not be resolved. Refusing to run without isolation \
                       — set unattended_write_mode=disabled/in_place or ensure \
                       the run starts inside a git repo."
                .to_string();
            tracing::error!(
                run_id = %run_id,
                "U1c stage 2: no repo_root under Worktree mode — failing run"
            );
            let _ = store.transition_run(run_id, TaskRunStatus::Failed);
            let _ = store.note(run_id, None, &msg);
            return Err(ExecError::Other(msg));
        }
    } else {
        None
    };

    // Drive the agent's ReAct loop in the run's context. The agent will call
    // plan_create (to build the plan) and plan_execute (which internally calls
    // execute_run). The Unattended attended_mode (set by the caller at
    // create_run) ensures unattended preflight checks activate.
    let run_id_for_scope = run_id.to_string();
    let cancel_for_scope = child_cancel.clone();
    let prompt_owned = prompt.to_string();
    let wt_path_for_scope = worktree.as_ref().map(|w| w.path.clone());

    super::task_tools::with_run_context(
        run_id_for_scope.clone(),
        cancel_for_scope.clone(),
        None, // trace_sink — no GUI event stream for unattended run
        async {
            let agent_inner = primary_agent.inner().clone();
            let agent = agent_inner.read().await;
            let invocation = echo_core::agent::AgentInvocationContext {
                runtime: Some(echo_core::tools::ExternalRunContext {
                    conversation_id: conversation_id_for_scope.clone(),
                    run_id: Some(run_id_for_scope.clone()),
                    turn_id: None,
                    execution_id: None,
                    message_id: None, // unattended/cron path has no chat message
                    cancel: Some(std::sync::Arc::new(cancel_for_scope.clone())),
                    trace_sink: None,
                    delegation_policy: None,
                }),
                working_dir: wt_path_for_scope,
                cancel: None,
                disabled_tools: None,
                run_budget: None,
            };
            let event_identity = echo_core::agent::EventIdentity::from_invocation(&invocation);

            // Execute the prompt. The agent's ReAct loop will call
            // plan_create + plan_execute, which runs the plan through
            // execute_run with all safety gates (preflight, approval skip).
            match agent
                .execute_stream_with_invocation_context(
                    &prompt_owned,
                    cancel_for_scope.clone(),
                    invocation,
                )
                .await
            {
                Ok(raw_stream) => {
                    let mut stream =
                        echo_core::agent::envelope_event_stream(raw_stream, event_identity);
                    // Drain the stream to completion. We don't forward events
                    // to a GUI (unattended run has no UI), but we must consume
                    // the stream so the agent finishes its work.
                    while let Some(event_result) = stream.next().await {
                        if cancel_for_scope.is_cancelled() {
                            break;
                        }
                        match event_result {
                            Ok(_) => {}
                            Err(e) => {
                                tracing::warn!(
                                    source_id = %source_id,
                                    run_id = %run_id_for_scope,
                                    error = %e,
                                    "Unattended agent stream error"
                                );
                                break;
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::error!(
                        source_id = %source_id,
                        run_id = %run_id_for_scope,
                        error = %e,
                        "Unattended agent failed to start stream"
                    );
                }
            }
        },
    )
    .await;

    // Determine final outcome from the store (plan_execute/execute_run
    // may have already transitioned the run to a terminal state).
    let final_status = store.get_run(run_id).ok().flatten().map(|r| r.status);

    // D7 stage 2: if we provisioned a worktree, record the diff as an
    // artifact and keep the worktree for later human review (Q1 decision:
    // no automatic merge, just preserve for inspection).
    if let Some(wt) = worktree {
        match wt.diff_summary() {
            Ok(diff) => {
                let artifact = Artifact {
                    id: format!("worktree-diff-{run_id}"),
                    run_id: run_id.to_string(),
                    task_id: None,
                    kind: ArtifactKind::Other,
                    title: format!("Worktree diff for {}", wt.branch),
                    path: Some(wt.path.to_string_lossy().to_string()),
                    metadata: serde_json::json!({
                        "diff": diff,
                        "worktree_path": wt.path.to_string_lossy(),
                        "branch": wt.branch,
                    }),
                };
                if let Err(e) = store.add_artifact(&artifact) {
                    tracing::warn!(
                        run_id = %run_id,
                        error = %e,
                        "Failed to record worktree diff artifact"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    run_id = %run_id,
                    error = %e.message,
                    "Failed to generate worktree diff summary"
                );
            }
        }
        // Q1: keep the worktree, don't remove it. User can review and merge/discard later.
        tracing::info!(
            run_id = %run_id,
            branch = %wt.branch,
            path = %wt.path.display(),
            "U1c stage 2: worktree kept for review (no automatic merge)"
        );
        // Drop worktree handle, but don't remove the directory.
        drop(wt);
    }

    match final_status {
        Some(TaskRunStatus::Completed) => {
            tracing::info!(
                source_id = %source_id,
                fire_id = %fire_id,
                run_id = %run_id,
                "Unattended run completed"
            );
            // B5.1 design: cron/unattended runs use an Ephemeral/DirectReview
            // memory policy — their results surface to the user via the kept
            // worktree diff artifact (above), NOT via recall. So we deliberately
            // do NOT write_memory_candidate here (cron has no recall closure;
            // adding one would be a separate, scoped change). This is distinct
            // from the autonomous chat path (create_complex_task), which DOES
            // block-write its completion memory for recall.
        }
        Some(TaskRunStatus::Failed) => {
            tracing::warn!(
                source_id = %source_id,
                fire_id = %fire_id,
                run_id = %run_id,
                "Unattended run failed"
            );
        }
        Some(TaskRunStatus::Cancelled) => {
            tracing::info!(
                source_id = %source_id,
                fire_id = %fire_id,
                run_id = %run_id,
                "Unattended run cancelled"
            );
        }
        Some(TaskRunStatus::Paused) => {
            tracing::info!(
                source_id = %source_id,
                run_id = %run_id,
                "Unattended run paused and remains resumable"
            );
        }
        _ => {
            // Still Running or unknown — the agent stream ending is not proof
            // that the plan satisfied its result contract.
            if child_cancel.is_cancelled() {
                let _ = store.transition_run(run_id, TaskRunStatus::Cancelled);
            } else {
                let blockers = run_completion_blockers(&store, run_id);
                if blockers.is_empty() {
                    let _ = store.transition_run(run_id, TaskRunStatus::Completed);
                } else {
                    let _ = store.note(
                        run_id,
                        None,
                        &format!(
                            "completion gate rejected unattended run: {}",
                            blockers.join("; ")
                        ),
                    );
                    let _ = store.transition_run(run_id, TaskRunStatus::Failed);
                }
            }
            tracing::info!(
                source_id = %source_id,
                fire_id = %fire_id,
                run_id = %run_id,
                final_status = ?final_status,
                "Unattended run agent stream finished; transitioned to terminal"
            );
        }
    }

    Ok(run_id.to_string())
}

/// Cron-specific thin wrapper: routes through `launch_unattended_run` with
/// the `ParallelReadonlyDelegation` route (legacy `[plan]` behavior, Phase 3.1).
pub async fn launch_cron_run(
    store: Arc<TaskRuntimeStore>,
    primary_agent: crate::agent_handle::AgentHandle,
    cron_task_id: &str,
    fire_id: &str,
    prompt: &str,
    parent_cancel: CancellationToken,
) -> Result<String, ExecError> {
    launch_unattended_run(
        store,
        primary_agent,
        "cron",
        cron_task_id,
        fire_id,
        prompt,
        parent_cancel,
        UnattendedWriteMode::default(), // D7 stage 2: Worktree (safe default)
        super::worktree::git_repo_root(std::path::Path::new(".")).ok(), // best-effort repo_root
    )
    .await
}

// ── Unattended preflight (dual-checkpoint, spec §4.2 v2) ───────────────

/// Preflight error for unattended runs — terminal, never Paused.
#[derive(Debug, Clone)]
pub struct PreflightRejection {
    pub reason: String,
}

/// Tool-name allowlist for unattended `ReadOnlyPlanNoShell` runs.
///
/// §A: A2 (allow network) — local read-only tools + readonly network tools.
/// Write / execute / shell tools are NOT on this list.
/// Tool names verified against actual `Tool::name()` registrations.
const UNATTENDED_READONLY_TOOLS: &[&str] = &[
    // Local read-only
    "read_file",
    "list_dir",
    "grep",
    "glob",
    "code_search",
    "task_list",
    "plan_create", // plan construction only (not execution)
    "task_update",
    "plan_execute", // plan materialisation trigger
    // Read-only network (§A = A2)
    "web_search",
    "web_fetch",
];

/// Checkpoint A: scan the full plan after materialisation, before execution.
///
/// Returns `Ok(())` if every task in the plan passes the three-layer check:
/// 1. task kind in `is_unattended_readonly_allowed()` whitelist
/// 2. every `allowed_tools` entry is in `UNATTENDED_READONLY_TOOLS`
/// 3. no shell/test commands (verification must be empty)
///
/// The three layers are enforced only when `mode` is [`UnattendedWriteMode::Disabled`]
/// (D7 stage 2). Under `Worktree` / `InPlace` the safety comes from isolation
/// rather than prohibition, so all layers are skipped.
///
/// On violation → `Err(PreflightRejection)` — terminal fail, never Paused.
pub fn preflight_unattended_plan(
    tasks: &[PlanTask],
    mode: UnattendedWriteMode,
) -> Result<(), PreflightRejection> {
    // Under Worktree / InPlace, write safety is provided by the execution
    // environment (isolated worktree or user consent), not by banning.
    if mode.writes_allowed() {
        return Ok(());
    }
    // Disabled: stage-1 read-only enforcement.
    for t in tasks {
        // Layer 1: task kind whitelist
        if !t.kind.is_unattended_readonly_allowed() {
            return Err(PreflightRejection {
                reason: format!(
                    "task kind '{}' is not allowed in unattended ReadOnlyPlanNoShell mode \
                     (allowed: ReadOnlyReview, Investigation, Summary)",
                    t.kind.as_str()
                ),
            });
        }
        // Layer 2: tool allowlist (only if the task declares tools)
        for tool_name in &t.allowed_tools {
            if !UNATTENDED_READONLY_TOOLS.contains(&tool_name.as_str()) {
                return Err(PreflightRejection {
                    reason: format!(
                        "tool '{}' is not in the unattended readonly allowlist (task '{}')",
                        tool_name, t.id
                    ),
                });
            }
        }
        // Layer 3: no shell/test commands
        if !t.verification.is_empty() {
            return Err(PreflightRejection {
                reason: format!(
                    "task '{}' declares verification/shell commands — \
                     shell is DisabledByDefault in unattended mode",
                    t.id
                ),
            });
        }
    }
    Ok(())
}

/// Checkpoint B: per-task preflight — same three layers, for a single task.
/// Called before each task acquires its permit in `execute_task`.
pub fn preflight_unattended_task(
    task: &PlanTask,
    mode: UnattendedWriteMode,
) -> Result<(), PreflightRejection> {
    preflight_unattended_plan(std::slice::from_ref(task), mode)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Preflight tests (Phase B) ─────────────────────────────────────────

    /// Helper: build a `PlanTask` stub with just the fields the preflight
    /// gate actually inspects (kind / allowed_tools / verification).
    fn preflight_task(
        id: &str,
        kind: PlanTaskKind,
        tools: &[&str],
        verification: &[&str],
    ) -> PlanTask {
        PlanTask {
            id: id.to_string(),
            title: id.to_string(),
            description: String::new(),
            kind,
            agent_role: "general".to_string(),
            domain_profile: DomainProfile::General,
            depends_on: Vec::new(),
            parallel_group: None,
            files: Vec::new(),
            allowed_tools: tools.iter().map(|s| s.to_string()).collect(),
            required_artifacts: Vec::new(),
            verification: verification.iter().map(|s| s.to_string()).collect(),
            retry_count: 0,
            max_retries: 0,
            failure_fingerprint: None,
            status: TodoStatus::Pending,
            sort_order: 0,
        }
    }

    fn ownership_task(id: &str, kind: PlanTaskKind, files: &[&str]) -> PlanTask {
        let mut task = preflight_task(id, kind, &[], &[]);
        task.files = files.iter().map(|file| file.to_string()).collect();
        task
    }

    #[test]
    fn ownership_wave_runs_disjoint_writers_together() {
        let wave = select_ownership_safe_wave(vec![
            ownership_task("writer-a", PlanTaskKind::Implementation, &["src/a.rs"]),
            ownership_task("writer-b", PlanTaskKind::Debugging, &["src/b.rs"]),
            ownership_task("reader", PlanTaskKind::Investigation, &["src/a.rs"]),
        ]);
        let ids: Vec<&str> = wave.iter().map(|task| task.id.as_str()).collect();
        assert_eq!(ids, vec!["writer-a", "writer-b", "reader"]);
    }

    #[test]
    fn ownership_wave_defers_overlapping_writer() {
        let wave = select_ownership_safe_wave(vec![
            ownership_task("writer-a", PlanTaskKind::Implementation, &["src/shared.rs"]),
            ownership_task("writer-b", PlanTaskKind::Debugging, &["src/shared.rs"]),
            ownership_task("writer-c", PlanTaskKind::Implementation, &["src/c.rs"]),
        ]);
        let ids: Vec<&str> = wave.iter().map(|task| task.id.as_str()).collect();
        assert_eq!(ids, vec!["writer-a", "writer-c"]);
    }

    #[test]
    fn ownership_wave_unknown_writer_serializes_from_writers_but_not_readers() {
        let wave = select_ownership_safe_wave(vec![
            ownership_task("unknown", PlanTaskKind::Implementation, &[]),
            ownership_task("writer", PlanTaskKind::Implementation, &["src/a.rs"]),
            ownership_task("reader", PlanTaskKind::Review, &["src/a.rs"]),
        ]);
        let ids: Vec<&str> = wave.iter().map(|task| task.id.as_str()).collect();
        assert_eq!(ids, vec!["unknown", "reader"]);
    }

    #[test]
    fn runtime_contract_distinguishes_requested_and_observed_isolation() -> Result<(), String> {
        let contract = SubagentRuntimeContract {
            prompt_source: "builtin:implementer".to_string(),
            isolation_requested: "worktree".to_string(),
            context_in: "task context".to_string(),
            returns: "summary".to_string(),
        };
        let started = runtime_contract_started_payload(&contract);
        if started.get("isolation").is_some() {
            return Err(
                "legacy isolation field must not claim configured isolation happened".into(),
            );
        }
        if started
            .get("isolation_requested")
            .and_then(|value| value.as_str())
            != Some("worktree")
        {
            return Err("started event must report requested worktree isolation".into());
        }
        if started.get("isolation_observed").is_some() {
            return Err("started event must not invent observed isolation".into());
        }

        let fallback = runtime_isolation_observed_payload(&contract, "primary-fallback");
        if fallback
            .get("isolation_observed")
            .and_then(|value| value.as_str())
            != Some("primary-fallback")
        {
            return Err("writer fallback must report primary-fallback observation".into());
        }
        Ok(())
    }

    #[tokio::test]
    async fn writer_runtime_contract_requests_worktree_without_claiming_fallback()
    -> Result<(), String> {
        let agent = echo_agent::agent::ReactAgentBuilder::new()
            .model("test-model")
            .system_prompt("test")
            .build()
            .map_err(|error| error.to_string())?;
        let handle = crate::agent_handle::AgentHandle::new(agent);

        let contract =
            subagent_runtime_contract(&handle, "missing-writer", &PlanTaskKind::Implementation)
                .await;
        if contract.isolation_requested != "worktree" {
            return Err(format!(
                "writer must request worktree isolation, got {}",
                contract.isolation_requested
            ));
        }
        Ok(())
    }

    #[test]
    fn primary_isolation_event_reaches_sink_before_terminal() -> Result<(), String> {
        let recorded = Arc::new(std::sync::Mutex::new(Vec::<ExecEvent>::new()));
        let sink_recorded = Arc::clone(&recorded);
        let sink: ExecSink = Arc::new(move |event| {
            sink_recorded
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(event);
        });
        let task = PlanTask {
            id: "task-1".to_string(),
            title: "Inspect runtime".to_string(),
            description: "Inspect context lifecycle".to_string(),
            kind: PlanTaskKind::Investigation,
            agent_role: "explorer".to_string(),
            ..PlanTask::default()
        };
        let contract = SubagentRuntimeContract {
            prompt_source: "builtin:explorer".to_string(),
            isolation_requested: "primary".to_string(),
            context_in: "task context".to_string(),
            returns: "summary".to_string(),
        };

        emit_task_started(Some(&sink), "run-1", "task-1", &task, &contract);
        emit_task_isolation_observed(Some(&sink), "run-1", "task-1", &task, &contract, "primary");
        emit_exec(
            Some(&sink),
            ExecEvent::for_task(
                "run-1",
                "task-1",
                "completed",
                serde_json::json!({"output": "done"}),
            ),
        );

        let events = recorded.lock().unwrap_or_else(|error| error.into_inner());
        let event_names: Vec<&str> = events.iter().map(|event| event.event.as_str()).collect();
        if event_names != ["started", "isolation_observed", "completed"] {
            return Err(format!("unexpected event ordering: {event_names:?}"));
        }
        let started = events
            .first()
            .ok_or_else(|| "missing started event".to_string())?;
        let observed = events
            .get(1)
            .ok_or_else(|| "missing isolation observation".to_string())?;
        if started.payload.get("isolation").is_some() || observed.payload.get("isolation").is_some()
        {
            return Err("backend must not emit the legacy isolation field".to_string());
        }
        if started
            .payload
            .get("isolation_requested")
            .and_then(|value| value.as_str())
            != Some("primary")
            || observed
                .payload
                .get("isolation_observed")
                .and_then(|value| value.as_str())
                != Some("primary")
        {
            return Err("requested/observed isolation fields were not delivered".to_string());
        }
        Ok(())
    }

    #[test]
    fn preflight_disabled_rejects_write_kinds() {
        // B1: stage-1 regression — under Disabled, write kinds are rejected.
        let task = preflight_task("t1", PlanTaskKind::Implementation, &[], &[]);
        let result = preflight_unattended_plan(&[task], UnattendedWriteMode::Disabled);
        assert!(
            result.is_err(),
            "write kind should be rejected under Disabled"
        );
        let reason = result.unwrap_err().reason;
        assert!(
            reason.contains("implementation"),
            "reason should mention 'implementation', got {reason:?}"
        );
    }

    #[test]
    fn worker_output_can_suggest_followup_tasks() -> Result<(), String> {
        let output = r#"
Read the runtime path and found one missing branch.

```json
{
  "suggested_tasks": [
    {
      "title": "Verify resume branch",
      "description": "Trace resume_task_run through the runtime store.",
      "kind": "investigation",
      "agent_role": "explorer",
      "dependencies": ["t1"],
      "why_needed": "The current task found an unverified resume path.",
      "risk": "low"
    }
  ]
}
```
"#;
        let tasks = extract_suggested_tasks_from_worker_output(output);
        assert_eq!(tasks.len(), 1);
        let task = tasks
            .first()
            .ok_or_else(|| "expected one suggested task".to_string())?;
        assert_eq!(task.title, "Verify resume branch");
        assert_eq!(task.kind, PlanTaskKind::Investigation);
        assert_eq!(task.agent_role, "explorer");
        assert_eq!(task.dependencies, vec!["t1".to_string()]);
        Ok(())
    }

    #[test]
    fn suggested_task_title_duplicate_detects_project_word_variation() {
        assert!(task_titles_look_duplicate(
            "Analyze the **task system** of echo-agent project. Focus on runtime todos.",
            "Analyze the **task system** of echo-agent. Focus on task runtime."
        ));
        assert!(task_titles_look_duplicate(
            "Analyze the **configuration and skills system** of echo-agent project.",
            "Analyze the **configuration and skills system** of echo-agent."
        ));
        assert!(!task_titles_look_duplicate(
            "Analyze the frontend runtime state.",
            "Analyze the configuration and skills system."
        ));
    }

    #[test]
    fn append_suggested_tasks_skips_existing_plan_titles() -> Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|e| e.to_string())?);
        store
            .create_run(
                "r-dedupe",
                "ws",
                "c1",
                "m1",
                DomainProfile::General,
                "analyze project",
                "",
                AttendedMode::Attended,
            )
            .map_err(|e| e.to_string())?;

        let parent = PlanTask {
            id: "t-parent".into(),
            title: "Analyze backend runtime".into(),
            description: "Inspect runtime architecture.".into(),
            kind: PlanTaskKind::Investigation,
            agent_role: "explorer".into(),
            ..Default::default()
        };
        let existing = PlanTask {
            id: "t-task-system".into(),
            title: "Analyze the **task system** of echo-agent. Focus on task runtime.".into(),
            description: "Inspect plan creation and execution.".into(),
            kind: PlanTaskKind::Investigation,
            agent_role: "explorer".into(),
            ..Default::default()
        };
        store
            .attach_plan(&TaskPlan {
                plan_id: "p-dedupe".into(),
                run_id: "r-dedupe".into(),
                domain_profile: DomainProfile::General,
                goal: "analyze project".into(),
                assumptions: vec![],
                risks: vec![],
                execution_mode: ExecutionMode::Parallel,
                tasks: vec![parent.clone(), existing],
            })
            .map_err(|e| e.to_string())?;

        append_suggested_tasks_to_plan(
            &store,
            "r-dedupe",
            &parent,
            &[SuggestedTask {
                title: "Analyze the **task system** of echo-agent project. Focus on todos.".into(),
                description: "Duplicate of an existing task-system analysis task.".into(),
                kind: PlanTaskKind::Investigation,
                agent_role: "explorer".into(),
                dependencies: vec!["t-parent".into()],
                why_needed: "Subagent thinks this is still needed.".into(),
                risk: "low".into(),
            }],
        );

        let plan = store
            .get_plan("r-dedupe")
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "expected plan after append".to_string())?;
        assert_eq!(plan.tasks.len(), 2);
        Ok(())
    }

    #[test]
    fn preflight_disabled_rejects_write_tools() {
        // B1: under Disabled, tools outside the readonly allowlist are rejected.
        let task = preflight_task("t1", PlanTaskKind::Investigation, &["write_file"], &[]);
        let result = preflight_unattended_plan(&[task], UnattendedWriteMode::Disabled);
        assert!(
            result.is_err(),
            "write tool should be rejected under Disabled"
        );
        let reason = result.unwrap_err().reason;
        assert!(
            reason.contains("write_file"),
            "reason should mention 'write_file', got {reason:?}"
        );
    }

    #[test]
    fn preflight_disabled_rejects_verification_shell() {
        // B1: under Disabled, any verification (shell) entry is rejected.
        let task = preflight_task("t1", PlanTaskKind::Investigation, &[], &["cargo test"]);
        let result = preflight_unattended_plan(&[task], UnattendedWriteMode::Disabled);
        assert!(
            result.is_err(),
            "shell verification should be rejected under Disabled"
        );
        let reason = result.unwrap_err().reason;
        assert!(
            reason.contains("verification/shell"),
            "reason should mention shell, got {reason:?}"
        );
    }

    #[test]
    fn preflight_disabled_passes_readonly_readonly() {
        // B1: read-only task with read-only tools and no verification passes.
        let task = preflight_task(
            "t1",
            PlanTaskKind::ReadOnlyReview,
            &["read_file", "grep"],
            &[],
        );
        let result = preflight_unattended_plan(&[task], UnattendedWriteMode::Disabled);
        assert!(
            result.is_ok(),
            "readonly plan should pass under Disabled, got {result:?}"
        );
    }

    #[test]
    fn preflight_worktree_permits_write_kinds_and_tools() {
        // B2: under Worktree, write safety comes from isolation — the
        // preflight gate is fully skipped.
        let write_task = preflight_task(
            "w1",
            PlanTaskKind::Implementation,
            &["write_file", "shell"],
            &["cargo check"],
        );
        let result = preflight_unattended_plan(&[write_task], UnattendedWriteMode::Worktree);
        assert!(
            result.is_ok(),
            "write task should pass under Worktree (safety from isolation), got {result:?}"
        );
    }

    #[test]
    fn preflight_inplace_permits_write_kinds_and_tools() {
        // B3: under InPlace, user has explicitly consented — preflight is
        // fully skipped.
        let write_task = preflight_task(
            "w1",
            PlanTaskKind::Implementation,
            &["write_file", "shell"],
            &["cargo check"],
        );
        let result = preflight_unattended_plan(&[write_task], UnattendedWriteMode::InPlace);
        assert!(
            result.is_ok(),
            "write task should pass under InPlace (user consent), got {result:?}"
        );
    }

    // ── Phase 3.4 regression ─────────────────────────────────────────────

    #[tokio::test]
    async fn launch_unattended_run_returns_run_id() {
        // Phase 3.4-1: launch_unattended_run must return the run_id so callers
        // (submit) can hand it to the Tauri layer. A simple prompt (mock returns
        // "ok", agent never calls plan_execute) auto-Completes (Q5).
        use echo_agent::testing::MockLlmClient;
        use std::sync::Arc;
        let store = Arc::new(TaskRuntimeStore::new_in_memory().expect("in-memory store"));
        let mock = Arc::new(
            MockLlmClient::new()
                .with_model_name("t")
                .with_response("ok"),
        );
        let agent = crate::agent_handle::AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("t")
                .llm_client(mock)
                .build()
                .expect("test agent should build"),
        );
        let cancel = echo_agent::agent::CancellationToken::new();
        let run_id = launch_unattended_run(
            store.clone(),
            agent,
            "test",
            "src-1",
            "fire-1",
            "hello",
            cancel,
            UnattendedWriteMode::Disabled,
            None,
        )
        .await
        .expect("unattended run should succeed");
        // The returned id must key a real run that auto-Completed (the mock
        // returns a direct answer, so plan_execute never runs and the finalize
        // branch auto-Completes — this verifies the contract survived the
        // extraction: a non-empty id that maps to a Completed run).
        let run = store
            .get_run(&run_id)
            .expect("get_run should succeed")
            .expect("run should exist");
        assert_eq!(run.status, TaskRunStatus::Completed);
    }

    #[test]
    fn concurrency_limits_clamp_pool_value() {
        // composite_parallelism reports 0/1/N → workers clamp to [1,8].
        // We can't easily build a pool in a unit test, so test the clamp math.
        let clamp = |n: usize| n.clamp(1, 8);
        assert_eq!(clamp(0), 1);
        assert_eq!(clamp(1), 1);
        assert_eq!(clamp(4), 4);
        assert_eq!(clamp(20), 8);
    }

    #[test]
    fn task_prompt_is_read_only_for_reviews() {
        let task = PlanTask {
            id: "t1".into(),
            title: "Review chat.rs".into(),
            description: "find bugs".into(),
            kind: PlanTaskKind::ReadOnlyReview,
            files: vec!["chat.rs".into()],
            verification: vec!["report root cause".into()],
            ..Default::default()
        };
        let p = build_task_prompt(
            &task,
            &[],
            echo_agent::tasks::NestedDelegationPolicy::default(),
            Some("Fix the GUI context runtime"),
        );
        assert!(p.contains("Parent goal: Fix the GUI context runtime"));
        assert!(p.contains("READ-ONLY"));
        assert!(p.contains("chat.rs"));
        assert!(p.contains("report root cause"));
        assert!(p.contains("Do not delegate this task to other agents"));
        assert!(p.contains("Return contract"));
        assert!(p.contains("## Result"));
        assert!(p.contains("\"contract_version\":1"));
        assert!(p.contains("\"touched_files\""));
    }

    #[test]
    fn task_prompt_marks_empty_writer_scope_as_unknown() {
        let task = PlanTask {
            id: "t2".into(),
            title: "Apply fix".into(),
            description: "patch the bug".into(),
            kind: PlanTaskKind::Implementation,
            ..Default::default()
        };
        let p = build_task_prompt(
            &task,
            &[],
            echo_agent::tasks::NestedDelegationPolicy::default(),
            None,
        );
        assert!(!p.contains("READ-ONLY"));
        assert!(p.contains("UNKNOWN-SCOPE WRITE"));
        assert!(p.contains("serialized from other writers"));
    }

    #[test]
    fn task_prompt_allows_nested_delegation_when_policy_allows() {
        let task = PlanTask {
            id: "t2_delegate".into(),
            title: "Coordinate review".into(),
            description: "split investigation across specialists".into(),
            kind: PlanTaskKind::Investigation,
            ..Default::default()
        };
        let p = build_task_prompt(
            &task,
            &[],
            echo_agent::tasks::NestedDelegationPolicy {
                can_spawn_subagents: true,
                delegate_depth: 0,
                max_delegate_depth: 2,
            },
            None,
        );
        assert!(p.contains("may use agent_tool"));
        assert!(p.contains("within this PlanTask only"));
        assert!(p.contains("do not modify the global plan"));
        assert!(!p.contains("Do not delegate this task to other agents"));
    }

    #[test]
    fn run_outcome_failed_carries_task_id() {
        let o = RunOutcome::Failed {
            failed_task_id: "t3".into(),
            error: "boom".into(),
        };
        match o {
            RunOutcome::Failed { failed_task_id, .. } => assert_eq!(failed_task_id, "t3"),
            _ => panic!(),
        }
    }

    /// Integration-ish test: a 4-task read-only wave + 1 implementation
    /// dependent should complete with all todos Completed, using an in-memory
    /// store. We can't run a real agent in a unit test, so this exercises the
    /// store/state-machine side only (the dispatcher path is covered by the
    /// GUI walkthrough in PR 6 + an integration test).
    #[tokio::test]
    async fn store_transitions_through_running_to_completed() {
        use std::sync::Arc;
        let store = Arc::new(TaskRuntimeStore::new_in_memory().expect("in-memory store"));
        // Seed a run + plan via the public store API, then drive the state
        // machine the way run_dag would.
        store
            .create_run(
                "r1",
                "ws",
                "c1",
                "m1",
                DomainProfile::AiCoding,
                "g",
                "",
                AttendedMode::Attended,
            )
            .unwrap();
        let plan = TaskPlan {
            plan_id: "p1".into(),
            run_id: "r1".into(),
            domain_profile: DomainProfile::AiCoding,
            goal: "g".into(),
            assumptions: vec![],
            risks: vec![],
            execution_mode: ExecutionMode::Parallel,
            tasks: vec![PlanTask {
                id: "t1".into(),
                title: "Review".into(),
                kind: PlanTaskKind::ReadOnlyReview,
                agent_role: "code_reviewer".into(),
                ..Default::default()
            }],
        };
        store.attach_plan(&plan).unwrap();

        // Simulate the executor: Running, mark task running then
        // completed, then Running → Completed.
        store.transition_run("r1", TaskRunStatus::Running).unwrap();
        store
            .set_task_status("r1", "t1", TodoStatus::Running, Some("code_reviewer"), None)
            .unwrap();
        store
            .set_task_status(
                "r1",
                "t1",
                TodoStatus::Completed,
                Some("code_reviewer"),
                Some("done"),
            )
            .unwrap();
        store
            .transition_run("r1", TaskRunStatus::Completed)
            .unwrap();

        let run = store.get_run("r1").unwrap().unwrap();
        assert_eq!(run.status, TaskRunStatus::Completed);
        let todos = store.list_todos("r1").unwrap();
        assert_eq!(todos[0].status, TodoStatus::Completed);
        assert!(todos[0].summary.as_deref() == Some("done"));
    }

    // ── run_dag integration tests with a scripted (mock) worker ──
    // These exercise the scheduling core — frontier computation, dependency
    // resolution, failure propagation, cancellation, stall detection — without
    // a real LLM. The dispatcher returns scripted results keyed by task id.

    use std::collections::HashMap as StdHashMap;
    use std::sync::Mutex as StdMutex;

    /// A dispatcher that returns scripted results per task id and records the
    /// order tasks were dispatched. Semaphores/locks are ignored (the mock
    /// answers instantly).
    struct ScriptedDispatcher {
        /// task_id → result to return. Missing id → generic success.
        results: StdMutex<StdHashMap<String, Result<SubagentTaskResult, String>>>,
        /// Dispatch order, appended as tasks are picked up.
        order: StdMutex<Vec<String>>,
        /// task_id → integration error returned after review.
        integration_failures: StdMutex<StdHashMap<String, String>>,
    }

    impl ScriptedDispatcher {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                results: StdMutex::new(StdHashMap::new()),
                order: StdMutex::new(Vec::new()),
                integration_failures: StdMutex::new(StdHashMap::new()),
            })
        }
        /// Script a success result for `id`.
        fn succeed(self: &Arc<Self>, id: &str, summary: &str) {
            self.results
                .lock()
                .unwrap()
                .insert(id.into(), Ok(successful_task_result(summary)));
        }
        /// Script a structured terminal result for `id`.
        fn respond(self: &Arc<Self>, id: &str, result: SubagentTaskResult) {
            self.results
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .insert(id.into(), Ok(result));
        }
        /// Script a failure result for `id`.
        fn fail(self: &Arc<Self>, id: &str, err: &str) {
            self.results
                .lock()
                .unwrap()
                .insert(id.into(), Err(err.into()));
        }
        fn order(&self) -> Vec<String> {
            self.order.lock().unwrap().clone()
        }
        fn fail_integration(self: &Arc<Self>, id: &str, error: &str) {
            self.integration_failures
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(id.to_string(), error.to_string());
        }
    }

    impl TaskDispatcher for Arc<ScriptedDispatcher> {
        fn dispatch(
            &self,
            _store: Arc<TaskRuntimeStore>,
            context: echo_agent::tasks::TaskWorkerContext,
            task: PlanTask,
            _worker_sem: Arc<Semaphore>,
            _write_sem: Arc<Semaphore>,
            _shell_sem: Arc<Semaphore>,
            _llm_sem: Arc<Semaphore>,
            _file_write_locks: Arc<std::sync::Mutex<HashMap<String, Arc<TokioMutex<()>>>>>,
            _trace_sink: Option<ExecSink>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = TaskDispatchResult> + Send>>
        {
            let results = self.results.lock().unwrap().get(&task.id).cloned();
            self.order.lock().unwrap().push(task.id.clone());
            let task_id = task.id.clone();
            Box::pin(async move {
                // Honor cancellation even in the mock.
                if context.cancel.is_cancelled() {
                    return Err((task_id, "cancelled".into()));
                }
                match results {
                    Some(Ok(result)) => Ok((task_id, result)),
                    Some(Err(e)) => Err((task_id, e)),
                    // Default: generic success for unscripted tasks.
                    None => Ok((task_id, successful_task_result("ok"))),
                }
            })
        }

        fn integrate(
            &self,
            _store: Arc<TaskRuntimeStore>,
            _run_id: String,
            task: PlanTask,
            _execution_id: String,
            _cancel: CancellationToken,
            _trace_sink: Option<ExecSink>,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<
                            Option<
                                crate::tasks::task_runtime::worktree::WorktreeIntegrationOutcome,
                            >,
                            String,
                        >,
                    > + Send,
            >,
        > {
            let error = self
                .integration_failures
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(&task.id)
                .cloned();
            Box::pin(async move {
                match error {
                    Some(error) => Err(error),
                    None => Ok(None),
                }
            })
        }
    }

    fn successful_task_result(summary: &str) -> SubagentTaskResult {
        SubagentTaskResult {
            contract_version: 1,
            status: SubagentRunStatus::Completed,
            summary: summary.to_string(),
            artifacts: Vec::new(),
            verification: Vec::new(),
            remaining_work: Vec::new(),
            touched_files: SubagentTouchedFiles::default(),
        }
    }

    #[test]
    fn task_result_contract_requires_observed_evidence_and_integrity() -> Result<(), String> {
        assert!(!verification_matches(
            "cargo test --workspace",
            "echo cargo test --workspace"
        ));
        let task = PlanTask {
            id: "contract".to_string(),
            title: "Contract".to_string(),
            verification: vec!["cargo test --workspace".to_string()],
            required_artifacts: vec!["reports/result.json".to_string()],
            ..PlanTask::default()
        };
        let mut result = successful_task_result("work finished");
        result.remaining_work = vec!["write final report".to_string()];
        result.verification.push(SubagentVerificationResult {
            check: "cargo test --workspace".to_string(),
            status: SubagentVerificationStatus::Passed,
            details: "claimed by worker".to_string(),
            source: SubagentVerificationSource::Reported,
        });
        result.artifacts.push(SubagentArtifactResult {
            path: "reports/result.json".to_string(),
            kind: "report".to_string(),
            bytes: Some(12),
            sha256: None,
            producer_execution_id: None,
            available: true,
        });

        let issues = validate_task_result(&task, &result)
            .err()
            .ok_or_else(|| "incomplete result unexpectedly passed".to_string())?;
        assert!(issues.iter().any(|issue| issue.contains("remaining work")));
        assert!(
            issues
                .iter()
                .any(|issue| issue.contains("no observed pass"))
        );
        assert!(
            issues
                .iter()
                .any(|issue| issue.contains("integrity metadata"))
        );

        result.remaining_work.clear();
        if let Some(verification) = result.verification.first_mut() {
            verification.source = SubagentVerificationSource::Observed;
        }
        if let Some(artifact) = result.artifacts.first_mut() {
            artifact.sha256 = Some("a".repeat(64));
            artifact.producer_execution_id = Some("contract:1".to_string());
        }
        assert!(validate_task_result(&task, &result).is_ok());
        Ok(())
    }

    #[tokio::test]
    async fn run_dag_rejects_result_without_observed_required_verification() -> Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let task = PlanTask {
            id: "verify".to_string(),
            title: "Verify".to_string(),
            kind: PlanTaskKind::ReadOnlyReview,
            agent_role: "reviewer".to_string(),
            verification: vec!["cargo test --workspace".to_string()],
            max_retries: 0,
            ..PlanTask::default()
        };
        let run_id = seed_run(&store, vec![task.clone()]);
        let worker = ScriptedDispatcher::new();
        let mut result = successful_task_result("tests claimed complete");
        result.verification.push(SubagentVerificationResult {
            check: "cargo test --workspace".to_string(),
            status: SubagentVerificationStatus::Passed,
            details: "worker report only".to_string(),
            source: SubagentVerificationSource::Reported,
        });
        worker.respond(&task.id, result);

        let outcome = run_dag(
            store.clone(),
            worker,
            None,
            &run_id,
            vec![task],
            ConcurrencyLimits::default(),
            CancellationToken::new(),
            None,
        )
        .await
        .map_err(|error| error.to_string())?;

        assert!(matches!(outcome, RunOutcome::Failed { .. }));
        let todo = store
            .list_todos(&run_id)
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|todo| todo.task_id == "verify")
            .ok_or_else(|| "verify todo missing".to_string())?;
        assert_eq!(todo.status, TodoStatus::Failed);
        Ok(())
    }

    #[test]
    fn run_completion_gate_requires_durable_structured_result() -> Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let task = solo_readonly_task("completed-task");
        let run_id = seed_run(&store, vec![task.clone()]);
        store
            .set_task_status(
                &run_id,
                &task.id,
                TodoStatus::Completed,
                Some(&task.agent_role),
                Some("claimed complete"),
            )
            .map_err(|error| error.to_string())?;

        let blockers = run_completion_blockers(&store, &run_id);
        assert!(
            blockers
                .iter()
                .any(|blocker| blocker.contains("without a structured result"))
        );

        store
            .put_summary(&TaskExecutionSummary {
                run_id: run_id.clone(),
                task_id: task.id.clone(),
                worker_agent: task.agent_role.clone(),
                result: successful_task_result("durable result"),
                decisions: Vec::new(),
                next_implications: Vec::new(),
                suggested_tasks: Vec::new(),
                created_at: chrono::Utc::now(),
            })
            .map_err(|error| error.to_string())?;
        assert!(run_completion_blockers(&store, &run_id).is_empty());
        Ok(())
    }

    /// Helper: a single-task plan (read-only, no review needed) that the
    /// scripted dispatcher can complete.
    fn solo_readonly_task(id: &str) -> PlanTask {
        PlanTask {
            id: id.into(),
            title: id.into(),
            description: "desc".into(),
            kind: PlanTaskKind::ReadOnlyReview,
            agent_role: "reviewer".into(),
            ..Default::default()
        }
    }

    /// Build a run + plan in the store and return the run id.
    ///
    /// Creates run (Pending), attaches plan (no status change), transitions
    /// Pending → Running so run_dag can start.
    fn seed_run(store: &Arc<TaskRuntimeStore>, tasks: Vec<PlanTask>) -> String {
        let run_id = format!("run_{}", uuid::Uuid::new_v4());
        store
            .create_run(
                &run_id,
                "ws_test",
                "conv_test",
                "msg_test",
                DomainProfile::General,
                "test goal",
                "",
                AttendedMode::Attended,
            )
            .unwrap();
        let plan = TaskPlan {
            plan_id: format!("plan_{}", run_id),
            run_id: run_id.clone(),
            domain_profile: DomainProfile::General,
            goal: "test goal".into(),
            assumptions: vec![],
            risks: vec![],
            execution_mode: ExecutionMode::Sequential,
            tasks,
        };
        store.attach_plan(&plan).unwrap();
        store
            .transition_run(&run_id, TaskRunStatus::Running)
            .unwrap();
        run_id
    }

    #[tokio::test]
    async fn run_dag_completes_single_task() {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().unwrap());
        let run_id = seed_run(&store, vec![solo_readonly_task("a")]);
        let worker = ScriptedDispatcher::new();
        worker.succeed("a", "reviewed");

        let outcome = run_dag(
            store.clone(),
            worker.clone(),
            None, // no reviewer LLM → read-only tasks auto-pass review
            &run_id,
            vec![solo_readonly_task("a")],
            ConcurrencyLimits::default(),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap();

        assert!(matches!(outcome, RunOutcome::Completed));
        let todos = store.list_todos(&run_id).unwrap();
        assert_eq!(todos[0].status, TodoStatus::Completed);
    }

    #[tokio::test]
    async fn run_dag_reuses_durable_worker_result_after_restart() -> Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let task = solo_readonly_task("a");
        let run_id = seed_run(&store, vec![task.clone()]);
        store
            .record_worker_assigned(&run_id, "a", "a:1", "reviewer", 1, true)
            .map_err(|error| error.to_string())?;
        let recovered_result = successful_task_result("recovered summary");
        store
            .record_worker_released(&run_id, "a", "a:1", "completed", Some(&recovered_result))
            .map_err(|error| error.to_string())?;
        let worker = ScriptedDispatcher::new();

        let outcome = run_dag(
            store.clone(),
            worker.clone(),
            None,
            &run_id,
            vec![task],
            ConcurrencyLimits::default(),
            CancellationToken::new(),
            None,
        )
        .await
        .map_err(|error| error.to_string())?;

        assert!(matches!(outcome, RunOutcome::Completed));
        assert!(
            worker.order().is_empty(),
            "durable worker was dispatched again"
        );
        let todo = store
            .list_todos(&run_id)
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|todo| todo.task_id == "a")
            .ok_or_else(|| "todo a missing".to_string())?;
        assert_eq!(todo.status, TodoStatus::Completed);
        assert_eq!(todo.summary.as_deref(), Some("recovered summary"));
        Ok(())
    }

    #[tokio::test]
    async fn run_dag_respects_dependency_order() {
        // b depends on a → a must be dispatched and completed before b.
        let mut a = solo_readonly_task("a");
        let mut b = solo_readonly_task("b");
        b.depends_on = vec!["a".into()];
        let _ = &mut a; // silence unused_mut
        let store = Arc::new(TaskRuntimeStore::new_in_memory().unwrap());
        let run_id = seed_run(&store, vec![a.clone(), b.clone()]);
        let worker = ScriptedDispatcher::new();
        worker.succeed("a", "done a");
        worker.succeed("b", "done b");

        let outcome = run_dag(
            store.clone(),
            worker.clone(),
            None,
            &run_id,
            vec![a, b],
            ConcurrencyLimits::default(),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap();

        assert!(matches!(outcome, RunOutcome::Completed));
        let order = worker.order();
        // a must appear before b in the dispatch order.
        let pos_a = order.iter().position(|x| x == "a").unwrap();
        let pos_b = order.iter().position(|x| x == "b").unwrap();
        assert!(pos_a < pos_b, "dependency violated: b dispatched before a");
    }

    #[tokio::test]
    async fn run_dag_failure_propagates_and_blocks_downstream() {
        // a fails; b depends on a and must be Blocked, run ends Failed
        // (because all non-terminal tasks are Failed/Blocked).
        let a = solo_readonly_task("a");
        let mut b = solo_readonly_task("b");
        b.depends_on = vec!["a".into()];
        let store = Arc::new(TaskRuntimeStore::new_in_memory().unwrap());
        let run_id = seed_run(&store, vec![a.clone(), b.clone()]);
        let worker = ScriptedDispatcher::new();
        worker.fail("a", "boom");

        let outcome = run_dag(
            store.clone(),
            worker.clone(),
            None,
            &run_id,
            vec![a, b],
            ConcurrencyLimits::default(),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap();

        match outcome {
            RunOutcome::Failed { failed_task_id, .. } => {
                assert_eq!(failed_task_id, "a");
            }
            other => panic!("expected Failed, got {:?}", other),
        }
        // b must be Blocked (downstream of failed a).
        let todos = store.list_todos(&run_id).unwrap();
        let b_todo = todos.iter().find(|t| t.task_id == "b").unwrap();
        assert_eq!(b_todo.status, TodoStatus::Blocked);
    }

    #[tokio::test]
    async fn run_dag_merge_failure_blocks_downstream() -> Result<(), String> {
        let mut writer = solo_readonly_task("writer");
        writer.kind = PlanTaskKind::Implementation;
        writer.agent_role = "implementer".to_string();
        writer.files = vec!["src/a.rs".to_string()];
        let mut downstream = solo_readonly_task("downstream");
        downstream.depends_on = vec![writer.id.clone()];
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let run_id = seed_run(&store, vec![writer.clone(), downstream.clone()]);
        let worker = ScriptedDispatcher::new();
        worker.succeed(&writer.id, "writer completed");
        worker.fail_integration(&writer.id, "synthetic merge conflict");

        let outcome = run_dag(
            store.clone(),
            worker,
            None,
            &run_id,
            vec![writer, downstream],
            ConcurrencyLimits::default(),
            CancellationToken::new(),
            None,
        )
        .await
        .map_err(|error| error.to_string())?;
        if !matches!(outcome, RunOutcome::Failed { .. }) {
            return Err(format!("expected failed run, got {outcome:?}"));
        }
        let todos = store
            .list_todos(&run_id)
            .map_err(|error| error.to_string())?;
        let writer_status = todos
            .iter()
            .find(|todo| todo.task_id == "writer")
            .map(|todo| todo.status)
            .ok_or_else(|| "writer todo missing".to_string())?;
        let downstream_status = todos
            .iter()
            .find(|todo| todo.task_id == "downstream")
            .map(|todo| todo.status)
            .ok_or_else(|| "downstream todo missing".to_string())?;
        assert_eq!(writer_status, TodoStatus::Failed);
        assert_eq!(downstream_status, TodoStatus::Blocked);
        Ok(())
    }

    #[tokio::test]
    async fn run_dag_cancellation_propagates_to_cancelled_outcome() {
        // Cancel BEFORE dispatching; run_dag should observe cancellation at the
        // top of its loop and return Cancelled without running any task.
        let store = Arc::new(TaskRuntimeStore::new_in_memory().unwrap());
        let run_id = seed_run(&store, vec![solo_readonly_task("a")]);
        let worker = ScriptedDispatcher::new();
        let cancel = CancellationToken::new();
        cancel.cancel();

        let outcome = run_dag(
            store.clone(),
            worker.clone(),
            None,
            &run_id,
            vec![solo_readonly_task("a")],
            ConcurrencyLimits::default(),
            cancel,
            None,
        )
        .await
        .unwrap();

        assert!(matches!(outcome, RunOutcome::Cancelled));
        // The worker must NOT have been dispatched into.
        assert!(worker.order().is_empty(), "task ran despite cancellation");
    }

    #[tokio::test]
    async fn run_dag_cancellation_preserves_explicit_pause() -> Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let task = solo_readonly_task("a");
        let run_id = seed_run(&store, vec![task.clone()]);
        store
            .transition_run(&run_id, TaskRunStatus::Paused)
            .map_err(|error| error.to_string())?;
        let cancel = CancellationToken::new();
        cancel.cancel();

        let outcome = run_dag(
            store,
            ScriptedDispatcher::new(),
            None,
            &run_id,
            vec![task],
            ConcurrencyLimits::default(),
            cancel,
            None,
        )
        .await
        .map_err(|error| error.to_string())?;

        assert!(matches!(outcome, RunOutcome::Paused { .. }));
        Ok(())
    }

    #[tokio::test]
    async fn run_dag_detects_cycle_as_stall() {
        // a depends on b, b depends on a → neither can ever become ready →
        // stall. (validate_plan would normally reject this, but run_dag must
        // still be robust to a malformed plan reaching it.)
        let mut a = solo_readonly_task("a");
        a.depends_on = vec!["b".into()];
        let mut b = solo_readonly_task("b");
        b.depends_on = vec!["a".into()];
        let store = Arc::new(TaskRuntimeStore::new_in_memory().unwrap());
        let run_id = seed_run(&store, vec![a.clone(), b.clone()]);
        let worker = ScriptedDispatcher::new();

        let outcome = run_dag(
            store.clone(),
            worker.clone(),
            None,
            &run_id,
            vec![a, b],
            ConcurrencyLimits::default(),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap();

        match outcome {
            RunOutcome::Failed { error, .. } => {
                assert!(
                    error.contains("stalled"),
                    "expected stall message, got: {error}"
                );
            }
            other => panic!("expected Failed (stall), got {:?}", other),
        }
        // Nothing should have been dispatched.
        assert!(worker.order().is_empty(), "subagent ran on a cyclic plan");
    }

    #[tokio::test]
    async fn run_dag_does_not_redispatch_in_flight_running_tasks() {
        // Regression: when the model emits several `plan_execute` calls as a
        // parallel tool batch, `RUN_EXECUTION_LOCKS` serializes them. The 2nd
        // call enters run_dag while an earlier task is still `Running`
        // (dispatched by the previous run_dag instance). Without the in_flight
        // guard, the ready filter would re-dispatch the Running task, causing
        // duplicate subagent work. Verify the Running task is left alone, the
        // genuinely-pending sibling is dispatched, and run_dag WAITS for the
        // in_flight task to reach Completed in the store (simulating the
        // sibling instance finishing it) before returning Completed.
        let mut in_flight = solo_readonly_task("in_flight");
        in_flight.status = TodoStatus::Running;
        let pending = solo_readonly_task("pending");
        let store = Arc::new(TaskRuntimeStore::new_in_memory().unwrap());
        let run_id = seed_run(&store, vec![in_flight.clone(), pending.clone()]);
        let worker = ScriptedDispatcher::new();
        worker.succeed("pending", "done");

        // Simulate the sibling run_dag instance finishing `in_flight` shortly
        // after `pending` is dispatched. Without this, run_dag's in_flight
        // wait loop would never observe Completed and the test would time out
        // (correctly — it means the task really is still in-flight).
        let store_bg = store.clone();
        let run_id_bg = run_id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let _ = store_bg.set_task_status(
                &run_id_bg,
                "in_flight",
                TodoStatus::Completed,
                Some("explorer"),
                Some("sibling done"),
            );
        });

        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            run_dag(
                store.clone(),
                worker.clone(),
                None,
                &run_id,
                vec![in_flight, pending],
                ConcurrencyLimits::default(),
                CancellationToken::new(),
                None,
            ),
        )
        .await
        .expect("run_dag did not complete within 10s (in_flight wait loop stuck)")
        .unwrap();

        // `in_flight` (Running) must NOT have been re-dispatched; only
        // `pending` should appear in the dispatch order.
        let order = worker.order();
        assert!(
            !order.contains(&"in_flight".to_string()),
            "Running task was re-dispatched (regression): {order:?}"
        );
        assert_eq!(order, vec!["pending".to_string()]);
        // run_dag waited for the sibling instance to finish `in_flight`, so
        // both tasks are now Completed and the run returns Completed.
        assert!(matches!(outcome, RunOutcome::Completed));
        let todos = store.list_todos(&run_id).unwrap();
        let in_flight_todo = todos.iter().find(|t| t.task_id == "in_flight").unwrap();
        assert_eq!(in_flight_todo.status, TodoStatus::Completed);
    }

    #[tokio::test]
    async fn main_agent_task_streams_tool_events_to_worker_trace() -> Result<(), String> {
        use crate::agent_handle::AgentHandle;
        use echo_agent::agent::react::builder::ReactAgentBuilder;
        use echo_agent::testing::{MockLlmClient, MockTool};
        use std::sync::Mutex;

        let llm = MockLlmClient::new()
            .then_tool_call("call_1", "mock_calc", r#"{"x":6,"y":7}"#)
            .with_response("The result is 42.");
        let agent = ReactAgentBuilder::new()
            .llm_client(Arc::new(llm))
            .system_prompt("You are a test assistant.")
            .tool(Box::new(MockTool::new("mock_calc").with_response("42")))
            .build()
            .map_err(|error| format!("test agent should build: {error}"))?;
        let handle = AgentHandle::new(agent);

        let task = PlanTask {
            id: "implementation-a".into(),
            title: "Run calculation".into(),
            description: "Use the tool and report the result".into(),
            kind: PlanTaskKind::Implementation,
            agent_role: "implementer".into(),
            ..Default::default()
        };
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = events.clone();
        let sink: ExecSink = Arc::new(move |event| {
            if let Ok(mut guard) = captured.lock() {
                guard.push(event);
            }
        });
        let store = Arc::new(
            TaskRuntimeStore::new_in_memory()
                .map_err(|error| format!("in-memory store should initialize: {error}"))?,
        );

        let output = run_main_agent_task(
            &handle,
            store,
            "run-trace",
            &task,
            "What is 6 times 7?",
            CancellationToken::new(),
            Some(sink),
        )
        .await
        .map_err(|error| format!("main agent task should complete: {error}"))?;

        assert!(output.1.contains("42"));
        let events = events
            .lock()
            .map_err(|error| format!("trace events lock poisoned: {error}"))?
            .clone();
        assert!(
            events.iter().any(|event| {
                event.event == "tool_started"
                    && event.task_id.as_deref() == Some("implementation-a")
                    && event.payload.get("name").and_then(|v| v.as_str()) == Some("mock_calc")
            }),
            "expected tool_started for mock_calc, got {events:?}"
        );
        assert!(
            events.iter().any(|event| {
                event.event == "tool_completed"
                    && event.task_id.as_deref() == Some("implementation-a")
                    && event.payload.get("success").and_then(|v| v.as_bool()) == Some(true)
                    && event
                        .payload
                        .get("result")
                        .and_then(|v| v.as_str())
                        .is_some_and(|text| text.contains("42"))
            }),
            "expected successful tool_completed with tool output, got {events:?}"
        );
        Ok(())
    }
}
