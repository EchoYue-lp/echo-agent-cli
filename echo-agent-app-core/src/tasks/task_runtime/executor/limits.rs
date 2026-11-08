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

use echo_agent::agent::ExecutionAdmission;
use echo_agent::agent::{Agent, AgentEvent, CancellationToken};
use echo_agent::runtime::{
    AgentTurnDriver, EventSink, SinkControl, TurnMode, TurnOutcome, TurnReceipt, TurnRequest,
};
use echo_agent::subagent::{ContextTransferPolicy, SubagentInvocation};
use echo_agent::tools::cell::CommandCellResourceAdmission;
use tokio::sync::{Mutex as TokioMutex, OwnedMutexGuard, Semaphore};

/// Process-wide EKO resource ceiling shared by every workspace and TaskRun.
/// Per-run limits still apply; a dispatch must hold both permits, so opening
/// more workspace hosts cannot multiply provider or machine concurrency.
pub(crate) struct ProcessExecutionGovernor {
    write: Arc<Semaphore>,
    shell: Arc<Semaphore>,
    llm: Arc<Semaphore>,
}

static PROCESS_EXECUTION_GOVERNOR: std::sync::LazyLock<Arc<ProcessExecutionGovernor>> =
    std::sync::LazyLock::new(|| {
        let limits = EkoExecutionLimits::default();
        Arc::new(ProcessExecutionGovernor {
            write: Arc::new(Semaphore::new(limits.max_concurrent_writes.max(1))),
            shell: Arc::new(Semaphore::new(limits.max_concurrent_shells.max(1))),
            llm: Arc::new(Semaphore::new(limits.max_parallel_llm_calls.max(1))),
        })
    });

static PROCESS_SUBAGENT_ADMISSION: std::sync::LazyLock<Arc<ExecutionAdmission>> =
    std::sync::LazyLock::new(|| {
        Arc::new(ExecutionAdmission::with_capacity(
            EkoExecutionLimits::default().max_concurrent_subagents,
        ))
    });

pub(crate) fn process_subagent_admission() -> Arc<ExecutionAdmission> {
    PROCESS_SUBAGENT_ADMISSION.clone()
}

pub(crate) fn process_execution_governor() -> Arc<ProcessExecutionGovernor> {
    PROCESS_EXECUTION_GOVERNOR.clone()
}

impl ProcessExecutionGovernor {
    #[cfg(test)]
    pub(crate) fn shell_semaphore(&self) -> Arc<Semaphore> {
        self.shell.clone()
    }

    /// Product resource policy for background command cells.
    ///
    /// The framework's cell runner awaits this in the Queued phase, after the
    /// launch call has already returned its receipt. That keeps the process-wide
    /// `shell = 1` ceiling without making an admitted cell's receipt wait for a
    /// slot that a long-running command may hold for an hour.
    pub(crate) fn shell_admission(&self) -> Arc<dyn CommandCellResourceAdmission> {
        echo_agent::tools::native_execution::command_cell_resource_admission(
            self.native_shell_admission(),
        )
    }

    /// 同一进程 shell 政策供所有 Agent 托管原生命令使用。
    pub(crate) fn native_shell_admission(
        &self,
    ) -> Arc<dyn echo_agent::tools::NativeExecutionResourceAdmission> {
        Arc::new(ProcessShellResourceAdmission {
            shell: self.shell.clone(),
        })
    }

    fn snapshot(&self) -> ProcessExecutionResourceSnapshot {
        let limits = EkoExecutionLimits::default();
        ProcessExecutionResourceSnapshot {
            subagent_active: PROCESS_SUBAGENT_ADMISSION.active_count(),
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
    RuntimeTaskProductSettlement, RuntimeTaskResolutionReceipt, StoreError, SubagentReleaseRecord,
    TaskRuntimeStore,
};
use super::types::*;

/// Product resource admission for one background command cell.
///
/// Eclip caps concurrent shells at one process-wide slot. The framework cell
/// runner awaits this after the cell is accepted, so the launch receipt never
/// waits for an unrelated long-running command, while the slot is still held
/// until the admitted process drains.
struct ProcessShellResourceAdmission {
    shell: Arc<Semaphore>,
}

impl ProcessShellResourceAdmission {
    async fn acquire(
        &self,
        cancel: &CancellationToken,
        deadline: std::time::Instant,
    ) -> Result<
        echo_agent::tools::InvocationResourceGuard,
        echo_agent::tools::NativeExecutionAdmissionError,
    > {
        use echo_agent::tools::NativeExecutionAdmissionError;
        let permit = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(NativeExecutionAdmissionError::Cancelled),
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => return Err(NativeExecutionAdmissionError::Deadline),
            result = self.shell.clone().acquire_owned() => result.map_err(|_| NativeExecutionAdmissionError::Closed)?,
        };
        Ok(echo_agent::tools::InvocationResourceGuard::new(permit))
    }
}

impl echo_agent::tools::NativeExecutionResourceAdmission for ProcessShellResourceAdmission {
    fn admit<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        deadline: std::time::Instant,
    ) -> futures::future::BoxFuture<
        'a,
        Result<
            echo_agent::tools::InvocationResourceGuard,
            echo_agent::tools::NativeExecutionAdmissionError,
        >,
    > {
        Box::pin(self.acquire(cancel, deadline))
    }
}

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
            max_concurrent_subagents: 5,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "ExecEventScope")]
pub enum ExecEventScope {
    Run,
    Task,
    Subagent,
}

/// Framework-owned identity and ordering attached to an EKO Subagent event.
///
/// The application keeps this metadata separate from its workspace address so
/// replay can validate both authorities without parsing execution-id formats.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, rename = "SubagentEventMetadata")]
pub struct SubagentEventMetadata {
    pub schema_version: u16,
    pub event_id: String,
    pub content_hash: String,
    #[ts(type = "number")]
    pub sequence: u64,
    pub stream_id: String,
    pub turn_id: String,
    pub message_id: Option<String>,
    pub execution_id: String,
    pub parent_event_id: Option<String>,
    pub timestamp: String,
    pub parent_agent: String,
    pub agent_name: String,
    pub parent_execution_id: Option<String>,
    pub agent_path: Option<String>,
    pub task_id: Option<String>,
    pub attempt: Option<u32>,
    #[ts(type = "number | null")]
    pub plan_revision: Option<u64>,
}

/// A lightweight execution-flow event committed as
/// `ChatDriverEvent::Execution` for every surface.
///
/// Replaces the pre-unification trace pair. `event` is typed inside the
/// runtime and serializes to the frontend's snake_case event name. `payload`
/// carries event-specific fields
/// (`content`/`name`/`args`/...) as a flat JSON object.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, rename = "ExecEvent")]
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
    #[ts(type = "unknown")]
    pub payload: serde_json::Value,
    /// Present only for events projected from the framework's authoritative
    /// Subagent envelope. Existing EKO run/task events remain compatible.
    #[serde(default)]
    pub framework_event: Option<SubagentEventMetadata>,
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
            framework_event: None,
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
            framework_event: None,
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
            framework_event: None,
        }
    }

    /// Construct one Subagent attempt even when it is not attached to a formal
    /// PlanTask (ordinary `agent_tool` dispatches have no task id).
    pub fn subagent_attempt(
        workspace_id: impl Into<String>,
        conversation_id: impl Into<String>,
        run_id: impl Into<String>,
        task_id: Option<String>,
        subagent_run_id: impl Into<String>,
        event: RuntimeEventKind,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            workspace_id: workspace_id.into(),
            conversation_id: conversation_id.into(),
            run_id: run_id.into(),
            scope: ExecEventScope::Subagent,
            task_id,
            subagent_run_id: Some(subagent_run_id.into()),
            event,
            agent: None,
            payload,
            framework_event: None,
        }
    }

    /// Attach the agent/role name. Builder-style for call-site readability.
    pub fn with_agent(mut self, agent: impl Into<String>) -> Self {
        self.agent = Some(agent.into());
        self
    }

    pub fn with_framework_event(mut self, metadata: SubagentEventMetadata) -> Self {
        self.framework_event = Some(metadata);
        self
    }
}

#[cfg(test)]
mod exec_event_typescript_contract_tests {
    use super::{ExecEvent, ExecEventScope, RuntimeEventKind, SubagentEventMetadata};
    use ts_rs::TS;

    #[test]
    fn export_exec_event_typescript_contract() -> Result<(), String> {
        ExecEventScope::export().map_err(|error| error.to_string())?;
        RuntimeEventKind::export().map_err(|error| error.to_string())?;
        SubagentEventMetadata::export().map_err(|error| error.to_string())?;
        ExecEvent::export().map_err(|error| error.to_string())?;
        Ok(())
    }

    #[test]
    fn app_owned_exec_event_serializes_absent_framework_metadata_as_null() -> Result<(), String> {
        let value = serde_json::to_value(ExecEvent::run(
            "workspace-1",
            "conversation-1",
            "run-1",
            RuntimeEventKind::RunStarted,
            serde_json::json!({}),
        ))
        .map_err(|error| error.to_string())?;
        assert!(
            value
                .get("framework_event")
                .is_some_and(serde_json::Value::is_null)
        );
        Ok(())
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
    completion: tokio::sync::oneshot::Receiver<Result<RunOutcome, super::store::RunDriverFailure>>,
}

impl TaskRunResumeLaunch {
    pub async fn wait(self) -> Result<RunOutcome, super::store::RunDriverFailure> {
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
    launch_task_run_resume_input(
        store,
        expected,
        primary_agent,
        pool_execution,
        review_integration,
        trace_sink,
        cancel,
        workspace_io,
        None,
    )
    .await
}

/// 同一原launch owner额外携带输入历史，源接受与原expected CAS在原run锁内同batch完成。
#[cfg(feature = "channels")]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn launch_task_run_resume_with_input(
    store: Arc<TaskRuntimeStore>,
    expected: TaskRunResumeIdentity,
    primary_agent: crate::agent_handle::AgentHandle,
    pool_execution: Option<crate::agent_pool::AgentPoolExecutionLease>,
    review_integration: Option<Arc<crate::evolution::ReviewIntegration>>,
    trace_sink: Option<ExecSink>,
    cancel: CancellationToken,
    workspace_io: Option<crate::state::WorkspaceIoInvocation>,
    turn_id: String,
    input: super::types::RunInputSnapshot,
) -> Result<TaskRunResumeLaunch, StoreError> {
    launch_task_run_resume_input(
        store,
        expected,
        primary_agent,
        pool_execution,
        review_integration,
        trace_sink,
        cancel,
        workspace_io,
        Some((turn_id, input)),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn launch_task_run_resume_input(
    store: Arc<TaskRuntimeStore>,
    expected: TaskRunResumeIdentity,
    primary_agent: crate::agent_handle::AgentHandle,
    pool_execution: Option<crate::agent_pool::AgentPoolExecutionLease>,
    review_integration: Option<Arc<crate::evolution::ReviewIntegration>>,
    trace_sink: Option<ExecSink>,
    cancel: CancellationToken,
    workspace_io: Option<crate::state::WorkspaceIoInvocation>,
    input: Option<(String, super::types::RunInputSnapshot)>,
) -> Result<TaskRunResumeLaunch, StoreError> {
    let run_id = expected.run_id.clone();
    let admission = store.reserve_run_driver_admission(run_id.clone(), cancel.clone())?;
    let generation_lease = store.lease_active_workspace_generation()?;
    let registration =
        store.register_observed_run_driver::<RunOutcome>(admission, generation_lease)?;
    if let Some(snapshot) = store.get_run_state(&run_id)? {
        if let Some(binding) = snapshot.domain_binding.as_ref() {
            let verification = match pool_execution.as_ref() {
                Some(lease) if Arc::ptr_eq(primary_agent.inner(), lease.agent().inner()) => {
                    lease.verify_domain_binding(binding).await
                }
                _ => Err(
                    "domain resume requires its original target Agent and live pool lease"
                        .to_string(),
                ),
            };
            if let Err(error) = verification {
                registration.reject(error.clone());
                return Err(StoreError::InvalidPlan(error));
            }
        } else if snapshot.run.domain_profile != DomainProfile::General {
            let error =
                "historical professional run cannot resume without its original plugin binding"
                    .to_string();
            registration.reject(error.clone());
            return Err(StoreError::InvalidPlan(error));
        }
    }
    let invocation_resources = match (
        store
            .get_run_state(&run_id)?
            .and_then(|state| state.domain_binding),
        pool_execution.as_ref(),
    ) {
        (Some(binding), Some(execution)) => {
            let projection = super::compact_context::task_runtime_projection_registry()
                .retain_for_invocation(&run_id, store.clone())
                .map_err(StoreError::InvalidPlan)?;
            vec![
                execution
                    .domain_resource_guard(&run_id, &binding, Some(Arc::new(projection)))
                    .map_err(StoreError::InvalidPlan)?,
            ]
        }
        _ => Vec::new(),
    };
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
            let resumed = match input.as_ref() {
                Some((turn_id, input)) => {
                    store.resume_task_run_expected_with_input(&expected, turn_id, input)
                }
                None => store.resume_task_run_expected(&expected),
            };
            if let Err(error) = resumed {
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
                        receipt_owner.retain(generation.clone())?;
                    }
                    if let Some(execution) = pool_execution {
                        receipt_owner.retain(execution)?;
                    }
                    if has_plan {
                        let run_store =
                            primary_agent.read(|agent| agent.run_store().cloned()).await;
                        let reviewer_llm = primary_agent
                            .read(|agent| agent.llm_client().cloned())
                            .await;
                        return execute_run_with_resources(
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
                            invocation_resources,
                        )
                        .await
                        .map_err(super::store::RunDriverFailure::from);
                    }

                    drive_agent_run_with_resources(
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
                        invocation_resources,
                    )
                    .await
                    .map_err(super::store::RunDriverFailure::from)?;
                    let outcome_run_id = preparation_run_id.clone();
                    let run = TaskRuntimeOperation::new(preparation_store.clone())
                        .run("load resumed direct run outcome", move |store| {
                            store
                                .get_run(&outcome_run_id)?
                                .ok_or(StoreError::RunNotFound(outcome_run_id))
                        })
                        .await
                        .map_err(super::store::RunDriverFailure::from)?;
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
                            )
                            .into());
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
    #[error(transparent)]
    Framework(#[from] echo_agent::error::ReactError),
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

impl From<ExecError> for super::store::RunDriverFailure {
    fn from(error: ExecError) -> Self {
        match error {
            ExecError::Framework(source) => Self::from(source),
            other => Self::Error(other.to_string()),
        }
    }
}

#[cfg(test)]
impl ProcessExecutionGovernor {
    /// 独立夹具模拟一个产品实例，复用真实默认政策与准入机制。
    /// 同一实例内的 Chat/Run 仍共享容量 1；生产全局接线另走真实构造器验证。
    pub(crate) fn isolated_for_test() -> Arc<Self> {
        let limits = EkoExecutionLimits::default();
        Arc::new(Self {
            write: Arc::new(Semaphore::new(limits.max_concurrent_writes.max(1))),
            shell: Arc::new(Semaphore::new(limits.max_concurrent_shells.max(1))),
            llm: Arc::new(Semaphore::new(limits.max_parallel_llm_calls.max(1))),
        })
    }
}
