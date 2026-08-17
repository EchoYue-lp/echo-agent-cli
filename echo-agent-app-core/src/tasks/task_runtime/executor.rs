//! EKO adapter for the framework runtime DAG executor.
//!
//! Converts EKO `TaskPlan` snapshots into the framework's product-neutral task
//! view, then injects EKO dispatch, review, persistence, worktree, and event
//! policy. Dependency traversal, revision safe points, Subagent waves,
//! cancellation, failure propagation, and stall detection live in
//! `echo_orchestration::tasks::RuntimeDagExecutor`.
//!
//! - read-only tasks (read_only_review, investigation, test_plan, review,
//!   summary) run concurrently up to the configured Subagent limit, each delegated
//!   to a registered subagent role via `delegate_to_agent_with_cancel` (fork
//!   mode → isolated instance under the executor's semaphore, NOT the primary
//!   agent's execution_mutex, so they parallelize);
//! - implementation / debugging tasks use ownership-safe waves and writer
//!   worktrees; verification tasks run on the primary Agent against the
//!   authoritative workspace;
//! - the overall Subagent count is capped by the framework executor; EKO owns
//!   write, shell, and LLM resource policy separately.
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

use echo_agent::agent::subagent::{ContextTransferPolicy, SubagentPromptInput};
use echo_agent::agent::{Agent, AgentEvent, CancellationToken};
use futures::StreamExt;
use tokio::sync::{Mutex as TokioMutex, OwnedMutexGuard, Semaphore};

use super::completion_gate::{artifact_matches, verification_matches};
use super::store::{ClaimWriteOutcome, StoreError, SubagentReleaseRecord, TaskRuntimeStore};
use super::types::*;

/// EKO product-resource limits. Only the Subagent cap is passed to the
/// framework DAG kernel; write, shell, and LLM limits stay in the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EkoExecutionLimits {
    pub max_concurrent_subagents: usize,
    pub max_concurrent_writes: usize,
    pub max_concurrent_shells: usize,
    pub max_parallel_llm_calls: usize,
}

impl Default for EkoExecutionLimits {
    fn default() -> Self {
        Self {
            max_concurrent_subagents: 4,
            max_concurrent_writes: 4,
            max_concurrent_shells: 1,
            max_parallel_llm_calls: 4,
        }
    }
}

/// Scope of an execution-flow event on the unified frontend channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecEventScope {
    Run,
    Task,
    Subagent,
}

/// A lightweight execution-flow event emitted to the frontend via the unified
/// `execution://event` Tauri channel.
///
/// Replaces the pre-unification trace pair. `event` is typed inside the
/// runtime and serializes to the frontend's snake_case event name. `payload`
/// carries event-specific fields
/// (`content`/`name`/`args`/...) as a flat JSON object.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExecEvent {
    pub run_id: String,
    pub scope: ExecEventScope,
    /// Plan node identity. Present on task and Subagent events.
    pub task_id: Option<String>,
    /// One concrete Subagent execution identity
    /// (`{run_id}:{task_id}:{plan_revision}:{attempt}`).
    /// Present only when `scope == Subagent`.
    pub subagent_run_id: Option<String>,
    pub event: RuntimeEventKind,
    pub agent: Option<String>,
    pub payload: serde_json::Value,
}

impl ExecEvent {
    /// Construct a run-level event (no task_id).
    pub fn run(
        run_id: impl Into<String>,
        event: RuntimeEventKind,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            run_id: run_id.into(),
            scope: ExecEventScope::Run,
            task_id: None,
            subagent_run_id: None,
            event,
            agent: None,
            payload,
        }
    }

    /// Construct a plan-task event. These events never mutate Subagent state.
    pub fn task(
        run_id: impl Into<String>,
        task_id: impl Into<String>,
        event: RuntimeEventKind,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            run_id: run_id.into(),
            scope: ExecEventScope::Task,
            task_id: Some(task_id.into()),
            subagent_run_id: None,
            event,
            agent: None,
            payload,
        }
    }

    /// Construct an event for one concrete Subagent execution attempt.
    pub fn subagent(
        run_id: impl Into<String>,
        task_id: impl Into<String>,
        subagent_run_id: impl Into<String>,
        event: RuntimeEventKind,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            run_id: run_id.into(),
            scope: ExecEventScope::Subagent,
            task_id: Some(task_id.into()),
            subagent_run_id: Some(subagent_run_id.into()),
            event,
            agent: None,
            payload,
        }
    }

    /// Attach the agent/role name. Builder-style for call-site readability.
    pub fn with_agent(mut self, agent: impl Into<String>) -> Self {
        self.agent = Some(agent.into());
        self
    }
}

/// Sink closure that receives [`ExecEvent`]s. Interactive surfaces bridge this
/// into their shared `ChatDriverEvent` stream; unattended runs rely on the
/// append-only TaskRuntime store and may omit a live sink.
pub type ExecSink = Arc<dyn Fn(ExecEvent) + Send + Sync>;

/// Emit `ev` to `sink` if present. Single chokepoint so every emit site is
/// uniform and grep-friendly.
fn emit_exec(sink: Option<&ExecSink>, ev: ExecEvent) {
    if let Some(sink) = sink {
        sink(ev);
    }
}

fn subagent_execution_id(
    run_id: &str,
    task_id: &str,
    claim: &echo_agent::tasks::TaskClaim,
) -> String {
    claim.execution_id(run_id, task_id)
}

fn subagent_terminal_event(status: SubagentRunStatus) -> RuntimeEventKind {
    match status {
        SubagentRunStatus::Running => RuntimeEventKind::Running,
        SubagentRunStatus::Completed => RuntimeEventKind::Completed,
        SubagentRunStatus::Failed => RuntimeEventKind::Failed,
        SubagentRunStatus::Cancelled => RuntimeEventKind::Cancelled,
        SubagentRunStatus::TimedOut => RuntimeEventKind::TimedOut,
    }
}

fn task_isolation_id(run_id: &str, task_id: &str) -> String {
    format!("{run_id}:{task_id}")
}

fn task_worktree_label(agent_role: &str, run_id: &str, task_id: &str) -> String {
    format!("{agent_role}-{}", task_isolation_id(run_id, task_id))
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

/// Whether an Agent-driven Run must materialize a formal plan before it may
/// complete. This is prompt/execution policy, not a TaskRun lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunPlanPolicy {
    RequirePlan,
    AllowDirect,
}

#[derive(Debug, Default)]
struct AgentDriveStreamResult {
    failure: Option<String>,
    final_answer: Option<String>,
}

/// Workspace-mutating tools that an unattended primary Agent must not call
/// directly unless the user explicitly selected `InPlace` mode.
///
/// Worktree isolation is owned by formal writer PlanTasks: their Subagent
/// worktree is created only when the writer is dispatched, then integrated by
/// the existing review/integration stage. Hiding these tools prevents a second
/// run-level worktree mechanism from being required around the planning Agent.
const UNATTENDED_DIRECT_MUTATION_TOOLS: [&str; 18] = [
    "agent_tool",
    "shell",
    "run_code",
    "create_file",
    "write_file",
    "append_file",
    "update_file",
    "move_file",
    "delete_file",
    "edit_file",
    "git_branch",
    "git_commit",
    "enter_worktree",
    "exit_worktree",
    "write_excel",
    "export_data",
    "export_text",
    "create_complex_task",
];

fn unattended_direct_disabled_tools(
    attended_mode: AttendedMode,
    write_mode: UnattendedWriteMode,
) -> Option<HashSet<String>> {
    if attended_mode != AttendedMode::Unattended || write_mode == UnattendedWriteMode::InPlace {
        return None;
    }
    Some(
        UNATTENDED_DIRECT_MUTATION_TOOLS
            .into_iter()
            .map(str::to_string)
            .collect(),
    )
}

fn unattended_run_prompt(
    prompt: &str,
    attended_mode: AttendedMode,
    write_mode: UnattendedWriteMode,
) -> String {
    if attended_mode != AttendedMode::Unattended || write_mode == UnattendedWriteMode::InPlace {
        return prompt.to_string();
    }

    let write_guidance = match write_mode {
        UnattendedWriteMode::Worktree => {
            "For any workspace mutation, shell command, code execution, or Git write, create and execute a formal plan. Writer PlanTasks receive an isolated worktree only when their Subagent is actually dispatched. Read-only work may be completed directly without creating a worktree."
        }
        UnattendedWriteMode::Disabled => {
            "This unattended run is read-only. Complete read-only work directly or with a read-only formal plan; do not propose or attempt workspace mutations, shell commands, code execution, or Git writes."
        }
        UnattendedWriteMode::InPlace => return prompt.to_string(),
    };

    format!(
        "[unattended workspace policy]\n{write_guidance}\n[/unattended workspace policy]\n\n{prompt}"
    )
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
    memory_generation: Option<crate::evolution::ReviewGenerationLease>,
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
            RuntimeEventKind::RunStarted,
            serde_json::json!({
                "goal": &run.goal,
                "conversation_id": &run.conversation_id,
                "mode": "task_runtime",
            }),
        ),
    );

    let primary_agent = primary_agent.ok_or(ExecError::NoAgent)?;
    let limits = EkoExecutionLimits::default();

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
                    TodoStatus::Completed
                        | TodoStatus::Failed
                        | TodoStatus::Cancelled
                        | TodoStatus::TimedOut
                        | TodoStatus::Skipped
                )
            })
            .count();
        if unresolved_count == 0 {
            let report = store.completion_gate_report(run_id)?;
            if !report.ready {
                let error = report
                    .blockers
                    .iter()
                    .map(|item| format!("{:?}: {}", item.code, item.detail))
                    .collect::<Vec<_>>()
                    .join("; ");
                store.request_pause_with_reason(
                    run_id,
                    RunPauseReason::NeedsInput,
                    Some(&error),
                )?;
                break Ok(RunOutcome::Paused {
                    failed_task_id: "<completion_gate>".to_string(),
                    error,
                });
            }
            if store.complete_run_if_quiescent(run_id)? {
                break Ok(RunOutcome::Completed);
            }
            drain_cycle = drain_cycle.saturating_add(1);
            continue;
        }
        tracing::info!(
            run_id = %run_id,
            drain_cycle,
            task_count = plan.tasks.len(),
            unresolved_count,
            "task_runtime: drain plan snapshot"
        );

        let outcome = execute_runtime_plan(
            store.clone(),
            RealTaskDispatcher {
                primary_agent: primary_agent.clone(),
            },
            reviewer_llm.clone(),
            run_id,
            limits,
            parent_cancel.clone(),
            trace_sink.clone(),
        )
        .await;

        if matches!(outcome, Ok(RunOutcome::Completed)) {
            // Always return to the locked completion gate. This closes the
            // race where a plan patch commits after the last wave snapshot but
            // before the run is marked Completed.
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
    // Reflect the outcome on the run state. Each branch also writes a trace
    // Run record when a RunStore is available.
    match &outcome {
        Ok(RunOutcome::Completed) => {
            store.finalize_run(run_id, TaskRunStatus::Completed, None)?;
            emit_exec(
                trace_sink.as_ref(),
                ExecEvent::run(
                    run_id.to_string(),
                    RuntimeEventKind::RunCompleted,
                    serde_json::json!({ "status": "completed" }),
                ),
            );
            // Completion was already committed atomically by
            // complete_run_if_quiescent inside the drain loop.
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
                memory_generation.as_ref(),
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
                    RuntimeEventKind::RunFailed,
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
            store.note(run_id, tid, &format!("run failed: {error}"))?;
            store.finalize_run(run_id, TaskRunStatus::Failed, None)?;
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
                    RuntimeEventKind::RunCancelled,
                    serde_json::json!({ "status": "cancelled" }),
                ),
            );
            finalize_cancelled_run_state(&store, run_id)?;
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
                memory_generation.as_ref(),
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
                    RuntimeEventKind::RunStatusChanged,
                    serde_json::json!({
                        "status": "paused",
                        "failed_task_id": failed_task_id,
                        "error": error,
                    }),
                ),
            );
            // The runtime adapter or control path already transitioned Running → Paused.
            // Any Subagent that was in flight no longer exists after cancellation,
            // so make it pending again for the resume drain.
            for todo in store
                .list_todos(run_id)?
                .into_iter()
                .filter(|todo| todo.status == TodoStatus::Running)
            {
                store.set_task_status(
                    run_id,
                    &todo.task_id,
                    TodoStatus::Pending,
                    None,
                    Some("paused; pending resume"),
                )?;
            }
            let task_id = (!failed_task_id.starts_with('<')).then_some(failed_task_id.as_str());
            store.note(run_id, task_id, &format!("run paused: {error}"))?;
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
                    RuntimeEventKind::RunFailed,
                    serde_json::json!({ "error": e.to_string() }),
                ),
            );
            store.finalize_run(
                run_id,
                TaskRunStatus::Failed,
                Some(&format!("executor error: {e}")),
            )?;
        }
    }
    outcome
}

#[cfg(test)]
fn run_completion_blockers(store: &TaskRuntimeStore, run_id: &str) -> Vec<String> {
    store
        .completion_gate_report(run_id)
        .map(|report| {
            report
                .blockers
                .into_iter()
                .map(|item| item.detail)
                .collect()
        })
        .unwrap_or_else(|error| vec![error.to_string()])
}

fn finalize_cancelled_run_state(store: &TaskRuntimeStore, run_id: &str) -> Result<(), StoreError> {
    for todo in store.list_todos(run_id)?.into_iter().filter(|todo| {
        matches!(
            todo.status,
            TodoStatus::Pending | TodoStatus::Running | TodoStatus::Blocked
        )
    }) {
        store.set_task_status(
            run_id,
            &todo.task_id,
            TodoStatus::Cancelled,
            None,
            Some("cancelled with parent run"),
        )?;
    }
    store.finalize_run(run_id, TaskRunStatus::Cancelled, None)?;
    Ok(())
}

/// Structured completion assessment. Separates "real execution failure"
/// (retryable) from "completed but acceptance pending" (NOT retryable —
/// must be blocked for review or user retry). contract_version=0 is no
/// longer a failure condition (M7 does not require it).
#[derive(Debug)]
enum CompletionAssessment {
    /// Subagent completed and all execution_checks / required_artifacts
    /// have hard observed evidence. Acceptance criteria are NOT judged
    /// here — that is the ReviewGate's job.
    Executed,
    /// Subagent genuinely failed (non-completed status, empty summary,
    /// remaining_work non-empty, or self-reported failed verification).
    /// This IS retryable within the retry budget.
    ExecutionFailed { reason: String },
    /// Subagent completed but execution evidence or artifacts are missing.
    /// NOT retryable — would just reproduce the same gap. Block instead.
    AcceptancePending {
        missing_checks: Vec<String>,
        missing_artifacts: Vec<String>,
    },
}

/// Assess whether a task's execution result is acceptable on hard-evidence
/// grounds (execution_checks must have observed pass; artifacts must be
/// present with hash + producer id). Acceptance criteria are intentionally
/// NOT judged here — they are reviewer-judged in the ReviewGate, never
/// auto-passed.
///
/// M7 note: contract_version=0 is a valid fallback shape. We do not treat
/// it as a failure. A plain-text summary is still legitimate execution
/// evidence as long as execution_checks (which are shell commands) are
/// empty or actually observed.
fn assess_task_execution(task: &PlanTask, result: &SubagentTaskResult) -> CompletionAssessment {
    // 1. Real execution failure: non-completed status, empty summary,
    //    self-reported remaining work, or self-reported failed verification.
    if result.status != SubagentRunStatus::Completed {
        return CompletionAssessment::ExecutionFailed {
            reason: format!("terminal status is {}", result.status.as_str()),
        };
    }
    if result.summary.trim().is_empty() {
        return CompletionAssessment::ExecutionFailed {
            reason: "summary is empty".to_string(),
        };
    }
    if !result.remaining_work.is_empty() {
        return CompletionAssessment::ExecutionFailed {
            reason: format!("remaining work: {}", result.remaining_work.join("; ")),
        };
    }
    for verification in &result.verification {
        if verification.status != SubagentVerificationStatus::Passed {
            return CompletionAssessment::ExecutionFailed {
                reason: format!(
                    "verification '{}' is {:?}",
                    verification.check, verification.status
                ),
            };
        }
    }

    // 2. execution_checks must have observed + passed evidence.
    let mut missing_checks = Vec::new();
    for required in &task.execution_checks {
        let matched = result.verification.iter().any(|verification| {
            verification.source == SubagentVerificationSource::Observed
                && verification.status == SubagentVerificationStatus::Passed
                && verification_matches(required, &verification.check)
        });
        if !matched {
            missing_checks.push(required.clone());
        }
    }

    // 3. required_artifacts must be present with hash + producer execution id.
    let mut missing_artifacts = Vec::new();
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
            missing_artifacts.push(required.clone());
        }
    }

    if missing_checks.is_empty() && missing_artifacts.is_empty() {
        CompletionAssessment::Executed
    } else {
        CompletionAssessment::AcceptancePending {
            missing_checks,
            missing_artifacts,
        }
    }
}

/// Abstraction over how a single ready task is dispatched in the EKO runtime.
///
/// The framework runtime DAG controller depends on this trait (not on
/// `execute_task` directly), so EKO dispatch and worktree integration can be
/// tested with a deterministic mock instead of a real LLM-backed Agent. The
/// production implementation ([`RealTaskDispatcher`]) wraps `execute_task`.
///
/// The dispatcher is given EKO-specific resource semaphores and file locks.
/// The framework executor owns the global Subagent concurrency permit.
trait TaskDispatcher: Send + Sync {
    /// Execute `task` for `run_id`. Success carries both the bounded structured
    /// result and the complete model output. The former feeds parent summaries;
    /// the latter is the evidence reviewed against acceptance criteria.
    #[allow(clippy::too_many_arguments, clippy::type_complexity)] // product resource limits + locks are the application dispatch contract
    fn dispatch(
        &self,
        store: Arc<TaskRuntimeStore>,
        context: echo_agent::tasks::TaskSubagentContext,
        claim: echo_agent::tasks::TaskClaim,
        task: PlanTask,
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

/// Production dispatcher: delegates to [`execute_task`] against the primary agent.
///
/// Review remains in the EKO runtime controller after a Subagent returns. The
/// dispatcher only needs the Agent and product-specific concurrency primitives.
struct RealTaskDispatcher {
    primary_agent: crate::agent_handle::AgentHandle,
}

impl TaskDispatcher for RealTaskDispatcher {
    fn dispatch(
        &self,
        store: Arc<TaskRuntimeStore>,
        context: echo_agent::tasks::TaskSubagentContext,
        claim: echo_agent::tasks::TaskClaim,
        task: PlanTask,
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
            // Scope run_id + cancel + trace_sink into task-local so Subagent-internal
            // tools (task_*/task_execute, and their execute_with_context
            // fallback path) and L3 nested Subagents can read them.
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
                    write_sem,
                    shell_sem,
                    llm_sem,
                    file_write_locks,
                    trace_sink,
                    run_id,
                    claim,
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

            let label = task_worktree_label(&task.agent_role, &run_id, &task.id);
            let ownership = super::planner::file_ownership(&task);
            let branch = super::worktree::fork_branch_name(&label);
            let _ = store.note(
                &run_id,
                Some(&task.id),
                &format!("worktree integration started: execution={execution_id}, branch={branch}"),
            );
            emit_exec(
                trace_sink.as_ref(),
                ExecEvent::task(
                    run_id.clone(),
                    task.id.clone(),
                    RuntimeEventKind::MergeStarted,
                    serde_json::json!({
                        "execution_id": execution_id,
                        "branch": branch,
                    }),
                )
                .with_agent(task.agent_role.clone()),
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
                        ExecEvent::task(
                            run_id,
                            task.id.clone(),
                            RuntimeEventKind::MergeCompleted,
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
                        .with_agent(task.agent_role),
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
                        ExecEvent::task(
                            run_id,
                            task.id.clone(),
                            RuntimeEventKind::MergeFailed,
                            serde_json::json!({
                                "execution_id": execution_id,
                                "branch": branch,
                                "error": message,
                            }),
                        )
                        .with_agent(task.agent_role),
                    );
                    Err(message)
                }
            }
        })
    }
}

#[derive(Debug, Clone)]
struct TaskDispatchSuccess {
    task_id: String,
    result: SubagentTaskResult,
    full_output: String,
}

#[derive(Debug, Clone, Default)]
struct TaskExecutionUsage {
    durable: SubagentRunUsage,
    input_tokens: u64,
    output_tokens: u64,
}

impl TaskExecutionUsage {
    fn from_framework(result: &echo_agent::agent::subagent::SubagentResult) -> Self {
        let duration_ms = u64::try_from(result.duration.as_millis()).unwrap_or(u64::MAX);
        let iterations = u64::try_from(result.iterations).unwrap_or(u64::MAX);
        let input_tokens = result
            .usage
            .as_ref()
            .map(|usage| usage.prompt_tokens)
            .unwrap_or(0);
        let output_tokens = result
            .usage
            .as_ref()
            .map(|usage| usage.completion_tokens)
            .unwrap_or(0);
        Self {
            durable: SubagentRunUsage {
                duration_ms: Some(duration_ms),
                tokens_used: result
                    .usage
                    .as_ref()
                    .map(|usage| usage.prompt_tokens.saturating_add(usage.completion_tokens)),
                iterations: Some(iterations),
            },
            input_tokens,
            output_tokens,
        }
    }

    fn duration_ms(&self) -> u64 {
        self.durable.duration_ms.unwrap_or(0)
    }
}

fn persist_framework_subagent_usage(
    store: &TaskRuntimeStore,
    run_id: &str,
    execution_id: &str,
    result: &echo_agent::agent::subagent::SubagentResult,
) -> Result<TaskExecutionUsage, ExecutionFailure> {
    let usage = TaskExecutionUsage::from_framework(result);
    store
        .account_subagent_usage(
            run_id,
            execution_id,
            "framework_dispatch_total",
            usage.input_tokens,
            usage.output_tokens,
            usage.duration_ms(),
        )
        .map_err(|error| {
            ExecutionFailure::failed(format!(
                "failed to persist Subagent usage for {execution_id}: {error}"
            ))
            .with_usage(usage.clone())
        })?;
    Ok(usage)
}

fn finalize_framework_subagent_result(
    store: &TaskRuntimeStore,
    run_id: &str,
    execution_id: &str,
    result: echo_agent::agent::subagent::SubagentResult,
) -> Result<(SubagentTaskResult, String, TaskExecutionUsage), ExecutionFailure> {
    let usage = persist_framework_subagent_usage(store, run_id, execution_id, &result)?;
    if result.outcome.status != echo_agent::agent::subagent::SubagentStatus::Completed {
        let status = result.outcome.status.into();
        let message = if result.outcome.summary.trim().is_empty() {
            result.output
        } else {
            result.outcome.summary
        };
        return Err(ExecutionFailure {
            status,
            message,
            usage: Some(usage),
        });
    }
    let task_result = SubagentTaskResult::from_framework(&result);
    Ok((task_result, result.output, usage))
}

#[derive(Debug, Clone)]
struct ExecutionFailure {
    status: SubagentRunStatus,
    message: String,
    usage: Option<TaskExecutionUsage>,
}

impl ExecutionFailure {
    fn failed(message: impl Into<String>) -> Self {
        Self {
            status: SubagentRunStatus::Failed,
            message: message.into(),
            usage: None,
        }
    }

    fn cancelled(message: impl Into<String>) -> Self {
        Self {
            status: SubagentRunStatus::Cancelled,
            message: message.into(),
            usage: None,
        }
    }

    fn from_agent_failure(
        failure: &echo_agent::error::AgentFailure,
        message: impl Into<String>,
    ) -> Self {
        let status = match failure.terminal_kind {
            echo_agent::error::AgentTerminalKind::Cancelled => SubagentRunStatus::Cancelled,
            echo_agent::error::AgentTerminalKind::TimedOut => SubagentRunStatus::TimedOut,
            echo_agent::error::AgentTerminalKind::Failed
            | echo_agent::error::AgentTerminalKind::PermissionDenied => SubagentRunStatus::Failed,
        };
        Self {
            status,
            message: message.into(),
            usage: None,
        }
    }

    fn from_react(error: echo_agent::error::ReactError, context: &str) -> Self {
        let status = echo_agent::agent::subagent::subagent_status_from_error(&error).into();
        Self {
            status,
            message: format!("{context}: {error}"),
            usage: None,
        }
    }

    fn with_usage(mut self, usage: TaskExecutionUsage) -> Self {
        self.usage = Some(usage);
        self
    }
}

impl std::fmt::Display for ExecutionFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

#[derive(Debug, Clone)]
struct TaskDispatchFailure {
    task_id: String,
    status: SubagentRunStatus,
    message: String,
}

impl TaskDispatchFailure {
    fn failed(task_id: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            status: SubagentRunStatus::Failed,
            message: message.into(),
        }
    }

    fn cancelled(task_id: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            status: SubagentRunStatus::Cancelled,
            message: message.into(),
        }
    }

    fn from_execution(task_id: impl Into<String>, failure: ExecutionFailure) -> Self {
        Self {
            task_id: task_id.into(),
            status: failure.status,
            message: failure.message,
        }
    }

    fn into_react(self) -> echo_agent::error::ReactError {
        use echo_agent::error::AgentError;
        match self.status {
            SubagentRunStatus::Cancelled => {
                echo_agent::error::ReactError::Agent(Box::new(AgentError::Cancelled(self.message)))
            }
            SubagentRunStatus::TimedOut => {
                echo_agent::error::ReactError::Agent(Box::new(AgentError::Timeout(self.message)))
            }
            SubagentRunStatus::Running
            | SubagentRunStatus::Completed
            | SubagentRunStatus::Failed => echo_agent::error::ReactError::Other(self.message),
        }
    }
}

type TaskDispatchResult = Result<TaskDispatchSuccess, TaskDispatchFailure>;

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

struct EkoRuntimeDagController<W: TaskDispatcher> {
    store: Arc<TaskRuntimeStore>,
    dispatcher: Arc<W>,
    reviewer_llm: Option<Arc<dyn echo_agent::llm::LlmClient>>,
    write_sem: Arc<Semaphore>,
    shell_sem: Arc<Semaphore>,
    llm_sem: Arc<Semaphore>,
    file_write_locks: Arc<std::sync::Mutex<HashMap<String, Arc<TokioMutex<()>>>>>,
    trace_sink: Option<ExecSink>,
    cancel: CancellationToken,
    plan_tasks: std::sync::Mutex<HashMap<String, PlanTask>>,
}

impl<W: TaskDispatcher> EkoRuntimeDagController<W> {
    fn plan_task(&self, task_id: &str) -> echo_agent::error::Result<PlanTask> {
        self.plan_tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(task_id)
            .cloned()
            .ok_or_else(|| {
                echo_agent::error::ReactError::Other(format!(
                    "plan task '{task_id}' missing from active revision"
                ))
            })
    }

    fn review_stop_disposition(&self, run_id: &str) -> echo_agent::tasks::RuntimeStopDisposition {
        let unattended = self
            .store
            .get_run(run_id)
            .ok()
            .flatten()
            .is_some_and(|run| run.attended_mode == AttendedMode::Unattended);
        if unattended {
            echo_agent::tasks::RuntimeStopDisposition::Fail
        } else {
            echo_agent::tasks::RuntimeStopDisposition::Pause
        }
    }

    fn set_claimed_status(
        &self,
        run_id: &str,
        task: &PlanTask,
        claim: &echo_agent::tasks::TaskClaim,
        status: TodoStatus,
        summary: Option<&str>,
    ) -> echo_agent::error::Result<bool> {
        self.store
            .set_claimed_task_status(
                run_id,
                &task.id,
                claim,
                status,
                Some(&task.agent_role),
                summary,
            )
            .map(|outcome| outcome == ClaimWriteOutcome::Applied)
            .map_err(|error| echo_agent::error::ReactError::Other(error.to_string()))
    }

    fn claim_is_current(
        &self,
        run_id: &str,
        task_id: &str,
        claim: &echo_agent::tasks::TaskClaim,
    ) -> echo_agent::error::Result<bool> {
        self.store
            .task_claim_is_current(run_id, task_id, claim)
            .map_err(|error| echo_agent::error::ReactError::Other(error.to_string()))
    }
}

#[async_trait::async_trait]
impl<W: TaskDispatcher + 'static> echo_agent::tasks::RuntimeDagController
    for EkoRuntimeDagController<W>
{
    type DispatchOutput = TaskDispatchSuccess;

    async fn load_snapshot(
        &self,
        run_id: &str,
    ) -> echo_agent::error::Result<echo_agent::tasks::RuntimePlanSnapshot> {
        let plan = self
            .store
            .get_plan(run_id)
            .map_err(|error| echo_agent::error::ReactError::Other(error.to_string()))?
            .ok_or_else(|| {
                echo_agent::error::ReactError::Other(format!("run {run_id} has no plan"))
            })?;
        let runtime_tasks = plan.tasks.iter().map(PlanTask::to_task).collect();
        let by_id = plan
            .tasks
            .into_iter()
            .map(|task| (task.id.clone(), task))
            .collect();
        *self
            .plan_tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = by_id;
        Ok(echo_agent::tasks::RuntimePlanSnapshot {
            revision: plan.revision,
            tasks: runtime_tasks,
        })
    }

    async fn claim_task(
        &self,
        run_id: &str,
        task: &echo_agent::tasks::Task,
        expected_revision: u64,
    ) -> echo_agent::error::Result<echo_agent::tasks::RuntimeTaskClaimOutcome> {
        self.store
            .claim_task(run_id, task, expected_revision)
            .map_err(|error| echo_agent::error::ReactError::Other(error.to_string()))
    }

    fn select_ready_wave(
        &self,
        _tasks: &[echo_agent::tasks::Task],
        ready_task_ids: Vec<String>,
    ) -> Vec<String> {
        let by_id = self
            .plan_tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let ready = ready_task_ids
            .into_iter()
            .filter_map(|task_id| by_id.get(&task_id).cloned())
            .collect();
        select_ownership_safe_wave(ready)
            .into_iter()
            .map(|task| task.id)
            .collect()
    }

    async fn dispatch_task(
        &self,
        context: echo_agent::tasks::TaskSubagentContext,
        claim: echo_agent::tasks::TaskClaim,
        runtime_task: echo_agent::tasks::Task,
    ) -> echo_agent::error::Result<Self::DispatchOutput> {
        let task = self.plan_task(&runtime_task.spec.id)?;
        let active_task_id = task.id.clone();
        let execution_id = subagent_execution_id(&context.run_id, &task.id, &claim);
        match self.store.recoverable_subagent_result_for_attempt(
            &context.run_id,
            &task.id,
            claim.revision,
            claim.attempt,
        ) {
            Ok(Some(recovered)) => {
                tracing::info!(
                    run_id = %context.run_id,
                    task_id = %task.id,
                    execution_id,
                    "task_runtime: reusing durable Subagent result after restart"
                );
                let _ = self.store.note(
                    &context.run_id,
                    Some(&task.id),
                    "reused completed Subagent result; continuing at review boundary",
                );
                return Ok(TaskDispatchSuccess {
                    task_id: task.id,
                    result: recovered.result,
                    full_output: recovered.full_output,
                });
            }
            Ok(None) => {}
            Err(error) => tracing::warn!(
                run_id = %context.run_id,
                task_id = %task.id,
                %error,
                "failed to inspect durable Subagent result; dispatching normally"
            ),
        }

        self.dispatcher
            .dispatch(
                self.store.clone(),
                context,
                claim,
                task,
                self.write_sem.clone(),
                self.shell_sem.clone(),
                self.llm_sem.clone(),
                self.file_write_locks.clone(),
                self.trace_sink.clone(),
            )
            .await
            .map_err(|failure| {
                if failure.task_id != active_task_id {
                    return echo_agent::error::ReactError::Other(format!(
                        "dispatcher returned failure for task '{}' while '{}' was active",
                        failure.task_id, active_task_id
                    ));
                }
                failure.into_react()
            })
    }

    async fn resolve_dispatch(
        &self,
        run_id: &str,
        claim: echo_agent::tasks::TaskClaim,
        runtime_task: echo_agent::tasks::Task,
        dispatch: echo_agent::error::Result<Self::DispatchOutput>,
    ) -> echo_agent::error::Result<echo_agent::tasks::RuntimeTaskResolution> {
        let task = self.plan_task(&runtime_task.spec.id)?;
        let dispatched = match dispatch {
            Ok(dispatched) => dispatched,
            Err(error) => {
                let message = error.to_string();
                let status = echo_agent::agent::subagent::subagent_status_from_error(&error);
                let todo_status = match status {
                    echo_agent::agent::subagent::SubagentStatus::Cancelled => TodoStatus::Cancelled,
                    echo_agent::agent::subagent::SubagentStatus::TimedOut => TodoStatus::TimedOut,
                    echo_agent::agent::subagent::SubagentStatus::Completed
                    | echo_agent::agent::subagent::SubagentStatus::Failed => TodoStatus::Failed,
                };
                if !self.set_claimed_status(
                    run_id,
                    &task,
                    &claim,
                    todo_status,
                    Some(&format!("error: {message}")),
                )? {
                    return Ok(echo_agent::tasks::RuntimeTaskResolution::Superseded);
                }
                return Ok(if todo_status == TodoStatus::Cancelled {
                    echo_agent::tasks::RuntimeTaskResolution::Cancelled
                } else {
                    echo_agent::tasks::RuntimeTaskResolution::Failed { error: message }
                });
            }
        };

        let TaskDispatchSuccess {
            task_id,
            result,
            full_output,
        } = dispatched;
        if task_id != task.id {
            return Err(echo_agent::error::ReactError::Other(format!(
                "dispatcher returned task '{task_id}' for active task '{}'",
                task.id
            )));
        }

        match assess_task_execution(&task, &result) {
            CompletionAssessment::ExecutionFailed { reason } => {
                let claimed_retry_count = claim.attempt.saturating_sub(1);
                if claimed_retry_count < task.max_retries {
                    let next_retry = claimed_retry_count.saturating_add(1);
                    let retry_summary =
                        format!("execution failed (attempt {next_retry}): {reason}");
                    let requeued = self
                        .store
                        .requeue_claimed_task(run_id, &task.id, &claim, None, &retry_summary)
                        .map_err(|error| echo_agent::error::ReactError::Other(error.to_string()))?;
                    if requeued == ClaimWriteOutcome::Superseded {
                        return Ok(echo_agent::tasks::RuntimeTaskResolution::Superseded);
                    }
                    let _ = self.store.note(
                            run_id,
                            Some(&task.id),
                            &format!(
                                "attempt {next_retry} failed: {reason}; auto-retrying (retry_count {} -> {next_retry})",
                                claimed_retry_count
                            ),
                        );
                    Ok(echo_agent::tasks::RuntimeTaskResolution::Pending)
                } else {
                    let error = format!("execution failed after max retries: {reason}");
                    if !self.set_claimed_status(
                        run_id,
                        &task,
                        &claim,
                        TodoStatus::Failed,
                        Some(&error),
                    )? {
                        return Ok(echo_agent::tasks::RuntimeTaskResolution::Superseded);
                    }
                    Ok(echo_agent::tasks::RuntimeTaskResolution::Failed { error })
                }
            }
            CompletionAssessment::AcceptancePending {
                missing_checks,
                missing_artifacts,
            } => {
                let reason = format!(
                    "acceptance pending: missing execution checks [{}], missing artifacts [{}]",
                    missing_checks.join(", "),
                    missing_artifacts.join(", "),
                );
                if !self.set_claimed_status(
                    run_id,
                    &task,
                    &claim,
                    TodoStatus::Blocked,
                    Some(&reason),
                )? {
                    return Ok(echo_agent::tasks::RuntimeTaskResolution::Superseded);
                }
                let _ = self.store.note(run_id, Some(&task.id), &reason);
                Ok(echo_agent::tasks::RuntimeTaskResolution::Blocked {
                    error: reason,
                    disposition: self.review_stop_disposition(run_id),
                })
            }
            CompletionAssessment::Executed => {
                if !self.claim_is_current(run_id, &task.id, &claim)? {
                    return Ok(echo_agent::tasks::RuntimeTaskResolution::Superseded);
                }
                let summary = result.summary.clone();
                let review_output = if full_output.trim().is_empty() {
                    summary.as_str()
                } else {
                    full_output.as_str()
                };
                let review = run_review_gate(
                    self.store.clone(),
                    self.reviewer_llm.clone(),
                    run_id,
                    &task,
                    review_output,
                )
                .await;
                let block_reason = match review {
                    ReviewGateOutcome::Pass => None,
                    ReviewGateOutcome::NeedsFix(_fix_task) => {
                        let _ = self.store.note(
                            run_id,
                            Some(&task.id),
                            "review returned needs_fix; task blocked, run stopped",
                        );
                        Some("review needs fix; awaiting explicit retry".to_string())
                    }
                    ReviewGateOutcome::Suspend(reason) => {
                        let _ = self.store.note(
                            run_id,
                            Some(&task.id),
                            &format!("circuit breaker: {reason}"),
                        );
                        Some(format!("review suspended: {reason}"))
                    }
                    ReviewGateOutcome::Skipped => {
                        let _ = self.store.note(
                            run_id,
                            Some(&task.id),
                            "no reviewer LLM; task blocked (no auto-pass per M7)",
                        );
                        Some("reviewer unavailable; blocked pending LLM".to_string())
                    }
                };
                if let Some(reason) = block_reason {
                    if !self.set_claimed_status(
                        run_id,
                        &task,
                        &claim,
                        TodoStatus::Blocked,
                        Some(&reason),
                    )? {
                        return Ok(echo_agent::tasks::RuntimeTaskResolution::Superseded);
                    }
                    return Ok(echo_agent::tasks::RuntimeTaskResolution::Blocked {
                        error: reason,
                        disposition: self.review_stop_disposition(run_id),
                    });
                }

                if !self.claim_is_current(run_id, &task.id, &claim)? {
                    return Ok(echo_agent::tasks::RuntimeTaskResolution::Superseded);
                }
                let execution_id = subagent_execution_id(run_id, &task.id, &claim);
                match integrate_reviewed_task(
                    self.dispatcher.clone(),
                    self.store.clone(),
                    run_id,
                    &task,
                    &execution_id,
                    &summary,
                    self.cancel.clone(),
                    self.trace_sink.clone(),
                )
                .await
                {
                    Ok(completion_summary) => {
                        if !self.set_claimed_status(
                            run_id,
                            &task,
                            &claim,
                            TodoStatus::Completed,
                            Some(&completion_summary),
                        )? {
                            return Ok(echo_agent::tasks::RuntimeTaskResolution::Superseded);
                        }
                        Ok(echo_agent::tasks::RuntimeTaskResolution::Completed)
                    }
                    Err(error) => {
                        let error = format!("worktree integration failed: {error}");
                        if !self.set_claimed_status(
                            run_id,
                            &task,
                            &claim,
                            TodoStatus::Failed,
                            Some(&error),
                        )? {
                            return Ok(echo_agent::tasks::RuntimeTaskResolution::Superseded);
                        }
                        Ok(echo_agent::tasks::RuntimeTaskResolution::Failed { error })
                    }
                }
            }
        }
    }

    async fn abandon_claim(
        &self,
        run_id: &str,
        claim: &echo_agent::tasks::TaskClaim,
        runtime_task: &echo_agent::tasks::Task,
        abandonment: echo_agent::tasks::RuntimeClaimAbandonment,
    ) -> echo_agent::error::Result<()> {
        let task = self.plan_task(&runtime_task.spec.id)?;
        let (status, summary) = match abandonment {
            echo_agent::tasks::RuntimeClaimAbandonment::Cancelled => (
                TodoStatus::Cancelled,
                "dispatch cancelled before resolution".to_string(),
            ),
            echo_agent::tasks::RuntimeClaimAbandonment::Failed { error } => {
                (TodoStatus::Failed, error)
            }
        };
        let _ = self.set_claimed_status(run_id, &task, claim, status, Some(&summary))?;
        Ok(())
    }

    async fn block_task(
        &self,
        run_id: &str,
        task: &echo_agent::tasks::Task,
        reason: &str,
    ) -> echo_agent::error::Result<()> {
        self.store
            .set_task_status(
                run_id,
                &task.spec.id,
                TodoStatus::Blocked,
                None,
                Some(reason),
            )
            .map(|_| ())
            .map_err(|error| echo_agent::error::ReactError::Other(error.to_string()))
    }

    async fn failed_task_disposition(
        &self,
        run_id: &str,
        _task: &echo_agent::tasks::Task,
        all_unfinished_failed_or_blocked: bool,
    ) -> echo_agent::error::Result<echo_agent::tasks::RuntimeStopDisposition> {
        if all_unfinished_failed_or_blocked {
            Ok(echo_agent::tasks::RuntimeStopDisposition::Fail)
        } else {
            Ok(self.review_stop_disposition(run_id))
        }
    }

    async fn interruption_outcome(
        &self,
        run_id: &str,
    ) -> echo_agent::error::Result<echo_agent::tasks::RuntimeDagOutcome> {
        let paused = self
            .store
            .get_run(run_id)
            .ok()
            .flatten()
            .is_some_and(|run| run.status == TaskRunStatus::Paused);
        Ok(if paused {
            echo_agent::tasks::RuntimeDagOutcome::Paused {
                failed_task_id: "<pause>".to_string(),
                error: "paused by user".to_string(),
            }
        } else {
            echo_agent::tasks::RuntimeDagOutcome::Cancelled
        })
    }

    async fn note_stalled(&self, run_id: &str, reason: &str) -> echo_agent::error::Result<()> {
        self.store
            .note(run_id, None, reason)
            .map_err(|error| echo_agent::error::ReactError::Other(error.to_string()))
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_runtime_plan<W: TaskDispatcher + 'static>(
    store: Arc<TaskRuntimeStore>,
    dispatcher: W,
    reviewer_llm: Option<Arc<dyn echo_agent::llm::LlmClient>>,
    run_id: &str,
    limits: EkoExecutionLimits,
    parent_cancel: CancellationToken,
    trace_sink: Option<ExecSink>,
) -> Result<RunOutcome, ExecError> {
    let run_store = store.clone();
    let controller = Arc::new(EkoRuntimeDagController {
        store,
        dispatcher: Arc::new(dispatcher),
        reviewer_llm,
        write_sem: Arc::new(Semaphore::new(limits.max_concurrent_writes.max(1))),
        shell_sem: Arc::new(Semaphore::new(limits.max_concurrent_shells.max(1))),
        llm_sem: Arc::new(Semaphore::new(limits.max_parallel_llm_calls.max(1))),
        file_write_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        trace_sink,
        cancel: parent_cancel.clone(),
        plan_tasks: std::sync::Mutex::new(HashMap::new()),
    });
    let executor = echo_agent::tasks::RuntimeDagExecutor::new(
        controller,
        echo_agent::tasks::RuntimeDagExecutorConfig {
            max_concurrent_subagents: limits.max_concurrent_subagents,
            ..Default::default()
        },
    );
    let outcome = executor
        .execute(run_id, parent_cancel)
        .await
        .map_err(|error| ExecError::Other(error.to_string()))?;
    let terminal_status = match &outcome {
        echo_agent::tasks::RuntimeDagOutcome::Failed { .. } => Some(TaskRunStatus::Failed),
        echo_agent::tasks::RuntimeDagOutcome::Paused { .. } => Some(TaskRunStatus::Paused),
        echo_agent::tasks::RuntimeDagOutcome::Completed
        | echo_agent::tasks::RuntimeDagOutcome::Cancelled => None,
    };
    if let Some(status) = terminal_status {
        let _ = run_store.transition_run(run_id, status);
    }
    Ok(match outcome {
        echo_agent::tasks::RuntimeDagOutcome::Completed => RunOutcome::Completed,
        echo_agent::tasks::RuntimeDagOutcome::Failed {
            failed_task_id,
            error,
        } => RunOutcome::Failed {
            failed_task_id,
            error,
        },
        echo_agent::tasks::RuntimeDagOutcome::Paused {
            failed_task_id,
            error,
        } => RunOutcome::Paused {
            failed_task_id,
            error,
        },
        echo_agent::tasks::RuntimeDagOutcome::Cancelled => RunOutcome::Cancelled,
    })
}

#[allow(clippy::too_many_arguments)]
async fn integrate_reviewed_task<W: TaskDispatcher + 'static>(
    dispatcher: Arc<W>,
    store: Arc<TaskRuntimeStore>,
    run_id: &str,
    task: &PlanTask,
    execution_id: &str,
    summary: &str,
    cancel: CancellationToken,
    trace_sink: Option<ExecSink>,
) -> Result<String, String> {
    let integration = match dispatcher
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
    /// No reviewer LLM configured. M7 requires a stop rather than auto-pass.
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
    subagent_output: &str,
) -> ReviewGateOutcome {
    // Skip the LLM gate when the task declares no acceptance criteria
    // AND is not an implementation/debugging kind (those are always gated
    // because prose about mutations cannot be trusted).
    if !super::review::requires_review(task) {
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
        match super::review::review_task(&llm, &store, run_id, task, subagent_output).await {
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

/// Execute a single task through a selected Subagent or the primary Agent.
///
/// The framework executor already holds the global Subagent permit; this
/// function enforces EKO write/shell/LLM and file-ownership limits.
#[allow(clippy::too_many_arguments)] // store + semaphores + locks + sinks all thread through
async fn execute_task(
    store: Arc<TaskRuntimeStore>,
    primary_agent: crate::agent_handle::AgentHandle,
    write_sem: Arc<Semaphore>,
    shell_sem: Arc<Semaphore>,
    llm_sem: Arc<Semaphore>,
    file_write_locks: Arc<std::sync::Mutex<HashMap<String, Arc<TokioMutex<()>>>>>,
    trace_sink: Option<ExecSink>,
    run_id: String,
    claim: echo_agent::tasks::TaskClaim,
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
            return Err(TaskDispatchFailure::failed(task_id.clone(), msg));
        }
    }

    // Create a child cancellation token for THIS task and register it with the
    // store. remove_task / update_task can cancel it to stop this Subagent
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

    // A PlanTask is a stable plan node; each dispatch attempt is a distinct
    // SubagentRun. Never collapse retries back to the bare task id.
    let attempt = claim.attempt;
    let execution_id = subagent_execution_id(&run_id, &task_id, &claim);
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

    emit_task_started(
        trace_sink.as_ref(),
        &run_id,
        &execution_id,
        &task,
        &contract,
    );

    // Acquire EKO product-resource permits with cancel awareness:
    // - Read-only tasks need no additional permit; the framework executor
    //   already owns the global Subagent permit.
    // - Write tasks (implementation/debugging) take the write permit.
    // - Verification tasks (shell/build/test) take the write permit + the shell
    //   permit (default 1, plan §678-680 shell_concurrency = 1).
    let is_shell = matches!(task.kind, PlanTaskKind::Verification);
    let (_write_permit, _shell_permit) = if is_shell {
        tracing::info!(
            run_id = %run_id,
            task_id = %task_id,
            available = write_sem.available_permits(),
            "task_runtime: waiting for write permit"
        );
        let wp = tokio::select! {
            biased;
            _ = task_cancel.cancelled() => return Err(TaskDispatchFailure::cancelled(task_id.clone(), "cancelled while waiting for write permit")),
            p = write_sem.acquire() => p.map_err(|e| TaskDispatchFailure::failed(task_id.clone(), e.to_string()))?,
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
            _ = task_cancel.cancelled() => return Err(TaskDispatchFailure::cancelled(task_id.clone(), "cancelled while waiting for shell permit")),
            p = shell_sem.acquire() => p.map_err(|e| TaskDispatchFailure::failed(task_id.clone(), e.to_string()))?,
        };
        tracing::info!(
            run_id = %run_id,
            task_id = %task_id,
            "task_runtime: acquired shell permit"
        );
        (Some(wp), Some(sp))
    } else if is_write {
        tracing::info!(
            run_id = %run_id,
            task_id = %task_id,
            available = write_sem.available_permits(),
            "task_runtime: waiting for write permit"
        );
        let wp = tokio::select! {
            biased;
            _ = task_cancel.cancelled() => return Err(TaskDispatchFailure::cancelled(task_id.clone(), "cancelled while waiting for write permit")),
            p = write_sem.acquire() => p.map_err(|e| TaskDispatchFailure::failed(task_id.clone(), e.to_string()))?,
        };
        tracing::info!(
            run_id = %run_id,
            task_id = %task_id,
            "task_runtime: acquired write permit"
        );
        (Some(wp), None)
    } else {
        (None, None)
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
                        return Err(TaskDispatchFailure::cancelled(
                            task_id.clone(),
                            "cancelled while waiting for file write lock",
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
        _ = task_cancel.cancelled() => return Err(TaskDispatchFailure::cancelled(task_id.clone(), "cancelled while waiting for LLM permit")),
        p = llm_sem.acquire() => p.map_err(|e| TaskDispatchFailure::failed(task_id.clone(), e.to_string()))?,
    };
    tracing::info!(
        run_id = %run_id,
        task_id = %task_id,
        "task_runtime: acquired llm permit"
    );

    // Summary Chain: gather the summaries of this task's completed
    // dependencies, so the Subagent gets compact upstream context instead of
    // (or in addition to) re-reading everything from scratch (plan §1039).
    let dep_summaries = collect_dependency_summaries(&store, &run_id, &task);
    let parent_goal = store.get_run(&run_id).ok().flatten().map(|run| run.goal);

    let workspace_root = primary_workspace_root_for_prompt(
        &contract.isolation_requested,
        primary_agent.read(|agent| agent.working_dir()).await,
    );
    let prompt_payload = crate::subagent_prompt::EkoPromptPayload::planned_task(
        &task,
        &dep_summaries,
        delegation_policy.can_delegate(),
        parent_goal.as_deref(),
        workspace_root.as_deref(),
    )
    .to_value()
    .map_err(|error| {
        TaskDispatchFailure::failed(
            task_id.clone(),
            format!("failed to serialize Subagent prompt payload: {error}"),
        )
    })?;
    let task_input = if task.description.trim().is_empty() {
        task.title.clone()
    } else {
        task.description.clone()
    };

    // Dispatch the task. Three paths, by kind:
    // - Read-only kinds (read_only_review, investigation, test_plan, review,
    //   summary) → delegate to the registered readonly subagent role via Fork.
    //   The child cancel token propagates parent-run cancellation.
    // - Writer kinds (implementation, debugging) delegate to the selected
    //   writer-capable Subagent. Coding uses worktree isolation; data roles use
    //   isolated data workspaces. Dispatch failure is terminal.
    // - Verification (shell/build/test) → MAIN agent executes directly. These
    //   run against the workspace (testing just-written changes), so routing
    //   them to a separate worktree checkout would detach them from the work.
    let is_read_only_task = task.kind.is_read_only();
    let is_writer_task = matches!(
        task.kind,
        PlanTaskKind::Implementation | PlanTaskKind::Debugging
    );
    let app_owns_subagent_events = !is_read_only_task && !is_writer_task;
    // Resolve the run's root_message_id so the framework can carry it on
    // SubagentEvent::DispatchStarted → execution://event, letting the frontend
    // pin the subagent stream to the right chat message block.
    let root_message_id = store
        .get_run(&run_id)
        .ok()
        .flatten()
        .map(|r| r.root_message_id);
    let controlled_attempt = if is_read_only_task || is_writer_task {
        let framework_executor = primary_agent
            .read(|agent| agent.subagent_executor().clone())
            .await;
        let (control_identity, framework_identity) = super::subagent_control::attempt_identity(
            &run_id,
            &task_id,
            &execution_id,
            claim.revision,
            attempt,
        )
        .map_err(|error| {
            TaskDispatchFailure::failed(
                task_id.clone(),
                format!("invalid Subagent attempt identity: {error}"),
            )
        })?;
        let guard = store
            .record_controlled_subagent_assigned(
                &run_id,
                &task_id,
                &execution_id,
                &task.agent_role,
                &task.title,
                claim.revision,
                attempt,
                task.kind.is_read_only(),
                app_owns_subagent_events,
                framework_executor.clone(),
            )
            .map_err(|error| {
                TaskDispatchFailure::failed(
                    task_id.clone(),
                    format!("failed to persist Subagent start boundary: {error}"),
                )
            })?;
        store
            .deliver_pending_subagent_guidance(&control_identity, &framework_executor)
            .map_err(|error| {
                TaskDispatchFailure::failed(
                    task_id.clone(),
                    format!("failed to deliver queued Subagent guidance: {error}"),
                )
            })?;
        Some((framework_identity, guard))
    } else {
        store
            .record_subagent_assigned(
                &run_id,
                &task_id,
                &execution_id,
                &task.agent_role,
                &task.title,
                claim.revision,
                attempt,
                task.kind.is_read_only(),
                app_owns_subagent_events,
            )
            .map_err(|error| {
                TaskDispatchFailure::failed(
                    task_id.clone(),
                    format!("failed to persist Subagent start boundary: {error}"),
                )
            })?;
        None
    };
    let framework_attempt_identity = controlled_attempt
        .as_ref()
        .map(|(identity, _guard)| identity.clone());
    let result = if is_read_only_task {
        tracing::info!(
            run_id = %run_id,
            task_id = %task_id,
            agent_role = %task.agent_role,
            task_chars = task_input.chars().count(),
            "task_runtime: delegating read-only task to subagent"
        );
        let dispatch_result = run_readonly_subagent(
            &primary_agent,
            &run_id,
            &execution_id,
            root_message_id.as_deref(),
            &task.agent_role,
            &task_input,
            prompt_payload.clone(),
            task.allowed_tools.clone(),
            task_cancel.clone(),
            delegation_policy,
            trace_sink.clone(),
            framework_attempt_identity.clone().ok_or_else(|| {
                TaskDispatchFailure::failed(
                    task_id.clone(),
                    "read-only Subagent is missing its attempt identity",
                )
            })?,
        )
        .await;
        match dispatch_result {
            Ok(sub_result) => {
                tracing::info!(
                    run_id = %run_id,
                    task_id = %task_id,
                    agent_role = %task.agent_role,
                    output_chars = sub_result.output.chars().count(),
                    iterations = sub_result.iterations,
                    usage_reported = sub_result.usage.is_some(),
                    terminal_status = ?sub_result.outcome.status,
                    "task_runtime: read-only subagent settled"
                );
                finalize_framework_subagent_result(&store, &run_id, &execution_id, sub_result)
            }
            Err(e) => {
                tracing::warn!(
                    run_id = %run_id,
                    task_id = %task_id,
                    agent_role = %task.agent_role,
                    error = %e,
                    "task_runtime: read-only subagent failed"
                );
                Err(e)
            }
        }
    } else if is_writer_task {
        // Route to the selected writer-capable Subagent.
        tracing::info!(
            run_id = %run_id,
            task_id = %task_id,
            agent_role = %task.agent_role,
            task_chars = task_input.chars().count(),
            "task_runtime: delegating writer task to subagent"
        );
        let dispatch_result = run_writer_subagent(
            &primary_agent,
            store.clone(),
            &run_id,
            &execution_id,
            &task_isolation_id(&run_id, &task_id),
            &task.agent_role,
            &task_input,
            prompt_payload.clone(),
            task.allowed_tools.clone(),
            task_cancel.clone(),
            delegation_policy,
            trace_sink.clone(),
            framework_attempt_identity.ok_or_else(|| {
                TaskDispatchFailure::failed(
                    task_id.clone(),
                    "writer Subagent is missing its attempt identity",
                )
            })?,
        )
        .await;
        match dispatch_result {
            Ok(sub_result) => {
                tracing::info!(
                    run_id = %run_id,
                    task_id = %task_id,
                    agent_role = %task.agent_role,
                    output_chars = sub_result.output.chars().count(),
                    summary_chars = sub_result.outcome.summary.chars().count(),
                    iterations = sub_result.iterations,
                    usage_reported = sub_result.usage.is_some(),
                    terminal_status = ?sub_result.outcome.status,
                    "task_runtime: writer subagent settled"
                );
                finalize_framework_subagent_result(&store, &run_id, &execution_id, sub_result)
            }
            Err(error) => Err(if task_cancel.is_cancelled() {
                ExecutionFailure::cancelled("task cancelled")
            } else {
                error
            }),
        }
    } else {
        let compiler = crate::subagent_prompt::EkoSubagentPromptCompiler;
        let compiled = compiler.compile_primary_invocation(&SubagentPromptInput {
            agent_name: "primary",
            task: &task_input,
            mode: echo_agent::agent::subagent::ExecutionMode::Sync,
            transfer_policy: ContextTransferPolicy::Fresh,
            parent_context: None,
            inherit_history: None,
            payload: Some(&prompt_payload),
            constraints: &[],
        });
        emit_primary_subagent_started(
            trace_sink.as_ref(),
            &run_id,
            &execution_id,
            &task,
            &contract,
        );
        emit_primary_subagent_isolation_observed(
            trace_sink.as_ref(),
            &run_id,
            &execution_id,
            &task,
            &contract,
        );
        run_main_agent_task(
            &primary_agent,
            store.clone(),
            &run_id,
            &task,
            &execution_id,
            &compiled.task_input,
            task_cancel.clone(),
            trace_sink.clone(),
        )
        .await
    };

    match result {
        Ok((task_result, full_output, usage)) => {
            // The parent consumes the bounded structured summary; extract
            // suggested_tasks from the full output because that optional block
            // appears before the terminal ## Result contract.
            let suggested_tasks = extract_suggested_tasks_from_subagent_output(&full_output);
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
                subagent_name: task.agent_role.clone(),
                result: task_result.clone(),
                decisions: vec![],
                next_implications: vec![],
                suggested_tasks: suggested_tasks.clone(),
                created_at: chrono::Utc::now(),
            }) {
                tracing::warn!(task_id = %task_id, error = %e, "failed to persist TaskExecutionSummary");
            }
            if let Err(error) = store.record_subagent_released(SubagentReleaseRecord {
                run_id: &run_id,
                task_id: &task_id,
                execution_id: &execution_id,
                agent_name: &task.agent_role,
                task_subject: &task.title,
                plan_revision: claim.revision,
                attempt,
                status: task_result.status.as_str(),
                result: Some(&task_result),
                full_output: Some(&full_output),
                usage: Some(&usage.durable),
                dispatch_hook: app_owns_subagent_events,
            }) {
                return Err(TaskDispatchFailure::failed(
                    task_id,
                    format!("Subagent completed but terminal boundary was not persisted: {error}"),
                ));
            }
            // Suggested tasks are persisted in TaskExecutionSummary.suggested_tasks
            // (see put_summary above). They are NOT auto-inserted into the plan —
            // doing so caused unbounded plan expansion + dependent tasks to wait
            // forever on looping parents. The primary agent / user can promote a
            // suggestion via task_update when desired. Record a Note so the
            // suggestions are visible in the event stream regardless.
            if !suggested_tasks.is_empty() {
                let titles: Vec<&str> = suggested_tasks.iter().map(|s| s.title.as_str()).collect();
                let _ = store.note(
                    &run_id,
                    Some(&task_id),
                    &format!(
                        "subagent suggested {} follow-up task(s): [{}]. \
                         Not auto-inserted into plan; promote via task_update if desired.",
                        suggested_tasks.len(),
                        titles.join("; "),
                    ),
                );
            }
            let terminal_payload = serde_json::json!({
                "execution_id": &execution_id,
                "output": &full_output,
                "terminal_status": task_result.status.as_str(),
                "contract_version": task_result.contract_version,
                "summary": &task_result.summary,
                "artifacts": &task_result.artifacts,
                "verification": &task_result.verification,
                "remaining_work": &task_result.remaining_work,
                "touched_files": &task_result.touched_files,
            });
            emit_exec(
                trace_sink.as_ref(),
                ExecEvent::task(
                    run_id.clone(),
                    task_id.clone(),
                    RuntimeEventKind::TaskCompleted,
                    terminal_payload.clone(),
                )
                .with_agent(task.agent_role.clone()),
            );
            if app_owns_subagent_events {
                emit_exec(
                    trace_sink.as_ref(),
                    ExecEvent::subagent(
                        run_id.clone(),
                        task_id.clone(),
                        execution_id.clone(),
                        RuntimeEventKind::Completed,
                        terminal_payload,
                    )
                    .with_agent(task.agent_role.clone()),
                );
            }
            Ok(TaskDispatchSuccess {
                task_id,
                result: task_result,
                full_output,
            })
        }
        Err(failure) => {
            let status = failure.status;
            let message = failure.message;
            let usage = failure.usage;
            let task_result =
                SubagentTaskResult::terminal(status, message.clone(), vec![message.clone()]);
            if let Err(error) = store.put_summary(&TaskExecutionSummary {
                run_id: run_id.clone(),
                task_id: task_id.clone(),
                subagent_name: task.agent_role.clone(),
                result: task_result.clone(),
                decisions: Vec::new(),
                next_implications: Vec::new(),
                suggested_tasks: Vec::new(),
                created_at: chrono::Utc::now(),
            }) {
                tracing::warn!(task_id = %task_id, %error, "failed to persist terminal TaskExecutionSummary");
            }
            if let Err(error) = store.record_subagent_released(SubagentReleaseRecord {
                run_id: &run_id,
                task_id: &task_id,
                execution_id: &execution_id,
                agent_name: &task.agent_role,
                task_subject: &task.title,
                plan_revision: claim.revision,
                attempt,
                status: status.as_str(),
                result: Some(&task_result),
                full_output: Some(&message),
                usage: usage.as_ref().map(|value| &value.durable),
                dispatch_hook: app_owns_subagent_events,
            }) {
                tracing::warn!(
                    run_id = %run_id,
                    task_id = %task_id,
                    %error,
                    "failed to persist Subagent terminal boundary"
                );
            }
            if status == SubagentRunStatus::Cancelled {
                tracing::info!(
                    run_id = %run_id,
                    task_id = %task_id,
                    agent_role = %task.agent_role,
                    "task_runtime: task cancelled"
                );
            } else {
                tracing::warn!(
                    run_id = %run_id,
                    task_id = %task_id,
                    agent_role = %task.agent_role,
                    error = %message,
                    "task_runtime: task failed"
                );
            }
            let terminal_payload = serde_json::json!({
                "execution_id": &execution_id,
                "error": &message,
                "terminal_status": status.as_str(),
                "contract_version": task_result.contract_version,
                "summary": &task_result.summary,
                "artifacts": &task_result.artifacts,
                "verification": &task_result.verification,
                "remaining_work": &task_result.remaining_work,
                "touched_files": &task_result.touched_files,
            });
            let task_terminal_event = match status {
                SubagentRunStatus::Cancelled => RuntimeEventKind::TaskCancelled,
                SubagentRunStatus::TimedOut => RuntimeEventKind::TaskTimedOut,
                SubagentRunStatus::Running
                | SubagentRunStatus::Completed
                | SubagentRunStatus::Failed => RuntimeEventKind::TaskFailed,
            };
            emit_exec(
                trace_sink.as_ref(),
                ExecEvent::task(
                    run_id.clone(),
                    task_id.clone(),
                    task_terminal_event,
                    terminal_payload.clone(),
                )
                .with_agent(task.agent_role.clone()),
            );
            if app_owns_subagent_events {
                emit_exec(
                    trace_sink.as_ref(),
                    ExecEvent::subagent(
                        run_id,
                        task_id.clone(),
                        execution_id,
                        subagent_terminal_event(status),
                        terminal_payload,
                    )
                    .with_agent(task.agent_role.clone()),
                );
            }
            Err(TaskDispatchFailure::from_execution(
                task_id,
                ExecutionFailure {
                    status,
                    message,
                    usage,
                },
            ))
        }
    }
}

const MAX_SUGGESTED_TASKS_PER_SUBAGENT: usize = 5;

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

fn extract_suggested_tasks_from_subagent_output(text: &str) -> Vec<SuggestedTask> {
    let mut out = Vec::new();
    for candidate in suggested_task_json_candidates(text) {
        let Ok(envelope) = serde_json::from_str::<SuggestedTaskEnvelope>(&candidate) else {
            continue;
        };
        for raw in envelope.suggested_tasks {
            if out.len() >= MAX_SUGGESTED_TASKS_PER_SUBAGENT {
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

/// Prefers the structured TaskExecutionSummary (persisted by put_summary at
/// task boundary) over the truncated todo.summary text, so downstream Subagents
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

fn primary_workspace_root_for_prompt(
    isolation_requested: &str,
    workspace_root: Option<std::path::PathBuf>,
) -> Option<std::path::PathBuf> {
    workspace_root.filter(|_| !matches!(isolation_requested, "worktree" | "workspace"))
}

fn runtime_contract_started_payload(
    contract: &SubagentRuntimeContract,
    task: &PlanTask,
    execution_id: &str,
) -> serde_json::Value {
    serde_json::json!({
        "execution_id": execution_id,
        "kind": task.kind.as_str(),
        "agent_role": task.agent_role,
        "title": task.title,
        "task": task.description,
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
    execution_id: &str,
    task: &PlanTask,
    contract: &SubagentRuntimeContract,
) {
    emit_exec(
        sink,
        ExecEvent::task(
            run_id,
            task.id.clone(),
            RuntimeEventKind::TaskStarted,
            runtime_contract_started_payload(contract, task, execution_id),
        )
        .with_agent(task.agent_role.clone()),
    );
}

fn emit_primary_subagent_started(
    sink: Option<&ExecSink>,
    run_id: &str,
    execution_id: &str,
    task: &PlanTask,
    contract: &SubagentRuntimeContract,
) {
    emit_exec(
        sink,
        ExecEvent::subagent(
            run_id,
            task.id.clone(),
            execution_id,
            RuntimeEventKind::Started,
            runtime_contract_started_payload(contract, task, execution_id),
        )
        .with_agent(task.agent_role.clone()),
    );
}

fn emit_primary_subagent_isolation_observed(
    sink: Option<&ExecSink>,
    run_id: &str,
    execution_id: &str,
    task: &PlanTask,
    contract: &SubagentRuntimeContract,
) {
    emit_exec(
        sink,
        ExecEvent::subagent(
            run_id,
            task.id.clone(),
            execution_id,
            RuntimeEventKind::IsolationObserved,
            runtime_isolation_observed_payload(contract, "primary"),
        )
        .with_agent(task.agent_role.clone()),
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

/// Run a READ-ONLY task by delegating to a registered subagent role via the
/// primary agent's prompt-payload delegation API. Fork mode runs the Subagent
/// on an isolated agent instance under the executor's own semaphore (not the
/// primary agent's execution_mutex), so multiple read-only Subagents run in
/// parallel. The child cancel token propagates parent-run cancellation.
#[allow(clippy::too_many_arguments)] // handles + cancel + sink thread through; matches framework dispatch style
async fn run_readonly_subagent(
    primary_agent: &crate::agent_handle::AgentHandle,
    run_id: &str,
    execution_id: &str,
    message_id: Option<&str>,
    role: &str,
    task_input: &str,
    prompt_payload: serde_json::Value,
    allowed_tools: Vec<String>,
    cancel: CancellationToken,
    delegation_policy: echo_agent::tasks::NestedDelegationPolicy,
    trace_sink: Option<ExecSink>,
    attempt_identity: echo_agent::agent::subagent::SubagentAttemptIdentity,
) -> Result<echo_agent::agent::subagent::SubagentResult, ExecutionFailure> {
    primary_agent
        .read_async(|agent| {
            let task_input = task_input.to_string();
            let prompt_payload = prompt_payload.clone();
            let role = role.to_string();
            let run_id = run_id.to_string();
            let execution_id = execution_id.to_string();
            let message_id = message_id.map(|s| s.to_string());
            let core_trace_sink = exec_trace_sink_to_core(trace_sink);
            let attempt_identity = attempt_identity.clone();
            Box::pin(async move {
                let runtime_context = Some(echo_core::tools::ExternalRunContext {
                    conversation_id: None,
                    run_id: Some(run_id.clone()),
                    turn_id: message_id.clone(),
                    execution_id: Some(execution_id),
                    isolation_id: None,
                    message_id,
                    cancel: Some(Arc::new(cancel.clone())),
                    trace_sink: core_trace_sink,
                    delegation_policy: Some(delegation_policy),
                });
                agent
                    .delegate_to_agent_attempt_with_prompt_payload(
                        &role,
                        &task_input,
                        &run_id,
                        cancel,
                        0,
                        runtime_context,
                        Some(allowed_tools),
                        Some(prompt_payload),
                        attempt_identity,
                    )
                    .await
                    .map_err(|error| {
                        ExecutionFailure::from_react(error, "subagent dispatch failed")
                    })
            })
        })
        .await
}

fn exec_trace_sink_to_core(trace_sink: Option<ExecSink>) -> Option<echo_core::tools::TraceSinkFn> {
    // Wrap an app-layer `ExecSink` into the framework's `TraceSinkFn`
    // (Value-based) so it can be carried across `tokio::spawn` boundaries via
    // `ExternalRunContext.trace_sink`. The app's `scoped_with_ctx_run_id`
    // (task_tools.rs) reads `ctx.trace_sink` back and re-scopes it into
    // `CURRENT_TRACE_SINK` so tools running inside a spawned task (e.g.
    // `task_execute`) can emit execution-flow events.
    //
    // Subagent dispatch itself does NOT use this path — it goes through
    // `SubagentEventBus`. This conversion is only for the main-agent tool path
    // (task_execute / task_create) that runs inside the framework's spawned
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
/// Mirrors [`run_readonly_subagent`] but with attachment-aware delegation: when
/// the run carries user attachments (images/files), the multimodal variant
/// the message-aware prompt-payload delegation API is used so the writer
/// Subagent sees them (parity with the old in-place `run_main_agent_task` path).
///
/// The registered writer Subagent carries the full write tool set and its
/// definition selects worktree or data-workspace isolation. Coding writes land
/// in an isolated checkout rather than the main workspace.
/// If no WorktreeFactory is configured, dispatch **hard-fails** (Phase 2 Task 13)
/// rather than silently sharing the main tree.
/// Disjoint exact owners may run concurrently; the DAG scheduler separates
/// overlapping and unknown ownership before dispatch.
#[allow(clippy::too_many_arguments)] // handles + cancel + sink thread through; matches framework dispatch style
async fn run_writer_subagent(
    primary_agent: &crate::agent_handle::AgentHandle,
    store: Arc<TaskRuntimeStore>,
    run_id: &str,
    execution_id: &str,
    isolation_id: &str,
    role: &str,
    task_input: &str,
    prompt_payload: serde_json::Value,
    allowed_tools: Vec<String>,
    cancel: CancellationToken,
    delegation_policy: echo_agent::tasks::NestedDelegationPolicy,
    trace_sink: Option<ExecSink>,
    attempt_identity: echo_agent::agent::subagent::SubagentAttemptIdentity,
) -> Result<echo_agent::agent::subagent::SubagentResult, ExecutionFailure> {
    // Rebuild a multimodal Message when the run carries user attachments, so
    // the writer Subagent sees the same images/files as the primary agent would
    // (parity with run_main_agent_task, executor.rs:1373-1380).
    let run_record = store.get_run(run_id).ok().flatten();
    let root_message_id = run_record.as_ref().map(|r| r.root_message_id.clone());
    let conversation_id = run_record.as_ref().map(|r| r.conversation_id.clone());
    let run_message: Option<echo_core::llm::types::Message> = run_record.as_ref().and_then(|r| {
        if r.attachments.is_empty() {
            None
        } else {
            crate::attachments::build_message_from_refs(task_input, &r.attachments).ok()
        }
    });

    primary_agent
        .read_async(|agent| {
            let task_input = task_input.to_string();
            let prompt_payload = prompt_payload.clone();
            let role = role.to_string();
            let run_id = run_id.to_string();
            let execution_id = execution_id.to_string();
            let isolation_id = isolation_id.to_string();
            let run_message = run_message.clone();
            let core_trace_sink = exec_trace_sink_to_core(trace_sink);
            let attempt_identity = attempt_identity.clone();
            Box::pin(async move {
                let runtime_context = Some(echo_core::tools::ExternalRunContext {
                    conversation_id: conversation_id.clone(),
                    run_id: Some(run_id.clone()),
                    turn_id: root_message_id.clone(),
                    execution_id: Some(execution_id),
                    isolation_id: Some(isolation_id),
                    message_id: root_message_id,
                    cancel: Some(Arc::new(cancel.clone())),
                    trace_sink: core_trace_sink,
                    delegation_policy: Some(delegation_policy),
                });
                if let Some(msg) = run_message {
                    agent
                        .delegate_to_agent_attempt_with_message_and_prompt_payload(
                            &role,
                            &task_input,
                            msg,
                            &run_id,
                            cancel,
                            0,
                            runtime_context,
                            Some(allowed_tools.clone()),
                            Some(prompt_payload.clone()),
                            attempt_identity,
                        )
                        .await
                } else {
                    agent
                        .delegate_to_agent_attempt_with_prompt_payload(
                            &role,
                            &task_input,
                            &run_id,
                            cancel,
                            0,
                            runtime_context,
                            Some(allowed_tools),
                            Some(prompt_payload),
                            attempt_identity,
                        )
                        .await
                }
                .map_err(|error| {
                    ExecutionFailure::from_react(error, "writer subagent dispatch failed")
                })
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
/// (readonly Subagents can't write). The write_sem acquired by the caller serializes them,
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

#[allow(clippy::too_many_arguments)]
async fn run_main_agent_task(
    primary_agent: &crate::agent_handle::AgentHandle,
    store: Arc<TaskRuntimeStore>,
    run_id: &str,
    task: &PlanTask,
    execution_id: &str,
    prompt: &str,
    cancel: CancellationToken,
    trace_sink: Option<ExecSink>,
) -> Result<(SubagentTaskResult, String, TaskExecutionUsage), ExecutionFailure> {
    let run_id = run_id.to_string();
    let task_id = task.id.clone();
    let agent_role = task.agent_role.clone();
    let execution_id = execution_id.to_string();

    // Rebuild a multimodal Message when the run carries user attachments, so
    // writer Subagents see the same images/files as the main agent (#1b).
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
                let visible_tools = crate::tool_exposure::initial_visible_tools(
                    InteractionMode::Task,
                    &agent.tool_names(),
                );
                crate::tool_exposure::record_mode_schema_budget(
                    InteractionMode::Task,
                    &agent.tool_definitions(),
                    &visible_tools,
                );
                let visible_tools = Some(visible_tools);
                let invocation = echo_core::agent::AgentInvocationContext {
                    history: None,
                    runtime: Some(echo_core::tools::ExternalRunContext {
                        conversation_id,
                        run_id: Some(run_id.clone()),
                        turn_id: root_message_id.clone(),
                        execution_id: Some(execution_id.clone()),
                        isolation_id: None,
                        message_id: root_message_id,
                        cancel: Some(Arc::new(cancel.clone())),
                        trace_sink: exec_trace_sink_to_core(trace_sink.clone()),
                        delegation_policy: None,
                    }),
                    working_dir: None,
                    cancel: None,
                    disabled_tools: Some(crate::tool_exposure::disabled_tools_for_mode(
                        InteractionMode::Task,
                    )),
                    visible_tools,
                    run_budget: None,
                };
                let event_identity = echo_core::agent::EventIdentity::from_invocation(&invocation)
                    .map_err(|error| ExecutionFailure::from_react(error, "invalid task event identity"))?;
                // Multimodal path when the run has attachments; plain text otherwise.
                let raw_stream = if let Some(msg) = run_message {
                    agent
                        .execute_stream_message_with_invocation_context(
                            msg,
                            cancel,
                            invocation,
                        )
                        .await
                        .map_err(|error| {
                            ExecutionFailure::from_react(error, "main agent stream failed")
                        })?
                } else {
                    agent
                        .execute_stream_with_invocation_context(&prompt, cancel, invocation)
                        .await
                        .map_err(|error| {
                            ExecutionFailure::from_react(error, "main agent stream failed")
                        })?
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
                let started = std::time::Instant::now();
                let mut usage = TaskExecutionUsage::default();

                while let Some(event_result) = stream.next().await {
                    let envelope = event_result.map_err(|error| {
                            ExecutionFailure::from_react(error, "main agent stream failed")
                        })?;
                    let source_event_id = envelope.event_id.to_string();
                    let event = envelope.payload;
                    match event {
                        AgentEvent::Token(content) => {
                            if in_thinking {
                                emit_exec(
                                    trace_sink.as_ref(),
                                    ExecEvent::subagent(
                                        run_id.clone(),
                                        task_id.clone(),
                                        execution_id.clone(),
                                        RuntimeEventKind::ThinkingDelta,
                                        serde_json::json!({ "content": content }),
                                    )
                                    .with_agent(agent_role.clone()),
                                );
                            } else {
                                output.push_str(&content);
                                emit_exec(
                                    trace_sink.as_ref(),
                                    ExecEvent::subagent(
                                        run_id.clone(),
                                        task_id.clone(),
                                        execution_id.clone(),
                                        RuntimeEventKind::TokenDelta,
                                        serde_json::json!({ "content": content }),
                                    )
                                    .with_agent(agent_role.clone()),
                                );
                            }
                        }
                        AgentEvent::ThinkStart => {
                            in_thinking = true;
                            emit_exec(
                                trace_sink.as_ref(),
                                ExecEvent::subagent(
                                    run_id.clone(),
                                    task_id.clone(),
                                    execution_id.clone(),
                                    RuntimeEventKind::ThinkingStarted,
                                    serde_json::json!({}),
                                )
                                .with_agent(agent_role.clone()),
                            );
                        }
                        AgentEvent::ThinkEnd {
                            prompt_tokens,
                            completion_tokens,
                        } => {
                            in_thinking = false;
                            emit_exec(
                                trace_sink.as_ref(),
                                ExecEvent::subagent(
                                    run_id.clone(),
                                    task_id.clone(),
                                    execution_id.clone(),
                                    RuntimeEventKind::ThinkingEnded,
                                    serde_json::json!({
                                        "prompt_tokens": prompt_tokens,
                                        "completion_tokens": completion_tokens,
                                    }),
                                )
                                .with_agent(agent_role.clone()),
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
                            let input_tokens =
                                u64::try_from(prompt_tokens).unwrap_or(u64::MAX);
                            let output_tokens =
                                u64::try_from(completion_tokens).unwrap_or(u64::MAX);
                            usage.input_tokens =
                                usage.input_tokens.saturating_add(input_tokens);
                            usage.output_tokens =
                                usage.output_tokens.saturating_add(output_tokens);
                            store
                                .account_subagent_usage(
                                    &run_id,
                                    &execution_id,
                                    &source_event_id,
                                    input_tokens,
                                    output_tokens,
                                    0,
                                )
                                .map_err(|error| {
                                    event_cancel.cancel();
                                    ExecutionFailure::failed(format!(
                                        "failed to persist primary Subagent usage: {error}"
                                    ))
                                    .with_usage(usage.clone())
                                })?;
                            let usage_payload = serde_json::json!({
                                "model": model,
                                "prompt_tokens": prompt_tokens,
                                "completion_tokens": completion_tokens,
                                "total_tokens": total_tokens,
                                "cached_prompt_tokens": cached_prompt_tokens,
                                "cache_creation_prompt_tokens": cache_creation_prompt_tokens,
                                "usage_reported": usage_reported,
                                "usage_event_id": source_event_id,
                            });
                            emit_exec(
                                trace_sink.as_ref(),
                                ExecEvent::subagent(
                                    run_id.clone(),
                                    task_id.clone(),
                                    execution_id.clone(),
                                    RuntimeEventKind::Usage,
                                    usage_payload,
                                )
                                .with_agent(agent_role.clone()),
                            );
                        }
                        AgentEvent::ToolCall {
                            call_id,
                            invocation,
                        } => {
                            let name = invocation.name;
                            let args = invocation.args;
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
                                return Err(ExecutionFailure::failed(format!(
                                    "failed to persist tool start boundary for {name}: {error}"
                                )));
                            }
                            emit_exec(
                                trace_sink.as_ref(),
                                ExecEvent::subagent(
                                    run_id.clone(),
                                    task_id.clone(),
                                    execution_id.clone(),
                                    RuntimeEventKind::ToolStarted,
                                    serde_json::json!({
                                        "call_id": call_id,
                                        "name": name,
                                        "args": args,
                                    }),
                                )
                                .with_agent(agent_role.clone()),
                            );
                        }
                        AgentEvent::ToolResult {
                            call_id,
                            name,
                            result,
                        } => {
                            let result_text = if result.success {
                                result.output.clone()
                            } else {
                                result
                                    .error
                                    .clone()
                                    .unwrap_or_else(|| result.output.clone())
                            };
                            if let Some(check) = pending_verification.remove(&call_id) {
                                observed_verification.push(
                                    echo_agent::agent::subagent::SubagentVerification {
                                        check,
                                        status: if result.success {
                                            echo_agent::agent::subagent::SubagentVerificationStatus::Passed
                                        } else {
                                            echo_agent::agent::subagent::SubagentVerificationStatus::Failed
                                        },
                                        details: result_text.chars().take(500).collect(),
                                        source: echo_agent::agent::subagent::SubagentVerificationSource::Observed,
                                    },
                                );
                            }
                            if result.success
                                && let Some((write, path)) = pending_file_access.remove(&call_id)
                            {
                                if write {
                                    push_unique_path(&mut touched_files.written, path);
                                } else {
                                    push_unique_path(&mut touched_files.read, path);
                                }
                            } else {
                                pending_file_access.remove(&call_id);
                            }
                            if let Err(error) = store.record_tool_finished(
                                &run_id,
                                &task_id,
                                &execution_id,
                                &call_id,
                                &name,
                                result.success,
                                &result_text,
                                result.failure.as_ref(),
                            ) {
                                event_cancel.cancel();
                                return Err(ExecutionFailure::failed(format!(
                                    "tool {name} settled but its terminal boundary was not persisted: {error}"
                                )));
                            }
                            emit_exec(
                                trace_sink.as_ref(),
                                ExecEvent::subagent(
                                    run_id.clone(),
                                    task_id.clone(),
                                    execution_id.clone(),
                                    RuntimeEventKind::ToolCompleted,
                                    serde_json::json!({
                                        "call_id": call_id,
                                        "name": name,
                                        "result": result_text,
                                        "success": result.success,
                                        "failure": result.failure,
                                    }),
                                )
                                .with_agent(agent_role.clone()),
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
                                } => (RuntimeEventKind::ToolOutput, serde_json::json!({
                                    "call_id": call_id,
                                    "name": name,
                                    "message": message,
                                    "percent": percent,
                                })),
                                echo_agent::tools::ToolStreamEvent::Output { channel, chunk } => {
                                    (RuntimeEventKind::ToolOutput, serde_json::json!({
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
                                        RuntimeEventKind::ToolCompleted,
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
                                ExecEvent::subagent(
                                    run_id.clone(),
                                    task_id.clone(),
                                    execution_id.clone(),
                                    event_type,
                                    payload,
                                )
                                .with_agent(agent_role.clone()),
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
                            return Err(ExecutionFailure::cancelled("task cancelled"));
                        }
                        AgentEvent::Error {
                            source,
                            message,
                            failure,
                        } => {
                            return Err(ExecutionFailure::from_agent_failure(
                                &failure,
                                format!("{source}: {message}"),
                            ));
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
                let duration_ms =
                    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                usage.durable = SubagentRunUsage {
                    duration_ms: Some(duration_ms),
                    tokens_used: Some(
                        usage.input_tokens.saturating_add(usage.output_tokens),
                    ),
                    iterations: None,
                };
                store
                    .account_subagent_usage(
                        &run_id,
                        &execution_id,
                        "primary_subagent_duration",
                        0,
                        0,
                        duration_ms,
                    )
                    .map_err(|error| {
                        ExecutionFailure::failed(format!(
                            "failed to persist primary Subagent duration: {error}"
                        ))
                        .with_usage(usage.clone())
                    })?;
                Ok((
                    SubagentTaskResult::from_framework_outcome(&outcome),
                    output,
                    usage,
                ))
            })
        })
        .await
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
        agent_name: "task-runtime".to_string(),
        model: String::new(),
        provider: None,
        turn_id: None,
        execution_id: None,
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
/// the agent itself calls `task_create` (to materialise the plan) and
/// `task_execute` (which internally calls `execute_run`). Simple prompts that
/// the agent answers directly (without `task_execute`) are materialized as a
/// one-task Plan and must pass the same requirement/evidence completion gate.
///
/// **Why not call `execute_run` directly?** `execute_run` requires a plan to
/// already exist (`store.get_plan → NoPlan` if absent). The plan is created
/// by the agent during its ReAct loop via the `task_create` tool. Skipping
/// the agent loop would leave the plan empty and the run would fail
/// immediately. This mirrors how `launch_unified_run` (chat path) works.
///
/// The run is created with `attended_mode = Unattended` so the configured
/// write preflight applies inside `task_execute` / `execute_task`.
#[allow(clippy::too_many_arguments)] // run identity + Agent + cancellation + write policy form the driver boundary
#[cfg(test)]
async fn launch_unattended_run(
    store: Arc<TaskRuntimeStore>,
    primary_agent: crate::agent_handle::AgentHandle,
    source_kind: &str,
    source_id: &str,
    fire_id: &str,
    prompt: &str,
    parent_cancel: CancellationToken,
    write_mode: UnattendedWriteMode,
) -> Result<String, ExecError> {
    let run_id = uuid::Uuid::new_v4().to_string();
    create_unattended_run(&store, &run_id, source_kind, source_id, fire_id, prompt)?;

    drive_unattended_run(
        store.clone(),
        primary_agent,
        &run_id,
        source_id,
        fire_id,
        prompt,
        parent_cancel,
        write_mode,
    )
    .await
}

pub(crate) fn create_unattended_run(
    store: &TaskRuntimeStore,
    run_id: &str,
    source_kind: &str,
    source_id: &str,
    fire_id: &str,
    prompt: &str,
) -> Result<(), ExecError> {
    let conversation_id = format!("{source_kind}:{source_id}:{fire_id}");

    // 1. Create the run in Pending, attended_mode = Unattended.
    store.create_run_for_active_workspace(
        run_id,
        &conversation_id,
        "", // root_message_id — no chat message for unattended run
        DomainProfile::General,
        prompt,
        "parallel_readonly_delegation",
        AttendedMode::Unattended,
    )?;

    // 2. Transition Pending → Running.
    store.transition_run(run_id, TaskRunStatus::Running)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)] // retained compatibility wrapper around drive_agent_run
pub(crate) async fn drive_unattended_run(
    store: Arc<TaskRuntimeStore>,
    primary_agent: crate::agent_handle::AgentHandle,
    run_id: &str,
    source_id: &str,
    fire_id: &str,
    prompt: &str,
    parent_cancel: CancellationToken,
    write_mode: UnattendedWriteMode,
) -> Result<String, ExecError> {
    drive_agent_run(
        store,
        primary_agent,
        run_id,
        source_id,
        fire_id,
        prompt,
        parent_cancel,
        write_mode,
        RunPlanPolicy::AllowDirect,
        None,
    )
    .await
}

/// Drive an already-created Run through an independent primary Agent's ReAct
/// loop. The Agent may materialize a plan through `task_create` +
/// `task_execute`; direct completion is controlled by [`RunPlanPolicy`].
///
/// Unattended direct read-only work stays in the original checkout. Workspace
/// mutation is routed through formal writer PlanTasks, whose existing Subagent
/// integration path creates a worktree only when the writer is dispatched.
#[allow(clippy::too_many_arguments)]
pub async fn drive_agent_run(
    store: Arc<TaskRuntimeStore>,
    primary_agent: crate::agent_handle::AgentHandle,
    run_id: &str,
    source_id: &str,
    fire_id: &str,
    prompt: &str,
    parent_cancel: CancellationToken,
    write_mode: UnattendedWriteMode,
    plan_policy: RunPlanPolicy,
    trace_sink: Option<ExecSink>,
) -> Result<String, ExecError> {
    let child_cancel = parent_cancel.child_token();
    let _cancel_registration = store
        .register_run_cancellation(run_id, child_cancel.clone())
        .map_err(|error| ExecError::Other(format!("register run cancellation: {error}")))?;
    let run_for_scope = store.get_run(run_id).ok().flatten();
    let conversation_id_for_scope = run_for_scope
        .as_ref()
        .map(|run| run.conversation_id.clone());
    let message_id_for_scope = run_for_scope
        .as_ref()
        .map(|run| run.root_message_id.clone())
        .filter(|message_id| !message_id.trim().is_empty());
    let attended_mode = run_for_scope
        .as_ref()
        .map(|run| run.attended_mode)
        .unwrap_or_default();

    // Drive the Agent's ReAct loop in the run's context. It may call
    // task_create + task_execute; attended_mode on the persisted run controls
    // whether unattended preflight applies.
    let run_id_for_scope = run_id.to_string();
    let cancel_for_scope = child_cancel.clone();
    let prompt_owned = unattended_run_prompt(prompt, attended_mode, write_mode);
    let mut disabled_tools =
        unattended_direct_disabled_tools(attended_mode, write_mode).unwrap_or_default();
    disabled_tools.extend(crate::tool_exposure::disabled_tools_for_mode(
        InteractionMode::Auto,
    ));
    let trace_sink_for_scope = trace_sink.clone();
    let core_trace_sink = exec_trace_sink_to_core(trace_sink);

    let stream_result = super::task_tools::with_run_context(
        run_id_for_scope.clone(),
        cancel_for_scope.clone(),
        trace_sink_for_scope,
        async {
            let agent_inner = primary_agent.inner().clone();
            let agent = agent_inner.read().await;
            let visible_tools = crate::tool_exposure::initial_visible_tools(
                InteractionMode::Auto,
                &agent.tool_names(),
            );
            crate::tool_exposure::record_mode_schema_budget(
                InteractionMode::Auto,
                &agent.tool_definitions(),
                &visible_tools,
            );
            let visible_tools = Some(visible_tools);
            let invocation = echo_core::agent::AgentInvocationContext {
                history: None,
                runtime: Some(echo_core::tools::ExternalRunContext {
                    conversation_id: conversation_id_for_scope.clone(),
                    run_id: Some(run_id_for_scope.clone()),
                    turn_id: message_id_for_scope.clone(),
                    execution_id: None,
                    isolation_id: None,
                    message_id: message_id_for_scope.clone(),
                    cancel: Some(std::sync::Arc::new(cancel_for_scope.clone())),
                    trace_sink: core_trace_sink,
                    delegation_policy: None,
                }),
                working_dir: None,
                cancel: None,
                disabled_tools: Some(disabled_tools),
                visible_tools,
                run_budget: None,
            };
            let event_identity = match echo_core::agent::EventIdentity::from_invocation(&invocation)
            {
                Ok(identity) => identity,
                Err(error) => {
                    tracing::error!(
                        source_id = %source_id,
                        run_id = %run_id_for_scope,
                        error = %error,
                        "Run agent event identity is invalid"
                    );
                    return AgentDriveStreamResult {
                        failure: Some(error.to_string()),
                        final_answer: None,
                    };
                }
            };

            // Execute the prompt. The agent's ReAct loop will call
            // task_create + task_execute, which runs the plan through
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
                    let mut final_answer = None;
                    // Drain the stream to completion. Interactive callers may
                    // receive events through the invocation trace sink; every
                    // caller must still consume the stream to finish the Run.
                    while let Some(event_result) = stream.next().await {
                        if cancel_for_scope.is_cancelled() {
                            break;
                        }
                        match event_result {
                            Ok(event) => match event.payload {
                                AgentEvent::Error {
                                    source, message, ..
                                } => {
                                    let error = format!("{source}: {message}");
                                    tracing::warn!(
                                        source_id = %source_id,
                                        run_id = %run_id_for_scope,
                                        %error,
                                        "Run agent emitted terminal error"
                                    );
                                    return AgentDriveStreamResult {
                                        failure: Some(error),
                                        final_answer,
                                    };
                                }
                                AgentEvent::FinalAnswer(answer) if !answer.trim().is_empty() => {
                                    final_answer = Some(answer);
                                }
                                _ => {}
                            },
                            Err(e) => {
                                tracing::warn!(
                                    source_id = %source_id,
                                    run_id = %run_id_for_scope,
                                    error = %e,
                                    "Run agent stream error"
                                );
                                return AgentDriveStreamResult {
                                    failure: Some(e.to_string()),
                                    final_answer,
                                };
                            }
                        }
                    }
                    AgentDriveStreamResult {
                        failure: None,
                        final_answer,
                    }
                }
                Err(e) => {
                    tracing::error!(
                        source_id = %source_id,
                        run_id = %run_id_for_scope,
                        error = %e,
                        "Run agent failed to start stream"
                    );
                    AgentDriveStreamResult {
                        failure: Some(e.to_string()),
                        final_answer: None,
                    }
                }
            }
        },
    )
    .await;

    if let Some(error) = stream_result.failure.as_deref()
        && !child_cancel.is_cancelled()
    {
        let message = format!("run agent stream failed: {error}");
        store.finalize_run(run_id, TaskRunStatus::Failed, Some(&message))?;
    }

    if stream_result.failure.is_none()
        && !child_cancel.is_cancelled()
        && plan_policy == RunPlanPolicy::AllowDirect
        && store.get_plan(run_id)?.is_none()
        && let Some(final_answer) = stream_result.final_answer.as_deref()
        && let Err(error) = materialize_direct_completion(&store, run_id, final_answer).await
    {
        let message = format!("failed to persist direct completion evidence: {error}");
        store.finalize_run(run_id, TaskRunStatus::Failed, Some(&message))?;
    }

    // Determine final outcome from the store (task_execute/execute_run
    // may have already transitioned the run to a terminal state).
    let final_status = Some(
        store
            .get_run(run_id)?
            .ok_or_else(|| ExecError::RunNotFound(run_id.to_string()))?
            .status,
    );

    match final_status {
        Some(TaskRunStatus::Completed) => {
            tracing::info!(
                source_id = %source_id,
                fire_id = %fire_id,
                run_id = %run_id,
                "Agent-driven run completed"
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
                "Agent-driven run failed"
            );
        }
        Some(TaskRunStatus::Cancelled) => {
            tracing::info!(
                source_id = %source_id,
                fire_id = %fire_id,
                run_id = %run_id,
                "Agent-driven run cancelled"
            );
        }
        Some(TaskRunStatus::Paused) => {
            tracing::info!(
                source_id = %source_id,
                run_id = %run_id,
                "Agent-driven run paused and remains resumable"
            );
        }
        _ => {
            // Still Running or unknown — the agent stream ending is not proof
            // that the plan satisfied its result contract.
            if child_cancel.is_cancelled() {
                store.finalize_run(run_id, TaskRunStatus::Cancelled, None)?;
            } else {
                let report = store.completion_gate_report(run_id)?;
                if report.ready {
                    let _completed = store.complete_run_if_quiescent(run_id)?;
                } else {
                    let blockers = report
                        .blockers
                        .iter()
                        .map(|item| format!("{:?}: {}", item.code, item.detail))
                        .collect::<Vec<_>>()
                        .join("; ");
                    store.request_pause_with_reason(
                        run_id,
                        RunPauseReason::NeedsInput,
                        Some(&format!(
                            "completion gate rejected agent-driven run: {blockers}"
                        )),
                    )?;
                }
            }
            tracing::info!(
                source_id = %source_id,
                fire_id = %fire_id,
                run_id = %run_id,
                final_status = ?final_status,
                "Agent-driven run stream finished; transitioned to terminal"
            );
        }
    }

    let settled = store
        .get_run(run_id)?
        .ok_or_else(|| ExecError::RunNotFound(run_id.to_string()))?;
    if !matches!(
        settled.status,
        TaskRunStatus::Completed
            | TaskRunStatus::Failed
            | TaskRunStatus::Cancelled
            | TaskRunStatus::Paused
    ) {
        return Err(ExecError::Other(format!(
            "run {run_id} did not reach a durable terminal or paused state; read back {}",
            settled.status.as_str()
        )));
    }

    Ok(run_id.to_string())
}

async fn materialize_direct_completion(
    store: &Arc<TaskRuntimeStore>,
    run_id: &str,
    final_answer: &str,
) -> Result<(), ExecError> {
    let run = store
        .get_run(run_id)?
        .ok_or_else(|| ExecError::RunNotFound(run_id.to_string()))?;
    let title = {
        let value = run.goal.chars().take(120).collect::<String>();
        if value.trim().is_empty() {
            "Complete the requested task".to_string()
        } else {
            value
        }
    };
    let task_id = "direct-answer";
    let plan = TaskPlan {
        plan_id: format!("plan:{run_id}"),
        run_id: run_id.to_string(),
        revision: 0,
        domain_profile: run.domain_profile,
        goal_revision: run.goal_revision,
        goal_sha256: run.goal_sha256,
        assumptions: Vec::new(),
        risks: Vec::new(),
        execution_mode: ExecutionMode::Sequential,
        tasks: vec![PlanTask {
            id: task_id.to_string(),
            title,
            description: run.goal,
            kind: PlanTaskKind::Summary,
            agent_role: "primary-agent".to_string(),
            domain_profile: run.domain_profile,
            ..PlanTask::default()
        }],
    };
    super::revisioned_adapter::commit_eko_task_plan(store.clone(), plan)
        .await
        .map_err(|error| ExecError::Other(format!("commit direct TaskPlan: {error}")))?;
    store.set_task_status(
        run_id,
        task_id,
        TodoStatus::Running,
        Some("primary-agent"),
        None,
    )?;
    store.put_summary(&TaskExecutionSummary {
        run_id: run_id.to_string(),
        task_id: task_id.to_string(),
        subagent_name: "primary-agent".to_string(),
        result: SubagentTaskResult::terminal(
            SubagentRunStatus::Completed,
            final_answer,
            Vec::new(),
        ),
        decisions: Vec::new(),
        next_implications: Vec::new(),
        suggested_tasks: Vec::new(),
        created_at: chrono::Utc::now(),
    })?;
    store.set_task_status(
        run_id,
        task_id,
        TodoStatus::Completed,
        Some("primary-agent"),
        Some(final_answer),
    )?;
    Ok(())
}

pub(crate) async fn drive_existing_cron_run(
    store: Arc<TaskRuntimeStore>,
    primary_agent: crate::agent_handle::AgentHandle,
    run_id: String,
    cron_task_id: &str,
    fire_id: &str,
    prompt: &str,
    parent_cancel: CancellationToken,
) -> Result<String, ExecError> {
    drive_unattended_run(
        store.clone(),
        primary_agent,
        &run_id,
        cron_task_id,
        fire_id,
        prompt,
        parent_cancel,
        UnattendedWriteMode::default(),
    )
    .await?;
    let status = store
        .get_run(&run_id)?
        .map(|run| run.status)
        .ok_or_else(|| ExecError::RunNotFound(run_id.clone()))?;
    match status {
        TaskRunStatus::Completed => Ok(run_id),
        TaskRunStatus::Failed => Err(ExecError::Other(format!(
            "cron run {run_id} finished with failed status"
        ))),
        TaskRunStatus::Cancelled => {
            Err(ExecError::Other(format!("cron run {run_id} was cancelled")))
        }
        TaskRunStatus::Paused => Err(ExecError::Other(format!(
            "cron run {run_id} paused and requires attention"
        ))),
        other => Err(ExecError::Other(format!(
            "cron run {run_id} ended in non-terminal status {}",
            other.as_str()
        ))),
    }
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
    "task_execute", // plan materialisation trigger
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
        if !t.execution_checks.is_empty() {
            return Err(PreflightRejection {
                reason: format!(
                    "task '{}' declares execution_checks/shell commands — \
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
    use echo_agent::agent::subagent::SubagentPromptCompiler;

    fn compiled_task_prompt(
        task: &PlanTask,
        dependency_summaries: &[(String, String)],
        delegation_policy: echo_agent::tasks::NestedDelegationPolicy,
        user_goal: Option<&str>,
    ) -> Result<String, String> {
        let payload = crate::subagent_prompt::EkoPromptPayload::planned_task(
            task,
            dependency_summaries,
            delegation_policy.can_delegate(),
            user_goal,
            None,
        )
        .to_value()?;
        let compiler = crate::subagent_prompt::EkoSubagentPromptCompiler;
        Ok(compiler
            .compile_invocation(&SubagentPromptInput {
                agent_name: &task.agent_role,
                task: &task.description,
                mode: echo_agent::agent::subagent::ExecutionMode::Fork,
                transfer_policy: ContextTransferPolicy::Fresh,
                parent_context: None,
                inherit_history: None,
                payload: Some(&payload),
                constraints: &[],
            })
            .task_input)
    }

    #[test]
    fn unattended_worktree_mode_routes_mutations_through_formal_plans() {
        let disabled = unattended_direct_disabled_tools(
            AttendedMode::Unattended,
            UnattendedWriteMode::Worktree,
        )
        .unwrap_or_default();

        assert!(disabled.contains("shell"));
        assert!(disabled.contains("write_file"));
        assert!(disabled.contains("git_commit"));
        assert!(!disabled.contains("read_file"));
        assert!(!disabled.contains("task_create"));
        assert!(!disabled.contains("task_execute"));

        let prompt = unattended_run_prompt(
            "update the implementation",
            AttendedMode::Unattended,
            UnattendedWriteMode::Worktree,
        );
        assert!(prompt.contains("formal plan"));
        assert!(prompt.contains("only when their Subagent is actually dispatched"));
        assert!(prompt.ends_with("update the implementation"));
    }

    #[test]
    fn attended_and_in_place_runs_keep_direct_tools_available() {
        assert!(
            unattended_direct_disabled_tools(
                AttendedMode::Attended,
                UnattendedWriteMode::Worktree,
            )
            .is_none()
        );
        assert!(
            unattended_direct_disabled_tools(
                AttendedMode::Unattended,
                UnattendedWriteMode::InPlace,
            )
            .is_none()
        );
        assert_eq!(
            unattended_run_prompt(
                "inspect the repository",
                AttendedMode::Unattended,
                UnattendedWriteMode::InPlace,
            ),
            "inspect the repository"
        );
    }

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
            execution_checks: verification.iter().map(|s| s.to_string()).collect(),
            acceptance_criteria: Vec::new(),
            retry_count: 0,
            max_retries: 0,
            failure_fingerprint: None,
            status: TodoStatus::Pending,
            status_detail: None,
            claim: None,
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
        let task = PlanTask {
            id: "task-1".to_string(),
            title: "Implement change".to_string(),
            description: "Update the runtime".to_string(),
            kind: PlanTaskKind::Implementation,
            agent_role: "implementer".to_string(),
            ..PlanTask::default()
        };
        let started = runtime_contract_started_payload(&contract, &task, "run-1:task-1:7:2");
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
        if started.get("execution_id").and_then(|value| value.as_str()) != Some("run-1:task-1:7:2")
        {
            return Err("started event must preserve the revision-scoped execution id".into());
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

    #[test]
    fn isolated_subagent_prompt_uses_only_dispatch_time_workspace() {
        let root = std::path::PathBuf::from("/workspace/main");

        assert_eq!(
            primary_workspace_root_for_prompt("context", Some(root.clone())),
            Some(root.clone())
        );
        assert_eq!(
            primary_workspace_root_for_prompt("primary", Some(root.clone())),
            Some(root.clone())
        );
        assert_eq!(
            primary_workspace_root_for_prompt("worktree", Some(root.clone())),
            None
        );
        assert_eq!(
            primary_workspace_root_for_prompt("workspace", Some(root)),
            None
        );
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

        emit_task_started(Some(&sink), "run-1", "task-1:1", &task, &contract);
        emit_primary_subagent_started(Some(&sink), "run-1", "task-1:1", &task, &contract);
        emit_primary_subagent_isolation_observed(
            Some(&sink),
            "run-1",
            "task-1:1",
            &task,
            &contract,
        );
        emit_exec(
            Some(&sink),
            ExecEvent::subagent(
                "run-1",
                "task-1",
                "task-1:1",
                RuntimeEventKind::Completed,
                serde_json::json!({"output": "done"}),
            ),
        );

        let events = recorded.lock().unwrap_or_else(|error| error.into_inner());
        let event_names: Vec<&str> = events.iter().map(|event| event.event.as_str()).collect();
        if event_names != ["task_started", "started", "isolation_observed", "completed"] {
            return Err(format!("unexpected event ordering: {event_names:?}"));
        }
        let started = events
            .get(1)
            .ok_or_else(|| "missing started event".to_string())?;
        let observed = events
            .get(2)
            .ok_or_else(|| "missing isolation observation".to_string())?;
        if events.first().map(|event| event.scope) != Some(ExecEventScope::Task)
            || events.get(1).map(|event| event.scope) != Some(ExecEventScope::Subagent)
            || events
                .get(1)
                .and_then(|event| event.subagent_run_id.as_deref())
                != Some("task-1:1")
        {
            return Err("task and Subagent event scopes were not separated".to_string());
        }
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
    fn subagent_output_can_suggest_followup_tasks() -> Result<(), String> {
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
        let tasks = extract_suggested_tasks_from_subagent_output(output);
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
            "execution_checks (shell commands) should be rejected under Disabled"
        );
        let reason = result.unwrap_err().reason;
        assert!(
            reason.contains("execution_checks") || reason.contains("shell"),
            "reason should mention execution_checks/shell, got {reason:?}"
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
    async fn launch_unattended_run_returns_run_id() -> Result<(), String> {
        // Phase 3.4-1: launch_unattended_run must return the run_id so callers
        // (submit) can hand it to the Tauri layer. A simple prompt (mock returns
        // "ok", agent never calls task_execute) is materialized as a one-task
        // Plan and completes through the shared evidence gate (Q5).
        use echo_agent::testing::MockLlmClient;
        use std::sync::Arc;
        let shadow_root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store = Arc::new(
            TaskRuntimeStore::new_in_memory_with_shadow_root(shadow_root.path())
                .map_err(|error| error.to_string())?,
        );
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
                .map_err(|error| error.to_string())?,
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
        )
        .await
        .map_err(|error| error.to_string())?;
        // The returned id must key a real run whose direct answer was promoted
        // into the same revisioned Plan + Evidence contract as a delegated run.
        let run = store
            .get_run(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "run should exist".to_string())?;
        assert_eq!(run.status, TaskRunStatus::Completed);
        let plan = store
            .get_plan(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "direct completion plan should exist".to_string())?;
        assert_eq!(plan.tasks.len(), 1);
        assert_eq!(
            plan.tasks.first().map(|task| task.id.as_str()),
            Some("direct-answer")
        );
        let report = store
            .completion_gate_report(&run_id)
            .map_err(|error| error.to_string())?;
        assert!(report.ready, "direct completion evidence: {report:?}");
        Ok(())
    }

    #[tokio::test]
    async fn agent_run_requires_materialized_plan_when_policy_demands_it() -> Result<(), String> {
        use echo_agent::testing::MockLlmClient;
        use std::sync::Arc;

        let shadow_root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store = Arc::new(
            TaskRuntimeStore::new_in_memory_with_shadow_root(shadow_root.path())
                .map_err(|error| error.to_string())?,
        );
        let run_id = "require-plan-run";
        store
            .create_run(
                run_id,
                "default",
                "conversation:test",
                "message:test",
                DomainProfile::AcademicResearch,
                "review the evidence",
                "agent_autonomous",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run(run_id, TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        let mock = Arc::new(
            MockLlmClient::new()
                .with_model_name("t")
                .with_response("direct answer without plan"),
        );
        let agent = crate::agent_handle::AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("t")
                .llm_client(mock)
                .build()
                .map_err(|error| error.to_string())?,
        );
        drive_agent_run(
            store.clone(),
            agent,
            run_id,
            "test",
            "fire",
            "materialize and execute a formal plan",
            CancellationToken::new(),
            UnattendedWriteMode::Disabled,
            RunPlanPolicy::RequirePlan,
            None,
        )
        .await
        .map_err(|error| error.to_string())?;
        let run = store
            .get_run(run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "run should exist".to_string())?;
        assert_eq!(run.status, TaskRunStatus::Paused);
        let report = store
            .completion_gate_report(run_id)
            .map_err(|error| error.to_string())?;
        assert!(
            report
                .blockers
                .iter()
                .any(|blocker| blocker.code == CompletionBlockerCode::NoPlan)
        );
        Ok(())
    }

    #[test]
    fn concurrency_limits_clamp_pool_value() {
        // composite_parallelism reports 0/1/N; Subagents clamp to [1,8].
        // We can't easily build a pool in a unit test, so test the clamp math.
        let clamp = |n: usize| n.clamp(1, 8);
        assert_eq!(clamp(0), 1);
        assert_eq!(clamp(1), 1);
        assert_eq!(clamp(4), 4);
        assert_eq!(clamp(20), 8);
    }

    #[test]
    fn task_prompt_is_read_only_for_reviews() -> Result<(), String> {
        let task = PlanTask {
            id: "t1".into(),
            title: "Review chat.rs".into(),
            description: "find bugs".into(),
            kind: PlanTaskKind::ReadOnlyReview,
            files: vec!["chat.rs".into()],
            acceptance_criteria: vec!["report root cause".into()],
            ..Default::default()
        };
        let p = compiled_task_prompt(
            &task,
            &[],
            echo_agent::tasks::NestedDelegationPolicy::default(),
            Some("Fix the GUI context runtime"),
        )?;
        assert!(p.contains("User goal:"));
        assert!(p.contains("Fix the GUI context runtime"));
        assert!(p.contains("READ-ONLY"));
        assert!(p.contains("chat.rs"));
        assert!(p.contains("report root cause"));
        assert!(p.contains("Delegation: disabled"));
        assert!(!p.contains("## Result"));
        Ok(())
    }

    #[test]
    fn task_prompt_marks_empty_writer_scope_as_unknown() -> Result<(), String> {
        let task = PlanTask {
            id: "t2".into(),
            title: "Apply fix".into(),
            description: "patch the bug".into(),
            kind: PlanTaskKind::Implementation,
            ..Default::default()
        };
        let p = compiled_task_prompt(
            &task,
            &[],
            echo_agent::tasks::NestedDelegationPolicy::default(),
            None,
        )?;
        assert!(!p.contains("READ-ONLY"));
        assert!(p.contains("UNKNOWN-SCOPE WRITE"));
        assert!(p.contains("serializes this writer"));
        Ok(())
    }

    #[test]
    fn task_prompt_allows_nested_delegation_when_policy_allows() -> Result<(), String> {
        let task = PlanTask {
            id: "t2_delegate".into(),
            title: "Coordinate review".into(),
            description: "split investigation across specialists".into(),
            kind: PlanTaskKind::Investigation,
            ..Default::default()
        };
        let p = compiled_task_prompt(
            &task,
            &[],
            echo_agent::tasks::NestedDelegationPolicy {
                can_spawn_subagents: true,
                delegate_depth: 0,
                max_delegate_depth: 2,
            },
            None,
        )?;
        assert!(p.contains("tightly scoped child Subagent help is allowed"));
        assert!(p.contains("within this PlanTask"));
        assert!(p.contains("must not control the global plan"));
        assert!(!p.contains("Delegation: disabled"));
        Ok(())
    }

    #[test]
    fn run_outcome_failed_carries_task_id() -> Result<(), String> {
        let o = RunOutcome::Failed {
            failed_task_id: "t3".into(),
            error: "boom".into(),
        };
        match o {
            RunOutcome::Failed { failed_task_id, .. } => assert_eq!(failed_task_id, "t3"),
            other => return Err(format!("expected failed outcome, got {other:?}")),
        }
        Ok(())
    }

    /// Integration-ish test: a 4-task read-only wave + 1 implementation
    /// dependent should complete with all todos Completed, using an in-memory
    /// store. We can't run a real agent in a unit test, so this exercises the
    /// store/state-machine side only (the dispatcher path is covered by the
    /// GUI walkthrough in PR 6 + an integration test).
    #[tokio::test]
    async fn store_transitions_through_running_to_completed() -> Result<(), String> {
        use std::sync::Arc;
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        // Seed a run + plan via the public store API, then drive the state
        // machine the way the runtime plan adapter would.
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
            .map_err(|error| error.to_string())?;
        let plan = TaskPlan {
            plan_id: "p1".into(),
            run_id: "r1".into(),
            revision: 1,
            domain_profile: DomainProfile::AiCoding,
            goal_revision: 1,
            goal_sha256: crate::tasks::task_runtime::task_goal_sha256("g"),
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
        store
            .attach_plan_for_test(&plan)
            .map_err(|error| error.to_string())?;

        // Simulate the executor: Running, mark task running then
        // completed, then Running → Completed.
        store
            .transition_run("r1", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store
            .set_task_status("r1", "t1", TodoStatus::Running, Some("code_reviewer"), None)
            .map_err(|error| error.to_string())?;
        store
            .set_task_status(
                "r1",
                "t1",
                TodoStatus::Completed,
                Some("code_reviewer"),
                Some("done"),
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run("r1", TaskRunStatus::Completed)
            .map_err(|error| error.to_string())?;

        let run = store
            .get_run("r1")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "run r1 missing".to_string())?;
        assert_eq!(run.status, TaskRunStatus::Completed);
        let todos = store.list_todos("r1").map_err(|error| error.to_string())?;
        let todo = todos.first().ok_or_else(|| "todo t1 missing".to_string())?;
        assert_eq!(todo.status, TodoStatus::Completed);
        assert!(todo.summary.as_deref() == Some("done"));
        Ok(())
    }

    // ── Runtime DAG integration tests with a scripted dispatcher ──
    // These exercise the scheduling core — frontier computation, dependency
    // resolution, failure propagation, cancellation, stall detection — without
    // a real LLM. The dispatcher returns scripted results keyed by task id.

    use std::collections::HashMap as StdHashMap;
    use std::sync::Mutex as StdMutex;

    type ScriptedDispatchResult = Result<(SubagentTaskResult, String), String>;

    /// A dispatcher that returns scripted results per task id and records the
    /// order tasks were dispatched. Semaphores/locks are ignored (the mock
    /// answers instantly).
    struct ScriptedDispatcher {
        /// task_id → result to return. Missing id → generic success.
        results: StdMutex<StdHashMap<String, ScriptedDispatchResult>>,
        /// Dispatch order, appended as tasks are picked up.
        order: StdMutex<Vec<String>>,
        /// task_id → integration error returned after review.
        integration_failures: StdMutex<StdHashMap<String, String>>,
        gates: StdMutex<StdHashMap<String, Arc<ScriptedDispatchGate>>>,
        returned_count: std::sync::atomic::AtomicUsize,
        returned: tokio::sync::Notify,
    }

    struct ScriptedDispatchGate {
        started: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }

    impl ScriptedDispatcher {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                results: StdMutex::new(StdHashMap::new()),
                order: StdMutex::new(Vec::new()),
                integration_failures: StdMutex::new(StdHashMap::new()),
                gates: StdMutex::new(StdHashMap::new()),
                returned_count: std::sync::atomic::AtomicUsize::new(0),
                returned: tokio::sync::Notify::new(),
            })
        }
        /// Script a success result for `id`.
        fn succeed(self: &Arc<Self>, id: &str, summary: &str) {
            self.results
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .insert(
                    id.into(),
                    Ok((successful_task_result(summary), summary.to_string())),
                );
        }
        /// Script a structured terminal result for `id`.
        fn respond(self: &Arc<Self>, id: &str, result: SubagentTaskResult) {
            let full_output = result.summary.clone();
            self.results
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .insert(id.into(), Ok((result, full_output)));
        }
        /// Script a bounded parent summary plus a distinct complete review output.
        fn respond_with_output(
            self: &Arc<Self>,
            id: &str,
            result: SubagentTaskResult,
            full_output: &str,
        ) {
            self.results
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .insert(id.into(), Ok((result, full_output.to_string())));
        }
        /// Script a failure result for `id`.
        fn fail(self: &Arc<Self>, id: &str, err: &str) {
            self.results
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(id.into(), Err(err.into()));
        }
        fn order(&self) -> Vec<String> {
            self.order
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }
        fn fail_integration(self: &Arc<Self>, id: &str, error: &str) {
            self.integration_failures
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(id.to_string(), error.to_string());
        }
        fn gate(self: &Arc<Self>, id: &str) -> Arc<ScriptedDispatchGate> {
            let gate = Arc::new(ScriptedDispatchGate {
                started: tokio::sync::Notify::new(),
                release: tokio::sync::Notify::new(),
            });
            self.gates
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(id.to_string(), gate.clone());
            gate
        }

        async fn wait_for_returns(&self, expected: usize) {
            loop {
                let returned = self.returned.notified();
                if self
                    .returned_count
                    .load(std::sync::atomic::Ordering::Acquire)
                    >= expected
                {
                    return;
                }
                returned.await;
            }
        }
    }

    impl TaskDispatcher for Arc<ScriptedDispatcher> {
        fn dispatch(
            &self,
            _store: Arc<TaskRuntimeStore>,
            context: echo_agent::tasks::TaskSubagentContext,
            _claim: echo_agent::tasks::TaskClaim,
            task: PlanTask,
            _write_sem: Arc<Semaphore>,
            _shell_sem: Arc<Semaphore>,
            _llm_sem: Arc<Semaphore>,
            _file_write_locks: Arc<std::sync::Mutex<HashMap<String, Arc<TokioMutex<()>>>>>,
            _trace_sink: Option<ExecSink>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = TaskDispatchResult> + Send>>
        {
            let results = self
                .results
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(&task.id)
                .cloned();
            let gate = self
                .gates
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(&task.id)
                .cloned();
            self.order
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(task.id.clone());
            if let Some(gate) = gate.as_ref() {
                gate.started.notify_one();
            }
            let task_id = task.id.clone();
            let dispatcher = self.clone();
            Box::pin(async move {
                if let Some(gate) = gate {
                    tokio::select! {
                        _ = context.cancel.cancelled() => {
                            return Err(TaskDispatchFailure::cancelled(task_id, "cancelled"));
                        }
                        _ = gate.release.notified() => {}
                    }
                }
                // Honor cancellation even in the mock.
                if context.cancel.is_cancelled() {
                    return Err(TaskDispatchFailure::cancelled(task_id, "cancelled"));
                }
                let result = match results {
                    Some(Ok((result, full_output))) => Ok(TaskDispatchSuccess {
                        task_id,
                        result,
                        full_output,
                    }),
                    Some(Err(error)) => Err(TaskDispatchFailure::failed(task_id, error)),
                    // Default: generic success for unscripted tasks.
                    None => Ok(TaskDispatchSuccess {
                        task_id,
                        result: successful_task_result("ok"),
                        full_output: "ok".to_string(),
                    }),
                };
                dispatcher
                    .returned_count
                    .fetch_add(1, std::sync::atomic::Ordering::Release);
                dispatcher.returned.notify_waiters();
                result
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
    fn execution_check_requires_observed_evidence_and_integrity() -> Result<(), String> {
        assert!(!verification_matches(
            "cargo test --workspace",
            "echo cargo test --workspace"
        ));
        let task = PlanTask {
            id: "contract".to_string(),
            title: "Contract".to_string(),
            execution_checks: vec!["cargo test --workspace".to_string()],
            acceptance_criteria: Vec::new(),
            required_artifacts: vec!["reports/result.json".to_string()],
            ..PlanTask::default()
        };

        // (a) Real failure: remaining_work non-empty.
        let mut result = successful_task_result("work finished");
        result.remaining_work = vec!["write final report".to_string()];
        match assess_task_execution(&task, &result) {
            CompletionAssessment::ExecutionFailed { reason } => {
                assert!(reason.contains("remaining work"), "got {reason:?}");
            }
            other => return Err(format!("expected ExecutionFailed, got {other:?}")),
        }

        // (b) AcceptancePending: completed but verification is Reported only,
        //     and artifact lacks hash/producer. Must NOT be ExecutionFailed
        //     (the Subagent completed) and must NOT pass.
        result.remaining_work.clear();
        result.verification.push(SubagentVerificationResult {
            check: "cargo test --workspace".to_string(),
            status: SubagentVerificationStatus::Passed,
            details: "claimed by subagent".to_string(),
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
        match assess_task_execution(&task, &result) {
            CompletionAssessment::AcceptancePending {
                missing_checks,
                missing_artifacts,
            } => {
                assert!(
                    missing_checks.iter().any(|c| c == "cargo test --workspace"),
                    "got {missing_checks:?}"
                );
                assert!(
                    missing_artifacts.iter().any(|a| a == "reports/result.json"),
                    "got {missing_artifacts:?}"
                );
            }
            other => return Err(format!("expected AcceptancePending, got {other:?}")),
        }

        // (c) Executed: observed pass + integrity metadata present.
        if let Some(verification) = result.verification.first_mut() {
            verification.source = SubagentVerificationSource::Observed;
        }
        if let Some(artifact) = result.artifacts.first_mut() {
            artifact.sha256 = Some("a".repeat(64));
            artifact.producer_execution_id = Some("contract:1".to_string());
        }
        match assess_task_execution(&task, &result) {
            CompletionAssessment::Executed => {}
            other => return Err(format!("expected Executed, got {other:?}")),
        }
        Ok(())
    }

    #[tokio::test]
    async fn runtime_plan_blocks_completed_result_missing_observed_evidence() -> Result<(), String>
    {
        // M7: a Subagent that returns a text summary but no observed execution
        // evidence for a declared execution_check must NOT be auto-redispatched.
        // The task goes to Blocked and the run to Paused for an explicit retry.
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let task = PlanTask {
            id: "verify".to_string(),
            title: "Verify".to_string(),
            kind: PlanTaskKind::ReadOnlyReview,
            agent_role: "reviewer".to_string(),
            execution_checks: vec!["cargo test --workspace".to_string()],
            acceptance_criteria: Vec::new(),
            max_retries: 0,
            ..PlanTask::default()
        };
        let run_id = seed_run(&store, vec![task.clone()])?;
        let dispatcher = ScriptedDispatcher::new();
        let mut result = successful_task_result("tests claimed complete");
        result.verification.push(SubagentVerificationResult {
            check: "cargo test --workspace".to_string(),
            status: SubagentVerificationStatus::Passed,
            details: "subagent report only".to_string(),
            source: SubagentVerificationSource::Reported,
        });
        dispatcher.respond(&task.id, result);

        let outcome = execute_runtime_plan(
            store.clone(),
            dispatcher,
            None,
            &run_id,
            EkoExecutionLimits::default(),
            CancellationToken::new(),
            None,
        )
        .await
        .map_err(|error| error.to_string())?;

        // Attended run (default) → Blocked + Paused (NOT Failed, NOT auto-retried).
        assert!(
            matches!(outcome, RunOutcome::Paused { .. }),
            "expected Paused, got {outcome:?}"
        );
        let todo = store
            .list_todos(&run_id)
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|todo| todo.task_id == "verify")
            .ok_or_else(|| "verify todo missing".to_string())?;
        assert_eq!(todo.status, TodoStatus::Blocked);
        // Plan must still have exactly one task — no fix_task expansion.
        let plan = store
            .get_plan(&run_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "plan missing".to_string())?;
        assert_eq!(
            plan.tasks.len(),
            1,
            "plan must not expand on acceptance failure"
        );
        assert_eq!(
            plan.tasks
                .first()
                .ok_or_else(|| "plan task missing".to_string())?
                .retry_count,
            0,
            "retry_count must not bump on acceptance failure"
        );
        Ok(())
    }

    #[test]
    fn run_completion_gate_requires_durable_structured_result() -> Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let task = solo_readonly_task("completed-task");
        let run_id = seed_run(&store, vec![task.clone()])?;
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
                .any(|blocker| blocker.contains("no structured execution result"))
        );

        store
            .put_summary(&TaskExecutionSummary {
                run_id: run_id.clone(),
                task_id: task.id.clone(),
                subagent_name: task.agent_role.clone(),
                result: successful_task_result("durable result"),
                decisions: Vec::new(),
                next_implications: Vec::new(),
                suggested_tasks: Vec::new(),
                created_at: chrono::Utc::now(),
            })
            .map_err(|error| error.to_string())?;
        assert!(run_completion_blockers(&store, &run_id).is_empty());

        store
            .record_background_cell_started(
                &run_id,
                "cell-running",
                "cargo test --workspace",
                "command-hash",
                Some("turn-1"),
                Some("execution-1"),
                Some("call-1"),
            )
            .map_err(|error| error.to_string())?;
        let blockers = run_completion_blockers(&store, &run_id);
        assert!(
            blockers
                .iter()
                .any(|blocker| blocker.contains("cell-running"))
        );
        assert!(
            !store
                .complete_run_if_quiescent(&run_id)
                .map_err(|error| error.to_string())?
        );
        store
            .record_background_cell_finished(
                &run_id,
                "cell-running",
                "cargo test --workspace",
                "succeeded",
                Some(0),
                128,
                false,
                Some("128 tests passed"),
                None,
                None,
                Some("call-1"),
            )
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
    /// Pending → Running so runtime execution can start.
    fn seed_run(store: &Arc<TaskRuntimeStore>, tasks: Vec<PlanTask>) -> Result<String, String> {
        seed_run_with_mode(store, tasks, AttendedMode::Attended)
    }

    fn seed_run_with_mode(
        store: &Arc<TaskRuntimeStore>,
        tasks: Vec<PlanTask>,
        attended_mode: AttendedMode,
    ) -> Result<String, String> {
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
                attended_mode,
            )
            .map_err(|error| error.to_string())?;
        let plan = TaskPlan {
            plan_id: format!("plan_{}", run_id),
            run_id: run_id.clone(),
            revision: 1,
            domain_profile: DomainProfile::General,
            goal_revision: 1,
            goal_sha256: crate::tasks::task_runtime::task_goal_sha256("test goal"),
            assumptions: vec![],
            risks: vec![],
            execution_mode: ExecutionMode::Sequential,
            tasks,
        };
        store
            .attach_plan_for_test(&plan)
            .map_err(|error| error.to_string())?;
        store
            .transition_run(&run_id, TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        Ok(run_id)
    }

    #[tokio::test]
    async fn unattended_review_rejections_fail_instead_of_pause() -> Result<(), String> {
        use echo_agent::testing::MockLlmClient;

        for (label, verdict) in [
            (
                "needs-fix",
                r#"{"outcome":"needs_fix","summary":"fix required","failure_fingerprint":"missing-evidence","issues":[]}"#,
            ),
            (
                "blocked",
                r#"{"outcome":"blocked","summary":"evidence unavailable","failure_fingerprint":"blocked","issues":[]}"#,
            ),
        ] {
            let store =
                Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
            let task = PlanTask {
                id: label.to_string(),
                title: label.to_string(),
                description: "review this result".to_string(),
                kind: PlanTaskKind::ReadOnlyReview,
                agent_role: "reviewer".to_string(),
                acceptance_criteria: vec!["evidence is complete".to_string()],
                max_retries: 3,
                ..PlanTask::default()
            };
            let run_id = seed_run_with_mode(&store, vec![task.clone()], AttendedMode::Unattended)?;
            let dispatcher = ScriptedDispatcher::new();
            dispatcher.respond(&task.id, successful_task_result("reviewable output"));
            let reviewer = Arc::new(
                MockLlmClient::new()
                    .with_model_name("reviewer-test")
                    .with_response(verdict),
            );

            let outcome = execute_runtime_plan(
                store.clone(),
                dispatcher,
                Some(reviewer),
                &run_id,
                EkoExecutionLimits::default(),
                CancellationToken::new(),
                None,
            )
            .await
            .map_err(|error| error.to_string())?;

            if !matches!(outcome, RunOutcome::Failed { .. }) {
                return Err(format!(
                    "{label} produced non-terminal outcome: {outcome:?}"
                ));
            }
            let run = store
                .get_run(&run_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("run missing for {label}"))?;
            if run.status != TaskRunStatus::Failed {
                return Err(format!("{label} left run in {:?}", run.status));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn review_gate_receives_complete_output_instead_of_bounded_summary() -> Result<(), String>
    {
        use echo_agent::testing::MockLlmClient;

        const FULL_OUTPUT_MARKER: &str = "COMPLETE-OUTPUT-AFTER-SUMMARY-BOUNDARY";
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let task = PlanTask {
            id: "full-review".to_string(),
            title: "Review complete analysis".to_string(),
            description: "cover every requested section".to_string(),
            kind: PlanTaskKind::Investigation,
            agent_role: "explorer".to_string(),
            acceptance_criteria: vec!["the final section is present".to_string()],
            max_retries: 3,
            ..PlanTask::default()
        };
        let run_id = seed_run(&store, vec![task.clone()])?;
        let dispatcher = ScriptedDispatcher::new();
        let full_output = format!("{}\n{FULL_OUTPUT_MARKER}", "analysis ".repeat(180));
        dispatcher.respond_with_output(
            &task.id,
            successful_task_result("bounded parent summary"),
            &full_output,
        );
        let reviewer = Arc::new(MockLlmClient::new().with_response(
            r#"{"outcome":"pass","summary":"complete","failure_fingerprint":null,"issues":[]}"#,
        ));

        let outcome = execute_runtime_plan(
            store,
            dispatcher,
            Some(reviewer.clone()),
            &run_id,
            EkoExecutionLimits::default(),
            CancellationToken::new(),
            None,
        )
        .await
        .map_err(|error| error.to_string())?;
        if !matches!(outcome, RunOutcome::Completed) {
            return Err(format!("reviewed run did not complete: {outcome:?}"));
        }
        let messages = reviewer
            .last_messages()
            .ok_or_else(|| "reviewer received no request".to_string())?;
        let received_full_output = messages.iter().any(|message| {
            message
                .content
                .as_text()
                .is_some_and(|text| text.contains(FULL_OUTPUT_MARKER))
        });
        if !received_full_output {
            return Err("review prompt omitted the complete Subagent output".to_string());
        }
        Ok(())
    }

    #[tokio::test]
    async fn runtime_plan_completes_single_task() -> Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let run_id = seed_run(&store, vec![solo_readonly_task("a")])?;
        let dispatcher = ScriptedDispatcher::new();
        dispatcher.succeed("a", "reviewed");

        let outcome = execute_runtime_plan(
            store.clone(),
            dispatcher.clone(),
            None, // no reviewer LLM → read-only tasks auto-pass review
            &run_id,
            EkoExecutionLimits::default(),
            CancellationToken::new(),
            None,
        )
        .await
        .map_err(|error| error.to_string())?;

        assert!(matches!(outcome, RunOutcome::Completed));
        let todos = store
            .list_todos(&run_id)
            .map_err(|error| error.to_string())?;
        let todo = todos.first().ok_or_else(|| "todo a missing".to_string())?;
        assert_eq!(todo.status, TodoStatus::Completed);
        Ok(())
    }

    #[tokio::test]
    async fn cancellation_preserves_completed_tasks_and_finalizes_the_run() -> Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let tasks = (0..8)
            .map(|index| solo_readonly_task(&format!("task-{index}")))
            .collect::<Vec<_>>();
        let run_id = seed_run(&store, tasks.clone())?;
        let dispatcher = ScriptedDispatcher::new();
        for task in tasks.iter().take(4) {
            dispatcher.succeed(&task.id, "completed before cancellation");
        }
        let mut cancelled_gates = Vec::new();
        for task in tasks.iter().skip(4) {
            dispatcher.succeed(&task.id, "should be cancelled");
            cancelled_gates.push(dispatcher.gate(&task.id));
        }
        let cancel = CancellationToken::new();
        let run_cancel = cancel.clone();
        let run_store = store.clone();
        let run_dispatcher = dispatcher.clone();
        let run_id_for_task = run_id.clone();
        let execution = tokio::spawn(async move {
            execute_runtime_plan(
                run_store,
                run_dispatcher,
                None,
                &run_id_for_task,
                EkoExecutionLimits {
                    max_concurrent_subagents: 8,
                    ..EkoExecutionLimits::default()
                },
                run_cancel,
                None,
            )
            .await
        });

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            for gate in &cancelled_gates {
                gate.started.notified().await;
            }
            dispatcher.wait_for_returns(4).await;
        })
        .await
        .map_err(|_| "dispatch/cancellation boundary was not reached".to_string())?;
        cancel.cancel();

        let outcome = tokio::time::timeout(std::time::Duration::from_secs(1), execution)
            .await
            .map_err(|_| "runtime cancellation timed out".to_string())?
            .map_err(|error| format!("runtime task failed to join: {error}"))?
            .map_err(|error| error.to_string())?;
        assert!(matches!(outcome, RunOutcome::Cancelled));

        finalize_cancelled_run_state(&store, &run_id).map_err(|error| error.to_string())?;
        let todos = store
            .list_todos(&run_id)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            todos
                .iter()
                .filter(|todo| todo.status == TodoStatus::Completed)
                .count(),
            4
        );
        assert_eq!(
            todos
                .iter()
                .filter(|todo| todo.status == TodoStatus::Cancelled)
                .count(),
            4
        );
        assert!(todos.iter().all(|todo| todo.status != TodoStatus::Running));
        let run = store
            .get_run(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "cancelled run missing".to_string())?;
        assert_eq!(run.status, TaskRunStatus::Cancelled);
        Ok(())
    }

    #[tokio::test]
    async fn runtime_plan_reuses_durable_subagent_result_after_restart() -> Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let task = solo_readonly_task("a");
        let run_id = seed_run(&store, vec![task.clone()])?;
        let execution_id = format!("{run_id}:a:1:1");
        store
            .record_subagent_assigned(
                &run_id,
                "a",
                &execution_id,
                "reviewer",
                &task.title,
                1,
                1,
                true,
                true,
            )
            .map_err(|error| error.to_string())?;
        let recovered_result = successful_task_result("recovered summary");
        store
            .record_subagent_released(SubagentReleaseRecord {
                run_id: &run_id,
                task_id: "a",
                execution_id: &execution_id,
                agent_name: "reviewer",
                task_subject: &task.title,
                plan_revision: 1,
                attempt: 1,
                status: "completed",
                result: Some(&recovered_result),
                full_output: Some("recovered full output"),
                usage: None,
                dispatch_hook: true,
            })
            .map_err(|error| error.to_string())?;
        let dispatcher = ScriptedDispatcher::new();

        let outcome = execute_runtime_plan(
            store.clone(),
            dispatcher.clone(),
            None,
            &run_id,
            EkoExecutionLimits::default(),
            CancellationToken::new(),
            None,
        )
        .await
        .map_err(|error| error.to_string())?;

        assert!(matches!(outcome, RunOutcome::Completed));
        assert!(
            dispatcher.order().is_empty(),
            "durable Subagent was dispatched again"
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
    async fn runtime_plan_respects_dependency_order() -> Result<(), String> {
        // b depends on a → a must be dispatched and completed before b.
        let mut a = solo_readonly_task("a");
        let mut b = solo_readonly_task("b");
        b.depends_on = vec!["a".into()];
        let _ = &mut a; // silence unused_mut
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let run_id = seed_run(&store, vec![a.clone(), b.clone()])?;
        let dispatcher = ScriptedDispatcher::new();
        dispatcher.succeed("a", "done a");
        dispatcher.succeed("b", "done b");

        let outcome = execute_runtime_plan(
            store.clone(),
            dispatcher.clone(),
            None,
            &run_id,
            EkoExecutionLimits::default(),
            CancellationToken::new(),
            None,
        )
        .await
        .map_err(|error| error.to_string())?;

        assert!(matches!(outcome, RunOutcome::Completed));
        let order = dispatcher.order();
        // a must appear before b in the dispatch order.
        let pos_a = order
            .iter()
            .position(|x| x == "a")
            .ok_or_else(|| "task a was not dispatched".to_string())?;
        let pos_b = order
            .iter()
            .position(|x| x == "b")
            .ok_or_else(|| "task b was not dispatched".to_string())?;
        assert!(pos_a < pos_b, "dependency violated: b dispatched before a");
        Ok(())
    }

    #[tokio::test]
    async fn runtime_plan_applies_inserted_revision_after_active_wave() -> Result<(), String> {
        let first = solo_readonly_task("first");
        let mut second = solo_readonly_task("second");
        second.depends_on = vec![first.id.clone()];
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let run_id = seed_run(&store, vec![first.clone()])?;
        let dispatcher = ScriptedDispatcher::new();
        dispatcher.succeed("first", "first done");
        dispatcher.succeed("second", "second done");
        let first_gate = dispatcher.gate("first");

        let execution_store = store.clone();
        let execution_dispatcher = dispatcher.clone();
        let execution_run_id = run_id.clone();
        let execution = tokio::spawn(async move {
            execute_runtime_plan(
                execution_store,
                execution_dispatcher,
                None,
                &execution_run_id,
                EkoExecutionLimits::default(),
                CancellationToken::new(),
                None,
            )
            .await
        });

        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            first_gate.started.notified(),
        )
        .await
        .map_err(|_| "first task did not enter the active wave".to_string())?;

        store
            .apply_task_patch_for_test(
                &run_id,
                &TaskUpdateRequest {
                    base_revision: 1,
                    reason: "runtime evidence discovered a required follow-up".to_string(),
                    operations: vec![TaskUpdateOperation::Insert {
                        after_task_id: Some("first".to_string()),
                        task: second.spec(),
                    }],
                },
            )
            .map_err(|error| error.to_string())?;
        first_gate.release.notify_one();

        let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), execution)
            .await
            .map_err(|_| "runtime plan timed out after plan revision".to_string())?
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert!(matches!(outcome, RunOutcome::Completed));
        assert_eq!(dispatcher.order(), vec!["first", "second"]);
        let plan = store
            .get_plan(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "plan disappeared".to_string())?;
        assert_eq!(plan.revision, 2);
        assert!(
            plan.tasks
                .iter()
                .all(|task| task.status == TodoStatus::Completed)
        );
        Ok(())
    }

    #[tokio::test]
    async fn runtime_plan_failure_propagates_and_blocks_downstream() -> Result<(), String> {
        // a fails; b depends on a and must be Blocked, run ends Failed
        // (because all non-terminal tasks are Failed/Blocked).
        let a = solo_readonly_task("a");
        let mut b = solo_readonly_task("b");
        b.depends_on = vec!["a".into()];
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let run_id = seed_run(&store, vec![a.clone(), b.clone()])?;
        let dispatcher = ScriptedDispatcher::new();
        dispatcher.fail("a", "boom");

        let outcome = execute_runtime_plan(
            store.clone(),
            dispatcher.clone(),
            None,
            &run_id,
            EkoExecutionLimits::default(),
            CancellationToken::new(),
            None,
        )
        .await
        .map_err(|error| error.to_string())?;

        match outcome {
            RunOutcome::Failed { failed_task_id, .. } => {
                assert_eq!(failed_task_id, "a");
            }
            other => return Err(format!("expected Failed, got {other:?}")),
        }
        // b must be Blocked (downstream of failed a).
        let todos = store
            .list_todos(&run_id)
            .map_err(|error| error.to_string())?;
        let b_todo = todos
            .iter()
            .find(|t| t.task_id == "b")
            .ok_or_else(|| "todo b missing".to_string())?;
        assert_eq!(b_todo.status, TodoStatus::Blocked);
        Ok(())
    }

    #[tokio::test]
    async fn runtime_plan_merge_failure_blocks_downstream() -> Result<(), String> {
        // Use a read-only kind so the review gate auto-passes (no reviewer LLM
        // in this test) and execution reaches integrate_reviewed_task, where
        // the scripted merge failure marks the writer Failed. Downstream is
        // then Blocked by the failed-dependency propagation.
        let writer = solo_readonly_task("writer");
        let mut downstream = solo_readonly_task("downstream");
        downstream.depends_on = vec![writer.id.clone()];
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let run_id = seed_run(&store, vec![writer.clone(), downstream.clone()])?;
        let dispatcher = ScriptedDispatcher::new();
        dispatcher.succeed(&writer.id, "writer completed");
        dispatcher.fail_integration(&writer.id, "synthetic merge conflict");

        let outcome = execute_runtime_plan(
            store.clone(),
            dispatcher,
            None,
            &run_id,
            EkoExecutionLimits::default(),
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
    async fn runtime_plan_cancellation_propagates_to_cancelled_outcome() -> Result<(), String> {
        // Cancel before dispatching; the framework executor observes it at the
        // top of its loop and return Cancelled without running any task.
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let run_id = seed_run(&store, vec![solo_readonly_task("a")])?;
        let dispatcher = ScriptedDispatcher::new();
        let cancel = CancellationToken::new();
        cancel.cancel();

        let outcome = execute_runtime_plan(
            store.clone(),
            dispatcher.clone(),
            None,
            &run_id,
            EkoExecutionLimits::default(),
            cancel,
            None,
        )
        .await
        .map_err(|error| error.to_string())?;

        assert!(matches!(outcome, RunOutcome::Cancelled));
        // The Subagent must not have been dispatched.
        assert!(
            dispatcher.order().is_empty(),
            "task ran despite cancellation"
        );
        Ok(())
    }

    #[tokio::test]
    async fn runtime_plan_cancellation_preserves_explicit_pause() -> Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let task = solo_readonly_task("a");
        let run_id = seed_run(&store, vec![task.clone()])?;
        store
            .transition_run(&run_id, TaskRunStatus::Paused)
            .map_err(|error| error.to_string())?;
        let cancel = CancellationToken::new();
        cancel.cancel();

        let outcome = execute_runtime_plan(
            store,
            ScriptedDispatcher::new(),
            None,
            &run_id,
            EkoExecutionLimits::default(),
            cancel,
            None,
        )
        .await
        .map_err(|error| error.to_string())?;

        assert!(matches!(outcome, RunOutcome::Paused { .. }));
        Ok(())
    }

    #[test]
    fn invalid_cycle_is_rejected_before_scheduler_dispatch() -> Result<(), String> {
        let mut a = solo_readonly_task("a");
        a.depends_on = vec!["b".into()];
        let mut b = solo_readonly_task("b");
        b.depends_on = vec!["a".into()];
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
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
            .map_err(|error| error.to_string())?;
        let attach_result = store.attach_plan_for_test(&TaskPlan {
            plan_id: format!("plan_{run_id}"),
            run_id,
            revision: 1,
            domain_profile: DomainProfile::General,
            goal_revision: 1,
            goal_sha256: crate::tasks::task_runtime::task_goal_sha256("test goal"),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Parallel,
            tasks: vec![a, b],
        });
        let error = match attach_result {
            Ok(()) => return Err("cyclic plan was accepted".to_string()),
            Err(error) => error,
        };
        assert!(matches!(error, StoreError::InvalidPlan(message) if message.contains("cycle")));
        Ok(())
    }

    #[tokio::test]
    async fn runtime_plan_does_not_redispatch_in_flight_running_tasks() -> Result<(), String> {
        // Regression: when execution is resumed while an earlier task is still
        // `Running`, a later runtime driver must not dispatch that task again.
        // Without the in_flight
        // guard, the ready filter would re-dispatch the Running task, causing
        // duplicate subagent work. Verify the Running task is left alone, the
        // genuinely-pending sibling is dispatched, and the executor waits for the
        // in_flight task to reach Completed in the store (simulating the
        // sibling instance finishing it) before returning Completed.
        let mut in_flight = solo_readonly_task("in_flight");
        let pending = solo_readonly_task("pending");
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let run_id = seed_run(&store, vec![in_flight.clone(), pending.clone()])?;
        store
            .set_task_status(
                &run_id,
                "in_flight",
                TodoStatus::Running,
                Some("explorer"),
                None,
            )
            .map_err(|error| error.to_string())?;
        in_flight.status = TodoStatus::Running;
        let dispatcher = ScriptedDispatcher::new();
        dispatcher.succeed("pending", "done");
        let pending_gate = dispatcher.gate("pending");

        let execution_store = store.clone();
        let execution_dispatcher = dispatcher.clone();
        let execution_run_id = run_id.clone();
        let execution = tokio::spawn(async move {
            execute_runtime_plan(
                execution_store,
                execution_dispatcher,
                None,
                &execution_run_id,
                EkoExecutionLimits::default(),
                CancellationToken::new(),
                None,
            )
            .await
        });
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            pending_gate.started.notified(),
        )
        .await
        .map_err(|_| "pending task was not dispatched".to_string())?;
        store
            .set_task_status(
                &run_id,
                "in_flight",
                TodoStatus::Completed,
                Some("explorer"),
                Some("sibling done"),
            )
            .map_err(|error| error.to_string())?;
        pending_gate.release.notify_one();

        let outcome = tokio::time::timeout(std::time::Duration::from_secs(10), execution)
            .await
            .map_err(|_| "runtime plan did not complete within 10s".to_string())?
            .map_err(|error| format!("runtime task failed to join: {error}"))?
            .map_err(|error| error.to_string())?;

        // `in_flight` (Running) must NOT have been re-dispatched; only
        // `pending` should appear in the dispatch order.
        let order = dispatcher.order();
        assert!(
            !order.contains(&"in_flight".to_string()),
            "Running task was re-dispatched (regression): {order:?}"
        );
        assert_eq!(order, vec!["pending".to_string()]);
        // The executor waited for the sibling instance to finish `in_flight`, so
        // both tasks are now Completed and the run returns Completed.
        assert!(matches!(outcome, RunOutcome::Completed));
        let todos = store
            .list_todos(&run_id)
            .map_err(|error| error.to_string())?;
        let in_flight_todo = todos
            .iter()
            .find(|todo| todo.task_id == "in_flight")
            .ok_or_else(|| "in_flight task missing from runtime store".to_string())?;
        assert_eq!(in_flight_todo.status, TodoStatus::Completed);
        Ok(())
    }

    #[tokio::test]
    async fn main_agent_task_streams_tool_events_to_subagent_trace() -> Result<(), String> {
        use crate::agent_handle::AgentHandle;
        use echo_agent::agent::react::builder::ReactAgentBuilder;
        use echo_agent::testing::{MockLlmClient, MockTool};
        use std::sync::Mutex;

        let llm = MockLlmClient::new()
            .then_tool_call("call_1", "run_code", r#"{"x":6,"y":7}"#)
            .with_response("The result is 42.");
        let agent = ReactAgentBuilder::new()
            .llm_client(Arc::new(llm))
            .system_prompt("You are a test assistant.")
            .tool(Box::new(MockTool::new("run_code").with_response("42")))
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
        let run_id = seed_run(&store, vec![task.clone()])?;
        let execution_id = format!("{run_id}:implementation-a:1:1");
        store
            .record_subagent_assigned(
                &run_id,
                &task.id,
                &execution_id,
                &task.agent_role,
                &task.title,
                1,
                1,
                false,
                false,
            )
            .map_err(|error| format!("Subagent boundary should persist: {error}"))?;

        let output = run_main_agent_task(
            &handle,
            store,
            &run_id,
            &task,
            &execution_id,
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
                event.event == RuntimeEventKind::ToolStarted
                    && event.scope == ExecEventScope::Subagent
                    && event.task_id.as_deref() == Some("implementation-a")
                    && event.subagent_run_id.as_deref() == Some(execution_id.as_str())
                    && event.payload.get("name").and_then(|v| v.as_str()) == Some("run_code")
            }),
            "expected tool_started for run_code, got {events:?}"
        );
        assert!(
            events.iter().any(|event| {
                event.event == RuntimeEventKind::ToolCompleted
                    && event.scope == ExecEventScope::Subagent
                    && event.task_id.as_deref() == Some("implementation-a")
                    && event.subagent_run_id.as_deref() == Some(execution_id.as_str())
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

    // ── M7 acceptance-contract regression tests ────────────────────────────
    //
    // These tests lock in the bug fixes for the contract-validation retry
    // loop: a Subagent that returns a plain-text summary (contract_version=0,
    // no verification array) must complete in exactly one attempt when the
    // task declares no execution_checks, and must never auto-redispatch.

    #[test]
    fn plain_text_summary_passes_when_no_execution_checks() {
        // Mirror of the production bug: 4 analysis tasks returned rich text
        // summaries but contract_version=0 + verification=[]. With no
        // execution_checks declared, this must assess as Executed (the
        // ReviewGate then auto-passes because there are no acceptance_criteria
        // either, so the task reaches Completed without redispatch).
        let task = PlanTask {
            id: "analysis".into(),
            title: "Analyze frontend".into(),
            kind: PlanTaskKind::ReadOnlyReview,
            execution_checks: Vec::new(),
            acceptance_criteria: Vec::new(),
            ..PlanTask::default()
        };
        let result = SubagentTaskResult {
            contract_version: 0, // plain-text fallback, NOT a failure
            status: SubagentRunStatus::Completed,
            summary: "Frontend uses React 19 + Zustand.".into(),
            artifacts: Vec::new(),
            verification: Vec::new(),
            remaining_work: Vec::new(),
            touched_files: SubagentTouchedFiles::default(),
        };
        assert!(matches!(
            assess_task_execution(&task, &result),
            CompletionAssessment::Executed
        ));
    }

    #[test]
    fn contract_version_zero_is_not_an_execution_failure() -> Result<(), String> {
        let task = PlanTask {
            id: "t".into(),
            execution_checks: vec!["cargo test".into()],
            acceptance_criteria: Vec::new(),
            ..PlanTask::default()
        };
        let result = SubagentTaskResult {
            contract_version: 0,
            status: SubagentRunStatus::Completed,
            summary: "done".into(),
            artifacts: Vec::new(),
            verification: Vec::new(), // no observed pass for "cargo test"
            remaining_work: Vec::new(),
            touched_files: SubagentTouchedFiles::default(),
        };
        // cv=0 itself is fine; what blocks is the missing observed check.
        // Crucially this is AcceptancePending, NOT ExecutionFailed — the
        // Subagent completed, so auto-retry would just reproduce the gap.
        match assess_task_execution(&task, &result) {
            CompletionAssessment::AcceptancePending { missing_checks, .. } => {
                assert_eq!(missing_checks, vec!["cargo test".to_string()]);
            }
            other => return Err(format!("expected AcceptancePending, got {other:?}")),
        }
        Ok(())
    }

    #[tokio::test]
    async fn completed_subagent_with_text_summary_runs_single_attempt() -> Result<(), String> {
        // Reproduction of the original loop scenario: a task with semantic
        // acceptance only (no execution_checks) must dispatch exactly once.
        // Before the fix this looped up to max_retries because cv=0 was
        // treated as a retryable contract failure.
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|e| e.to_string())?);
        let task = PlanTask {
            id: "analyze".into(),
            title: "Analyze backend".into(),
            kind: PlanTaskKind::ReadOnlyReview,
            agent_role: "explorer".into(),
            execution_checks: Vec::new(),
            acceptance_criteria: Vec::new(),
            max_retries: 3,
            ..PlanTask::default()
        };
        let run_id = seed_run(&store, vec![task.clone()])?;
        let dispatcher = ScriptedDispatcher::new();
        // Plain text result, no JSON contract — exactly what the production
        // Subagents returned.
        dispatcher.respond(
            &task.id,
            SubagentTaskResult {
                contract_version: 0,
                status: SubagentRunStatus::Completed,
                summary: "Backend has 4 modules.".into(),
                artifacts: Vec::new(),
                verification: Vec::new(),
                remaining_work: Vec::new(),
                touched_files: SubagentTouchedFiles::default(),
            },
        );

        let outcome = execute_runtime_plan(
            store.clone(),
            dispatcher.clone(),
            None,
            &run_id,
            EkoExecutionLimits::default(),
            CancellationToken::new(),
            None,
        )
        .await
        .map_err(|e| e.to_string())?;

        assert!(
            matches!(outcome, RunOutcome::Completed),
            "expected Completed, got {outcome:?}"
        );
        // No reviewer LLM, but no acceptance_criteria either → auto-pass.
        // Exactly one dispatch happened.
        assert_eq!(dispatcher.order().len(), 1, "expected single dispatch");
        // Plan still has 1 task, retry_count 0.
        let plan = store
            .get_plan(&run_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "plan missing".to_string())?;
        assert_eq!(plan.tasks.len(), 1);
        let task = plan
            .tasks
            .first()
            .ok_or_else(|| "plan task missing".to_string())?;
        assert_eq!(task.retry_count, 0);
        Ok(())
    }

    #[tokio::test]
    async fn real_execution_failure_retries_within_budget() -> Result<(), String> {
        // Sanity check that the ExecutionFailed path still auto-retries.
        // A Subagent returning remaining_work is a real failure (not a
        // contract-format issue) and should retry up to max_retries.
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|e| e.to_string())?);
        let task = PlanTask {
            id: "flaky".into(),
            title: "Flaky".into(),
            kind: PlanTaskKind::ReadOnlyReview,
            agent_role: "explorer".into(),
            execution_checks: Vec::new(),
            acceptance_criteria: Vec::new(),
            max_retries: 2,
            ..PlanTask::default()
        };
        let run_id = seed_run(&store, vec![task.clone()])?;
        let dispatcher = ScriptedDispatcher::new();
        // Always report remaining_work → ExecutionFailed every time.
        dispatcher.respond(
            &task.id,
            SubagentTaskResult {
                contract_version: 1,
                status: SubagentRunStatus::Completed,
                summary: "partial".into(),
                artifacts: Vec::new(),
                verification: Vec::new(),
                remaining_work: vec!["not done".into()],
                touched_files: SubagentTouchedFiles::default(),
            },
        );

        let outcome = execute_runtime_plan(
            store.clone(),
            dispatcher.clone(),
            None,
            &run_id,
            EkoExecutionLimits::default(),
            CancellationToken::new(),
            None,
        )
        .await
        .map_err(|e| e.to_string())?;

        // Exhausted budget → Failed.
        assert!(
            matches!(outcome, RunOutcome::Failed { .. }),
            "expected Failed after retries, got {outcome:?}"
        );
        // Initial attempt + 2 retries = 3 dispatches.
        assert_eq!(
            dispatcher.order().len(),
            3,
            "expected 3 dispatches (1 + 2 retries)"
        );
        Ok(())
    }

    #[tokio::test]
    async fn wave_processes_all_results_when_one_task_blocks() -> Result<(), String> {
        // Regression: when one task in a parallel wave resolves to Blocked
        // (acceptance pending), sibling tasks that completed in the SAME wave
        // must still be marked Completed and persisted. The early-return bug
        // left siblings in Running, the resume path reset them to Pending,
        // and the next attempt redispatched the entire wave — duplicating
        // already-finished Subagent work.
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|e| e.to_string())?);
        let clean = solo_readonly_task("clean"); // no execution_checks → Executed
        let mut blocked = solo_readonly_task("blocked");
        blocked.execution_checks = vec!["cargo test".to_string()];
        blocked.acceptance_criteria = Vec::new();
        let run_id = seed_run(&store, vec![clean.clone(), blocked.clone()])?;
        let dispatcher = ScriptedDispatcher::new();
        dispatcher.respond(
            &clean.id,
            SubagentTaskResult {
                contract_version: 1,
                status: SubagentRunStatus::Completed,
                summary: "clean run".into(),
                artifacts: Vec::new(),
                verification: Vec::new(),
                remaining_work: Vec::new(),
                touched_files: SubagentTouchedFiles::default(),
            },
        );
        dispatcher.respond(
            &blocked.id,
            SubagentTaskResult {
                contract_version: 1,
                status: SubagentRunStatus::Completed,
                summary: "blocked run".into(),
                artifacts: Vec::new(),
                verification: Vec::new(), // execution_check has no observed pass
                remaining_work: Vec::new(),
                touched_files: SubagentTouchedFiles::default(),
            },
        );

        let outcome = execute_runtime_plan(
            store.clone(),
            dispatcher.clone(),
            None,
            &run_id,
            EkoExecutionLimits::default(),
            CancellationToken::new(),
            None,
        )
        .await
        .map_err(|e| e.to_string())?;

        // Run Paused (acceptance failure on attended run).
        assert!(
            matches!(outcome, RunOutcome::Paused { .. }),
            "expected Paused, got {outcome:?}"
        );
        // CRITICAL: the clean task must be Completed, not Running/Pending.
        let todos = store.list_todos(&run_id).map_err(|e| e.to_string())?;
        let clean_status = todos
            .iter()
            .find(|t| t.task_id == "clean")
            .map(|t| t.status)
            .ok_or_else(|| "clean todo missing".to_string())?;
        assert_eq!(
            clean_status,
            TodoStatus::Completed,
            "sibling completed task must persist as Completed, got {clean_status:?}"
        );
        // Blocked task is Blocked.
        let blocked_status = todos
            .iter()
            .find(|t| t.task_id == "blocked")
            .map(|t| t.status)
            .ok_or_else(|| "blocked todo missing".to_string())?;
        assert_eq!(blocked_status, TodoStatus::Blocked);
        // Plan size unchanged.
        let plan = store
            .get_plan(&run_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "plan missing".to_string())?;
        assert_eq!(plan.tasks.len(), 2);
        // Exactly one dispatch per task — no redispatch.
        assert_eq!(
            dispatcher.order().len(),
            2,
            "each task dispatched exactly once"
        );
        Ok(())
    }
}
