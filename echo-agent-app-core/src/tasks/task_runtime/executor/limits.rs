// EKO adapter for the framework runtime task service.
//
// Converts EKO `TaskPlan` snapshots into the framework's product-neutral task
// view, then injects EKO dispatch, review, persistence, worktree, and event
// policy. Dependency traversal, revision safe points, Subagent waves,
// cancellation, failure propagation, and stall detection live in
// `echo_orchestration::tasks::RuntimeTaskService`.
//
// - read-only tasks (read_only_review, investigation, test_plan, review,
//   summary) run concurrently up to the configured Subagent limit, each delegated
//   to a registered subagent role via `delegate_to_agent_with_cancel` (fork
//   mode → isolated instance under the executor's semaphore, NOT the primary
//   agent's execution_mutex, so they parallelize);
// - implementation / debugging tasks use ownership-safe waves and writer
//   worktrees; verification tasks run on the primary Agent against the
//   authoritative workspace;
// - the overall Subagent count is capped by the framework executor; EKO owns
//   write, shell, and LLM resource policy separately.
//
// Cancellation: each dispatched task gets a child of the parent run's
// CancellationToken. Read-only delegation propagates cancel through
// `delegate_to_agent_with_cancel`; mutating tasks race `Agent::execute`
// against the cancel token. Cancelling the run therefore cancels every
// in-flight task.
//
// Guarantees:
// - the run transitions Running → (Completed | Failed | Cancelled | Paused);
// - every task boundary writes a RuntimeTaskEvent + updates the todo projection;
// - implementation/debugging tasks pass a review gate before being marked
//   Completed; a failing review either re-queues a fix task or trips the
//   circuit breaker (Paused);
// - cancellation propagates to all in-flight tasks;
// - a failed task marks itself Failed but lets already-running siblings
//   finish (the run ends Failed); downstream tasks are skipped.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use echo_agent::subagent::{ContextTransferPolicy, SubagentInvocation};
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

fn subagent_terminal_event(status: SubagentStatus) -> RuntimeEventKind {
    match status {
        SubagentStatus::Running => RuntimeEventKind::Running,
        SubagentStatus::Completed => RuntimeEventKind::Completed,
        SubagentStatus::Failed => RuntimeEventKind::Failed,
        SubagentStatus::Cancelled => RuntimeEventKind::Cancelled,
        SubagentStatus::TimedOut => RuntimeEventKind::TimedOut,
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

pub struct TaskRunResumeLaunch {
    pub run_id: String,
    completion: tokio::sync::oneshot::Receiver<Result<RunOutcome, String>>,
}

impl TaskRunResumeLaunch {
    pub async fn wait(self) -> Result<RunOutcome, String> {
        self.completion
            .await
            .map_err(|error| format!("TaskRun resume completion channel closed: {error}"))?
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn launch_task_run_resume(
    store: Arc<TaskRuntimeStore>,
    expected: TaskRunResumeIdentity,
    primary_agent: crate::agent_handle::AgentHandle,
    pool_execution: Option<crate::agent_pool::AgentPoolExecutionLease>,
    review_integration: Option<Arc<crate::evolution::ReviewIntegration>>,
    trace_sink: Option<ExecSink>,
    cancel: CancellationToken,
    workspace_io: Option<crate::state::WorkspaceIoInvocation>,
) -> Result<TaskRunResumeLaunch, StoreError> {
    let run_id = expected.run_id.clone();
    let admission = store.reserve_run_driver_admission(run_id.clone(), cancel.clone())?;
    let generation_lease = store.lease_active_workspace_generation()?;
    let registration = store.register_run_driver::<RunOutcome>(admission, generation_lease)?;
    TaskRuntimeOperation::new(store.clone())
        .run_owned("prepare exact TaskRun resume", move || {
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
            let snapshot = store
                .get_run_state(&run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.clone()))?;
            let has_plan = store.get_plan(&run_id)?.is_some();
            if !has_plan && snapshot.execution_profile.plan_policy == RunPlanPolicy::RequirePlan {
                let error = StoreError::InvalidPlan(format!(
                    "run {run_id} has no persisted plan to resume"
                ));
                registration.reject(error.to_string());
                return Err(error);
            }
            let execution_profile = snapshot.execution_profile;
            let run_goal = snapshot.run.goal;
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
                    if has_plan {
                        let run_store = primary_agent.read(|agent| agent.run_store().cloned()).await;
                        let reviewer_llm = primary_agent
                            .read(|agent| agent.llm_client().cloned())
                            .await;
                        return execute_run(
                            preparation_store,
                            Some(primary_agent),
                            reviewer_llm,
                            memory_generation,
                            run_store,
                            trace_sink,
                            &preparation_run_id,
                            cancel,
                            super::memory_bridge::MemoryPolicy::BestEffortSettled,
                            workspace_io,
                        )
                        .await
                        .map_err(|error| error.to_string());
                    }

                    drive_agent_run(
                        preparation_store.clone(),
                        primary_agent,
                        &preparation_run_id,
                        "task_run_resume",
                        &preparation_run_id,
                        &run_goal,
                        cancel,
                        UnattendedWriteMode::Disabled,
                        execution_profile.plan_policy,
                        trace_sink,
                        workspace_io,
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                    let outcome_run_id = preparation_run_id.clone();
                    let run = TaskRuntimeOperation::new(preparation_store.clone())
                        .run("load resumed direct run outcome", move |store| {
                            store
                                .get_run(&outcome_run_id)?
                                .ok_or(StoreError::RunNotFound(outcome_run_id))
                        })
                        .await
                        .map_err(|error| error.to_string())?;
                    let outcome = match run.status {
                        TaskRunStatus::Completed => RunOutcome::Completed,
                        TaskRunStatus::Cancelled => RunOutcome::Cancelled,
                        TaskRunStatus::Paused => RunOutcome::Paused {
                            failed_task_id: None,
                            error: "agent-driven run paused".to_string(),
                        },
                        TaskRunStatus::Failed => RunOutcome::Failed {
                            failed_task_id: None,
                            error: "agent-driven run failed".to_string(),
                        },
                        status => {
                            return Err(format!(
                                "resumed direct run {} ended in non-terminal status {}",
                                preparation_run_id,
                                status.as_str()
                            ));
                        }
                    };
                    if matches!(outcome, RunOutcome::Completed | RunOutcome::Cancelled) {
                        let event = if matches!(outcome, RunOutcome::Completed) {
                            super::memory_bridge::MemoryEvent::RunCompleted {
                                run_id: preparation_run_id.clone(),
                                goal: run.goal,
                            }
                        } else {
                            super::memory_bridge::MemoryEvent::RunCancelledByUser {
                                run_id: preparation_run_id.clone(),
                                goal: run.goal,
                            }
                        };
                        super::memory_bridge::write_memory_candidate_dispatch(
                            super::memory_bridge::MemoryPolicy::BestEffortSettled,
                            memory_generation.as_ref(),
                            &preparation_store,
                            event,
                        )
                        .await;
                    }
                    Ok(outcome)
                },
            );
            Ok(TaskRunResumeLaunch { run_id, completion })
        })
        .await
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
