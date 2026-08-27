//! EKO adapter for the framework runtime task service.
//!
//! Converts EKO `TaskPlan` snapshots into the framework's product-neutral task
//! view, then injects EKO dispatch, review, persistence, worktree, and event
//! policy. Dependency traversal, revision safe points, Subagent waves,
//! cancellation, failure propagation, and stall detection live in
//! `echo_orchestration::tasks::RuntimeTaskService`.
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
use echo_agent::runtime::{
    AgentTurnDriver, EventSink, SinkControl, TurnMode, TurnOutcome, TurnReceipt, TurnRequest,
};
use tokio::sync::{Mutex as TokioMutex, OwnedMutexGuard, Semaphore};

/// Process-wide EKO resource ceiling shared by every workspace and TaskRun.
/// Per-run limits still apply; a dispatch must hold both permits, so opening
/// more workspace hosts cannot multiply provider or machine concurrency.
pub(crate) struct ProcessExecutionGovernor {
    subagent: Arc<Semaphore>,
    write: Arc<Semaphore>,
    shell: Arc<Semaphore>,
    llm: Arc<Semaphore>,
}

static PROCESS_EXECUTION_GOVERNOR: std::sync::LazyLock<Arc<ProcessExecutionGovernor>> =
    std::sync::LazyLock::new(|| {
        let limits = EkoExecutionLimits::default();
        Arc::new(ProcessExecutionGovernor {
            subagent: Arc::new(Semaphore::new(limits.max_concurrent_subagents.max(1))),
            write: Arc::new(Semaphore::new(limits.max_concurrent_writes.max(1))),
            shell: Arc::new(Semaphore::new(limits.max_concurrent_shells.max(1))),
            llm: Arc::new(Semaphore::new(limits.max_parallel_llm_calls.max(1))),
        })
    });

pub(crate) fn process_execution_governor() -> Arc<ProcessExecutionGovernor> {
    PROCESS_EXECUTION_GOVERNOR.clone()
}

impl ProcessExecutionGovernor {
    pub(crate) fn shell_semaphore(&self) -> Arc<Semaphore> {
        self.shell.clone()
    }

    pub(crate) fn subagent_semaphore(&self) -> Arc<Semaphore> {
        self.subagent.clone()
    }

    fn snapshot(&self) -> ProcessExecutionResourceSnapshot {
        let limits = EkoExecutionLimits::default();
        ProcessExecutionResourceSnapshot {
            subagent_active: limits
                .max_concurrent_subagents
                .saturating_sub(self.subagent.available_permits()),
            subagent_limit: limits.max_concurrent_subagents,
            write_active: limits
                .max_concurrent_writes
                .saturating_sub(self.write.available_permits()),
            write_limit: limits.max_concurrent_writes,
            shell_active: limits
                .max_concurrent_shells
                .saturating_sub(self.shell.available_permits()),
            shell_limit: limits.max_concurrent_shells,
            llm_active: limits
                .max_parallel_llm_calls
                .saturating_sub(self.llm.available_permits()),
            llm_limit: limits.max_parallel_llm_calls,
        }
    }
}

use super::completion_gate::{artifact_matches, verification_matches};
use super::store::{
    RuntimeTaskProductSettlement, StoreError, SubagentReleaseRecord, TaskRuntimeStore,
};
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

/// Content-free process resource counters for diagnostics and soak evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProcessExecutionResourceSnapshot {
    pub subagent_active: usize,
    pub subagent_limit: usize,
    pub write_active: usize,
    pub write_limit: usize,
    pub shell_active: usize,
    pub shell_limit: usize,
    pub llm_active: usize,
    pub llm_limit: usize,
}

impl ProcessExecutionResourceSnapshot {
    pub fn within_limits(self) -> bool {
        self.subagent_active <= self.subagent_limit
            && self.write_active <= self.write_limit
            && self.shell_active <= self.shell_limit
            && self.llm_active <= self.llm_limit
    }
}

pub fn process_execution_resource_snapshot() -> ProcessExecutionResourceSnapshot {
    PROCESS_EXECUTION_GOVERNOR.snapshot()
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
    pub workspace_id: String,
    pub conversation_id: String,
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
        workspace_id: impl Into<String>,
        conversation_id: impl Into<String>,
        run_id: impl Into<String>,
        event: RuntimeEventKind,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            workspace_id: workspace_id.into(),
            conversation_id: conversation_id.into(),
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
        workspace_id: impl Into<String>,
        conversation_id: impl Into<String>,
        run_id: impl Into<String>,
        task_id: impl Into<String>,
        event: RuntimeEventKind,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            workspace_id: workspace_id.into(),
            conversation_id: conversation_id.into(),
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
        workspace_id: impl Into<String>,
        conversation_id: impl Into<String>,
        run_id: impl Into<String>,
        task_id: impl Into<String>,
        subagent_run_id: impl Into<String>,
        event: RuntimeEventKind,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            workspace_id: workspace_id.into(),
            conversation_id: conversation_id.into(),
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
        failed_task_id: Option<String>,
        error: String,
    },
    Cancelled,
    /// Execution paused for a user or product decision. The optional task id
    /// identifies the direct cause; dependency blockers remain a derived DAG
    /// projection rather than persisted descendant status.
    Paused {
        failed_task_id: Option<String>,
        error: String,
    },
}

pub struct PlannedRunResumeLaunch {
    pub run_id: String,
    completion: tokio::sync::oneshot::Receiver<Result<RunOutcome, String>>,
}

impl PlannedRunResumeLaunch {
    pub async fn wait(self) -> Result<RunOutcome, String> {
        self.completion
            .await
            .map_err(|error| format!("planned resume completion channel closed: {error}"))?
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn launch_planned_run_resume(
    store: Arc<TaskRuntimeStore>,
    expected: TaskRunResumeIdentity,
    primary_agent: crate::agent_handle::AgentHandle,
    pool_execution: Option<crate::agent_pool::AgentPoolExecutionLease>,
    review_integration: Option<Arc<crate::evolution::ReviewIntegration>>,
    trace_sink: Option<ExecSink>,
    cancel: CancellationToken,
    workspace_io: Option<crate::state::WorkspaceIoInvocation>,
) -> Result<PlannedRunResumeLaunch, StoreError> {
    let run_id = expected.run_id.clone();
    let admission = store.reserve_run_driver_admission(run_id.clone(), cancel.clone())?;
    let generation_lease = store.lease_active_workspace_generation()?;
    let registration = store.register_run_driver::<RunOutcome>(admission, generation_lease)?;
    TaskRuntimeBlockingAdapter::new(store.clone())
        .run_owned("prepare exact planned resume", move || {
            let mut registration = registration;
            let memory_generation = match review_integration
                .as_ref()
                .map(|integration| integration.lease_generation())
                .transpose()
            {
                Ok(generation) => generation,
                Err(error) => {
                    let error =
                        StoreError::InvalidPlan(format!("memory generation unavailable: {error}"));
                    registration.reject(error.to_string());
                    return Err(error);
                }
            };
            let layer_manager = match memory_generation
                .as_ref()
                .map(|generation| generation.create_layer_manager().map(Arc::new))
                .transpose()
            {
                Ok(manager) => manager,
                Err(error) => {
                    let error =
                        StoreError::InvalidPlan(format!("layered memory unavailable: {error}"));
                    registration.reject(error.to_string());
                    return Err(error);
                }
            };
            if store.get_plan(&run_id)?.is_none() {
                let error = StoreError::InvalidPlan(format!(
                    "run {run_id} has no persisted plan to resume"
                ));
                registration.reject(error.to_string());
                return Err(error);
            }
            if let Err(error) = store.resume_task_run_expected(&expected) {
                let detail = error.to_string();
                if matches!(error, StoreError::ResumeOutcomeUnknown { .. }) {
                    registration.fail_preparation(detail);
                } else {
                    registration.reject(detail);
                }
                return Err(error);
            }
            registration.mark_preparation_started();
            let preparation_store = store.clone();
            let preparation_run_id = run_id.clone();
            let completion = registration.start(
                move |mut receipt_owner: super::store::RunDriverReceiptOwner| async move {
                    if let Some(generation) = memory_generation.as_ref() {
                        receipt_owner.retain(generation.clone());
                    }
                    if let Some(execution) = pool_execution {
                        receipt_owner.retain(execution);
                    }
                    let run_store = primary_agent.read(|agent| agent.run_store().cloned()).await;
                    let reviewer_llm = primary_agent
                        .read(|agent| agent.llm_client().cloned())
                        .await;
                    execute_run(
                        preparation_store,
                        Some(primary_agent),
                        reviewer_llm,
                        layer_manager,
                        memory_generation,
                        run_store,
                        trace_sink,
                        &preparation_run_id,
                        cancel,
                        super::memory_bridge::MemoryPolicy::BestEffortSettled,
                        workspace_io,
                    )
                    .await
                    .map_err(|error| error.to_string())
                },
            );
            Ok(PlannedRunResumeLaunch { run_id, completion })
        })
        .await
}

/// Whether an Agent-driven Run must materialize a formal plan before it may
/// complete. This is prompt/execution policy, not a TaskRun lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunPlanPolicy {
    RequirePlan,
    AllowDirect,
}

/// Workspace-mutating tools that an unattended primary Agent must not call
/// directly unless the user explicitly selected `InPlace` mode.
///
/// Worktree isolation is owned by formal writer PlanTasks: their Subagent
/// worktree is created only when the writer is dispatched, then integrated by
/// the existing review/integration stage. Hiding these tools prevents a second
/// run-level worktree mechanism from being required around the planning Agent.
const UNATTENDED_DIRECT_MUTATION_TOOLS: [&str; 12] = [
    "agent_tool",
    "shell",
    "run_code",
    "apply_patch",
    "git_branch",
    "git_commit",
    "enter_worktree",
    "exit_worktree",
    "write_excel",
    "export_data",
    "export_text",
    "create_complex_task",
];

fn direct_mutation_disabled_tools(
    attended_mode: AttendedMode,
    write_mode: UnattendedWriteMode,
) -> Option<HashSet<String>> {
    let _ = attended_mode;
    if write_mode == UnattendedWriteMode::InPlace {
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
    workspace_io: Option<crate::state::WorkspaceIoInvocation>,
) -> Result<RunOutcome, ExecError> {
    let blocking = TaskRuntimeBlockingAdapter::new(store.clone());
    let initial_run_id = run_id.to_string();
    let (run, initial_plan) = blocking
        .run("load runtime execution admission", move |store| {
            let run = store
                .get_run(&initial_run_id)?
                .ok_or_else(|| StoreError::RunNotFound(initial_run_id.clone()))?;
            let plan = store
                .get_plan(&initial_run_id)?
                .ok_or(StoreError::PlanNotFound(initial_run_id))?;
            Ok((run, plan))
        })
        .await
        .map_err(|error| ExecError::Other(error.to_string()))?;
    // The caller must have transitioned Pending → Running before spawning
    // the executor. Here we only accept Running.
    if run.status != TaskRunStatus::Running {
        return Err(ExecError::NotRunning(run_id.to_string(), run.status));
    }
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
            run.workspace_id.clone(),
            run.conversation_id.clone(),
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
        let plan_run_id = run_id.to_string();
        let plan = blocking
            .run("load runtime drain plan", move |store| {
                store
                    .get_plan(&plan_run_id)?
                    .ok_or(StoreError::PlanNotFound(plan_run_id))
            })
            .await
            .map_err(|error| ExecError::Other(error.to_string()))?;
        let unresolved_count = plan
            .tasks
            .iter()
            .filter(|task| !task.status.is_terminal())
            .count();
        if unresolved_count == 0 {
            let report_run_id = run_id.to_string();
            let report = blocking
                .run("load runtime completion gate", move |store| {
                    store.completion_gate_report(&report_run_id)
                })
                .await
                .map_err(|error| ExecError::Other(error.to_string()))?;
            if !report.ready {
                let error = report
                    .blockers
                    .iter()
                    .map(|item| format!("{:?}: {}", item.code, item.detail))
                    .collect::<Vec<_>>()
                    .join("; ");
                let pause_run_id = run_id.to_string();
                let pause_error = error.clone();
                blocking
                    .run("pause rejected runtime completion", move |store| {
                        store
                            .request_pause_with_reason(
                                &pause_run_id,
                                RunPauseReason::NeedsInput,
                                Some(&pause_error),
                            )
                            .map(|_| ())
                    })
                    .await
                    .map_err(|error| ExecError::Other(error.to_string()))?;
                break Ok(RunOutcome::Paused {
                    failed_task_id: None,
                    error,
                });
            }
            let complete_run_id = run_id.to_string();
            if blocking
                .run("commit quiescent runtime completion", move |store| {
                    store.complete_run_if_quiescent(&complete_run_id)
                })
                .await
                .map_err(|error| ExecError::Other(error.to_string()))?
            {
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
                workspace_io: workspace_io.clone(),
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
            let status_run_id = run_id.to_string();
            let goal_completed = blocking
                .run("inspect runtime Goal completion", move |store| {
                    store
                        .get_run(&status_run_id)?
                        .map(|run| run.status == TaskRunStatus::Completed)
                        .ok_or(StoreError::RunNotFound(status_run_id))
                })
                .await
                .map_err(|error| ExecError::Other(error.to_string()))?;
            if !goal_completed {
                // The active RunTurn owns the atomic RunTurnFinished + Goal
                // completion batch and publishes the terminal projection.
                return outcome;
            }
            emit_exec(
                trace_sink.as_ref(),
                ExecEvent::run(
                    run.workspace_id.clone(),
                    run.conversation_id.clone(),
                    run_id.to_string(),
                    RuntimeEventKind::RunCompleted,
                    serde_json::json!({ "status": "completed" }),
                ),
            );
            // With an active primary RunTurn, Goal completion is committed by
            // turn_lifecycle in the same batch as RunTurnFinished. Without an
            // active turn, complete_run_if_quiescent committed it above.
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
                    run.workspace_id.clone(),
                    run.conversation_id.clone(),
                    run_id.to_string(),
                    RuntimeEventKind::RunFailed,
                    serde_json::json!({
                        "failed_task_id": failed_task_id,
                        "error": error,
                    }),
                ),
            );
            let final_run_id = run_id.to_string();
            let final_task_id = failed_task_id.clone();
            let final_error = format!("run failed: {error}");
            blocking
                .run("finalize failed runtime run", move |store| {
                    store
                        .finalize_run_with_note_task(
                            &final_run_id,
                            TaskRunStatus::Failed,
                            final_task_id.as_deref(),
                            Some(&final_error),
                        )
                        .map(|_| ())
                })
                .await
                .map_err(|error| ExecError::Other(error.to_string()))?;
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
                    run.workspace_id.clone(),
                    run.conversation_id.clone(),
                    run_id.to_string(),
                    RuntimeEventKind::RunCancelled,
                    serde_json::json!({ "status": "cancelled" }),
                ),
            );
            let final_run_id = run_id.to_string();
            blocking
                .run("finalize cancelled runtime run", move |store| {
                    store
                        .finalize_run(&final_run_id, TaskRunStatus::Cancelled, None)
                        .map(|_| ())
                })
                .await
                .map_err(|error| ExecError::Other(error.to_string()))?;
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
                    run.workspace_id.clone(),
                    run.conversation_id.clone(),
                    run_id.to_string(),
                    RuntimeEventKind::RunStatusChanged,
                    serde_json::json!({
                        "status": "paused",
                        "failed_task_id": failed_task_id,
                        "error": error,
                    }),
                ),
            );
            let note_run_id = run_id.to_string();
            let note_task_id = failed_task_id.clone();
            let note = format!("run paused: {error}");
            blocking
                .run("note paused runtime run", move |store| {
                    store.note(&note_run_id, note_task_id.as_deref(), &note)
                })
                .await
                .map_err(|error| ExecError::Other(error.to_string()))?;
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
                    run.workspace_id.clone(),
                    run.conversation_id.clone(),
                    run_id.to_string(),
                    RuntimeEventKind::RunFailed,
                    serde_json::json!({ "error": e.to_string() }),
                ),
            );
            let final_run_id = run_id.to_string();
            let final_error = format!("executor error: {e}");
            blocking
                .run("finalize runtime executor error", move |store| {
                    store
                        .finalize_run(&final_run_id, TaskRunStatus::Failed, Some(&final_error))
                        .map(|_| ())
                })
                .await
                .map_err(|error| ExecError::Other(error.to_string()))?;
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
/// The dispatcher is given EKO-specific per-run semaphores and file locks.
/// EKO additionally holds one process-wide permit across all workspace runs.
trait TaskDispatcher: Send + Sync {
    /// Execute `task` for `run_id`. Success carries both the bounded structured
    /// result and the complete model output. The former feeds parent summaries;
    /// the latter is the evidence reviewed against acceptance criteria.
    #[allow(clippy::too_many_arguments, clippy::type_complexity)] // product resource limits + locks are the application dispatch contract
    fn dispatch(
        &self,
        store: Arc<TaskRuntimeStore>,
        blocking: TaskRuntimeBlockingAdapter,
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
        _blocking: TaskRuntimeBlockingAdapter,
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

/// Production dispatcher: delegates to [`execute_task`] against the task's
/// local or frozen cross-workspace Agent target.
///
/// Review remains in the EKO runtime controller after a Subagent returns. The
/// dispatcher only needs the Agent and product-specific concurrency primitives.
struct RealTaskDispatcher {
    primary_agent: crate::agent_handle::AgentHandle,
    workspace_io: Option<crate::state::WorkspaceIoInvocation>,
}

async fn resolve_task_execution_agent(
    store: &TaskRuntimeStore,
    blocking: &TaskRuntimeBlockingAdapter,
    run_id: &str,
    task: &PlanTask,
    local_agent: crate::agent_handle::AgentHandle,
) -> Result<
    (
        crate::agent_handle::AgentHandle,
        Option<crate::agent_pool::AgentPoolExecutionLease>,
    ),
    String,
> {
    let Some(target) = task.execution_target.as_ref() else {
        return Ok((local_agent, None));
    };
    if target.subagent_role != task.agent_role {
        return Err(format!(
            "task '{}' target role '{}' does not match Subagent role '{}'",
            task.id, target.subagent_role, task.agent_role
        ));
    }
    let load_run_id = run_id.to_string();
    let run = blocking
        .run("load task execution target run", move |store| {
            store
                .get_run(&load_run_id)?
                .ok_or(StoreError::RunNotFound(load_run_id))
        })
        .await
        .map_err(|error| error.to_string())?;
    let leader = crate::agent_router::AgentAddress::new(
        crate::workspace::WorkspaceId::from_raw(run.workspace_id),
        run.conversation_id,
    );
    let resolver = store.execution_target_resolver().ok_or_else(|| {
        format!(
            "task '{}' targets Agent group '{}' but no cross-workspace resolver is installed",
            task.id, target.group_id
        )
    })?;
    let lease = resolver.acquire(&leader, target).await?;
    let agent = lease.agent();
    Ok((agent, Some(lease)))
}

impl TaskDispatcher for RealTaskDispatcher {
    fn dispatch(
        &self,
        store: Arc<TaskRuntimeStore>,
        blocking: TaskRuntimeBlockingAdapter,
        context: echo_agent::tasks::TaskSubagentContext,
        claim: echo_agent::tasks::TaskClaim,
        task: PlanTask,
        write_sem: Arc<Semaphore>,
        shell_sem: Arc<Semaphore>,
        llm_sem: Arc<Semaphore>,
        file_write_locks: Arc<std::sync::Mutex<HashMap<String, Arc<TokioMutex<()>>>>>,
        trace_sink: Option<ExecSink>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = TaskDispatchResult> + Send>> {
        let local_agent = self.primary_agent.clone();
        let workspace_io = self.workspace_io.clone();
        Box::pin(async move {
            let run_id = context.run_id;
            let cancel = context.cancel;
            let delegation_policy = context.delegation_policy;
            let task_id = task.id.clone();
            let _process_subagent_permit = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(TaskDispatchFailure::cancelled(task_id.clone(), "cancelled while waiting for process Subagent permit")),
                permit = PROCESS_EXECUTION_GOVERNOR.subagent.acquire() => permit.map_err(|error| TaskDispatchFailure::failed(task_id.clone(), error.to_string()))?,
            };
            let (execution_agent, target_lease) =
                resolve_task_execution_agent(&store, &blocking, &run_id, &task, local_agent)
                    .await
                    .map_err(|error| TaskDispatchFailure::failed(task_id, error))?;
            // A cross-workspace target needs its own target-runtime receipt.
            // Never reuse the leader workspace authority for that Agent.
            let workspace_io = target_lease.is_none().then_some(workspace_io).flatten();
            // Scope run_id + cancel + trace_sink into task-local so Subagent-internal
            // tools (task_*/task_execute, and their execute_with_context
            // fallback path) and L3 nested Subagents can read them.
            // NOTE: trace_sink/cancel are also passed as explicit params to
            // execute_task (which uses them directly, not via task_local) — but
            // scoping them here keeps the task_local consistent for any code
            // path that reads CURRENT_TRACE_SINK/CURRENT_CANCEL directly.
            let sink_clone = trace_sink.clone();
            let cancel_clone = cancel.clone();
            let result = super::task_tools::with_run_context(
                run_id.clone(),
                cancel_clone,
                sink_clone,
                async {
                    execute_task(
                        store,
                        blocking,
                        execution_agent,
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
                        workspace_io,
                    )
                    .await
                },
            )
            .await;
            drop(target_lease);
            result
        })
    }

    fn integrate(
        &self,
        store: Arc<TaskRuntimeStore>,
        blocking: TaskRuntimeBlockingAdapter,
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
        let local_agent = self.primary_agent.clone();
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

            let (execution_agent, target_lease) =
                resolve_task_execution_agent(&store, &blocking, &run_id, &task, local_agent)
                    .await?;
            let load_run_id = run_id.clone();
            let run = blocking
                .run("load worktree integration run", move |store| {
                    store
                        .get_run(&load_run_id)?
                        .ok_or(StoreError::RunNotFound(load_run_id))
                })
                .await
                .map_err(|error| error.to_string())?;
            let workspace_id = run.workspace_id;
            let conversation_id = run.conversation_id;

            let working_dir = execution_agent
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
            let start_run_id = run_id.clone();
            let start_task_id = task.id.clone();
            let start_message =
                format!("worktree integration started: execution={execution_id}, branch={branch}");
            if let Err(error) = blocking
                .run("note worktree integration start", move |store| {
                    store.note(&start_run_id, Some(&start_task_id), &start_message)
                })
                .await
            {
                tracing::warn!(run_id, task_id = %task.id, %error, "failed to note worktree integration start");
            }
            emit_exec(
                trace_sink.as_ref(),
                ExecEvent::task(
                    workspace_id.clone(),
                    conversation_id.clone(),
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

            let result = match outcome {
                Ok(outcome) => {
                    let summary = outcome.summary();
                    let note_run_id = run_id.clone();
                    let note_task_id = task.id.clone();
                    let note_summary = summary.clone();
                    let cleanup_warning = outcome.cleanup_warning.clone();
                    if let Err(error) = blocking
                        .run("note worktree integration result", move |store| {
                            store.note(&note_run_id, Some(&note_task_id), &note_summary)?;
                            if let Some(warning) = cleanup_warning {
                                store.note(
                                    &note_run_id,
                                    Some(&note_task_id),
                                    &format!("worktree cleanup warning: {warning}"),
                                )?;
                            }
                            Ok(())
                        })
                        .await
                    {
                        tracing::warn!(run_id, task_id = %task.id, %error, "failed to note worktree integration result");
                    }
                    emit_exec(
                        trace_sink.as_ref(),
                        ExecEvent::task(
                            workspace_id.clone(),
                            conversation_id.clone(),
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
                    let note_run_id = run_id.clone();
                    let note_task_id = task.id.clone();
                    let failure_note = format!("worktree integration failed: {message}");
                    if let Err(error) = blocking
                        .run("note worktree integration failure", move |store| {
                            store.note(&note_run_id, Some(&note_task_id), &failure_note)
                        })
                        .await
                    {
                        tracing::warn!(run_id, task_id = %task.id, %error, "failed to note worktree integration failure");
                    }
                    emit_exec(
                        trace_sink.as_ref(),
                        ExecEvent::task(
                            workspace_id,
                            conversation_id,
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
            };
            drop(target_lease);
            result
        })
    }
}

#[derive(Debug, Clone)]
struct TaskDispatchSuccess {
    task_id: String,
    result: SubagentTaskResult,
    full_output: String,
    suggested_tasks: Vec<SuggestedTask>,
}

fn task_execution_summary_candidate(
    run_id: &str,
    task: &PlanTask,
    result: SubagentTaskResult,
    suggested_tasks: Vec<SuggestedTask>,
    decisions: Vec<String>,
) -> TaskExecutionSummary {
    TaskExecutionSummary {
        run_id: run_id.to_string(),
        task_id: task.id.clone(),
        subagent_name: task.agent_role.clone(),
        result,
        decisions,
        next_implications: Vec::new(),
        suggested_tasks,
        created_at: chrono::Utc::now(),
    }
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

    fn from_turn_receipt(receipt: &TurnReceipt) -> Self {
        let duration_ms = u64::try_from(receipt.elapsed.as_millis()).unwrap_or(u64::MAX);
        Self {
            durable: SubagentRunUsage {
                duration_ms: Some(duration_ms),
                tokens_used: (receipt.llm_calls > 0).then(|| {
                    receipt
                        .prompt_tokens
                        .saturating_add(receipt.completion_tokens)
                }),
                iterations: None,
            },
            input_tokens: receipt.prompt_tokens,
            output_tokens: receipt.completion_tokens,
        }
    }
}

#[allow(clippy::result_large_err)]
async fn finalize_framework_subagent_result(
    blocking: TaskRuntimeBlockingAdapter,
    run_id: &str,
    execution_id: &str,
    result: echo_agent::agent::subagent::SubagentResult,
) -> Result<(SubagentTaskResult, String, TaskExecutionUsage), ExecutionFailure> {
    let usage = TaskExecutionUsage::from_framework(&result);
    let usage_run_id = run_id.to_string();
    let usage_execution_id = execution_id.to_string();
    let persisted_usage = usage.clone();
    blocking
        .run("persist framework Subagent usage", move |store| {
            store.account_subagent_usage(
                &usage_run_id,
                &usage_execution_id,
                "framework_dispatch_total",
                persisted_usage.input_tokens,
                persisted_usage.output_tokens,
                persisted_usage.duration_ms(),
            )
        })
        .await
        .map_err(|error| {
            ExecutionFailure::failed(format!(
                "failed to persist Subagent usage for {execution_id}: {error}"
            ))
            .with_usage(usage.clone())
        })?;
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
            agent_failure: None,
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
    agent_failure: Option<echo_agent::error::AgentFailure>,
}

impl ExecutionFailure {
    fn failed(message: impl Into<String>) -> Self {
        Self {
            status: SubagentRunStatus::Failed,
            message: message.into(),
            usage: None,
            agent_failure: None,
        }
    }

    fn cancelled(message: impl Into<String>) -> Self {
        Self {
            status: SubagentRunStatus::Cancelled,
            message: message.into(),
            usage: None,
            agent_failure: None,
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
            agent_failure: Some(failure.clone()),
        }
    }

    fn from_react(error: echo_agent::error::ReactError, context: &str) -> Self {
        let status = echo_agent::agent::subagent::subagent_status_from_error(&error).into();
        Self {
            status,
            message: format!("{context}: {error}"),
            usage: None,
            agent_failure: Some(echo_agent::error::AgentFailure::from_react_error(&error)),
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

fn attach_agent_failure_evidence(
    result: &mut SubagentTaskResult,
    failure: &echo_agent::error::AgentFailure,
) {
    result.evidence.push(SubagentEvidenceResult {
        kind: "agent_failure".to_string(),
        subject: failure.code.clone(),
        outcome: Some(
            match failure.terminal_kind {
                echo_agent::error::AgentTerminalKind::Failed => "failed",
                echo_agent::error::AgentTerminalKind::Cancelled => "cancelled",
                echo_agent::error::AgentTerminalKind::TimedOut => "timed_out",
                echo_agent::error::AgentTerminalKind::PermissionDenied => "permission_denied",
            }
            .to_string(),
        ),
        details: failure.message.chars().take(1_200).collect(),
        source: SubagentVerificationSource::Observed,
        attributes: serde_json::to_value(failure).unwrap_or(serde_json::Value::Null),
    });
}

#[derive(Clone)]
struct EkoAgentTurnContext {
    workspace_id: String,
    conversation_id: String,
    run_id: String,
    task_id: Option<String>,
    execution_id: Option<String>,
    agent_role: Option<String>,
}

impl EkoAgentTurnContext {
    fn run(run: &TaskRun) -> Self {
        Self {
            workspace_id: run.workspace_id.clone(),
            conversation_id: run.conversation_id.clone(),
            run_id: run.run_id.clone(),
            task_id: None,
            execution_id: None,
            agent_role: None,
        }
    }

    fn primary_task(run: &TaskRun, task: &PlanTask, execution_id: &str) -> Self {
        Self {
            workspace_id: run.workspace_id.clone(),
            conversation_id: run.conversation_id.clone(),
            run_id: run.run_id.clone(),
            task_id: Some(task.id.clone()),
            execution_id: Some(execution_id.to_string()),
            agent_role: Some(task.agent_role.clone()),
        }
    }

    fn event(&self, kind: RuntimeEventKind, payload: serde_json::Value) -> ExecEvent {
        let event = match (&self.task_id, &self.execution_id) {
            (Some(task_id), Some(execution_id)) => ExecEvent::subagent(
                self.workspace_id.clone(),
                self.conversation_id.clone(),
                self.run_id.clone(),
                task_id.clone(),
                execution_id.clone(),
                kind,
                payload,
            ),
            _ => ExecEvent::run(
                self.workspace_id.clone(),
                self.conversation_id.clone(),
                self.run_id.clone(),
                kind,
                payload,
            ),
        };
        if let Some(agent_role) = self.agent_role.as_ref() {
            event.with_agent(agent_role.clone())
        } else {
            event
        }
    }
}

struct PrimaryTaskTurnPersistence {
    blocking: TaskRuntimeBlockingAdapter,
    replay_safe_tools: HashSet<String>,
}

struct RunTurnPersistence {
    blocking: TaskRuntimeBlockingAdapter,
    turn_id: String,
}

#[derive(Default)]
struct EkoAgentTurnState {
    output: String,
    in_thinking: bool,
    pending_verification: HashMap<String, String>,
    pending_file_access: HashMap<String, (bool, String)>,
    observed_evidence: Vec<echo_agent::agent::subagent::SubagentEvidence>,
    observed_artifacts: Vec<echo_agent::agent::subagent::SubagentArtifact>,
    mutating_tool_observed: bool,
}

struct EkoAgentTurnObservation {
    output: String,
    observed_evidence: Vec<echo_agent::agent::subagent::SubagentEvidence>,
    observed_artifacts: Vec<echo_agent::agent::subagent::SubagentArtifact>,
    mutating_tool_observed: bool,
}

/// The sole EKO adapter below [`AgentTurnDriver`] for TaskRuntime-owned turns.
///
/// Framework code owns stream startup, envelope sequencing, exact terminal
/// detection, typed failures, cancellation, and provider-reported receipt
/// accounting. This sink owns only EKO product projection and persistence:
/// `ExecEvent`, exact event-id usage, tool boundaries, evidence, and artifacts.
struct EkoAgentTurnSink {
    context: EkoAgentTurnContext,
    trace_sink: Option<ExecSink>,
    primary_task: Option<PrimaryTaskTurnPersistence>,
    run_turn: Option<RunTurnPersistence>,
    mutating_tools: HashSet<String>,
    state: std::sync::Mutex<EkoAgentTurnState>,
}

impl EkoAgentTurnSink {
    fn for_run(
        run: &TaskRun,
        turn_id: &str,
        blocking: TaskRuntimeBlockingAdapter,
        mutating_tools: HashSet<String>,
        trace_sink: Option<ExecSink>,
    ) -> Self {
        Self {
            context: EkoAgentTurnContext::run(run),
            trace_sink,
            primary_task: None,
            run_turn: Some(RunTurnPersistence {
                blocking,
                turn_id: turn_id.to_string(),
            }),
            mutating_tools,
            state: std::sync::Mutex::new(EkoAgentTurnState::default()),
        }
    }

    fn for_primary_task(
        run: &TaskRun,
        task: &PlanTask,
        execution_id: &str,
        blocking: TaskRuntimeBlockingAdapter,
        replay_safe_tools: HashSet<String>,
        trace_sink: Option<ExecSink>,
    ) -> Self {
        Self {
            context: EkoAgentTurnContext::primary_task(run, task, execution_id),
            trace_sink,
            primary_task: Some(PrimaryTaskTurnPersistence {
                blocking,
                replay_safe_tools,
            }),
            run_turn: None,
            mutating_tools: HashSet::new(),
            state: std::sync::Mutex::new(EkoAgentTurnState::default()),
        }
    }

    fn emit(&self, kind: RuntimeEventKind, payload: serde_json::Value) {
        emit_exec(self.trace_sink.as_ref(), self.context.event(kind, payload));
    }

    fn finish(&self, final_answer: Option<&str>) -> EkoAgentTurnObservation {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(final_answer) = final_answer.filter(|answer| !answer.is_empty()) {
            state.output = final_answer.to_string();
        }
        EkoAgentTurnObservation {
            output: std::mem::take(&mut state.output),
            observed_evidence: std::mem::take(&mut state.observed_evidence),
            observed_artifacts: std::mem::take(&mut state.observed_artifacts),
            mutating_tool_observed: state.mutating_tool_observed,
        }
    }

    fn persistence_error(
        operation: &str,
        error: impl std::fmt::Display,
    ) -> echo_agent::error::ReactError {
        echo_agent::error::ReactError::Other(format!("{operation}: {error}"))
    }
}

#[async_trait::async_trait]
impl EventSink for EkoAgentTurnSink {
    async fn on_event(
        &self,
        envelope: echo_agent::agent::EventEnvelope,
    ) -> echo_agent::error::Result<SinkControl> {
        let source_event_id = envelope.event_id.to_string();
        match envelope.payload {
            AgentEvent::Token(content) => {
                let in_thinking = {
                    let mut state = self
                        .state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    if !state.in_thinking {
                        state.output.push_str(&content);
                    }
                    state.in_thinking
                };
                self.emit(
                    if in_thinking {
                        RuntimeEventKind::ThinkingDelta
                    } else {
                        RuntimeEventKind::TokenDelta
                    },
                    serde_json::json!({ "content": content }),
                );
            }
            AgentEvent::ThinkStart => {
                self.state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .in_thinking = true;
                self.emit(RuntimeEventKind::ThinkingStarted, serde_json::json!({}));
            }
            AgentEvent::ThinkEnd {
                prompt_tokens,
                completion_tokens,
            } => {
                self.state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .in_thinking = false;
                self.emit(
                    RuntimeEventKind::ThinkingEnded,
                    serde_json::json!({
                        "prompt_tokens": prompt_tokens,
                        "completion_tokens": completion_tokens,
                    }),
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
                if usage_reported && let Some(primary_task) = self.primary_task.as_ref() {
                    let run_id = self.context.run_id.clone();
                    let execution_id = self.context.execution_id.clone().ok_or_else(|| {
                        echo_agent::error::ReactError::Other(
                            "primary task turn is missing its execution identity".to_string(),
                        )
                    })?;
                    let input_tokens = u64::try_from(prompt_tokens).unwrap_or(u64::MAX);
                    let output_tokens = u64::try_from(completion_tokens).unwrap_or(u64::MAX);
                    let usage_event_id = source_event_id.clone();
                    primary_task
                        .blocking
                        .run("persist primary Subagent usage", move |store| {
                            store.account_subagent_usage(
                                &run_id,
                                &execution_id,
                                &usage_event_id,
                                input_tokens,
                                output_tokens,
                                0,
                            )
                        })
                        .await
                        .map_err(|error| {
                            Self::persistence_error(
                                "failed to persist primary Subagent usage",
                                error,
                            )
                        })?;
                }
                if usage_reported && let Some(run_turn) = self.run_turn.as_ref() {
                    let run_id = self.context.run_id.clone();
                    let turn_id = run_turn.turn_id.clone();
                    let input_tokens = u64::try_from(prompt_tokens).unwrap_or(u64::MAX);
                    let output_tokens = u64::try_from(completion_tokens).unwrap_or(u64::MAX);
                    let usage_event_id = source_event_id.clone();
                    run_turn
                        .blocking
                        .run("persist primary RunTurn usage", move |store| {
                            store
                                .account_run_turn_usage(
                                    &run_id,
                                    &turn_id,
                                    &usage_event_id,
                                    input_tokens,
                                    output_tokens,
                                )
                                .map(|_| ())
                        })
                        .await
                        .map_err(|error| {
                            Self::persistence_error(
                                "failed to persist primary RunTurn usage",
                                error,
                            )
                        })?;
                }
                self.emit(
                    RuntimeEventKind::Usage,
                    serde_json::json!({
                        "model": model,
                        "prompt_tokens": prompt_tokens,
                        "completion_tokens": completion_tokens,
                        "total_tokens": total_tokens,
                        "cached_prompt_tokens": cached_prompt_tokens,
                        "cache_creation_prompt_tokens": cache_creation_prompt_tokens,
                        "usage_reported": usage_reported,
                        "usage_event_id": source_event_id,
                    }),
                );
            }
            AgentEvent::ToolCall {
                call_id,
                invocation,
            } => {
                let name = invocation.name.clone();
                let args = invocation.args.clone();
                {
                    let mut state = self
                        .state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    state.mutating_tool_observed |= self.mutating_tools.contains(&name);
                    if let Some(check) = verification_check_from_agent_tool(&name, &args) {
                        state.pending_verification.insert(call_id.clone(), check);
                    }
                    if let Some(access) = file_access_from_agent_tool(&name, &args) {
                        state.pending_file_access.insert(call_id.clone(), access);
                    }
                }
                if let Some(primary_task) = self.primary_task.as_ref() {
                    let run_id = self.context.run_id.clone();
                    let task_id = self.context.task_id.clone().ok_or_else(|| {
                        echo_agent::error::ReactError::Other(
                            "primary task turn is missing its task identity".to_string(),
                        )
                    })?;
                    let execution_id = self.context.execution_id.clone().ok_or_else(|| {
                        echo_agent::error::ReactError::Other(
                            "primary task turn is missing its execution identity".to_string(),
                        )
                    })?;
                    let tool_call_id = call_id.clone();
                    let tool_name = name.clone();
                    let replay_safe = primary_task.replay_safe_tools.contains(&name);
                    primary_task
                        .blocking
                        .run("persist tool start boundary", move |store| {
                            store.record_tool_started(
                                &run_id,
                                &task_id,
                                &execution_id,
                                &tool_call_id,
                                &tool_name,
                                replay_safe,
                            )
                        })
                        .await
                        .map_err(|error| {
                            Self::persistence_error("failed to persist tool start boundary", error)
                        })?;
                }
                self.emit(
                    RuntimeEventKind::ToolStarted,
                    serde_json::json!({
                        "call_id": call_id,
                        "invocation": invocation,
                    }),
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
                {
                    let mut state = self
                        .state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    if let Some(check) = state.pending_verification.remove(&call_id) {
                        state.observed_evidence.push(
                            echo_agent::agent::subagent::SubagentEvidence {
                                kind: "verification".to_string(),
                                subject: check,
                                outcome: Some(if result.success {
                                    "passed".to_string()
                                } else {
                                    "failed".to_string()
                                }),
                                details: result_text.chars().take(500).collect(),
                                source:
                                    echo_agent::agent::subagent::SubagentEvidenceSource::Observed,
                                attributes: serde_json::Value::Null,
                            },
                        );
                    }
                    if result.success
                        && let Some((write, path)) = state.pending_file_access.remove(&call_id)
                    {
                        state.observed_evidence.push(
                            echo_agent::agent::subagent::SubagentEvidence {
                                kind: if write { "file_write" } else { "file_read" }.to_string(),
                                subject: path,
                                outcome: Some("succeeded".to_string()),
                                details: String::new(),
                                source:
                                    echo_agent::agent::subagent::SubagentEvidenceSource::Observed,
                                attributes: serde_json::Value::Null,
                            },
                        );
                    } else {
                        state.pending_file_access.remove(&call_id);
                    }
                    if let Some(artifact) =
                        echo_agent::tools::artifact::ToolOutputArtifactRef::from_metadata(
                            &result.metadata,
                        )
                    {
                        state.observed_artifacts.push(
                            echo_agent::agent::subagent::SubagentArtifact {
                                path: artifact.path.to_string_lossy().to_string(),
                                kind: "tool_log".to_string(),
                                bytes: Some(artifact.artifact_bytes),
                                sha256: Some(artifact.sha256),
                                producer_execution_id: self.context.execution_id.clone(),
                                available: artifact.path.is_file(),
                            },
                        );
                    }
                }
                if let Some(primary_task) = self.primary_task.as_ref() {
                    let run_id = self.context.run_id.clone();
                    let task_id = self.context.task_id.clone().ok_or_else(|| {
                        echo_agent::error::ReactError::Other(
                            "primary task turn is missing its task identity".to_string(),
                        )
                    })?;
                    let execution_id = self.context.execution_id.clone().ok_or_else(|| {
                        echo_agent::error::ReactError::Other(
                            "primary task turn is missing its execution identity".to_string(),
                        )
                    })?;
                    let tool_call_id = call_id.clone();
                    let tool_name = name.clone();
                    let tool_result_text = result_text.clone();
                    let tool_success = result.success;
                    let tool_failure = result.failure.clone();
                    primary_task
                        .blocking
                        .run("persist tool terminal boundary", move |store| {
                            store.record_tool_finished(
                                &run_id,
                                &task_id,
                                &execution_id,
                                &tool_call_id,
                                &tool_name,
                                tool_success,
                                &tool_result_text,
                                tool_failure.as_ref(),
                            )
                        })
                        .await
                        .map_err(|error| {
                            Self::persistence_error(
                                "tool settled but its terminal boundary was not persisted",
                                error,
                            )
                        })?;
                }
                self.emit(
                    RuntimeEventKind::ToolCompleted,
                    serde_json::json!({
                        "call_id": call_id,
                        "name": name,
                        "result": result,
                    }),
                );
            }
            AgentEvent::ToolStream {
                call_id,
                name,
                event,
            } => {
                let payload = match event {
                    echo_agent::tools::ToolStreamEvent::Progress { message, percent } => {
                        serde_json::json!({
                            "call_id": call_id,
                            "name": name,
                            "message": message,
                            "percent": percent,
                        })
                    }
                    echo_agent::tools::ToolStreamEvent::Output { channel, chunk } => {
                        serde_json::json!({
                            "call_id": call_id,
                            "name": name,
                            "channel": match channel {
                                echo_agent::tools::ToolOutputChannel::Stdout => "stdout",
                                echo_agent::tools::ToolOutputChannel::Stderr => "stderr",
                                echo_agent::tools::ToolOutputChannel::Log => "log",
                            },
                            "chunk": chunk,
                        })
                    }
                    echo_agent::tools::ToolStreamEvent::Complete(_) => {
                        return Ok(SinkControl::Continue);
                    }
                };
                self.emit(RuntimeEventKind::ToolOutput, payload);
            }
            AgentEvent::FinalAnswer(answer) => {
                if !answer.is_empty() {
                    self.state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .output = answer;
                }
            }
            AgentEvent::Cancelled | AgentEvent::Error { .. } => {}
            _ => {}
        }
        Ok(SinkControl::Continue)
    }
}

#[derive(Debug, Clone)]
struct TaskDispatchFailure {
    task_id: String,
    status: SubagentRunStatus,
    message: String,
    agent_failure: Option<echo_agent::error::AgentFailure>,
}

impl TaskDispatchFailure {
    fn failed(task_id: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            status: SubagentRunStatus::Failed,
            message: message.into(),
            agent_failure: None,
        }
    }

    fn cancelled(task_id: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            status: SubagentRunStatus::Cancelled,
            message: message.into(),
            agent_failure: None,
        }
    }

    fn from_execution(task_id: impl Into<String>, failure: ExecutionFailure) -> Self {
        Self {
            task_id: task_id.into(),
            status: failure.status,
            message: failure.message,
            agent_failure: failure.agent_failure,
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
    blocking: TaskRuntimeBlockingAdapter,
    dispatcher: Arc<W>,
    reviewer_llm: Option<Arc<dyn echo_agent::llm::LlmClient>>,
    write_sem: Arc<Semaphore>,
    shell_sem: Arc<Semaphore>,
    llm_sem: Arc<Semaphore>,
    file_write_locks: Arc<std::sync::Mutex<HashMap<String, Arc<TokioMutex<()>>>>>,
    trace_sink: Option<ExecSink>,
    cancel: CancellationToken,
    resolution_metadata: std::sync::Mutex<HashMap<String, RuntimeTaskProductSettlement>>,
    dispatch_failures: std::sync::Mutex<HashMap<String, TaskDispatchFailure>>,
}

#[derive(Clone)]
pub struct TaskRuntimeBlockingAdapter {
    store: Arc<TaskRuntimeStore>,
    supervisor: Arc<TaskRuntimeOperationSupervisor>,
}

const PROCESS_TASK_RUNTIME_FILE_IO_LIMIT: usize = 8;
static PROCESS_TASK_RUNTIME_FILE_IO: std::sync::LazyLock<Arc<Semaphore>> =
    std::sync::LazyLock::new(|| Arc::new(Semaphore::new(PROCESS_TASK_RUNTIME_FILE_IO_LIMIT)));
tokio::task_local! {
    static CURRENT_TASK_RUNTIME_OPERATION_SUPERVISOR: usize;
}

#[derive(Default)]
struct TaskRuntimeOperationState {
    accepting: bool,
    active: usize,
    orphan_failures: Vec<String>,
}

/// Store-owned authority for every accepted async or blocking TaskRuntime
/// operation. Callers only await receipts; dropping a caller never owns or
/// aborts the operation itself.
pub(crate) struct TaskRuntimeOperationSupervisor {
    state: std::sync::Mutex<TaskRuntimeOperationState>,
    idle: tokio::sync::Notify,
}

struct TaskRuntimeOperationReceipt {
    supervisor: Arc<TaskRuntimeOperationSupervisor>,
}

pub(crate) struct TaskRuntimeSettlementReservation {
    receipt: TaskRuntimeOperationReceipt,
}

impl Drop for TaskRuntimeOperationReceipt {
    fn drop(&mut self) {
        if let Ok(mut state) = self.supervisor.state.lock() {
            state.active = state.active.saturating_sub(1);
            if state.active == 0 {
                self.supervisor.idle.notify_waiters();
            }
        }
    }
}

impl TaskRuntimeOperationSupervisor {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: std::sync::Mutex::new(TaskRuntimeOperationState {
                accepting: true,
                ..TaskRuntimeOperationState::default()
            }),
            idle: tokio::sync::Notify::new(),
        })
    }

    fn is_nested_operation(self: &Arc<Self>) -> bool {
        let identity = Arc::as_ptr(self) as usize;
        CURRENT_TASK_RUNTIME_OPERATION_SUPERVISOR
            .try_with(|id| *id == identity)
            .unwrap_or(false)
    }

    fn register(
        self: &Arc<Self>,
        operation: &'static str,
    ) -> Result<TaskRuntimeOperationReceipt, StoreError> {
        let nested = self.is_nested_operation();
        let mut state = self.state.lock().map_err(|_| StoreError::LockPoisoned)?;
        if !state.accepting && !nested {
            return Err(StoreError::InvalidPlan(format!(
                "TaskRuntime operation admission is closed during {operation}"
            )));
        }
        state.active = state.active.checked_add(1).ok_or_else(|| {
            StoreError::InvalidPlan("TaskRuntime operation capacity exhausted".to_string())
        })?;
        drop(state);
        Ok(TaskRuntimeOperationReceipt {
            supervisor: Arc::clone(self),
        })
    }

    pub(crate) fn begin_shutdown(&self) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "TaskRuntime operation supervisor lock is poisoned".to_string())?;
        state.accepting = false;
        Ok(())
    }

    pub(crate) fn active_count(&self) -> usize {
        self.state
            .lock()
            .map(|state| state.active)
            .unwrap_or(usize::MAX)
    }

    pub(crate) async fn join(&self) -> Result<(), String> {
        loop {
            let notified = self.idle.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let failures = {
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| "TaskRuntime operation supervisor lock is poisoned".to_string())?;
                if state.active == 0 {
                    Some(std::mem::take(&mut state.orphan_failures))
                } else {
                    None
                }
            };
            if let Some(failures) = failures {
                return if failures.is_empty() {
                    Ok(())
                } else {
                    Err(failures.join("; "))
                };
            }
            notified.await;
        }
    }

    fn record_orphan_failure(&self, operation: &'static str, error: &StoreError) {
        if let Ok(mut state) = self.state.lock() {
            state.orphan_failures.push(format!("{operation}: {error}"));
        }
    }
}

impl TaskRuntimeBlockingAdapter {
    pub fn new(store: Arc<TaskRuntimeStore>) -> Self {
        let supervisor = store.operation_supervisor();
        Self { store, supervisor }
    }

    pub async fn run<T, F>(
        &self,
        operation: &'static str,
        function: F,
    ) -> echo_agent::error::Result<T>
    where
        T: Send + 'static,
        F: FnOnce(Arc<TaskRuntimeStore>) -> Result<T, StoreError> + Send + 'static,
    {
        self.run_store(operation, function).await.map_err(|error| {
            echo_agent::error::ReactError::Other(format!(
                "TaskRuntime blocking operation {operation} failed: {error}"
            ))
        })
    }

    pub async fn run_store<T, F>(
        &self,
        operation: &'static str,
        function: F,
    ) -> Result<T, StoreError>
    where
        T: Send + 'static,
        F: FnOnce(Arc<TaskRuntimeStore>) -> Result<T, StoreError> + Send + 'static,
    {
        let store = self.store.clone();
        self.run_owned(operation, move || function(store)).await
    }

    pub async fn run_owned<T, F>(
        &self,
        operation: &'static str,
        function: F,
    ) -> Result<T, StoreError>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, StoreError> + Send + 'static,
    {
        let permit = PROCESS_TASK_RUNTIME_FILE_IO
            .clone()
            .acquire_owned()
            .await
            .map_err(|error| {
                StoreError::InvalidPlan(format!(
                    "TaskRuntime blocking adapter closed during {operation}: {error}"
                ))
            })?;
        let receipt = self.supervisor.register(operation)?;
        let supervisor = Arc::clone(&self.supervisor);
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let execution = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            function()
        });
        tokio::spawn(async move {
            let _receipt = receipt;
            let result = match execution.await {
                Ok(result) => result,
                Err(error) => Err(StoreError::InvalidPlan(format!(
                    "TaskRuntime blocking operation {operation} failed to join: {error}"
                ))),
            };
            if let Err(orphaned) = sender.send(result)
                && let Err(error) = orphaned
            {
                supervisor.record_orphan_failure(operation, &error);
            }
        });
        receiver.await.map_err(|_| {
            StoreError::InvalidPlan(format!(
                "TaskRuntime blocking operation {operation} ended without a receipt"
            ))
        })?
    }

    /// Run a multi-stage async command under store ownership. Nested blocking
    /// settlements retain admission after phase-one shutdown so an accepted
    /// command can always publish its terminal fact.
    pub async fn run_async_owned<T, F>(
        &self,
        operation: &'static str,
        future: F,
    ) -> Result<T, StoreError>
    where
        T: Send + 'static,
        F: std::future::Future<Output = Result<T, StoreError>> + Send + 'static,
    {
        let receipt = self.supervisor.register(operation)?;
        let receiver = self.spawn_async_with_receipt(operation, receipt, future);
        receiver.await.map_err(|_| {
            StoreError::InvalidPlan(format!(
                "TaskRuntime async operation {operation} ended without a receipt"
            ))
        })?
    }

    pub(crate) fn reserve_settlement(
        &self,
        operation: &'static str,
    ) -> Result<TaskRuntimeSettlementReservation, StoreError> {
        self.supervisor
            .register(operation)
            .map(|receipt| TaskRuntimeSettlementReservation { receipt })
    }

    pub(crate) fn record_lifecycle_debt(&self, operation: &'static str, error: &StoreError) {
        self.supervisor.record_orphan_failure(operation, error);
    }

    pub(crate) fn spawn_reserved_settlement<T, F>(
        &self,
        operation: &'static str,
        reservation: TaskRuntimeSettlementReservation,
        future: F,
    ) -> tokio::sync::oneshot::Receiver<Result<T, StoreError>>
    where
        T: Send + 'static,
        F: std::future::Future<Output = Result<T, StoreError>> + Send + 'static,
    {
        self.spawn_async_with_receipt(operation, reservation.receipt, future)
    }

    fn spawn_async_with_receipt<T, F>(
        &self,
        operation: &'static str,
        receipt: TaskRuntimeOperationReceipt,
        future: F,
    ) -> tokio::sync::oneshot::Receiver<Result<T, StoreError>>
    where
        T: Send + 'static,
        F: std::future::Future<Output = Result<T, StoreError>> + Send + 'static,
    {
        let supervisor = Arc::clone(&self.supervisor);
        let supervisor_id = Arc::as_ptr(&self.supervisor) as usize;
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let execution =
            tokio::spawn(CURRENT_TASK_RUNTIME_OPERATION_SUPERVISOR.scope(supervisor_id, future));
        tokio::spawn(async move {
            let _receipt = receipt;
            let result = match execution.await {
                Ok(result) => result,
                Err(error) => Err(StoreError::InvalidPlan(format!(
                    "TaskRuntime async operation {operation} failed to join: {error}"
                ))),
            };
            if let Err(orphaned) = sender.send(result)
                && let Err(error) = orphaned
            {
                supervisor.record_orphan_failure(operation, &error);
            }
        });
        receiver
    }
}

impl<W: TaskDispatcher> EkoRuntimeDagController<W> {
    fn plan_task(runtime_task: &echo_agent::tasks::Task) -> echo_agent::error::Result<PlanTask> {
        PlanTask::try_from(runtime_task.clone()).map_err(echo_agent::error::ReactError::Other)
    }

    async fn review_stop_disposition(
        &self,
        run_id: &str,
    ) -> echo_agent::error::Result<echo_agent::tasks::RuntimeStopDisposition> {
        let run_id = run_id.to_string();
        let run = self
            .blocking
            .run("load run review disposition", move |store| {
                store
                    .get_run(&run_id)?
                    .ok_or(StoreError::RunNotFound(run_id))
            })
            .await?;
        Ok(if run.attended_mode == AttendedMode::Unattended {
            echo_agent::tasks::RuntimeStopDisposition::Fail
        } else {
            echo_agent::tasks::RuntimeStopDisposition::Pause
        })
    }

    async fn note(
        &self,
        run_id: &str,
        task_id: Option<&str>,
        message: impl Into<String>,
    ) -> echo_agent::error::Result<()> {
        let run_id = run_id.to_string();
        let task_id = task_id.map(str::to_string);
        let message = message.into();
        self.blocking
            .run("append runtime task note", move |store| {
                store.note(&run_id, task_id.as_deref(), &message)
            })
            .await
    }

    fn stage_resolution_metadata(
        &self,
        claim_id: &str,
        metadata: RuntimeTaskProductSettlement,
    ) -> echo_agent::error::Result<()> {
        let mut metadata_by_claim = self
            .resolution_metadata
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match metadata_by_claim.entry(claim_id.to_string()) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(metadata);
                Ok(())
            }
            std::collections::hash_map::Entry::Occupied(_) => {
                Err(echo_agent::error::ReactError::Other(format!(
                    "resolution metadata for claim '{claim_id}' was staged more than once"
                )))
            }
        }
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
        let run_id = run_id.to_string();
        self.blocking
            .run("load exact revisioned task graph", move |store| {
                store.load_runtime_plan_snapshot(&run_id)
            })
            .await
    }

    async fn claim_task(
        &self,
        run_id: &str,
        task: &echo_agent::tasks::Task,
        expected_revision: u64,
    ) -> echo_agent::error::Result<echo_agent::tasks::RuntimeTaskClaimOutcome> {
        let run_id = run_id.to_string();
        let task = task.clone();
        self.blocking
            .run("claim runtime task", move |store| {
                store.claim_runtime_task(&run_id, &task, expected_revision)
            })
            .await
    }

    async fn claim_is_current(
        &self,
        run_id: &str,
        task_id: &str,
        claim: &echo_agent::tasks::TaskClaim,
    ) -> echo_agent::error::Result<bool> {
        let run_id = run_id.to_string();
        let task_id = task_id.to_string();
        let claim = claim.clone();
        self.blocking
            .run("check runtime task claim", move |store| {
                store.runtime_task_claim_is_current(&run_id, &task_id, &claim)
            })
            .await
    }

    fn select_ready_wave(
        &self,
        tasks: &[echo_agent::tasks::Task],
        ready_task_ids: Vec<String>,
    ) -> Vec<String> {
        let ready = ready_task_ids
            .iter()
            .filter_map(|task_id| {
                tasks
                    .iter()
                    .find(|task| task.spec.id == *task_id)
                    .cloned()
                    .and_then(|task| match PlanTask::try_from(task) {
                        Ok(task) => Some(task),
                        Err(error) => {
                            tracing::error!(task_id, %error, "invalid EKO task extension in ready frontier");
                            None
                        }
                    })
            })
            .collect::<Vec<_>>();
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
        let task = Self::plan_task(&runtime_task)?;
        let active_task_id = task.id.clone();
        let execution_id = subagent_execution_id(&context.run_id, &task.id, &claim);
        let recovery_run_id = context.run_id.clone();
        let recovery_task_id = task.id.clone();
        let recovery_execution_id = execution_id.clone();
        let recovery_revision = claim.revision;
        let recovery_attempt = claim.attempt;
        let recovery = self
            .blocking
            .run("load recoverable Subagent result", move |store| {
                store.recoverable_subagent_result_for_attempt(
                    &recovery_run_id,
                    &recovery_task_id,
                    &recovery_execution_id,
                    recovery_revision,
                    recovery_attempt,
                )
            })
            .await;
        match recovery {
            Ok(Some(recovered)) => {
                tracing::info!(
                    run_id = %context.run_id,
                    task_id = %task.id,
                    execution_id,
                    "task_runtime: reusing durable Subagent result after restart"
                );
                let note_run_id = context.run_id.clone();
                let note_task_id = task.id.clone();
                if let Err(error) = self
                    .blocking
                    .run("note recovered Subagent result", move |store| {
                        store.note(
                            &note_run_id,
                            Some(&note_task_id),
                            "reused completed Subagent result; continuing at review boundary",
                        )
                    })
                    .await
                {
                    tracing::warn!(run_id = %context.run_id, task_id = %task.id, %error, "failed to note recovered Subagent result");
                }
                return Ok(TaskDispatchSuccess {
                    task_id: task.id,
                    result: recovered.result,
                    full_output: recovered.full_output,
                    suggested_tasks: Vec::new(),
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

        let claim_id = claim.claim_id.clone();
        self.dispatcher
            .dispatch(
                self.store.clone(),
                self.blocking.clone(),
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
                if failure.agent_failure.is_some() {
                    self.dispatch_failures
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .insert(claim_id, failure.clone());
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
    ) -> echo_agent::error::Result<echo_agent::tasks::RuntimeTaskResolutionRequest> {
        let task = Self::plan_task(&runtime_task)?;
        let dispatched = match dispatch {
            Ok(dispatched) => dispatched,
            Err(error) => {
                let typed_failure = self
                    .dispatch_failures
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .remove(&claim.claim_id)
                    .and_then(|failure| failure.agent_failure);
                let message = typed_failure
                    .as_ref()
                    .map(|failure| failure.message.clone())
                    .unwrap_or_else(|| error.to_string());
                let status = typed_failure
                    .as_ref()
                    .map(|failure| match failure.terminal_kind {
                        echo_agent::error::AgentTerminalKind::Cancelled => {
                            echo_agent::agent::subagent::SubagentStatus::Cancelled
                        }
                        echo_agent::error::AgentTerminalKind::TimedOut => {
                            echo_agent::agent::subagent::SubagentStatus::TimedOut
                        }
                        echo_agent::error::AgentTerminalKind::Failed
                        | echo_agent::error::AgentTerminalKind::PermissionDenied => {
                            echo_agent::agent::subagent::SubagentStatus::Failed
                        }
                    })
                    .unwrap_or_else(|| {
                        echo_agent::agent::subagent::subagent_status_from_error(&error)
                    });
                let request = if let Some(failure) = typed_failure.as_ref().filter(|failure| {
                    failure.retryable
                        && failure.category == echo_agent::error::AgentFailureCategory::Llm
                }) {
                    echo_agent::tasks::RuntimeTaskResolutionRequest::Requeue {
                        failure_fingerprint: Some(
                            super::turn_lifecycle::agent_failure_fingerprint(failure),
                        ),
                        error: failure.message.clone(),
                    }
                } else {
                    match status {
                        echo_agent::agent::subagent::SubagentStatus::Cancelled => {
                            echo_agent::tasks::RuntimeTaskResolutionRequest::Cancelled
                        }
                        echo_agent::agent::subagent::SubagentStatus::TimedOut => {
                            echo_agent::tasks::RuntimeTaskResolutionRequest::Failed {
                                error: format!("Subagent timed out: {message}"),
                            }
                        }
                        echo_agent::agent::subagent::SubagentStatus::Completed
                        | echo_agent::agent::subagent::SubagentStatus::Failed => {
                            echo_agent::tasks::RuntimeTaskResolutionRequest::Failed {
                                error: message.clone(),
                            }
                        }
                    }
                };
                let mut result = SubagentTaskResult::terminal(
                    status.into(),
                    message.clone(),
                    vec![message.clone()],
                );
                if let Some(failure) = typed_failure.as_ref() {
                    attach_agent_failure_evidence(&mut result, failure);
                }
                self.stage_resolution_metadata(
                    &claim.claim_id,
                    RuntimeTaskProductSettlement {
                        summary: Some(message.clone()),
                        execution_summary: Some(task_execution_summary_candidate(
                            run_id,
                            &task,
                            result,
                            Vec::new(),
                            vec![message],
                        )),
                        review: None,
                        diagnostic_note: None,
                        typed_terminal: typed_failure,
                    },
                )?;
                return Ok(request);
            }
        };

        let TaskDispatchSuccess {
            task_id,
            mut result,
            full_output,
            suggested_tasks,
        } = dispatched;
        if task_id != task.id {
            return Err(echo_agent::error::ReactError::Other(format!(
                "dispatcher returned task '{task_id}' for active task '{}'",
                task.id
            )));
        }

        match assess_task_execution(&task, &result) {
            CompletionAssessment::ExecutionFailed { reason } => {
                self.stage_resolution_metadata(
                    &claim.claim_id,
                    RuntimeTaskProductSettlement {
                        summary: Some(reason.clone()),
                        execution_summary: Some(task_execution_summary_candidate(
                            run_id,
                            &task,
                            result,
                            suggested_tasks,
                            vec![format!("execution failed: {reason}")],
                        )),
                        review: None,
                        diagnostic_note: None,
                        typed_terminal: None,
                    },
                )?;
                Ok(echo_agent::tasks::RuntimeTaskResolutionRequest::Requeue {
                    failure_fingerprint: None,
                    error: format!("execution failed: {reason}"),
                })
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
                let disposition = self.review_stop_disposition(run_id).await?;
                self.stage_resolution_metadata(
                    &claim.claim_id,
                    RuntimeTaskProductSettlement {
                        summary: Some(reason.clone()),
                        execution_summary: Some(task_execution_summary_candidate(
                            run_id,
                            &task,
                            result,
                            suggested_tasks,
                            vec![reason.clone()],
                        )),
                        review: None,
                        diagnostic_note: Some(reason.clone()),
                        typed_terminal: None,
                    },
                )?;
                Ok(echo_agent::tasks::RuntimeTaskResolutionRequest::Blocked {
                    error: reason,
                    disposition,
                })
            }
            CompletionAssessment::Executed => {
                if !echo_agent::tasks::RuntimeDagController::claim_is_current(
                    self, run_id, &task.id, &claim,
                )
                .await?
                {
                    return Ok(echo_agent::tasks::RuntimeTaskResolutionRequest::Failed {
                        error: "dispatch completed after its claim was superseded".to_string(),
                    });
                }
                let summary = result.summary.clone();
                let review_output = if full_output.trim().is_empty() {
                    summary.as_str()
                } else {
                    full_output.as_str()
                };
                let review = run_review_gate(
                    self.blocking.clone(),
                    self.reviewer_llm.clone(),
                    run_id,
                    &task,
                    review_output,
                )
                .await;
                let (block_reason, review_candidate) = match review {
                    ReviewGateOutcome::Pass(review) => (None, review),
                    ReviewGateOutcome::NeedsFix(_fix_task, review) => (
                        Some("review needs fix; awaiting explicit retry".to_string()),
                        Some(review),
                    ),
                    ReviewGateOutcome::Suspend { reason, review } => {
                        (Some(format!("review suspended: {reason}")), review)
                    }
                    ReviewGateOutcome::Skipped => (
                        Some("reviewer unavailable; blocked pending LLM".to_string()),
                        None,
                    ),
                };
                if let Some(reason) = block_reason {
                    let disposition = self.review_stop_disposition(run_id).await?;
                    self.stage_resolution_metadata(
                        &claim.claim_id,
                        RuntimeTaskProductSettlement {
                            summary: Some(reason.clone()),
                            execution_summary: Some(task_execution_summary_candidate(
                                run_id,
                                &task,
                                result,
                                suggested_tasks,
                                vec![reason.clone()],
                            )),
                            review: review_candidate,
                            diagnostic_note: Some(reason.clone()),
                            typed_terminal: None,
                        },
                    )?;
                    return Ok(echo_agent::tasks::RuntimeTaskResolutionRequest::Blocked {
                        error: reason,
                        disposition,
                    });
                }

                if !echo_agent::tasks::RuntimeDagController::claim_is_current(
                    self, run_id, &task.id, &claim,
                )
                .await?
                {
                    return Ok(echo_agent::tasks::RuntimeTaskResolutionRequest::Failed {
                        error: "review completed after its claim was superseded".to_string(),
                    });
                }
                let execution_id = subagent_execution_id(run_id, &task.id, &claim);
                match integrate_reviewed_task(
                    self.dispatcher.clone(),
                    self.store.clone(),
                    self.blocking.clone(),
                    run_id,
                    &task,
                    &execution_id,
                    &summary,
                    self.cancel.clone(),
                    self.trace_sink.clone(),
                )
                .await
                {
                    Ok((completion_summary, changed_files)) => {
                        if !changed_files.is_empty() {
                            result.touched_files.written = changed_files;
                        }
                        let execution_summary = task_execution_summary_candidate(
                            run_id,
                            &task,
                            result,
                            suggested_tasks,
                            vec![completion_summary.clone()],
                        );
                        self.stage_resolution_metadata(
                            &claim.claim_id,
                            RuntimeTaskProductSettlement {
                                summary: Some(completion_summary),
                                execution_summary: Some(execution_summary),
                                review: review_candidate,
                                diagnostic_note: None,
                                typed_terminal: None,
                            },
                        )?;
                        Ok(echo_agent::tasks::RuntimeTaskResolutionRequest::Completed)
                    }
                    Err(error) => {
                        let error = format!("worktree integration failed: {error}");
                        result.status = SubagentRunStatus::Failed;
                        if !result.remaining_work.contains(&error) {
                            result.remaining_work.push(error.clone());
                        }
                        self.stage_resolution_metadata(
                            &claim.claim_id,
                            RuntimeTaskProductSettlement {
                                summary: Some(error.clone()),
                                execution_summary: Some(task_execution_summary_candidate(
                                    run_id,
                                    &task,
                                    result,
                                    suggested_tasks,
                                    vec![error.clone()],
                                )),
                                review: review_candidate,
                                diagnostic_note: Some(error.clone()),
                                typed_terminal: None,
                            },
                        )?;
                        Ok(echo_agent::tasks::RuntimeTaskResolutionRequest::Failed { error })
                    }
                }
            }
        }
    }

    async fn settle_resolution(
        &self,
        run_id: &str,
        claim: &echo_agent::tasks::TaskClaim,
        runtime_task: &echo_agent::tasks::Task,
        request: echo_agent::tasks::RuntimeTaskResolutionRequest,
    ) -> echo_agent::error::Result<echo_agent::tasks::RuntimeTaskResolution> {
        let product = self
            .resolution_metadata
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&claim.claim_id)
            .unwrap_or_default();
        let typed_timeout = matches!(
            product
                .typed_terminal
                .as_ref()
                .map(|failure| failure.terminal_kind),
            Some(echo_agent::error::AgentTerminalKind::TimedOut)
        );
        let committed_payload = product
            .execution_summary
            .as_ref()
            .map(|summary| {
                serde_json::json!({
                    "terminal_status": summary.result.status.as_str(),
                    "summary": &summary.result.summary,
                    "artifacts": &summary.result.artifacts,
                    "verification": &summary.result.verification,
                    "remaining_work": &summary.result.remaining_work,
                    "touched_files": &summary.result.touched_files,
                    "agent_failure": &product.typed_terminal,
                })
            })
            .unwrap_or_else(|| serde_json::json!({}));
        let run_id = run_id.to_string();
        let task_id = runtime_task.spec.id.clone();
        let agent_role = Self::plan_task(runtime_task)?.agent_role;
        let claim = claim.clone();
        let (outcome, run) = self
            .blocking
            .run("settle runtime task resolution", move |store| {
                let outcome = store
                    .settle_runtime_task_resolution(&run_id, &task_id, &claim, request, product)?;
                let run = store.get_run(&run_id)?;
                Ok((outcome, run))
            })
            .await?;
        let terminal_event = match &outcome {
            echo_agent::tasks::RuntimeTaskResolution::Completed => {
                Some(RuntimeEventKind::TaskCompleted)
            }
            echo_agent::tasks::RuntimeTaskResolution::Skipped => {
                Some(RuntimeEventKind::TaskSkipped)
            }
            echo_agent::tasks::RuntimeTaskResolution::Failed { .. } if typed_timeout => {
                Some(RuntimeEventKind::TaskTimedOut)
            }
            echo_agent::tasks::RuntimeTaskResolution::Failed { .. } => {
                Some(RuntimeEventKind::TaskFailed)
            }
            echo_agent::tasks::RuntimeTaskResolution::Blocked { .. } => {
                Some(RuntimeEventKind::TaskBlocked)
            }
            echo_agent::tasks::RuntimeTaskResolution::Cancelled => {
                Some(RuntimeEventKind::TaskCancelled)
            }
            echo_agent::tasks::RuntimeTaskResolution::Pending
            | echo_agent::tasks::RuntimeTaskResolution::Superseded => None,
        };
        if let Some(run) = run
            && let Some(terminal_event) = terminal_event
        {
            emit_exec(
                self.trace_sink.as_ref(),
                ExecEvent::task(
                    run.workspace_id,
                    run.conversation_id,
                    run.run_id,
                    runtime_task.spec.id.clone(),
                    terminal_event,
                    committed_payload,
                )
                .with_agent(agent_role),
            );
        }
        Ok(outcome)
    }

    async fn abandon_claim(
        &self,
        run_id: &str,
        claim: &echo_agent::tasks::TaskClaim,
        runtime_task: &echo_agent::tasks::Task,
        abandonment: echo_agent::tasks::RuntimeClaimAbandonment,
    ) -> echo_agent::error::Result<echo_agent::tasks::RuntimeTaskSettlementOutcome> {
        let (status, summary) = match abandonment {
            echo_agent::tasks::RuntimeClaimAbandonment::Interrupted { disposition } => {
                match disposition {
                    echo_agent::tasks::RuntimeInterruptionDisposition::Cancelled => (
                        echo_agent::tasks::TaskStatus::Cancelled,
                        "dispatch cancelled before resolution".to_string(),
                    ),
                    echo_agent::tasks::RuntimeInterruptionDisposition::Paused { reason } => (
                        echo_agent::tasks::TaskStatus::Paused(reason.clone()),
                        reason,
                    ),
                }
            }
            echo_agent::tasks::RuntimeClaimAbandonment::Failed { error } => {
                (echo_agent::tasks::TaskStatus::Failed(error.clone()), error)
            }
        };
        let terminal_event = match &status {
            echo_agent::tasks::TaskStatus::Cancelled => RuntimeEventKind::TaskCancelled,
            echo_agent::tasks::TaskStatus::Paused(_) => RuntimeEventKind::TaskBlocked,
            echo_agent::tasks::TaskStatus::Failed(_) => RuntimeEventKind::TaskFailed,
            _ => RuntimeEventKind::TaskFailed,
        };
        let payload = serde_json::json!({ "summary": &summary });
        let agent_role = Self::plan_task(runtime_task)?.agent_role;
        let run_id = run_id.to_string();
        let task_id = runtime_task.spec.id.clone();
        let claim = claim.clone();
        let (outcome, run) = self
            .blocking
            .run("settle abandoned runtime task claim", move |store| {
                let outcome = store.settle_runtime_task_claim(
                    &run_id,
                    &task_id,
                    &claim,
                    status,
                    Some(summary),
                )?;
                let run = store.get_run(&run_id)?;
                Ok((outcome, run))
            })
            .await?;
        if outcome == echo_agent::tasks::RuntimeTaskSettlementOutcome::Settled
            && let Some(run) = run
        {
            emit_exec(
                self.trace_sink.as_ref(),
                ExecEvent::task(
                    run.workspace_id,
                    run.conversation_id,
                    run.run_id,
                    runtime_task.spec.id.clone(),
                    terminal_event,
                    payload,
                )
                .with_agent(agent_role),
            );
        }
        Ok(outcome)
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
            self.review_stop_disposition(run_id).await
        }
    }

    async fn interruption_disposition(
        &self,
        run_id: &str,
    ) -> echo_agent::error::Result<echo_agent::tasks::RuntimeInterruptionDisposition> {
        let run_id = run_id.to_string();
        let run = self
            .blocking
            .run("load runtime interruption intent", move |store| {
                store
                    .get_run(&run_id)?
                    .ok_or(StoreError::RunNotFound(run_id))
            })
            .await?;
        Ok(if run.status == TaskRunStatus::Paused {
            echo_agent::tasks::RuntimeInterruptionDisposition::Paused {
                reason: "paused by user".to_string(),
            }
        } else {
            echo_agent::tasks::RuntimeInterruptionDisposition::Cancelled
        })
    }

    async fn settle_interruption(
        &self,
        run_id: &str,
        expected_revision: u64,
        disposition: echo_agent::tasks::RuntimeInterruptionDisposition,
    ) -> echo_agent::error::Result<echo_agent::tasks::RuntimeInterruptionSettlementOutcome> {
        let run_id = run_id.to_string();
        self.blocking
            .run("settle runtime task interruption", move |store| {
                store.settle_runtime_task_interruption(&run_id, expected_revision, disposition)
            })
            .await
    }

    async fn note_stalled(&self, run_id: &str, reason: &str) -> echo_agent::error::Result<()> {
        self.note(run_id, None, reason.to_string()).await
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
    let blocking = TaskRuntimeBlockingAdapter::new(store.clone());
    let controller = Arc::new(EkoRuntimeDagController {
        store,
        blocking: blocking.clone(),
        dispatcher: Arc::new(dispatcher),
        reviewer_llm,
        write_sem: Arc::new(Semaphore::new(limits.max_concurrent_writes.max(1))),
        shell_sem: Arc::new(Semaphore::new(limits.max_concurrent_shells.max(1))),
        llm_sem: Arc::new(Semaphore::new(limits.max_parallel_llm_calls.max(1))),
        file_write_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        trace_sink,
        cancel: parent_cancel.clone(),
        resolution_metadata: std::sync::Mutex::new(HashMap::new()),
        dispatch_failures: std::sync::Mutex::new(HashMap::new()),
    });
    let runtime_tasks = echo_agent::tasks::RuntimeTaskService::new(
        controller,
        echo_agent::tasks::RuntimeTaskServiceConfig {
            max_concurrent_subagents: limits.max_concurrent_subagents,
            ..Default::default()
        },
    );
    let outcome = runtime_tasks
        .execute(run_id, parent_cancel)
        .await
        .map_err(|error| ExecError::Other(error.to_string()))?;
    let terminal_status = match &outcome {
        echo_agent::tasks::RuntimeDagOutcome::Failed { .. } => Some(TaskRunStatus::Failed),
        echo_agent::tasks::RuntimeDagOutcome::Stalled { .. } => Some(TaskRunStatus::Failed),
        echo_agent::tasks::RuntimeDagOutcome::Paused { .. } => Some(TaskRunStatus::Paused),
        echo_agent::tasks::RuntimeDagOutcome::Cancelled => Some(TaskRunStatus::Cancelled),
        echo_agent::tasks::RuntimeDagOutcome::Completed => None,
    };
    if let Some(status) = terminal_status {
        let transition_run_id = run_id.to_string();
        blocking
            .run("transition runtime task run", move |store| {
                let current = store
                    .get_run(&transition_run_id)?
                    .ok_or_else(|| StoreError::RunNotFound(transition_run_id.clone()))?;
                if current.status == status {
                    Ok(current)
                } else {
                    store.transition_run(&transition_run_id, status)
                }
            })
            .await
            .map_err(|error| ExecError::Other(error.to_string()))?;
    }
    Ok(match outcome {
        echo_agent::tasks::RuntimeDagOutcome::Completed => RunOutcome::Completed,
        echo_agent::tasks::RuntimeDagOutcome::Failed {
            failed_task_id,
            error,
        } => RunOutcome::Failed {
            failed_task_id: Some(failed_task_id),
            error,
        },
        echo_agent::tasks::RuntimeDagOutcome::Paused { task_id, reason } => RunOutcome::Paused {
            failed_task_id: task_id,
            error: reason,
        },
        echo_agent::tasks::RuntimeDagOutcome::Stalled { reason } => RunOutcome::Failed {
            failed_task_id: None,
            error: reason,
        },
        echo_agent::tasks::RuntimeDagOutcome::Cancelled => RunOutcome::Cancelled,
    })
}

#[allow(clippy::too_many_arguments)]
async fn integrate_reviewed_task<W: TaskDispatcher + 'static>(
    dispatcher: Arc<W>,
    store: Arc<TaskRuntimeStore>,
    blocking: TaskRuntimeBlockingAdapter,
    run_id: &str,
    task: &PlanTask,
    execution_id: &str,
    summary: &str,
    cancel: CancellationToken,
    trace_sink: Option<ExecSink>,
) -> Result<(String, Vec<String>), String> {
    let integration = match dispatcher
        .integrate(
            store.clone(),
            blocking.clone(),
            run_id.to_string(),
            task.clone(),
            execution_id.to_string(),
            cancel,
            trace_sink,
        )
        .await
    {
        Ok(integration) => integration,
        Err(error) => return Err(error),
    };
    let Some(integration) = integration else {
        return Ok((summary.to_string(), Vec::new()));
    };

    let integration_summary = integration.summary();
    let changed_files = integration.changed_files.clone();
    Ok((format!("{summary} | {integration_summary}"), changed_files))
}

/// Outcome of the review gate over a freshly-completed task.
#[allow(clippy::large_enum_variant)] // PlanTask is Clone and short-lived in the review path; Box would add indirection with no win
enum ReviewGateOutcome {
    /// Task passed review (or is read-only and self-reviewing). Mark Completed.
    Pass(Option<ReviewResult>),
    /// Review found fixable issues. The claim-bound review candidate is
    /// published with a typed Blocked settlement; only an explicit retry may
    /// restart the task.
    NeedsFix(PlanTask, ReviewResult),
    /// Circuit breaker tripped (retry budget exhausted or repeated fingerprint).
    /// The run should be Suspended.
    Suspend {
        reason: String,
        review: Option<ReviewResult>,
    },
    /// No reviewer LLM configured. M7 requires a stop rather than auto-pass.
    Skipped,
}

/// Run the review gate for a task that just finished executing. Read-only
/// kinds auto-pass; implementation/debugging kinds are reviewed by the LLM
/// (when available) against the domain checklist. Applies the circuit
/// breaker on NeedsFix/Blocked outcomes.
async fn run_review_gate(
    blocking: TaskRuntimeBlockingAdapter,
    reviewer_llm: Option<Arc<dyn echo_agent::llm::LlmClient>>,
    run_id: &str,
    task: &PlanTask,
    subagent_output: &str,
) -> ReviewGateOutcome {
    // Skip the LLM gate when the task declares no acceptance criteria
    // AND is not an implementation/debugging kind (those are always gated
    // because prose about mutations cannot be trusted).
    if !super::review::requires_review(task) {
        return ReviewGateOutcome::Pass(None);
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
        match super::review::review_task(&llm, run_id, task, subagent_output).await {
            Ok(review) => break review,
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
                return ReviewGateOutcome::Suspend {
                    reason,
                    review: None,
                };
            }
        }
    };

    match review.outcome {
        ReviewOutcome::Pass => ReviewGateOutcome::Pass(Some(review)),
        ReviewOutcome::NeedsFix => {
            let prior_run_id = review.run_id.clone();
            let prior_task_id = task.id.clone();
            let mut prior = match blocking
                .run("load runtime task review history", move |store| {
                    store.list_reviews(&prior_run_id, &prior_task_id)
                })
                .await
            {
                Ok(prior) => prior,
                Err(error) => {
                    return ReviewGateOutcome::Suspend {
                        reason: format!("review history unavailable: {error}"),
                        review: Some(review),
                    };
                }
            };
            prior.push(review.clone());
            match super::review::circuit_breaker_action_from_prior(task, &review, &prior, 2) {
                super::review::BreakerAction::CreateFix => ReviewGateOutcome::NeedsFix(
                    super::review::build_fix_task(task, &review),
                    review,
                ),
                super::review::BreakerAction::Suspend { reason } => ReviewGateOutcome::Suspend {
                    reason,
                    review: Some(review),
                },
            }
        }
        ReviewOutcome::Blocked => ReviewGateOutcome::Suspend {
            reason: "review returned blocked".to_string(),
            review: Some(review),
        },
    }
}

/// Execute a single task through a selected Subagent or the primary Agent.
///
/// The framework executor holds the per-run Subagent permit; the dispatcher
/// also holds EKO's process permit. This function enforces the same two-level
/// write/shell/LLM limits plus file ownership.
#[allow(clippy::too_many_arguments)] // store + semaphores + locks + sinks all thread through
async fn execute_task(
    store: Arc<TaskRuntimeStore>,
    blocking: TaskRuntimeBlockingAdapter,
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
    workspace_io: Option<crate::state::WorkspaceIoInvocation>,
) -> TaskDispatchResult {
    let task_id = task.id.clone();
    let is_write = !task.kind.is_read_only();
    let load_run_id = run_id.clone();
    let run_context = blocking
        .run("load dispatch run identity", move |store| {
            store
                .get_run(&load_run_id)?
                .ok_or(StoreError::RunNotFound(load_run_id))
        })
        .await
        .map_err(|error| {
            TaskDispatchFailure::failed(
                task_id.clone(),
                format!("failed to load TaskRun identity: {error}"),
            )
        })?;
    let workspace_id = run_context.workspace_id.clone();
    let conversation_id = run_context.conversation_id.clone();
    let root_message_id = run_context.root_message_id.clone();

    // ── U1c phase-1 CP B: per-task unattended preflight ──
    // Re-check the task (kind + tools + shell) before acquiring permits.
    // Chat runs (Attended) skip this; only Unattended runs are checked.
    // Terminal fail on violation — never Paused, never awaits a human.
    {
        let attended_mode = run_context.attended_mode;
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
    let claim_revision = claim.revision;
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
        &workspace_id,
        &conversation_id,
        &run_id,
        &execution_id,
        &task,
        &contract,
    );

    // Acquire EKO product-resource permits with cancel awareness:
    // - Read-only tasks need no additional write/shell permit; the framework
    //   and EKO process Subagent permits are already held by the dispatcher.
    // - Write tasks (implementation/debugging) take the write permit.
    // - Verification tasks (shell/build/test) take the write permit + the shell
    //   permit (default 1, plan §678-680 shell_concurrency = 1).
    let is_shell = matches!(task.kind, PlanTaskKind::Verification);
    let (_process_write_permit, _write_permit, _process_shell_permit, _shell_permit) = if is_shell {
        tracing::info!(
            run_id = %run_id,
            task_id = %task_id,
            available = write_sem.available_permits(),
            "task_runtime: waiting for write permit"
        );
        let process_wp = tokio::select! {
            biased;
            _ = task_cancel.cancelled() => return Err(TaskDispatchFailure::cancelled(task_id.clone(), "cancelled while waiting for process write permit")),
            p = PROCESS_EXECUTION_GOVERNOR.write.acquire() => p.map_err(|e| TaskDispatchFailure::failed(task_id.clone(), e.to_string()))?,
        };
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
        let process_sp = tokio::select! {
            biased;
            _ = task_cancel.cancelled() => return Err(TaskDispatchFailure::cancelled(task_id.clone(), "cancelled while waiting for process shell permit")),
            p = PROCESS_EXECUTION_GOVERNOR.shell.acquire() => p.map_err(|e| TaskDispatchFailure::failed(task_id.clone(), e.to_string()))?,
        };
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
        (Some(process_wp), Some(wp), Some(process_sp), Some(sp))
    } else if is_write {
        tracing::info!(
            run_id = %run_id,
            task_id = %task_id,
            available = write_sem.available_permits(),
            "task_runtime: waiting for write permit"
        );
        let process_wp = tokio::select! {
            biased;
            _ = task_cancel.cancelled() => return Err(TaskDispatchFailure::cancelled(task_id.clone(), "cancelled while waiting for process write permit")),
            p = PROCESS_EXECUTION_GOVERNOR.write.acquire() => p.map_err(|e| TaskDispatchFailure::failed(task_id.clone(), e.to_string()))?,
        };
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
        (Some(process_wp), Some(wp), None, None)
    } else {
        (None, None, None, None)
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
    let _process_llm_permit = tokio::select! {
        biased;
        _ = task_cancel.cancelled() => return Err(TaskDispatchFailure::cancelled(task_id.clone(), "cancelled while waiting for process LLM permit")),
        p = PROCESS_EXECUTION_GOVERNOR.llm.acquire() => p.map_err(|e| TaskDispatchFailure::failed(task_id.clone(), e.to_string()))?,
    };
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
    let prompt_run_id = run_id.clone();
    let prompt_task = task.clone();
    let (dep_summaries, parent_goal) = blocking
        .run("load task prompt context", move |store| {
            let dependencies =
                collect_dependency_summaries(store.as_ref(), &prompt_run_id, &prompt_task)?;
            let goal = store.get_run(&prompt_run_id)?.map(|run| run.goal);
            Ok((dependencies, goal))
        })
        .await
        .map_err(|error| {
            TaskDispatchFailure::failed(
                task_id.clone(),
                format!("failed to load Subagent prompt context: {error}"),
            )
        })?;

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
    let dispatch_hooks_from_runtime = !is_read_only_task && !is_writer_task;
    // Resolve the run's root_message_id so the framework can carry it on
    // SubagentEvent::DispatchStarted → execution://event, letting the frontend
    // pin the subagent stream to the right chat message block.
    let controlled_attempt = if is_read_only_task || is_writer_task {
        let framework_executor = primary_agent
            .read(|agent| agent.subagent_executor().clone())
            .await;
        let (control_identity, framework_identity) = super::subagent_control::attempt_identity(
            &run_id,
            &task_id,
            &execution_id,
            claim_revision,
            attempt,
        )
        .map_err(|error| {
            TaskDispatchFailure::failed(
                task_id.clone(),
                format!("invalid Subagent attempt identity: {error}"),
            )
        })?;
        let assigned_run_id = run_id.clone();
        let assigned_task_id = task_id.clone();
        let assigned_execution_id = execution_id.clone();
        let assigned_role = task.agent_role.clone();
        let assigned_title = task.title.clone();
        let assigned_read_only = task.kind.is_read_only();
        let assigned_control_identity = control_identity.clone();
        let assigned_executor = framework_executor.clone();
        let guard = blocking
            .run("persist controlled Subagent assignment", move |store| {
                let guard = store.record_controlled_subagent_assigned(
                    &assigned_run_id,
                    &assigned_task_id,
                    &assigned_execution_id,
                    &assigned_role,
                    &assigned_title,
                    claim_revision,
                    attempt,
                    assigned_read_only,
                    dispatch_hooks_from_runtime,
                    assigned_executor.clone(),
                )?;
                store.deliver_pending_subagent_guidance(
                    &assigned_control_identity,
                    &assigned_executor,
                )?;
                Ok(guard)
            })
            .await
            .map_err(|error| {
                TaskDispatchFailure::failed(
                    task_id.clone(),
                    format!("failed to persist Subagent start boundary: {error}"),
                )
            })?;
        Some((framework_identity, guard))
    } else {
        let assigned_run_id = run_id.clone();
        let assigned_task_id = task_id.clone();
        let assigned_execution_id = execution_id.clone();
        let assigned_role = task.agent_role.clone();
        let assigned_title = task.title.clone();
        let assigned_read_only = task.kind.is_read_only();
        blocking
            .run("persist Subagent assignment", move |store| {
                store.record_subagent_assigned(
                    &assigned_run_id,
                    &assigned_task_id,
                    &assigned_execution_id,
                    &assigned_role,
                    &assigned_title,
                    claim_revision,
                    attempt,
                    assigned_read_only,
                    dispatch_hooks_from_runtime,
                )
            })
            .await
            .map_err(|error| {
                TaskDispatchFailure::failed(
                    task_id.clone(),
                    format!("failed to persist Subagent start boundary: {error}"),
                )
            })?;
        None
    };
    emit_subagent_started(
        trace_sink.as_ref(),
        &workspace_id,
        &run_id,
        &execution_id,
        &task,
        &contract,
        claim_revision,
        attempt,
        &conversation_id,
        Some(&root_message_id),
    );
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
            Some(&root_message_id),
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
            workspace_io.clone(),
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
                finalize_framework_subagent_result(
                    blocking.clone(),
                    &run_id,
                    &execution_id,
                    sub_result,
                )
                .await
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
            blocking.clone(),
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
            workspace_io.clone(),
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
                finalize_framework_subagent_result(
                    blocking.clone(),
                    &run_id,
                    &execution_id,
                    sub_result,
                )
                .await
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
        emit_primary_subagent_isolation_observed(
            trace_sink.as_ref(),
            &workspace_id,
            &conversation_id,
            &run_id,
            &execution_id,
            &task,
            &contract,
        );
        run_main_agent_task(
            &primary_agent,
            blocking.clone(),
            &run_id,
            &task,
            &execution_id,
            &compiled.task_input,
            task_cancel.clone(),
            trace_sink.clone(),
            workspace_io,
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
            let persisted_run_id = run_id.clone();
            let persisted_task_id = task_id.clone();
            let persisted_execution_id = execution_id.clone();
            let persisted_agent_role = task.agent_role.clone();
            let persisted_task_title = task.title.clone();
            let persisted_result = task_result.clone();
            let persisted_output = full_output.clone();
            let persisted_usage = usage.durable.clone();
            let suggestion_note = (!suggested_tasks.is_empty()).then(|| {
                let titles = suggested_tasks
                    .iter()
                    .map(|suggestion| suggestion.title.as_str())
                    .collect::<Vec<_>>()
                    .join("; ");
                format!(
                    "subagent suggested {} follow-up task(s): [{titles}]. Not auto-inserted into plan; promote via task_update if desired.",
                    suggested_tasks.len()
                )
            });
            if let Err(error) = blocking
                .run("persist successful Subagent boundary", move |store| {
                    super::ledger::archive_trace(
                        &persisted_run_id,
                        &persisted_task_id,
                        &persisted_output,
                        None,
                    );
                    super::ledger::write_progress(store.as_ref(), &persisted_run_id, None)?;
                    store.record_subagent_released(SubagentReleaseRecord {
                        run_id: &persisted_run_id,
                        task_id: &persisted_task_id,
                        execution_id: &persisted_execution_id,
                        agent_name: &persisted_agent_role,
                        task_subject: &persisted_task_title,
                        plan_revision: claim_revision,
                        attempt,
                        status: persisted_result.status.as_str(),
                        result: Some(&persisted_result),
                        full_output: Some(&persisted_output),
                        usage: Some(&persisted_usage),
                        dispatch_hook: dispatch_hooks_from_runtime,
                    })?;
                    if let Some(note) = suggestion_note {
                        store.note(&persisted_run_id, Some(&persisted_task_id), &note)?;
                    }
                    Ok(())
                })
                .await
            {
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
            let terminal_payload = serde_json::json!({
                "execution_id": &execution_id,
                "plan_revision": claim_revision,
                "attempt": attempt,
                "conversation_id": conversation_id,
                "message_id": root_message_id,
                "output": &full_output,
                "terminal_status": task_result.status.as_str(),
                "contract_version": task_result.contract_version,
                "summary": &task_result.summary,
                "artifacts": &task_result.artifacts,
                "verification": &task_result.verification,
                "remaining_work": &task_result.remaining_work,
                "touched_files": &task_result.touched_files,
                "usage": &usage.durable,
            });
            emit_exec(
                trace_sink.as_ref(),
                ExecEvent::subagent(
                    workspace_id.clone(),
                    conversation_id.clone(),
                    run_id.clone(),
                    task_id.clone(),
                    execution_id.clone(),
                    subagent_terminal_event(task_result.status),
                    terminal_payload.clone(),
                )
                .with_agent(task.agent_role.clone()),
            );
            Ok(TaskDispatchSuccess {
                task_id,
                result: task_result,
                full_output,
                suggested_tasks,
            })
        }
        Err(failure) => {
            let status = failure.status;
            let message = failure.message;
            let usage = failure.usage;
            let agent_failure = failure.agent_failure;
            let mut task_result =
                SubagentTaskResult::terminal(status, message.clone(), vec![message.clone()]);
            if let Some(agent_failure) = agent_failure.as_ref() {
                attach_agent_failure_evidence(&mut task_result, agent_failure);
            }
            let persisted_run_id = run_id.clone();
            let persisted_task_id = task_id.clone();
            let persisted_execution_id = execution_id.clone();
            let persisted_agent_role = task.agent_role.clone();
            let persisted_task_title = task.title.clone();
            let persisted_result = task_result.clone();
            let persisted_message = message.clone();
            let persisted_usage = usage.as_ref().map(|value| value.durable.clone());
            if let Err(error) = blocking
                .run("persist failed Subagent boundary", move |store| {
                    store.record_subagent_released(SubagentReleaseRecord {
                        run_id: &persisted_run_id,
                        task_id: &persisted_task_id,
                        execution_id: &persisted_execution_id,
                        agent_name: &persisted_agent_role,
                        task_subject: &persisted_task_title,
                        plan_revision: claim_revision,
                        attempt,
                        status: status.as_str(),
                        result: Some(&persisted_result),
                        full_output: Some(&persisted_message),
                        usage: persisted_usage.as_ref(),
                        dispatch_hook: dispatch_hooks_from_runtime,
                    })
                })
                .await
            {
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
                "plan_revision": claim_revision,
                "attempt": attempt,
                "conversation_id": conversation_id,
                "message_id": root_message_id,
                "error": &message,
                "terminal_status": status.as_str(),
                "contract_version": task_result.contract_version,
                "summary": &task_result.summary,
                "artifacts": &task_result.artifacts,
                "verification": &task_result.verification,
                "remaining_work": &task_result.remaining_work,
                "touched_files": &task_result.touched_files,
                "usage": usage.as_ref().map(|value| &value.durable),
                "agent_failure": &agent_failure,
            });
            emit_exec(
                trace_sink.as_ref(),
                ExecEvent::subagent(
                    workspace_id.clone(),
                    conversation_id.clone(),
                    run_id.clone(),
                    task_id.clone(),
                    execution_id.clone(),
                    subagent_terminal_event(status),
                    terminal_payload.clone(),
                )
                .with_agent(task.agent_role.clone()),
            );
            Err(TaskDispatchFailure::from_execution(
                task_id,
                ExecutionFailure {
                    status,
                    message,
                    usage,
                    agent_failure,
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
    store: &TaskRuntimeStore,
    run_id: &str,
    task: &PlanTask,
) -> Result<Vec<(String, String)>, StoreError> {
    if task.depends_on.is_empty() {
        return Ok(Vec::new());
    }
    let plan = store
        .get_plan(run_id)?
        .ok_or_else(|| StoreError::PlanNotFound(run_id.to_string()))?;
    let todos = store.list_todos(run_id)?;
    let summaries = task
        .depends_on
        .iter()
        .map(|dep_id| {
            plan.tasks
                .iter()
                .find(|dependency| &dependency.id == dep_id)
                .map_or(Ok(None), |dependency| {
                    if dependency.status != echo_agent::tasks::TaskStatus::Completed {
                        return Ok(None);
                    }
                    let todo = todos.iter().find(|todo| todo.task_id == dependency.id);
                    // Prefer the structured summary when available; fall back to
                    // the truncated todo text for tasks that predate put_summary.
                    let structured = store.get_summary(run_id, &dependency.id)?.map(|s| {
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
                        (dependency.title.clone(), parts.join(" | "))
                    });
                    Ok(structured.or_else(|| {
                        todo.and_then(|item| item.summary.as_deref())
                            .map(|s| (dependency.title.clone(), s.to_string()))
                    }))
                })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;
    Ok(summaries.into_iter().flatten().collect())
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
    workspace_id: &str,
    conversation_id: &str,
    run_id: &str,
    execution_id: &str,
    task: &PlanTask,
    contract: &SubagentRuntimeContract,
) {
    emit_exec(
        sink,
        ExecEvent::task(
            workspace_id,
            conversation_id,
            run_id,
            task.id.clone(),
            RuntimeEventKind::TaskStarted,
            runtime_contract_started_payload(contract, task, execution_id),
        )
        .with_agent(task.agent_role.clone()),
    );
}

#[allow(clippy::too_many_arguments)]
fn emit_subagent_started(
    sink: Option<&ExecSink>,
    workspace_id: &str,
    run_id: &str,
    execution_id: &str,
    task: &PlanTask,
    contract: &SubagentRuntimeContract,
    plan_revision: u64,
    attempt: u32,
    conversation_id: &str,
    message_id: Option<&str>,
) {
    let mut payload = runtime_contract_started_payload(contract, task, execution_id);
    if let serde_json::Value::Object(fields) = &mut payload {
        fields.insert("plan_revision".to_string(), plan_revision.into());
        fields.insert("attempt".to_string(), attempt.into());
        fields.insert("conversation_id".to_string(), conversation_id.into());
        if let Some(message_id) = message_id {
            fields.insert("message_id".to_string(), message_id.into());
        }
    }
    emit_exec(
        sink,
        ExecEvent::subagent(
            workspace_id,
            conversation_id,
            run_id,
            task.id.clone(),
            execution_id,
            RuntimeEventKind::Started,
            payload,
        )
        .with_agent(task.agent_role.clone()),
    );
}

fn emit_primary_subagent_isolation_observed(
    sink: Option<&ExecSink>,
    workspace_id: &str,
    conversation_id: &str,
    run_id: &str,
    execution_id: &str,
    task: &PlanTask,
    contract: &SubagentRuntimeContract,
) {
    emit_exec(
        sink,
        ExecEvent::subagent(
            workspace_id,
            conversation_id,
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
#[allow(clippy::result_large_err)]
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
    workspace_io: Option<crate::state::WorkspaceIoInvocation>,
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
            let resource_guards = workspace_io
                .as_ref()
                .map(crate::state::WorkspaceIoInvocation::resource_guards)
                .unwrap_or_default();
            Box::pin(async move {
                let runtime_context = Some(echo_agent::tools::ExternalRunContext {
                    conversation_id: None,
                    run_id: Some(run_id.clone()),
                    turn_id: message_id.clone(),
                    execution_id: Some(execution_id),
                    isolation_id: None,
                    message_id,
                    cancel: Some(Arc::new(cancel.clone())),
                    trace_sink: core_trace_sink,
                    delegation_policy: Some(delegation_policy),
                    resource_guards,
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

fn exec_trace_sink_to_core(trace_sink: Option<ExecSink>) -> Option<echo_agent::tools::TraceSinkFn> {
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
        }) as echo_agent::tools::TraceSinkFn
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
/// If EKO cannot establish the requested worktree, dispatch hard-fails.
/// rather than silently sharing the main tree.
/// Disjoint exact owners may run concurrently; the DAG scheduler separates
/// overlapping and unknown ownership before dispatch.
#[allow(clippy::too_many_arguments)] // handles + cancel + sink thread through; matches framework dispatch style
#[allow(clippy::result_large_err)]
async fn run_writer_subagent(
    primary_agent: &crate::agent_handle::AgentHandle,
    blocking: TaskRuntimeBlockingAdapter,
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
    workspace_io: Option<crate::state::WorkspaceIoInvocation>,
) -> Result<echo_agent::agent::subagent::SubagentResult, ExecutionFailure> {
    // Rebuild a multimodal Message when the run carries user attachments, so
    // the writer Subagent sees the same images/files as the primary agent would
    // (parity with run_main_agent_task, executor.rs:1373-1380).
    let load_run_id = run_id.to_string();
    let run_record = blocking
        .run("load writer Subagent attachments", move |store| {
            store.get_run(&load_run_id)
        })
        .await
        .map_err(|error| {
            ExecutionFailure::failed(format!(
                "failed to load writer Subagent attachments: {error}"
            ))
        })?;
    let root_message_id = run_record.as_ref().map(|r| r.root_message_id.clone());
    let conversation_id = run_record.as_ref().map(|r| r.conversation_id.clone());
    let run_message: Option<echo_agent::llm::types::Message> = run_record.as_ref().and_then(|r| {
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
            let resource_guards = workspace_io
                .as_ref()
                .map(crate::state::WorkspaceIoInvocation::resource_guards)
                .unwrap_or_default();
            Box::pin(async move {
                let runtime_context = Some(echo_agent::tools::ExternalRunContext {
                    conversation_id: conversation_id.clone(),
                    run_id: Some(run_id.clone()),
                    turn_id: root_message_id.clone(),
                    execution_id: Some(execution_id),
                    isolation_id: Some(isolation_id),
                    message_id: root_message_id,
                    cancel: Some(Arc::new(cancel.clone())),
                    trace_sink: core_trace_sink,
                    delegation_policy: Some(delegation_policy),
                    resource_guards,
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

fn tool_call_may_mutate_workspace(agent: &echo_agent::agent::ReactAgent, tool_name: &str) -> bool {
    if UNATTENDED_DIRECT_MUTATION_TOOLS.contains(&tool_name) {
        return true;
    }
    agent
        .tool_manager()
        .get_tool(tool_name)
        .is_some_and(|tool| {
            tool.permissions().iter().any(|permission| {
                matches!(
                    permission,
                    echo_agent::prelude::ToolPermission::Write
                        | echo_agent::prelude::ToolPermission::Execute
                )
            })
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

#[allow(clippy::too_many_arguments)]
#[allow(clippy::result_large_err)]
async fn run_main_agent_task(
    primary_agent: &crate::agent_handle::AgentHandle,
    blocking: TaskRuntimeBlockingAdapter,
    run_id: &str,
    task: &PlanTask,
    execution_id: &str,
    prompt: &str,
    cancel: CancellationToken,
    trace_sink: Option<ExecSink>,
    workspace_io: Option<crate::state::WorkspaceIoInvocation>,
) -> Result<(SubagentTaskResult, String, TaskExecutionUsage), ExecutionFailure> {
    let run_id = run_id.to_string();
    let execution_id = execution_id.to_string();

    // Preserve the user's attachments for primary verification tasks.
    let load_run_id = run_id.clone();
    let run_record = blocking
        .run("load primary task attachments", move |store| {
            store
                .get_run(&load_run_id)?
                .ok_or(StoreError::RunNotFound(load_run_id))
        })
        .await
        .map_err(|error| {
            ExecutionFailure::failed(format!("failed to load TaskRun identity: {error}"))
        })?;
    let root_message_id = Some(run_record.root_message_id.clone());
    let run_message = if run_record.attachments.is_empty() {
        None
    } else {
        crate::attachments::build_message_from_refs(prompt, &run_record.attachments).ok()
    };

    primary_agent
        .read_async(|agent| {
            let prompt = prompt.to_string();
            let run_message = run_message.clone();
            let execution_id = execution_id.clone();
            let blocking = blocking.clone();
            let run_record = run_record.clone();
            let task = task.clone();
            let working_dir = workspace_io
                .as_ref()
                .map(|scope| scope.data_root().to_path_buf());
            let resource_guards = workspace_io
                .as_ref()
                .map(crate::state::WorkspaceIoInvocation::resource_guards)
                .unwrap_or_default();
            Box::pin(async move {
                let visible_tools = crate::tool_exposure::initial_visible_tools(
                    InteractionMode::Task,
                    &agent.tool_names(),
                );
                crate::tool_exposure::record_mode_schema_budget(
                    InteractionMode::Task,
                    &agent.tool_definitions(),
                    &visible_tools,
                );
                let runtime_state_id = agent.conversation_id().map(str::to_string);
                let transcript_generation_id = runtime_state_id
                    .as_ref()
                    .filter(|runtime_state_id| {
                        Some(*runtime_state_id) != Some(&run_record.conversation_id)
                    })
                    .cloned();
                let invocation = echo_agent::agent::AgentInvocationContext {
                    history: None,
                    runtime_state_id,
                    transcript_generation_id,
                    input_lifecycle: None,
                    runtime: Some(echo_agent::tools::ExternalRunContext {
                        conversation_id: Some(run_record.conversation_id.clone()),
                        run_id: Some(run_id.clone()),
                        turn_id: root_message_id.clone(),
                        execution_id: Some(execution_id.clone()),
                        isolation_id: None,
                        message_id: root_message_id,
                        cancel: Some(Arc::new(cancel.clone())),
                        trace_sink: exec_trace_sink_to_core(trace_sink.clone()),
                        delegation_policy: None,
                        resource_guards: Vec::new(),
                    }),
                    working_dir,
                    cancel: None,
                    disabled_tools: Some(crate::tool_exposure::disabled_tools_for_mode(
                        InteractionMode::Task,
                    )),
                    visible_tools: Some(visible_tools),
                    run_budget: None,
                    resource_guards,
                };
                let event_identity = echo_agent::agent::EventIdentity::from_invocation(&invocation)
                    .map_err(|error| {
                        ExecutionFailure::from_react(error, "invalid task event identity")
                    })?;
                let replay_safe_tools = agent
                    .tool_names()
                    .into_iter()
                    .filter(|name| tool_call_is_replay_safe(agent, name))
                    .collect();
                let sink = EkoAgentTurnSink::for_primary_task(
                    &run_record,
                    &task,
                    &execution_id,
                    blocking.clone(),
                    replay_safe_tools,
                    trace_sink,
                );
                let request = match run_message {
                    Some(message) => TurnRequest::from_message(event_identity, message),
                    None => TurnRequest::new(event_identity, prompt),
                }
                .mode(TurnMode::Execute)
                .cancel(cancel)
                .invocation(invocation);
                let receipt = AgentTurnDriver.drive(agent, request, &sink).await;
                let usage = TaskExecutionUsage::from_turn_receipt(&receipt);
                let observation = sink.finish(receipt.final_answer.as_deref());

                match receipt.outcome {
                    TurnOutcome::Completed => {}
                    TurnOutcome::Cancelled => {
                        return Err(ExecutionFailure::cancelled("task cancelled").with_usage(usage));
                    }
                    TurnOutcome::Failed(failure) => {
                        let message = failure.message.clone();
                        return Err(ExecutionFailure::from_agent_failure(&failure, message)
                            .with_usage(usage));
                    }
                }

                let working_dir = agent.working_dir();
                let mut outcome = echo_agent::agent::subagent::parse_subagent_outcome(
                    &observation.output,
                    echo_agent::agent::subagent::SubagentStatus::Completed,
                    Some(&execution_id),
                    working_dir.as_deref(),
                );
                echo_agent::agent::subagent::merge_observed_evidence(
                    &mut outcome,
                    observation.observed_evidence,
                    observation.observed_artifacts,
                );
                let duration_run_id = run_id.clone();
                let duration_execution_id = execution_id.clone();
                let duration_ms = usage.duration_ms();
                blocking
                    .run("persist primary Subagent duration", move |store| {
                        store.account_subagent_usage(
                            &duration_run_id,
                            &duration_execution_id,
                            "primary_subagent_duration",
                            0,
                            0,
                            duration_ms,
                        )
                    })
                    .await
                    .map_err(|error| {
                        ExecutionFailure::failed(format!(
                            "failed to persist primary Subagent duration: {error}"
                        ))
                        .with_usage(usage.clone())
                    })?;
                Ok((
                    SubagentTaskResult::from_framework_outcome(&outcome),
                    observation.output,
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
    let Some(trace_status) = trace_run_status(status) else {
        // The framework trace schema has no Paused state. Omitting this optional
        // diagnostic record is truthful; projecting Paused as Completed is not.
        return;
    };
    let run = echo_agent::trace::Run {
        run_id: run_id.to_string(),
        parent_run_id: None,
        agent_name: "task-runtime".to_string(),
        model: String::new(),
        provider: None,
        turn_id: None,
        execution_id: None,
        session_id: conversation_id.to_string(),
        status: trace_status,
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

fn trace_run_status(status: &str) -> Option<echo_agent::trace::RunStatus> {
    match status {
        "completed" => Some(echo_agent::trace::RunStatus::Completed),
        "failed" => Some(echo_agent::trace::RunStatus::Failed),
        "cancelled" => Some(echo_agent::trace::RunStatus::Cancelled),
        "paused" => None,
        _ => None,
    }
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
        None,
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
    store.configure_run_continuation(run_id, true, true, None, None)?;

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
    workspace_io: Option<crate::state::WorkspaceIoInvocation>,
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
        workspace_io,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn drive_owned_agent_turn(
    blocking: TaskRuntimeBlockingAdapter,
    primary_agent: &crate::agent_handle::AgentHandle,
    run: &TaskRun,
    turn_id: &str,
    prompt: &str,
    cancel: CancellationToken,
    disabled_tools: HashSet<String>,
    trace_sink: Option<ExecSink>,
    workspace_io: Option<crate::state::WorkspaceIoInvocation>,
) -> Result<(TurnReceipt, EkoAgentTurnObservation), ExecError> {
    let run_id = run.run_id.clone();
    let conversation_id = run.conversation_id.clone();
    let message_id = Some(run.root_message_id.clone()).filter(|value| !value.trim().is_empty());
    let turn_id = turn_id.to_string();
    let prompt = prompt.to_string();
    let core_trace_sink = exec_trace_sink_to_core(trace_sink.clone());
    let trace_sink_for_scope = trace_sink.clone();
    let working_dir = workspace_io
        .as_ref()
        .map(|scope| scope.data_root().to_path_buf());
    let resource_guards = workspace_io
        .as_ref()
        .map(crate::state::WorkspaceIoInvocation::resource_guards)
        .unwrap_or_default();
    super::task_tools::with_run_context(
        run_id.clone(),
        cancel.clone(),
        trace_sink_for_scope,
        async {
            let agent_inner = primary_agent.inner().clone();
            let agent = agent_inner.read().await;
            let visible_tools = crate::tool_exposure::initial_visible_tools_for_profile(
                InteractionMode::Auto,
                run.domain_profile,
                &agent.tool_names(),
            );
            crate::tool_exposure::record_mode_schema_budget(
                InteractionMode::Auto,
                &agent.tool_definitions(),
                &visible_tools,
            );
            let mutating_tools: HashSet<String> = agent
                .tool_names()
                .into_iter()
                .filter(|name| tool_call_may_mutate_workspace(&agent, name))
                .collect();
            let mut disabled_tools = disabled_tools;
            disabled_tools.extend(mutating_tools.iter().cloned());
            let runtime_state_id = agent.conversation_id().map(str::to_string);
            let transcript_generation_id = runtime_state_id
                .as_ref()
                .filter(|runtime_state_id| Some(*runtime_state_id) != Some(&conversation_id))
                .cloned();
            let invocation = echo_agent::agent::AgentInvocationContext {
                history: None,
                runtime_state_id,
                transcript_generation_id,
                input_lifecycle: None,
                runtime: Some(echo_agent::tools::ExternalRunContext {
                    conversation_id: Some(conversation_id),
                    run_id: Some(run_id.clone()),
                    turn_id: Some(turn_id.clone()),
                    execution_id: None,
                    isolation_id: None,
                    message_id,
                    cancel: Some(Arc::new(cancel.clone())),
                    trace_sink: core_trace_sink,
                    delegation_policy: None,
                    resource_guards: Vec::new(),
                }),
                working_dir,
                cancel: None,
                disabled_tools: Some(disabled_tools),
                visible_tools: Some(visible_tools),
                run_budget: None,
                resource_guards,
            };
            let event_identity = echo_agent::agent::EventIdentity::from_invocation(&invocation)
                .map_err(|error| {
                    ExecError::Other(format!("invalid run agent event identity: {error}"))
                })?;
            let sink = EkoAgentTurnSink::for_run(
                run,
                &turn_id,
                blocking,
                mutating_tools,
                trace_sink.clone(),
            );
            let request = TurnRequest::new(event_identity, prompt)
                .mode(TurnMode::Execute)
                .cancel(cancel)
                .invocation(invocation);
            let receipt = AgentTurnDriver.drive(&*agent, request, &sink).await;
            let observation = sink.finish(receipt.final_answer.as_deref());
            Ok((receipt, observation))
        },
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
    workspace_io: Option<crate::state::WorkspaceIoInvocation>,
) -> Result<String, ExecError> {
    let child_cancel = parent_cancel.child_token();
    let blocking = TaskRuntimeBlockingAdapter::new(store.clone());
    let admission_run_id = run_id.to_string();
    let admission_cancel = child_cancel.clone();
    let (_cancel_registration, run_for_scope) = blocking
        .run("register agent-driven run", move |store| {
            let registration = store
                .register_run_cancellation(&admission_run_id, admission_cancel)
                .map_err(|error| StoreError::InvalidPlan(error.to_string()))?;
            let run = store
                .get_run(&admission_run_id)?
                .ok_or(StoreError::RunNotFound(admission_run_id))?;
            Ok((registration, run))
        })
        .await
        .map_err(|error| ExecError::Other(format!("register run cancellation: {error}")))?;
    let attended_mode = run_for_scope.attended_mode;
    let prompt = unattended_run_prompt(prompt, attended_mode, write_mode);
    let mut disabled_tools =
        direct_mutation_disabled_tools(attended_mode, write_mode).unwrap_or_default();
    disabled_tools.extend(crate::tool_exposure::disabled_tools_for_mode(
        InteractionMode::Auto,
    ));
    let continuation_configured = blocking
        .run("validate agent-driven continuation", {
            let run_id = run_id.to_string();
            move |store| {
                store.get_run_state(&run_id).map(|snapshot| {
                    snapshot
                        .and_then(|state| state.continuation)
                        .is_some_and(|continuation| continuation.enabled)
                })
            }
        })
        .await
        .map_err(|error| ExecError::Other(error.to_string()))?;
    if !continuation_configured {
        return Err(ExecError::Other(format!(
            "run {run_id} must configure continuation in its creation transaction"
        )));
    }

    let mut origin = RunTurnOrigin::User;
    loop {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let claim_run_id = run_id.to_string();
        let claim_turn_id = turn_id.clone();
        let claim = blocking
            .run("claim owned agent RunTurn", move |store| {
                store.claim_run_turn(
                    &claim_run_id,
                    &claim_turn_id,
                    origin,
                    TurnVisibility::Internal,
                )
            })
            .await
            .map_err(|error| ExecError::Other(error.to_string()))?;
        match claim {
            super::store::RunTurnClaimOutcome::Started(_) => {}
            super::store::RunTurnClaimOutcome::NotSubmitted(reason) => {
                return Err(ExecError::Other(format!(
                    "owned RunTurn was not submitted for {run_id}: {reason:?}"
                )));
            }
        }
        let (turn_receipt, turn_observation) = drive_owned_agent_turn(
            blocking.clone(),
            &primary_agent,
            &run_for_scope,
            &turn_id,
            &prompt,
            child_cancel.clone(),
            disabled_tools.clone(),
            trace_sink.clone(),
            workspace_io.clone(),
        )
        .await?;
        let mut terminal = turn_receipt.outcome;
        let plan_exists = blocking
            .run("inspect agent-driven run plan", {
                let run_id = run_id.to_string();
                move |store| store.get_plan(&run_id).map(|plan| plan.is_some())
            })
            .await
            .map_err(|error| ExecError::Other(error.to_string()))?;
        if matches!(&terminal, TurnOutcome::Completed)
            && !child_cancel.is_cancelled()
            && plan_policy == RunPlanPolicy::AllowDirect
            && !plan_exists
        {
            if turn_observation.mutating_tool_observed {
                terminal = TurnOutcome::Failed(echo_agent::error::AgentFailure::message(
                    "direct_mutation_requires_plan",
                    "a mutating tool was attempted outside a materialized TaskPlan",
                ));
            } else if !turn_observation.output.trim().is_empty()
                && let Err(error) =
                    materialize_direct_completion(&store, run_id, turn_observation).await
            {
                terminal = TurnOutcome::Failed(echo_agent::error::AgentFailure::message(
                    "direct_completion",
                    format!("failed to persist direct completion evidence: {error}"),
                ));
            }
        }
        if let TurnOutcome::Failed(failure) = &terminal {
            tracing::warn!(
                source_id,
                run_id,
                failure_category = ?failure.category,
                terminal_kind = ?failure.terminal_kind,
                failure_code = %failure.code,
                retryable = failure.retryable,
                http_status = ?failure.http_status,
                "Run agent emitted typed terminal failure"
            );
        }
        let terminal_record = super::turn_lifecycle::RunTurnTerminal {
            turn_id: &turn_id,
            terminal: &terminal,
            elapsed_seconds: u64::try_from(turn_receipt.elapsed.as_millis())
                .unwrap_or(u64::MAX)
                .saturating_add(999)
                / 1_000,
            final_message_id: None,
        };
        let persisted =
            super::turn_lifecycle::persist_run_turn_terminal(&blocking, run_id, &terminal_record)
                .await
                .map_err(ExecError::Other)?;
        let decision = super::turn_lifecycle::decide_after_persisted_run_turn(
            &blocking,
            &store,
            run_id,
            &terminal_record,
            persisted,
            trace_sink.as_ref(),
        )
        .await
        .map_err(ExecError::Other)?;
        if decision == super::turn_lifecycle::RunTurnDecision::Stop {
            break;
        }
        match super::continuation::await_owned_continue(&store, run_id, &child_cancel).await {
            super::continuation::OwnedContinueOutcome::Ready => {
                origin = RunTurnOrigin::Continuation;
            }
            super::continuation::OwnedContinueOutcome::Stop => break,
            super::continuation::OwnedContinueOutcome::Cancelled => {
                let cancelled_run_id = run_id.to_string();
                blocking
                    .run("cancel owned agent continuation", move |store| {
                        store.transition_run(&cancelled_run_id, TaskRunStatus::Cancelled)?;
                        store.stop_owned_command_cells(&cancelled_run_id)
                    })
                    .await
                    .map_err(|error| ExecError::Other(error.to_string()))?;
                break;
            }
            super::continuation::OwnedContinueOutcome::Shutdown => {
                let paused_run_id = run_id.to_string();
                blocking
                    .run(
                        "pause owned agent continuation for shutdown",
                        move |store| {
                            store
                                .request_pause_with_reason(
                                    &paused_run_id,
                                    RunPauseReason::BootRecovery,
                                    Some("application shutdown interrupted an owned continuation"),
                                )
                                .map(|_| ())
                        },
                    )
                    .await
                    .map_err(|error| ExecError::Other(error.to_string()))?;
                break;
            }
        }
    }

    // `task_execute`, direct completion, or the shared RunTurn lifecycle owns
    // settlement. The driver only verifies the durable result.
    let final_run_id = run_id.to_string();
    let final_status = blocking
        .run("load agent-driven run outcome", move |store| {
            store
                .get_run(&final_run_id)?
                .map(|run| run.status)
                .ok_or(StoreError::RunNotFound(final_run_id))
        })
        .await
        .map_err(|error| ExecError::Other(error.to_string()))?;

    match final_status {
        TaskRunStatus::Completed => {
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
        TaskRunStatus::Failed => {
            tracing::warn!(
                source_id = %source_id,
                fire_id = %fire_id,
                run_id = %run_id,
                "Agent-driven run failed"
            );
        }
        TaskRunStatus::Cancelled => {
            tracing::info!(
                source_id = %source_id,
                fire_id = %fire_id,
                run_id = %run_id,
                "Agent-driven run cancelled"
            );
        }
        TaskRunStatus::Paused => {
            tracing::info!(
                source_id = %source_id,
                run_id = %run_id,
                "Agent-driven run paused and remains resumable"
            );
        }
        status => {
            return Err(ExecError::Other(format!(
                "run {run_id} did not settle after its owned continuation; read back {}",
                status.as_str()
            )));
        }
    }

    let settled_run_id = run_id.to_string();
    let settled = blocking
        .run("verify agent-driven run settlement", move |store| {
            store
                .get_run(&settled_run_id)?
                .ok_or(StoreError::RunNotFound(settled_run_id))
        })
        .await
        .map_err(|error| ExecError::Other(error.to_string()))?;
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
    observation: EkoAgentTurnObservation,
) -> Result<(), ExecError> {
    let final_answer = observation.output;
    let load_run_id = run_id.to_string();
    let run = TaskRuntimeBlockingAdapter::new(store.clone())
        .run("load direct completion run", move |store| {
            store
                .get_run(&load_run_id)?
                .ok_or(StoreError::RunNotFound(load_run_id))
        })
        .await
        .map_err(|error| ExecError::Other(error.to_string()))?;
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
    let mut framework_outcome = echo_agent::agent::subagent::parse_subagent_outcome(
        &final_answer,
        echo_agent::agent::subagent::SubagentStatus::Completed,
        Some(&format!("{run_id}:direct-answer")),
        None,
    );
    echo_agent::agent::subagent::merge_observed_evidence(
        &mut framework_outcome,
        observation.observed_evidence,
        observation.observed_artifacts,
    );
    let summary = TaskExecutionSummary {
        run_id: run_id.to_string(),
        task_id: task_id.to_string(),
        subagent_name: "primary-agent".to_string(),
        result: SubagentTaskResult::from_framework_outcome(&framework_outcome),
        decisions: Vec::new(),
        next_implications: Vec::new(),
        suggested_tasks: Vec::new(),
        created_at: chrono::Utc::now(),
    };
    super::revisioned_adapter::commit_eko_direct_completion(
        store.clone(),
        plan,
        summary,
        final_answer,
    )
    .await
    .map_err(|error| ExecError::Other(format!("commit direct TaskPlan: {error}")))?;
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
        None,
    )
    .await?;
    let status_run_id = run_id.clone();
    let status = TaskRuntimeBlockingAdapter::new(store.clone())
        .run("load cron run outcome", move |store| {
            store
                .get_run(&status_run_id)?
                .map(|run| run.status)
                .ok_or(StoreError::RunNotFound(status_run_id))
        })
        .await
        .map_err(|error| ExecError::Other(error.to_string()))?;
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
    use futures::future::BoxFuture;
    use futures::stream::{self, BoxStream};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct ScriptedTurnAgent {
        script: fn() -> Vec<AgentEvent>,
    }

    struct PermissionCountingTool {
        name: String,
        permission: echo_agent::prelude::ToolPermission,
        calls: Arc<AtomicUsize>,
    }

    impl PermissionCountingTool {
        fn new(
            name: &str,
            permission: echo_agent::prelude::ToolPermission,
            calls: Arc<AtomicUsize>,
        ) -> Self {
            Self {
                name: name.to_string(),
                permission,
                calls,
            }
        }
    }

    impl echo_agent::tools::Tool for PermissionCountingTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "Dynamically registered permission test tool"
        }

        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            })
        }

        fn execute<'a>(
            &'a self,
            _parameters: echo_agent::tools::ToolParameters,
        ) -> futures::future::BoxFuture<'a, echo_agent::error::Result<echo_agent::tools::ToolResult>>
        {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {
                Ok(echo_agent::tools::ToolResult::success(
                    "dynamic tool executed",
                ))
            })
        }

        fn permissions(&self) -> Vec<echo_agent::prelude::ToolPermission> {
            vec![self.permission]
        }
    }

    impl ScriptedTurnAgent {
        fn new(script: fn() -> Vec<AgentEvent>) -> Self {
            Self { script }
        }
    }

    impl Agent for ScriptedTurnAgent {
        fn name(&self) -> &str {
            "scripted-task-turn"
        }

        fn model_name(&self) -> &str {
            "scripted-model"
        }

        fn system_prompt(&self) -> &str {
            ""
        }

        fn execute<'a>(
            &'a self,
            _task: &'a str,
        ) -> BoxFuture<'a, echo_agent::error::Result<String>> {
            Box::pin(async { Ok(String::new()) })
        }

        fn execute_stream<'a>(
            &'a self,
            _task: &'a str,
        ) -> BoxFuture<
            'a,
            echo_agent::error::Result<BoxStream<'a, echo_agent::error::Result<AgentEvent>>>,
        > {
            let events = (self.script)();
            Box::pin(async move {
                Ok(Box::pin(stream::iter(events.into_iter().map(Ok))) as BoxStream<'a, _>)
            })
        }
    }

    fn turn_identity(
        run_id: &str,
        turn_id: &str,
    ) -> Result<echo_agent::agent::EventIdentity, String> {
        echo_agent::agent::EventIdentity::for_chat(
            Some("task-turn-conversation".to_string()),
            turn_id,
            turn_id,
            Some(run_id.to_string()),
        )
        .map_err(|error| error.to_string())
    }

    async fn drive_scripted_run_turn(
        run: &TaskRun,
        turn_id: &str,
        script: fn() -> Vec<AgentEvent>,
    ) -> Result<TurnReceipt, String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let sink = EkoAgentTurnSink::for_run(
            run,
            turn_id,
            TaskRuntimeBlockingAdapter::new(store),
            HashSet::new(),
            None,
        );
        let request =
            TurnRequest::new(turn_identity(&run.run_id, turn_id)?, "test").mode(TurnMode::Execute);
        Ok(AgentTurnDriver
            .drive(&ScriptedTurnAgent::new(script), request, &sink)
            .await)
    }

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

    async fn drive_dynamic_permission_case(
        permission: echo_agent::prelude::ToolPermission,
        tool_name: &str,
        source_id: &str,
    ) -> Result<(TaskRunStatus, usize, bool), String> {
        use echo_agent::testing::MockLlmClient;

        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let calls = Arc::new(AtomicUsize::new(0));
        let mock = Arc::new(
            MockLlmClient::new()
                .with_model_name("dynamic-permission")
                .then_tool_call("dynamic-call", tool_name, r#"{"path":"README.md"}"#)
                .with_response("x".repeat(1_301)),
        );
        let agent = crate::agent_handle::AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("dynamic-permission")
                .llm_client(mock)
                .tool(Box::new(PermissionCountingTool::new(
                    tool_name,
                    permission,
                    calls.clone(),
                )))
                .build()
                .map_err(|error| error.to_string())?,
        );
        let run_id = launch_unattended_run(
            store.clone(),
            agent,
            "test",
            source_id,
            "fire-1",
            "exercise a dynamically registered tool",
            CancellationToken::new(),
            UnattendedWriteMode::Disabled,
        )
        .await
        .map_err(|error| error.to_string())?;
        let status = store
            .get_run(&run_id)
            .map_err(|error| error.to_string())?
            .map(|run| run.status)
            .ok_or_else(|| "dynamic permission run missing".to_string())?;
        let has_plan = store
            .get_plan(&run_id)
            .map_err(|error| error.to_string())?
            .is_some();
        Ok((status, calls.load(Ordering::SeqCst), has_plan))
    }

    #[tokio::test]
    async fn task_turn_driver_rejects_stream_without_terminal() -> Result<(), String> {
        fn missing_terminal() -> Vec<AgentEvent> {
            vec![AgentEvent::Token("partial output".to_string())]
        }

        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let run_id = seed_run(&store, vec![solo_readonly_task("missing-terminal")])?;
        let run = store
            .get_run(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "missing-terminal run was not created".to_string())?;
        let receipt =
            drive_scripted_run_turn(&run, "missing-terminal-turn", missing_terminal).await?;

        assert!(matches!(receipt.outcome, TurnOutcome::Failed(_)));
        assert!(receipt.final_answer.is_none());
        assert_eq!(
            TaskExecutionUsage::from_turn_receipt(&receipt)
                .durable
                .tokens_used,
            None
        );
        Ok(())
    }

    #[tokio::test]
    async fn task_turn_usage_counts_only_provider_reported_events() -> Result<(), String> {
        fn usage_script() -> Vec<AgentEvent> {
            vec![
                AgentEvent::LlmUsage {
                    model: "unknown-usage".to_string(),
                    prompt_tokens: 100,
                    completion_tokens: 200,
                    total_tokens: 300,
                    cached_prompt_tokens: 0,
                    cache_creation_prompt_tokens: 0,
                    usage_reported: false,
                },
                AgentEvent::LlmUsage {
                    model: "reported-usage".to_string(),
                    prompt_tokens: 3,
                    completion_tokens: 4,
                    total_tokens: 7,
                    cached_prompt_tokens: 1,
                    cache_creation_prompt_tokens: 0,
                    usage_reported: true,
                },
                AgentEvent::FinalAnswer("done".to_string()),
            ]
        }

        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let task = PlanTask {
            id: "reported-usage".to_string(),
            title: "Count reported usage".to_string(),
            agent_role: "primary".to_string(),
            ..PlanTask::default()
        };
        let run_id = seed_run(&store, vec![task.clone()])?;
        store
            .configure_run_continuation(&run_id, true, false, None, None)
            .map_err(|error| error.to_string())?;
        let execution_id = format!("{run_id}:reported-usage:1:1");
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
            .map_err(|error| error.to_string())?;
        let run = store
            .get_run(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "reported-usage run was not created".to_string())?;
        let sink = EkoAgentTurnSink::for_primary_task(
            &run,
            &task,
            &execution_id,
            TaskRuntimeBlockingAdapter::new(store.clone()),
            HashSet::new(),
            None,
        );
        let request = TurnRequest::new(turn_identity(&run_id, "reported-usage-turn")?, "test")
            .mode(TurnMode::Execute);
        let receipt = AgentTurnDriver
            .drive(&ScriptedTurnAgent::new(usage_script), request, &sink)
            .await;

        assert!(matches!(receipt.outcome, TurnOutcome::Completed));
        assert_eq!(receipt.prompt_tokens, 3);
        assert_eq!(receipt.completion_tokens, 4);
        assert_eq!(receipt.llm_calls, 1);
        let subagent_run = store
            .list_subagent_runs(&run_id)
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|candidate| candidate.subagent_run_id == execution_id)
            .ok_or_else(|| "reported-usage SubagentRun was not persisted".to_string())?;
        assert_eq!(subagent_run.usage.tokens_used, Some(7));
        Ok(())
    }

    #[tokio::test]
    async fn task_turn_driver_preserves_typed_provider_timeout_and_cancel() -> Result<(), String> {
        fn provider_failure() -> Vec<AgentEvent> {
            let failure = echo_agent::error::AgentFailure {
                category: echo_agent::error::AgentFailureCategory::Llm,
                terminal_kind: echo_agent::error::AgentTerminalKind::TimedOut,
                retryable: true,
                code: "llm_timeout".to_string(),
                http_status: Some(504),
                message: "provider timed out".to_string(),
            };
            vec![AgentEvent::Error {
                source: "llm".to_string(),
                message: failure.message.clone(),
                failure,
            }]
        }

        fn cancelled() -> Vec<AgentEvent> {
            vec![AgentEvent::Cancelled]
        }

        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let run_id = seed_run(&store, vec![solo_readonly_task("typed-terminal")])?;
        let run = store
            .get_run(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "typed-terminal run was not created".to_string())?;
        let failed = drive_scripted_run_turn(&run, "provider-timeout", provider_failure).await?;
        match failed.outcome {
            TurnOutcome::Failed(failure) => {
                assert_eq!(
                    failure.category,
                    echo_agent::error::AgentFailureCategory::Llm
                );
                assert_eq!(
                    failure.terminal_kind,
                    echo_agent::error::AgentTerminalKind::TimedOut
                );
                assert!(failure.retryable);
                assert_eq!(failure.code, "llm_timeout");
                assert_eq!(failure.http_status, Some(504));
            }
            other => return Err(format!("expected typed provider failure, got {other:?}")),
        }
        let cancelled = drive_scripted_run_turn(&run, "cancelled-turn", cancelled).await?;
        assert!(matches!(cancelled.outcome, TurnOutcome::Cancelled));
        Ok(())
    }

    #[test]
    fn typed_provider_timeout_requeues_then_settles_canonical_timed_out_status()
    -> Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let task = PlanTask {
            id: "provider-timeout".to_string(),
            title: "Call provider".to_string(),
            max_retries: 1,
            ..PlanTask::default()
        };
        let run_id = seed_run(&store, vec![task.clone()])?;
        let failure = echo_agent::error::AgentFailure {
            category: echo_agent::error::AgentFailureCategory::Llm,
            terminal_kind: echo_agent::error::AgentTerminalKind::TimedOut,
            retryable: true,
            code: "llm_timeout".to_string(),
            http_status: Some(504),
            message: "provider timed out".to_string(),
        };

        for expected_outcome in ["pending", "failed"] {
            let snapshot = store
                .load_runtime_plan_snapshot(&run_id)
                .map_err(|error| error.to_string())?;
            let runtime_task = snapshot
                .tasks
                .iter()
                .find(|candidate| candidate.spec.id == task.id)
                .cloned()
                .ok_or_else(|| "provider timeout task missing".to_string())?;
            let claim = match store
                .claim_runtime_task(&run_id, &runtime_task, snapshot.revision)
                .map_err(|error| error.to_string())?
            {
                echo_agent::tasks::RuntimeTaskClaimOutcome::Claimed(claim) => claim,
                echo_agent::tasks::RuntimeTaskClaimOutcome::ReloadSnapshot => {
                    return Err("provider timeout claim unexpectedly reloaded".to_string());
                }
            };
            let mut result = SubagentTaskResult::terminal(
                SubagentRunStatus::TimedOut,
                failure.message.clone(),
                vec![failure.message.clone()],
            );
            attach_agent_failure_evidence(&mut result, &failure);
            let resolution = store
                .settle_runtime_task_resolution(
                    &run_id,
                    &task.id,
                    &claim,
                    echo_agent::tasks::RuntimeTaskResolutionRequest::Requeue {
                        failure_fingerprint: Some(
                            crate::tasks::task_runtime::turn_lifecycle::agent_failure_fingerprint(
                                &failure,
                            ),
                        ),
                        error: failure.message.clone(),
                    },
                    RuntimeTaskProductSettlement {
                        summary: Some(failure.message.clone()),
                        execution_summary: Some(task_execution_summary_candidate(
                            &run_id,
                            &task,
                            result,
                            Vec::new(),
                            vec![failure.message.clone()],
                        )),
                        review: None,
                        diagnostic_note: None,
                        typed_terminal: Some(failure.clone()),
                    },
                )
                .map_err(|error| error.to_string())?;
            match expected_outcome {
                "pending" => assert_eq!(
                    resolution,
                    echo_agent::tasks::RuntimeTaskResolution::Pending
                ),
                "failed" => assert!(matches!(
                    resolution,
                    echo_agent::tasks::RuntimeTaskResolution::Failed { .. }
                )),
                _ => return Err("invalid expected timeout outcome".to_string()),
            }
        }

        let todo = store
            .list_todos(&run_id)
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|todo| todo.task_id == task.id)
            .ok_or_else(|| "provider timeout Todo missing".to_string())?;
        assert_eq!(todo.status, TodoStatus::TimedOut);
        let summary = store
            .get_summary(&run_id, &task.id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "provider timeout summary missing".to_string())?;
        assert!(summary.result.evidence.iter().any(|evidence| {
            evidence.kind == "agent_failure"
                && evidence
                    .attributes
                    .get("code")
                    .and_then(serde_json::Value::as_str)
                    == Some("llm_timeout")
        }));
        Ok(())
    }

    #[tokio::test]
    async fn dynamic_write_execute_tools_are_disabled_before_handler_dispatch() -> Result<(), String>
    {
        let write = drive_dynamic_permission_case(
            echo_agent::prelude::ToolPermission::Write,
            "read_file",
            "dynamic-write",
        )
        .await?;
        assert_eq!(write, (TaskRunStatus::Failed, 0, false));

        let execute = drive_dynamic_permission_case(
            echo_agent::prelude::ToolPermission::Execute,
            "grep",
            "dynamic-execute",
        )
        .await?;
        assert_eq!(execute, (TaskRunStatus::Failed, 0, false));

        let read = drive_dynamic_permission_case(
            echo_agent::prelude::ToolPermission::Read,
            "read_file",
            "dynamic-read",
        )
        .await?;
        assert_eq!(read, (TaskRunStatus::Completed, 1, true));
        Ok(())
    }

    #[test]
    fn unattended_worktree_mode_routes_mutations_through_formal_plans() {
        let disabled =
            direct_mutation_disabled_tools(AttendedMode::Unattended, UnattendedWriteMode::Worktree)
                .unwrap_or_default();

        assert!(disabled.contains("shell"));
        assert!(disabled.contains("apply_patch"));
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
    fn independent_runs_hide_mutations_unless_in_place_is_explicit() {
        assert!(
            direct_mutation_disabled_tools(AttendedMode::Attended, UnattendedWriteMode::Worktree,)
                .is_some_and(|disabled| disabled.contains("apply_patch"))
        );
        assert!(
            direct_mutation_disabled_tools(AttendedMode::Unattended, UnattendedWriteMode::InPlace,)
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

    #[test]
    fn paused_run_is_not_projected_as_completed_trace() {
        assert_eq!(trace_run_status("paused"), None);
        assert_eq!(
            trace_run_status("completed"),
            Some(echo_agent::trace::RunStatus::Completed)
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
            execution_target: None,
            files: Vec::new(),
            allowed_tools: tools.iter().map(|s| s.to_string()).collect(),
            required_artifacts: Vec::new(),
            execution_checks: verification.iter().map(|s| s.to_string()).collect(),
            acceptance_criteria: Vec::new(),
            retry_count: 0,
            max_retries: 0,
            failure_fingerprint: None,
            status: echo_agent::tasks::TaskStatus::Pending,
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

        emit_task_started(
            Some(&sink),
            "workspace-1",
            "conversation-1",
            "run-1",
            "task-1:1",
            &task,
            &contract,
        );
        emit_subagent_started(
            Some(&sink),
            "workspace-1",
            "run-1",
            "task-1:1",
            &task,
            &contract,
            1,
            1,
            "conversation-1",
            Some("message-1"),
        );
        emit_primary_subagent_isolation_observed(
            Some(&sink),
            "workspace-1",
            "conversation-1",
            "run-1",
            "task-1:1",
            &task,
            &contract,
        );
        emit_exec(
            Some(&sink),
            ExecEvent::subagent(
                "workspace-1",
                "conversation-1",
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
        let task = preflight_task("t1", PlanTaskKind::Investigation, &["apply_patch"], &[]);
        let result = preflight_unattended_plan(&[task], UnattendedWriteMode::Disabled);
        assert!(
            result.is_err(),
            "write tool should be rejected under Disabled"
        );
        let reason = result.unwrap_err().reason;
        assert!(
            reason.contains("apply_patch"),
            "reason should mention 'apply_patch', got {reason:?}"
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
            &["apply_patch", "shell"],
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
            &["apply_patch", "shell"],
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
        use echo_agent::testing::{MockLlmClient, MockTool};
        use std::sync::Arc;
        let shadow_root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let direct_answer = "x".repeat(1_301);
        let store = Arc::new(
            TaskRuntimeStore::new_in_memory_with_shadow_root(shadow_root.path())
                .map_err(|error| error.to_string())?,
        );
        let mock = Arc::new(
            MockLlmClient::new()
                .with_model_name("t")
                .then_tool_call("direct-read", "read_file", r#"{"path":"README.md"}"#)
                .with_response_usage(
                    direct_answer.clone(),
                    echo_agent::llm::types::Usage {
                        prompt_tokens: Some(3),
                        completion_tokens: Some(4),
                        total_tokens: Some(7),
                        ..Default::default()
                    },
                ),
        );
        let agent = crate::agent_handle::AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("t")
                .llm_client(mock)
                .tool(Box::new(
                    MockTool::new("read_file")
                        .with_parameters(serde_json::json!({
                            "type": "object",
                            "properties": { "path": { "type": "string" } },
                            "required": ["path"]
                        }))
                        .with_response("project documentation"),
                ))
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
        assert_eq!(
            run.status,
            TaskRunStatus::Completed,
            "direct run events: {:?}",
            store
                .list_events(&run_id, 0)
                .map_err(|error| error.to_string())?
        );
        let continuation = store
            .get_run_state(&run_id)
            .map_err(|error| error.to_string())?
            .and_then(|state| state.continuation)
            .ok_or_else(|| "direct completion continuation missing".to_string())?;
        assert_eq!(continuation.tokens_used, 7);
        assert_eq!(
            continuation
                .last_turn
                .as_ref()
                .map(|turn| (turn.input_tokens, turn.output_tokens)),
            Some((3, 4))
        );
        let plan = store
            .get_plan(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "direct completion plan should exist".to_string())?;
        assert_eq!(plan.tasks.len(), 1);
        assert_eq!(
            plan.tasks.first().map(|task| task.id.as_str()),
            Some("direct-answer")
        );
        let todo = store
            .list_todos(&run_id)
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|todo| todo.task_id == "direct-answer")
            .ok_or_else(|| "direct completion Todo missing".to_string())?;
        assert_eq!(todo.summary.as_deref(), Some(direct_answer.as_str()));
        let summary = store
            .get_summary(&run_id, "direct-answer")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "direct completion summary missing".to_string())?;
        assert!(
            summary.result.evidence.iter().any(|evidence| {
                evidence.kind == "file_read" && evidence.subject == "README.md"
            })
        );
        let report = store
            .completion_gate_report(&run_id)
            .map_err(|error| error.to_string())?;
        assert!(report.ready, "direct completion evidence: {report:?}");
        let journal =
            std::fs::read_to_string(shadow_root.path().join(&run_id).join("events.jsonl"))
                .map_err(|error| error.to_string())?;
        let frames = journal
            .lines()
            .map(serde_json::from_str::<serde_json::Value>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        let frame = frames
            .iter()
            .find(|frame| {
                frame
                    .get("records")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|records| {
                        records.iter().any(|record| {
                            record
                                .get("event")
                                .and_then(|event| event.get("event_type"))
                                .and_then(serde_json::Value::as_str)
                                == Some("plan_revision_committed")
                        })
                    })
            })
            .ok_or_else(|| "direct completion transaction frame missing".to_string())?;
        let event_types = frame
            .get("records")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "direct completion frame has no records".to_string())?
            .iter()
            .filter_map(|record| {
                record
                    .get("event")
                    .and_then(|event| event.get("event_type"))
                    .and_then(serde_json::Value::as_str)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            event_types,
            [
                "plan_revision_committed",
                "task_started",
                "note",
                "task_completed",
            ]
        );
        let completion_frame = frames
            .iter()
            .find(|frame| {
                frame
                    .get("records")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|records| {
                        let kinds = records
                            .iter()
                            .filter_map(|record| {
                                record
                                    .get("event")
                                    .and_then(|event| event.get("event_type"))
                                    .and_then(serde_json::Value::as_str)
                            })
                            .collect::<Vec<_>>();
                        kinds == ["run_turn_finished", "run_status_changed"]
                    })
            })
            .ok_or_else(|| {
                "RunTurn terminal and Goal completion were not committed atomically".to_string()
            })?;
        assert!(completion_frame.get("records").is_some());
        Ok(())
    }

    #[tokio::test]
    async fn owned_run_turn_uses_durable_provider_retry_before_direct_completion()
    -> Result<(), String> {
        use echo_agent::testing::MockLlmClient;

        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let mock = Arc::new(
            MockLlmClient::new()
                .with_model_name("t")
                .with_network_error("provider temporarily unavailable")
                .with_network_error("provider temporarily unavailable")
                .with_network_error("provider temporarily unavailable")
                .with_network_error("provider temporarily unavailable")
                .with_response("recovered provider answer"),
        );
        let agent = crate::agent_handle::AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("t")
                .llm_client(mock)
                .build()
                .map_err(|error| error.to_string())?,
        );
        let run_id = launch_unattended_run(
            store.clone(),
            agent,
            "test",
            "provider-retry",
            "fire-1",
            "recover from provider failure",
            CancellationToken::new(),
            UnattendedWriteMode::Disabled,
        )
        .await
        .map_err(|error| error.to_string())?;
        let run = store
            .get_run(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "provider retry run missing".to_string())?;
        assert_eq!(run.status, TaskRunStatus::Completed);
        let state = store
            .get_run_state(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "provider retry state missing".to_string())?;
        let continuation = state
            .continuation
            .ok_or_else(|| "provider retry continuation missing".to_string())?;
        assert!(continuation.provider_retry.is_none());
        assert!(continuation.next_turn_ordinal >= 2);
        let events = store
            .list_events(&run_id, 0)
            .map_err(|error| error.to_string())?;
        assert!(
            events
                .iter()
                .any(|event| { event.event_type == RuntimeEventKind::RunProviderRetryScheduled })
        );
        assert!(events.iter().any(|event| {
            event.event_type == RuntimeEventKind::RunTurnFinished
                && event
                    .payload
                    .get("agent_failure")
                    .and_then(|failure| failure.get("code"))
                    .and_then(serde_json::Value::as_str)
                    == Some("llm_network")
        }));
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event_type == RuntimeEventKind::RunTurnStarted)
                .count(),
            2
        );
        Ok(())
    }

    #[tokio::test]
    async fn direct_completion_rejects_real_apply_patch_tool_path() -> Result<(), String> {
        use echo_agent::testing::{MockLlmClient, MockTool};

        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let mock = Arc::new(
            MockLlmClient::new()
                .with_model_name("t")
                .then_tool_call(
                    "direct-write",
                    "apply_patch",
                    r#"{"path":"src/lib.rs","patch":"unsafe mutation"}"#,
                )
                .with_response("mutation attempted"),
        );
        let agent = crate::agent_handle::AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("t")
                .llm_client(mock)
                .tool(Box::new(
                    MockTool::new("apply_patch")
                        .with_parameters(serde_json::json!({
                            "type": "object",
                            "properties": {
                                "path": { "type": "string" },
                                "patch": { "type": "string" }
                            },
                            "required": ["path", "patch"]
                        }))
                        .with_response("must not execute directly"),
                ))
                .build()
                .map_err(|error| error.to_string())?,
        );
        let run_id = launch_unattended_run(
            store.clone(),
            agent,
            "test",
            "direct-write",
            "fire-1",
            "attempt a direct mutation",
            CancellationToken::new(),
            UnattendedWriteMode::Disabled,
        )
        .await
        .map_err(|error| error.to_string())?;
        let run = store
            .get_run(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "direct mutation run missing".to_string())?;
        assert_eq!(run.status, TaskRunStatus::Failed);
        assert!(
            store
                .get_plan(&run_id)
                .map_err(|error| error.to_string())?
                .is_none()
        );
        let events = store
            .list_events(&run_id, 0)
            .map_err(|error| error.to_string())?;
        assert!(events.iter().any(|event| {
            event.event_type == RuntimeEventKind::RunTurnFinished
                && event
                    .payload
                    .get("agent_failure")
                    .and_then(|failure| failure.get("code"))
                    .and_then(serde_json::Value::as_str)
                    == Some("direct_mutation_requires_plan")
        }));
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
            .configure_run_continuation(run_id, true, false, None, None)
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

    #[tokio::test(flavor = "current_thread")]
    async fn task_runtime_blocking_adapter_keeps_async_heartbeat_responsive() -> Result<(), String>
    {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let adapter = TaskRuntimeBlockingAdapter::new(store);
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let operation = tokio::spawn(async move {
            adapter
                .run("blocking adapter heartbeat test", move |_store| {
                    let _ignored = entered_tx.send(());
                    release_rx
                        .recv_timeout(std::time::Duration::from_secs(2))
                        .map_err(|error| {
                            StoreError::InvalidPlan(format!(
                                "blocking adapter test release failed: {error}"
                            ))
                        })?;
                    Ok(())
                })
                .await
        });

        tokio::time::timeout(std::time::Duration::from_secs(1), entered_rx)
            .await
            .map_err(|_| "blocking operation did not start".to_string())?
            .map_err(|_| "blocking operation start signal was dropped".to_string())?;
        let heartbeat_started = std::time::Instant::now();
        tokio::time::timeout(std::time::Duration::from_millis(250), async {
            for _ in 0..64 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "async heartbeat stalled behind TaskRuntime file I/O".to_string())?;
        if heartbeat_started.elapsed() >= std::time::Duration::from_millis(250) {
            return Err("async heartbeat did not remain responsive".to_string());
        }
        release_tx
            .send(())
            .map_err(|error| format!("failed to release blocking operation: {error}"))?;
        operation
            .await
            .map_err(|error| format!("blocking adapter task failed to join: {error}"))?
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn accepted_blocking_operation_finishes_after_caller_drop() -> Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let adapter = TaskRuntimeBlockingAdapter::new(store.clone());
        let completed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let completed_in_operation = completed.clone();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let caller = tokio::spawn(async move {
            adapter
                .run_owned("caller drop contract", move || {
                    let _ignored = entered_tx.send(());
                    release_rx
                        .recv_timeout(std::time::Duration::from_secs(2))
                        .map_err(|error| StoreError::InvalidPlan(error.to_string()))?;
                    completed_in_operation.store(true, std::sync::atomic::Ordering::SeqCst);
                    Ok(())
                })
                .await
        });
        entered_rx
            .await
            .map_err(|_| "blocking operation never started".to_string())?;
        caller.abort();
        let caller_result = caller.await;
        if !caller_result.is_err_and(|error| error.is_cancelled()) {
            return Err("blocking caller was not cancelled".to_string());
        }
        let shutdown_store = store.clone();
        let shutdown = tokio::spawn(async move { shutdown_store.shutdown_operations().await });
        tokio::task::yield_now().await;
        if shutdown.is_finished() {
            return Err("operation shutdown ignored the accepted blocking task".to_string());
        }
        release_tx
            .send(())
            .map_err(|error| format!("failed to release detached operation: {error}"))?;
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !completed.load(std::sync::atomic::Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "accepted blocking operation stopped with its caller".to_string())?;
        shutdown
            .await
            .map_err(|error| format!("operation shutdown failed to join: {error}"))??;
        if store.active_operation_count() != 0 {
            return Err("operation supervisor did not return to idle".to_string());
        }
        Ok(())
    }

    #[tokio::test]
    async fn sealed_operation_admission_cannot_revive_after_join() -> Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let adapter = TaskRuntimeBlockingAdapter::new(store.clone());
        let parked_adapter = adapter.clone();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let parked = tokio::spawn(async move {
            let _ = entered_tx.send(());
            let _ = release_rx.await;
            parked_adapter.reserve_settlement("parked settlement after seal")
        });
        entered_rx
            .await
            .map_err(|_| "parked settlement never reached its admission barrier".to_string())?;
        store.shutdown_operations().await?;
        release_tx
            .send(())
            .map_err(|_| "failed to release parked settlement".to_string())?;
        let result = parked
            .await
            .map_err(|error| format!("parked settlement task failed to join: {error}"))?;
        if !result.is_err_and(|error| error.to_string().contains("admission is closed")) {
            return Err("sealed TaskRuntime admission accepted a post-join settlement".to_string());
        }
        if store.active_operation_count() != 0 {
            return Err("post-join settlement revived TaskRuntime operation activity".to_string());
        }
        Ok(())
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
            failed_task_id: Some("t3".into()),
            error: "boom".into(),
        };
        match o {
            RunOutcome::Failed { failed_task_id, .. } => {
                assert_eq!(failed_task_id.as_deref(), Some("t3"));
            }
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
            .set_task_status(
                "r1",
                "t1",
                echo_agent::tasks::TaskStatus::Running,
                Some("code_reviewer"),
                None,
            )
            .map_err(|error| error.to_string())?;
        store
            .set_task_status(
                "r1",
                "t1",
                echo_agent::tasks::TaskStatus::Completed,
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

    struct RecordingExecutionTargetResolver {
        agent: crate::agent_handle::AgentHandle,
        calls: StdMutex<Vec<(crate::agent_router::AgentAddress, TaskExecutionTarget)>>,
    }

    #[async_trait::async_trait]
    impl super::super::execution_target::TaskExecutionTargetResolver
        for RecordingExecutionTargetResolver
    {
        async fn acquire(
            &self,
            leader: &crate::agent_router::AgentAddress,
            target: &TaskExecutionTarget,
        ) -> Result<crate::agent_pool::AgentPoolExecutionLease, String> {
            self.calls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push((leader.clone(), target.clone()));
            Ok(crate::agent_pool::AgentPoolExecutionLease::unpooled(
                self.agent.clone(),
            ))
        }
    }

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
            _blocking: TaskRuntimeBlockingAdapter,
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
                        suggested_tasks: Vec::new(),
                    }),
                    Some(Err(error)) => Err(TaskDispatchFailure::failed(task_id, error)),
                    // Default: generic success for unscripted tasks.
                    None => Ok(TaskDispatchSuccess {
                        task_id,
                        result: successful_task_result("ok"),
                        full_output: "ok".to_string(),
                        suggested_tasks: Vec::new(),
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
            _blocking: TaskRuntimeBlockingAdapter,
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
            evidence: Vec::new(),
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
                echo_agent::tasks::TaskStatus::Completed,
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
                BackgroundCellPhase::Succeeded,
                Some(BackgroundCellTerminalCause::Exited),
                None,
                Some(0),
                BackgroundCellArtifactStatus::NotRequested,
                None,
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
    async fn planned_resume_launcher_rejects_stale_journal_epoch_before_driver_start()
    -> Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let run_id = seed_run(&store, vec![solo_readonly_task("resume")])?;
        store
            .transition_run(&run_id, TaskRunStatus::Paused)
            .map_err(|error| error.to_string())?;
        let snapshot = store
            .get_run_state(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "planned resume snapshot missing".to_string())?;
        let expected = TaskRunResumeIdentity::capture(&snapshot);
        store
            .configure_run_continuation(&run_id, true, false, None, None)
            .map_err(|error| error.to_string())?;
        let agent = crate::agent_handle::AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("planned-resume-test")
                .build()
                .map_err(|error| error.to_string())?,
        );

        let error = launch_planned_run_resume(
            store.clone(),
            expected.clone(),
            agent,
            None,
            None,
            None,
            CancellationToken::new(),
            None,
        )
        .await
        .err()
        .ok_or_else(|| "stale planned resume unexpectedly launched".to_string())?;
        assert!(
            error.to_string().contains("identity changed"),
            "stale planned resume failed for the wrong reason: {error}"
        );
        store.wait_for_run_driver_idle(&run_id).await;
        assert_eq!(store.active_run_driver_count()?, 0);
        assert_eq!(store.active_run_driver_receipt_count()?, 0);
        assert_eq!(
            store
                .get_run(&run_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "planned resume run disappeared".to_string())?
                .status,
            TaskRunStatus::Paused
        );

        store
            .resume_task_run(&run_id)
            .map_err(|error| error.to_string())?;
        let running_event_count = store
            .list_events(&run_id, 0)
            .map_err(|error| error.to_string())?
            .len();
        let running_agent = crate::agent_handle::AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("planned-resume-running-test")
                .build()
                .map_err(|error| error.to_string())?,
        );
        let running_error = launch_planned_run_resume(
            store.clone(),
            expected,
            running_agent,
            None,
            None,
            None,
            CancellationToken::new(),
            None,
        )
        .await
        .err()
        .ok_or_else(|| "stale identity unexpectedly relaunched a Running run".to_string())?;
        assert!(running_error.to_string().contains("identity changed"));
        store.wait_for_run_driver_idle(&run_id).await;
        assert_eq!(
            store
                .list_events(&run_id, 0)
                .map_err(|error| error.to_string())?
                .len(),
            running_event_count
        );
        assert_eq!(
            store
                .get_run(&run_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "Running planned resume run disappeared".to_string())?
                .status,
            TaskRunStatus::Running
        );
        Ok(())
    }

    #[tokio::test]
    async fn real_dispatcher_executes_frozen_cross_workspace_target_in_leader_run()
    -> Result<(), String> {
        use echo_agent::testing::MockLlmClient;

        const REMOTE_MARKER: &str = "REMOTE_AGENT_EXECUTED";
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let target = TaskExecutionTarget {
            group_id: "group-alpha".to_string(),
            subagent_role: "verifier".to_string(),
            address: crate::agent_router::AgentAddress::new(
                crate::workspace::WorkspaceId::from_raw("ws_remote".to_string()),
                "conv_remote",
            ),
        };
        let task = PlanTask {
            id: "remote-verification".to_string(),
            title: "Verify remotely".to_string(),
            description: "Return the verification marker".to_string(),
            kind: PlanTaskKind::Verification,
            agent_role: "verifier".to_string(),
            execution_target: Some(target.clone()),
            ..PlanTask::default()
        };
        let run_id = seed_run(&store, vec![task])?;

        let remote_agent = crate::agent_handle::AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("remote-test")
                .llm_client(Arc::new(
                    MockLlmClient::new()
                        .with_model_name("remote-test")
                        .with_response(REMOTE_MARKER),
                ))
                .build()
                .map_err(|error| error.to_string())?,
        );
        let local_agent = crate::agent_handle::AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("local-test")
                .llm_client(Arc::new(
                    MockLlmClient::new()
                        .with_model_name("local-test")
                        .with_response("LOCAL_AGENT_MUST_NOT_RUN"),
                ))
                .build()
                .map_err(|error| error.to_string())?,
        );
        let resolver = Arc::new(RecordingExecutionTargetResolver {
            agent: remote_agent,
            calls: StdMutex::new(Vec::new()),
        });
        store.attach_execution_target_resolver(resolver.clone());

        let outcome = execute_runtime_plan(
            store.clone(),
            RealTaskDispatcher {
                primary_agent: local_agent,
                workspace_io: None,
            },
            None,
            &run_id,
            EkoExecutionLimits::default(),
            CancellationToken::new(),
            None,
        )
        .await
        .map_err(|error| error.to_string())?;
        assert!(matches!(outcome, RunOutcome::Completed));

        let calls = resolver
            .calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (leader, acquired_target) = calls
            .first()
            .ok_or_else(|| "cross-workspace resolver was not called".to_string())?;
        assert_eq!(leader.workspace_id.as_str(), "ws_test");
        assert_eq!(leader.conversation_id, "conv_test");
        assert_eq!(acquired_target, &target);
        drop(calls);

        let subagent_runs = store
            .list_subagent_runs(&run_id)
            .map_err(|error| error.to_string())?;
        let subagent_run = subagent_runs
            .first()
            .ok_or_else(|| "leader TaskRun has no SubagentRun".to_string())?;
        assert_eq!(subagent_runs.len(), 1);
        assert_eq!(subagent_run.run_id, run_id);
        assert_eq!(subagent_run.task_id, "remote-verification");
        assert_eq!(subagent_run.status, SubagentRunStatus::Completed);
        let result = subagent_run
            .result
            .as_ref()
            .ok_or_else(|| "SubagentRun result is missing".to_string())?;
        assert!(result.summary.contains(REMOTE_MARKER));
        assert!(!result.summary.contains("LOCAL_AGENT_MUST_NOT_RUN"));
        Ok(())
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
        let observed_statuses = Arc::new(std::sync::Mutex::new(Vec::new()));
        let status_store = store.clone();
        let status_run_id = run_id.clone();
        let captured_statuses = observed_statuses.clone();
        let trace_sink: ExecSink = Arc::new(move |event| {
            if event.event == RuntimeEventKind::TaskCompleted
                && let Ok(todos) = status_store.list_todos(&status_run_id)
                && let Some(status) = todos
                    .into_iter()
                    .find(|todo| todo.task_id == "a")
                    .map(|todo| todo.status)
                && let Ok(mut statuses) = captured_statuses.lock()
            {
                statuses.push(status);
            }
        });

        let outcome = execute_runtime_plan(
            store.clone(),
            dispatcher.clone(),
            None, // no reviewer LLM → read-only tasks auto-pass review
            &run_id,
            EkoExecutionLimits::default(),
            CancellationToken::new(),
            Some(trace_sink),
        )
        .await
        .map_err(|error| error.to_string())?;

        assert!(matches!(outcome, RunOutcome::Completed));
        let todos = store
            .list_todos(&run_id)
            .map_err(|error| error.to_string())?;
        let todo = todos.first().ok_or_else(|| "todo a missing".to_string())?;
        assert_eq!(todo.status, TodoStatus::Completed);
        assert_eq!(
            *observed_statuses
                .lock()
                .map_err(|error| error.to_string())?,
            [TodoStatus::Completed]
        );
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

        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            for gate in &cancelled_gates {
                gate.started.notified().await;
            }
            dispatcher.wait_for_returns(4).await;
        })
        .await
        .map_err(|_| "dispatch/cancellation boundary was not reached".to_string())?;
        cancel.cancel();

        let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), execution)
            .await
            .map_err(|_| "runtime cancellation timed out".to_string())?
            .map_err(|error| format!("runtime task failed to join: {error}"))?
            .map_err(|error| error.to_string())?;
        assert!(matches!(outcome, RunOutcome::Cancelled));

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
    async fn mid_wave_pause_preserves_completed_siblings_without_retry() -> Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let tasks = (0..8)
            .map(|index| solo_readonly_task(&format!("pause-task-{index}")))
            .collect::<Vec<_>>();
        let run_id = seed_run(&store, tasks.clone())?;
        let dispatcher = ScriptedDispatcher::new();
        for task in tasks.iter().take(4) {
            dispatcher.succeed(&task.id, "completed before pause");
        }
        let mut paused_gates = Vec::new();
        for task in tasks.iter().skip(4) {
            dispatcher.succeed(&task.id, "pending after pause");
            paused_gates.push(dispatcher.gate(&task.id));
        }
        let cancel = CancellationToken::new();
        let execution = {
            let run_store = store.clone();
            let run_dispatcher = dispatcher.clone();
            let run_id = run_id.clone();
            let run_cancel = cancel.clone();
            tokio::spawn(async move {
                execute_runtime_plan(
                    run_store,
                    run_dispatcher,
                    None,
                    &run_id,
                    EkoExecutionLimits {
                        max_concurrent_subagents: 8,
                        ..EkoExecutionLimits::default()
                    },
                    run_cancel,
                    None,
                )
                .await
            })
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            for gate in &paused_gates {
                gate.started.notified().await;
            }
            dispatcher.wait_for_returns(4).await;
        })
        .await
        .map_err(|_| "dispatch/pause boundary was not reached".to_string())?;
        store
            .transition_run(&run_id, TaskRunStatus::Paused)
            .map_err(|error| error.to_string())?;
        cancel.cancel();

        let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), execution)
            .await
            .map_err(|_| "runtime pause timed out".to_string())?
            .map_err(|error| format!("runtime task failed to join: {error}"))?
            .map_err(|error| error.to_string())?;
        assert!(matches!(outcome, RunOutcome::Paused { .. }));
        let plan = store
            .get_plan(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "paused plan missing".to_string())?;
        assert_eq!(
            plan.tasks
                .iter()
                .filter(|task| task.status == echo_agent::tasks::TaskStatus::Completed)
                .count(),
            4
        );
        assert_eq!(
            plan.tasks
                .iter()
                .filter(|task| { matches!(&task.status, echo_agent::tasks::TaskStatus::Paused(_)) })
                .count(),
            4
        );
        assert!(plan.tasks.iter().all(|task| task.retry_count == 0));
        assert!(plan.tasks.iter().all(|task| task.claim.is_none()));
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
                .all(|task| task.status == echo_agent::tasks::TaskStatus::Completed)
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
                assert_eq!(failed_task_id.as_deref(), Some("a"));
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
                echo_agent::tasks::TaskStatus::Running,
                Some("explorer"),
                None,
            )
            .map_err(|error| error.to_string())?;
        in_flight.status = echo_agent::tasks::TaskStatus::Running;
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
                echo_agent::tasks::TaskStatus::Completed,
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
            TaskRuntimeBlockingAdapter::new(store),
            &run_id,
            &task,
            &execution_id,
            "What is 6 times 7?",
            CancellationToken::new(),
            Some(sink),
            None,
        )
        .await
        .map_err(|error| format!("main agent task should complete: {error}"))?;

        assert!(output.1.contains("42"));
        let events = events
            .lock()
            .map_err(|error| format!("trace events lock poisoned: {error}"))?
            .clone();
        let tool_started_position = events
            .iter()
            .position(|event| event.event == RuntimeEventKind::ToolStarted)
            .ok_or_else(|| "tool_started event was not emitted".to_string())?;
        let tool_completed_position = events
            .iter()
            .position(|event| event.event == RuntimeEventKind::ToolCompleted)
            .ok_or_else(|| "tool_completed event was not emitted".to_string())?;
        assert!(
            tool_started_position < tool_completed_position,
            "tool terminal event overtook its start boundary: {events:?}"
        );
        assert!(
            events.iter().any(|event| {
                event.event == RuntimeEventKind::ToolStarted
                    && event.scope == ExecEventScope::Subagent
                    && event.task_id.as_deref() == Some("implementation-a")
                    && event.subagent_run_id.as_deref() == Some(execution_id.as_str())
                    && event
                        .payload
                        .get("invocation")
                        .and_then(|value| value.get("name"))
                        .and_then(|value| value.as_str())
                        == Some("run_code")
            }),
            "expected tool_started for run_code, got {events:?}"
        );
        assert!(
            events.iter().any(|event| {
                event.event == RuntimeEventKind::ToolCompleted
                    && event.scope == ExecEventScope::Subagent
                    && event.task_id.as_deref() == Some("implementation-a")
                    && event.subagent_run_id.as_deref() == Some(execution_id.as_str())
                    && event
                        .payload
                        .get("result")
                        .and_then(|value| value.get("success"))
                        .and_then(|value| value.as_bool())
                        == Some(true)
                    && event
                        .payload
                        .get("result")
                        .and_then(|value| value.get("output"))
                        .and_then(|value| value.as_str())
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
            evidence: Vec::new(),
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
            evidence: Vec::new(),
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
                evidence: Vec::new(),
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
                evidence: Vec::new(),
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
                evidence: Vec::new(),
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
                evidence: Vec::new(),
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
