//! File-backed canonical store for the TaskRuntime.
//!
//! The file system (`events.jsonl` plus deterministic `plan.json` and
//! `run-state.json` projections) is the source of truth for task/plan data and
//! runtime usage. Conversation-replay events remain in memory. No SQLite
//! dependency.
//!
//! Every state mutation appends a [`RuntimeTaskEvent`] to `events.jsonl` and
//! refreshes only the affected projection through the shared checkpoint-aware
//! event fold.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use echo_agent::state::journal::{CheckpointApplyStatus, JournalDurabilityStatus};
use sha2::{Digest, Sha256};

use super::history_projection::HistoryProjectionApplyStatus;
use super::run_authority::RuntimeJournalEvent;
use super::types::*;

/// Error returned by store operations. Kept separate from `anyhow::Error`
/// so callers can distinguish invariant violations (e.g. illegal status
/// transition) from infrastructure failures.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("run not found: {0}")]
    RunNotFound(String),
    #[error("plan not found for run: {0}")]
    PlanNotFound(String),
    #[error("task not found: {0}")]
    TaskNotFound(String),
    #[error("illegal transition {from} -> {to} for run {run_id}")]
    IllegalTransition {
        run_id: String,
        from: String,
        to: String,
    },
    #[error("lock poisoned")]
    LockPoisoned,
    #[error(
        "task runtime workspace transition admission is busy ({active_operations} active operations)"
    )]
    WorkspaceTransitionBusy { active_operations: usize },
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid plan: {0}")]
    InvalidPlan(String),
    #[error("runtime task mutation: {0}")]
    RuntimeTaskMutation(#[from] echo_agent::tasks::RuntimeTaskMutationError),
    #[error("plan revision conflict for run {run_id}: expected {expected}, current {current}")]
    PlanConflict {
        run_id: String,
        expected: u64,
        current: u64,
    },
    #[error("goal revision conflict for run {run_id}: expected {expected}, current {current}")]
    GoalConflict {
        run_id: String,
        expected: u64,
        current: u64,
    },
    #[error("goal update rejected for run {run_id}: {reason}")]
    GoalUpdateRejected { run_id: String, reason: String },
    #[error("requirement skip rejected for run {run_id}: {reason}")]
    RequirementSkipRejected { run_id: String, reason: String },
    #[error(
        "plan revision {plan_revision} for run {run_id} is bound to goal revision {plan_goal_revision}, current goal revision is {run_goal_revision}"
    )]
    PlanGoalMismatch {
        run_id: String,
        plan_revision: u64,
        plan_goal_revision: u64,
        run_goal_revision: u64,
    },
    #[error("file shadow: {0}")]
    Shadow(#[from] super::file_shadow::ShadowError),
    #[error("run {run_id} has unresolved recovery barriers: {details}")]
    RecoveryBlocked { run_id: String, details: String },
    #[error("exact resume outcome is unknown for run {run_id}: {details}")]
    ResumeOutcomeUnknown { run_id: String, details: String },
    #[error("conversation {conversation_id} still has active task runs: {run_ids:?}")]
    ConversationHasActiveRuns {
        conversation_id: String,
        run_ids: Vec<String>,
    },
}

/// Bounded journal query used by application adapters that must not
/// materialize a TaskRun's complete event history. Empty `event_types` means
/// all event kinds; `execution_id` binds the query to one Subagent attempt.
#[derive(Debug, Clone)]
pub struct RuntimeEventQuery {
    pub after_sequence: i64,
    pub limit: usize,
    pub execution_id: Option<String>,
    pub event_types: Vec<RuntimeEventKind>,
}

impl RuntimeEventQuery {
    pub fn new(after_sequence: i64, limit: usize) -> Self {
        Self {
            after_sequence,
            limit,
            execution_id: None,
            event_types: Vec::new(),
        }
    }

    pub fn for_execution(mut self, execution_id: impl Into<String>) -> Self {
        self.execution_id = Some(execution_id.into());
        self
    }

    pub fn with_event_types(mut self, event_types: Vec<RuntimeEventKind>) -> Self {
        self.event_types = event_types;
        self
    }
}

/// One exact Subagent attempt plus the event metadata required by bounded
/// Agent-control projections. The `run` fields are reconstructed without
/// dropping usage or terminal result data.
#[derive(Debug, Clone)]
pub struct SubagentRunSnapshot {
    pub run: SubagentRun,
    pub plan_revision: Option<u64>,
    pub latest_event: RuntimeTaskEvent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionCommitReceipt {
    Durable { seq: i64 },
    CommittedProjectionDegraded { seq: i64, detail: String },
}

/// Canonical result of preparing a user-requested task retry while one
/// TaskRuntime generation is pinned by the accepted driver registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskRetryPreparation {
    Acceptance { next_attempt: u32 },
    Recovery,
}

/// Result of the event-sourced start-if-idle continuation claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunTurnClaimOutcome {
    Started(RunTurnSummary),
    NotSubmitted(ContinuationNotSubmittedReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContinuationNotSubmittedReason {
    Disabled,
    Deferred,
    AlreadyRunning,
    RunNotRunning,
    TokenBudgetExhausted,
    TimeBudgetExhausted,
    ProviderRetryBackoff,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderRetryDisposition {
    Scheduled(ProviderRetryState),
    Exhausted(ProviderRetryState),
}

impl ProviderRetryDisposition {
    pub fn state(&self) -> &ProviderRetryState {
        match self {
            Self::Scheduled(state) | Self::Exhausted(state) => state,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootAutoResumeBlocker {
    RunNotPaused,
    NotBootRecovery,
    ContinuationDisabled,
    AutoResumeDisabled,
    LauncherUnavailable,
    InteractiveOwnerUnavailable,
    WorkspaceMismatch,
    PlanUnavailable,
    GoalPlanMismatch,
    TokenBudgetExhausted,
    TimeBudgetExhausted,
    ActiveRunTurn,
    ActiveSubagent,
    ActiveCommandCell,
    RecoveryBlocker,
}

impl BootAutoResumeBlocker {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RunNotPaused => "run_not_paused",
            Self::NotBootRecovery => "not_boot_recovery",
            Self::ContinuationDisabled => "continuation_disabled",
            Self::AutoResumeDisabled => "auto_resume_disabled",
            Self::LauncherUnavailable => "launcher_unavailable",
            Self::InteractiveOwnerUnavailable => "interactive_owner_unavailable",
            Self::WorkspaceMismatch => "workspace_mismatch",
            Self::PlanUnavailable => "plan_unavailable",
            Self::GoalPlanMismatch => "goal_plan_mismatch",
            Self::TokenBudgetExhausted => "token_budget_exhausted",
            Self::TimeBudgetExhausted => "time_budget_exhausted",
            Self::ActiveRunTurn => "active_run_turn",
            Self::ActiveSubagent => "active_subagent",
            Self::ActiveCommandCell => "active_command_cell",
            Self::RecoveryBlocker => "recovery_blocker",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootAutoResumeDecision {
    Ready {
        retry_not_before: Option<DateTime<Utc>>,
    },
    Blocked(Vec<BootAutoResumeBlocker>),
}

#[derive(Debug, Clone)]
pub enum BootAutoResumeOutcome {
    Resumed(Box<TaskRun>),
    WaitingUntil(DateTime<Utc>),
    Blocked(Vec<BootAutoResumeBlocker>),
}

#[derive(Debug, Clone)]
pub(crate) struct InitialRunTriggerMetadata {
    pub source: String,
    pub kind: String,
    pub prompt: String,
    pub priority: u8,
    pub dependencies: Vec<String>,
}

#[allow(clippy::too_many_arguments)]
fn new_pending_run(
    run_id: &str,
    workspace_id: &str,
    conversation_id: &str,
    root_message_id: &str,
    domain_profile: DomainProfile,
    goal: &str,
    route: &str,
    attended_mode: AttendedMode,
) -> TaskRun {
    let now = Utc::now();
    TaskRun {
        run_id: run_id.to_string(),
        workspace_id: workspace_id.to_string(),
        conversation_id: conversation_id.to_string(),
        root_message_id: root_message_id.to_string(),
        domain_profile,
        status: TaskRunStatus::Pending,
        goal: goal.to_string(),
        goal_revision: 1,
        goal_sha256: task_goal_sha256(goal),
        plan_id: None,
        route: route.to_string(),
        attended_mode,
        attachments: Vec::new(),
        created_at: now,
        updated_at: now,
    }
}

struct PreparedRevisionCommit {
    next: echo_agent::tasks::RevisionedTaskGraph,
    plan: PlanRevision,
    payload: serde_json::Value,
}

fn prepare_revisioned_graph_commit(
    run_id: &str,
    run: &TaskRun,
    current: Option<&echo_agent::tasks::RevisionedTaskGraph>,
    commit: echo_agent::tasks::TaskGraphCommit,
) -> Result<PreparedRevisionCommit, StoreError> {
    let echo_agent::tasks::TaskGraphCommit {
        expected_revision,
        mut next,
        reason,
        effects,
    } = commit;
    let current_revision = current.map(|graph| graph.snapshot.revision);
    if current_revision != expected_revision {
        return Err(StoreError::PlanConflict {
            run_id: run_id.to_string(),
            expected: expected_revision.unwrap_or_default(),
            current: current_revision.unwrap_or_default(),
        });
    }
    let next_revision = expected_revision
        .unwrap_or_default()
        .checked_add(1)
        .ok_or_else(|| StoreError::InvalidPlan("plan revision overflow".to_string()))?;
    if next.snapshot.revision != next_revision {
        return Err(StoreError::InvalidPlan(format!(
            "invalid next plan revision: expected {next_revision}, got {}",
            next.snapshot.revision
        )));
    }
    let previous_task_ids = current
        .map(|graph| {
            graph
                .snapshot
                .tasks
                .iter()
                .map(|task| task.spec.id.as_str())
                .collect::<std::collections::HashSet<_>>()
        })
        .unwrap_or_default();
    let plan_metadata: EkoPlanMetadata = serde_json::from_value(next.context.metadata.clone())?;
    if next.context.goal != run.goal {
        return Err(StoreError::GoalUpdateRejected {
            run_id: run_id.to_string(),
            reason: "TaskPlan cannot modify the authoritative TaskRun Goal".to_string(),
        });
    }
    if let Some(current) = current {
        let current_metadata: EkoPlanMetadata =
            serde_json::from_value(current.context.metadata.clone())?;
        if plan_metadata.goal_revision != current_metadata.goal_revision
            || plan_metadata.goal_sha256 != current_metadata.goal_sha256
        {
            return Err(StoreError::GoalUpdateRejected {
                run_id: run_id.to_string(),
                reason: "TaskPlan Goal binding can only advance through update_run_goal"
                    .to_string(),
            });
        }
    } else if plan_metadata.goal_revision != run.goal_revision
        || plan_metadata.goal_sha256 != run.goal_sha256
    {
        return Err(StoreError::GoalUpdateRejected {
            run_id: run_id.to_string(),
            reason: "initial TaskPlan Goal binding does not match TaskRun".to_string(),
        });
    }
    if effects.reordered {
        for (position, task) in next.snapshot.tasks.iter_mut().enumerate() {
            let mut extension: EkoTaskExtension =
                serde_json::from_value(task.spec.extension.clone())?;
            extension.sort_order = i64::try_from(position).unwrap_or(i64::MAX);
            task.spec.extension = serde_json::to_value(extension)?;
        }
    }
    let mut specifications = Vec::with_capacity(next.snapshot.tasks.len());
    for task in &next.snapshot.tasks {
        if task.spec.id != task.execution.task_id {
            return Err(StoreError::InvalidPlan(format!(
                "task spec id '{}' does not match execution id '{}'",
                task.spec.id, task.execution.task_id
            )));
        }
        specifications
            .push(EkoTaskSpec::from_task_spec(task.spec.clone()).map_err(StoreError::InvalidPlan)?);
    }
    if expected_revision.is_none()
        && next.snapshot.tasks.iter().any(|task| {
            task.execution.status != echo_agent::tasks::TaskStatus::Pending
                || task.execution.retry_count != 0
                || task.execution.failure_fingerprint.is_some()
                || task.execution.claim.is_some()
        })
    {
        return Err(StoreError::InvalidPlan(
            "initial plan tasks must have pending execution state".to_string(),
        ));
    }
    let plan = PlanRevision {
        plan_id: plan_metadata.plan_id,
        run_id: run_id.to_string(),
        revision: next.snapshot.revision,
        domain_profile: plan_metadata.domain_profile,
        goal_revision: run.goal_revision,
        goal_sha256: run.goal_sha256.clone(),
        assumptions: next.context.assumptions.clone(),
        risks: next.context.risks.clone(),
        execution_mode: match next.context.execution_mode {
            echo_agent::tasks::TaskGraphExecutionMode::Sequential => ExecutionMode::Sequential,
            echo_agent::tasks::TaskGraphExecutionMode::Parallel => ExecutionMode::Parallel,
        },
        tasks: specifications,
    };
    let created_task_ids = plan
        .tasks
        .iter()
        .filter(|task| !previous_task_ids.contains(task.id.as_str()))
        .map(|task| task.id.clone())
        .collect::<Vec<_>>();
    let payload = serde_json::json!({
        "base_revision": expected_revision.unwrap_or_default(),
        "reason": reason,
        "skipped_task_ids": effects.skipped_task_ids,
        "reset_task_ids": effects.reset_task_ids,
        "created_task_ids": created_task_ids,
        "plan": &plan,
    });
    Ok(PreparedRevisionCommit {
        next,
        plan,
        payload,
    })
}

/// Bounded control-plane facts produced by one finite primary-Agent turn.
pub struct RunTurnCompletion<'a> {
    pub turn_id: &'a str,
    pub status: RunTurnStatus,
    pub elapsed_seconds: u64,
    pub final_message_id: Option<&'a str>,
    pub error_fingerprint: Option<&'a str>,
}

const MAX_PROVIDER_RETRY_ATTEMPTS: u32 = 5;
const PROVIDER_RETRY_BASE_MILLIS: u64 = 1_000;
const PROVIDER_RETRY_MAX_MILLIS: u64 = 60_000;

fn stable_provider_retry_delay_millis(
    run_id: &str,
    error_fingerprint: &str,
    attempt_count: u32,
) -> u64 {
    let exponent = attempt_count.saturating_sub(1).min(20);
    let multiplier = 1_u64.checked_shl(exponent).unwrap_or(u64::MAX);
    let ceiling = PROVIDER_RETRY_BASE_MILLIS
        .saturating_mul(multiplier)
        .min(PROVIDER_RETRY_MAX_MILLIS);
    let seed = format!("{run_id}\0{error_fingerprint}\0{attempt_count}");
    let sample = Sha256::digest(seed.as_bytes())
        .iter()
        .take(8)
        .fold(0_u64, |value, byte| {
            value.wrapping_shl(8) | u64::from(*byte)
        });
    sample % ceiling + 1
}

fn is_run_progress_event(event: &RuntimeTaskEvent) -> bool {
    matches!(
        event.event_type,
        RuntimeEventKind::PlanRevisionCommitted
            | RuntimeEventKind::TaskStarted
            | RuntimeEventKind::TaskCompleted
            | RuntimeEventKind::TaskFailed
            | RuntimeEventKind::TaskCancelled
            | RuntimeEventKind::TaskTimedOut
            | RuntimeEventKind::TaskSkipped
            | RuntimeEventKind::TaskBlocked
            | RuntimeEventKind::TaskStatusChanged
            | RuntimeEventKind::ArtifactProduced
            | RuntimeEventKind::MergeStarted
            | RuntimeEventKind::MergeCompleted
            | RuntimeEventKind::MergeFailed
            | RuntimeEventKind::ReviewPassed
            | RuntimeEventKind::ReviewNeedsFix
            | RuntimeEventKind::ReviewBlocked
            | RuntimeEventKind::RecoveryBlocked
            | RuntimeEventKind::RecoveryResolved
            | RuntimeEventKind::BackgroundCellStarted
            | RuntimeEventKind::BackgroundCellFinished
    )
}

fn run_progress_fingerprint(events: &[RuntimeTaskEvent]) -> String {
    events
        .iter()
        .rev()
        .find(|event| is_run_progress_event(event))
        .map(|event| format!("{}:{}", event.seq, event.event_type.as_str()))
        .unwrap_or_else(|| "no-task-progress".to_string())
}

fn run_turn_made_progress(events: &[RuntimeTaskEvent], turn_id: &str) -> bool {
    let Some(start_seq) = events.iter().rev().find_map(|event| {
        (event.event_type == RuntimeEventKind::RunTurnStarted
            && event
                .payload
                .get("turn_id")
                .and_then(serde_json::Value::as_str)
                == Some(turn_id))
        .then_some(event.seq)
    }) else {
        return false;
    };
    events
        .iter()
        .any(|event| event.seq > start_seq && is_run_progress_event(event))
}

fn blocker_fingerprint(error_fingerprint: Option<&str>, progress_fingerprint: &str) -> String {
    error_fingerprint
        .map(str::trim)
        .filter(|fingerprint| !fingerprint.is_empty())
        .map(|fingerprint| {
            let bounded = fingerprint
                .chars()
                .filter(|character| !character.is_control())
                .take(160)
                .collect::<String>();
            format!("error:{}", bounded.to_ascii_lowercase())
        })
        .unwrap_or_else(|| format!("stalled:{progress_fingerprint}"))
}

/// File-backed TaskRuntime store. One instance per process; cheap to clone
/// behind `Arc`. The event stream is authoritative; plan and execution files
/// are deterministic read projections.
pub struct TaskRuntimeStore {
    /// Per-task cancellation tokens (in-memory runtime state, not persisted).
    /// Key = `"{run_id}::{task_id}"`. `execute_task` registers a token when a
    /// task starts and removes it on completion; runtime control actions use
    /// the token to stop that Subagent promptly.
    task_cancel_tokens:
        std::sync::Mutex<std::collections::HashMap<String, echo_agent::agent::CancellationToken>>,
    /// Exact execution-to-framework routing only. Durable commands and their
    /// outcomes remain in events.jsonl; no message is stored in this map.
    pub(super) active_subagent_controls: std::sync::Mutex<
        std::collections::HashMap<String, super::subagent_control::ActiveSubagentControlTarget>,
    >,
    /// Active TaskRun driver tokens. Every entry point registers here so pause
    /// and cancel target the real executor instead of a surface-local map.
    run_cancel_tokens: std::sync::Mutex<
        std::collections::HashMap<String, Vec<(u64, echo_agent::agent::CancellationToken)>>,
    >,
    next_run_cancel_registration: std::sync::atomic::AtomicU64,
    /// Accepted TaskRun driver tasks. The store is the existing runtime owner,
    /// so dropping an individual surface waiter never drops the actual driver.
    run_driver_supervisor: std::sync::Mutex<RunDriverSupervisor>,
    /// Wakes the store-owned shutdown settlement after the last pre-shutdown
    /// driver admission reservation either registers a driver or is released.
    run_driver_admission_idle: tokio::sync::Notify,
    /// Wakes the continuation coordinator after the exact current driver has
    /// released its run-scoped cancellation registration.
    run_driver_idle: tokio::sync::Notify,
    /// EKO-owned control plane for finite primary-Agent RunTurns. The runtime
    /// keeps only a weak store reference, so this does not create an Arc cycle.
    pub(super) continuation_runtime:
        std::sync::OnceLock<std::sync::Arc<super::continuation::TaskContinuationRuntime>>,
    pub(super) boot_reconciler:
        std::sync::OnceLock<std::sync::Arc<super::boot_reconciler::TaskRunBootReconciler>>,
    /// Process routing adapter for optional cross-workspace PlanTask targets.
    /// The adapter owns no task state and is intentionally absent in tests or
    /// embedding applications that only execute local tasks.
    execution_target_resolver: std::sync::RwLock<
        Option<std::sync::Arc<dyn super::execution_target::TaskExecutionTargetResolver>>,
    >,
    command_cell_runtime:
        std::sync::RwLock<Option<std::sync::Weak<super::command_cells::CommandCellRuntimeService>>>,
    #[cfg(test)]
    run_driver_shutdown_started: tokio::sync::Notify,
    #[cfg(test)]
    abort_next_run_driver_shutdown_reporter: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    run_driver_admission_test_barrier: std::sync::Mutex<Option<RunDriverAdmissionTestBarrier>>,
    #[cfg(test)]
    run_driver_registration_test_barrier:
        std::sync::Mutex<Option<RunDriverRegistrationTestBarrier>>,
    #[cfg(any(test, feature = "test-utils"))]
    fail_next_run_driver_registration: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    fail_next_recovery_commit: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    fail_next_recovery_projection: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    fail_next_cell_started: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    fail_next_cell_started_projection: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    fail_next_runtime_mutation_projection: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    fail_cell_terminal_remaining: std::sync::atomic::AtomicUsize,
    /// File-backed event authority and deterministic projections.
    pub(super) shadow: std::sync::Arc<super::file_shadow::FileTaskShadow>,
    shadow_generation: std::sync::Mutex<ShadowGeneration>,
    /// Owns the bounded task/subagent hook consumer so shutdown can drain it.
    hook_event_dispatcher:
        std::sync::Mutex<Option<super::hook_event_dispatcher::HookEventDispatcher>>,
    /// Per-run plan/state 写互斥锁 (F2-1 / F3-3 / F3-4)。
    ///
    /// revision compare-and-commit / transition_run 都是
    /// "读事件 → 校验 → 追加 → 重建投影"事务, 必须按 run 串行化。
    /// Different runs keep independent locks.
    plan_locks: dashmap::DashMap<String, std::sync::Arc<std::sync::Mutex<()>>>,
    operation_supervisor: std::sync::Arc<super::executor::TaskRuntimeOperationSupervisor>,
}

struct ShadowGeneration {
    active_operations: usize,
    workspace_id: String,
    transitioning: bool,
}

struct ShadowOperation<'a> {
    store: &'a TaskRuntimeStore,
}

/// Keeps one product operation bound to the current workspace generation.
/// Rebinding returns Busy until every lease from the previous generation drops.
#[must_use]
pub(crate) struct WorkspaceGenerationLease {
    store: std::sync::Arc<TaskRuntimeStore>,
}

/// Opaque application receipt used by foreground surfaces to establish the
/// canonical lock order before memory and pool admission. The TaskRuntime
/// store remains the only generation authority; this type only retains its
/// existing lease until the outer foreground driver settles.
#[must_use]
pub struct TaskRuntimeGenerationReceipt {
    _lease: WorkspaceGenerationLease,
}

struct RunDriverSupervisor {
    accepting: bool,
    pending_admissions: usize,
    driver_cancels: std::collections::HashMap<u64, echo_agent::agent::CancellationToken>,
    /// Opaque capability and exact run identity for every live driver token.
    /// Framework-spawned tool calls must match both before transferring a
    /// receipt here; sequential internal tokens are never exposed as authority.
    driver_contexts: std::collections::HashMap<String, RunDriverExecutionContext>,
    driver_settlements: tokio::task::JoinSet<(u64, Result<(), String>)>,
    settlement_debts: Vec<RunSettlementDebt>,
    next_driver_token: u64,
    execution_receipts: std::collections::HashMap<u64, Vec<Box<dyn RunDriverExecutionReceipt>>>,
    shutdown_result_sender:
        Option<tokio::sync::watch::Sender<Option<Result<(), TaskRunDriverShutdownError>>>>,
    shutdown_result:
        Option<tokio::sync::watch::Receiver<Option<Result<(), TaskRunDriverShutdownError>>>>,
    shutdown_owner: Option<std::sync::Arc<tokio::sync::Mutex<RunDriverShutdownOwner>>>,
    /// Canonical store-owned reporter. Polling its JoinHandle through this
    /// shared mutex is cancellation-safe: a dropped waiter never takes it.
    shutdown_reporter: Option<std::sync::Arc<tokio::sync::Mutex<RunDriverShutdownReporter>>>,
    shutdown_reporter_errors: Vec<String>,
}

struct RunDriverExecutionContext {
    driver_token: u64,
    run_id: String,
}

enum RunDriverShutdownReporter {
    Running(tokio::task::JoinHandle<()>),
    Completed,
}

enum RunDriverShutdownOwner {
    Running(tokio::task::JoinHandle<Result<(), TaskRunDriverShutdownError>>),
    Completed(Result<(), TaskRunDriverShutdownError>),
}

#[cfg(test)]
struct RunDriverAdmissionTestBarrier {
    reserved: std::sync::mpsc::SyncSender<()>,
    release: std::sync::mpsc::Receiver<()>,
}

#[cfg(test)]
struct RunDriverRegistrationTestBarrier {
    registered: std::sync::mpsc::SyncSender<()>,
    release: std::sync::mpsc::Receiver<()>,
}

/// Durable TaskRun terminal state that could not be written during the final
/// shutdown retry. The on-disk run remains authoritative; this diagnostic
/// records the uncommitted target and why execution resources were abandoned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbandonedRunSettlement {
    pub run_id: String,
    pub driver_token: Option<u64>,
    pub root: PathBuf,
    pub target: TaskRunStatus,
    pub error: String,
}

impl std::fmt::Display for AbandonedRunSettlement {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let driver_token = self
            .driver_token
            .map(|token| token.to_string())
            .unwrap_or_else(|| "none".to_string());
        write!(
            formatter,
            "run={} driver_token={} root={} target={} error={}",
            self.run_id,
            driver_token,
            self.root.display(),
            self.target.as_str(),
            self.error
        )
    }
}

/// Aggregated shutdown degradation. Accepted drivers are fully drained and
/// all exact execution receipts are released before this error is returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRunDriverShutdownError {
    pub driver_errors: Vec<String>,
    pub abandoned_settlements: Vec<AbandonedRunSettlement>,
}

impl std::fmt::Display for TaskRunDriverShutdownError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut details = self.driver_errors.clone();
        details.extend(self.abandoned_settlements.iter().map(ToString::to_string));
        write!(
            formatter,
            "TaskRun driver shutdown degraded: {}",
            details.join("; ")
        )
    }
}

impl std::error::Error for TaskRunDriverShutdownError {}

fn add_shutdown_driver_error(
    result: &mut Result<(), TaskRunDriverShutdownError>,
    driver_error: String,
) {
    match result {
        Ok(()) => {
            *result = Err(TaskRunDriverShutdownError {
                driver_errors: vec![driver_error],
                abandoned_settlements: Vec::new(),
            });
        }
        Err(error) => error.driver_errors.push(driver_error),
    }
}

/// One execution resource retained by the canonical TaskRun driver until its
/// durable terminal state (or settlement debt) has completed.
pub trait RunDriverExecutionReceipt: Send {
    /// Release the resource after later-acquired receipts have settled.
    fn release(self: Box<Self>) -> futures::future::BoxFuture<'static, ()>;
}

/// Capability handed only to an accepted TaskRun driver. Pool-backed adapters
/// transfer their execution receipt here immediately after acquisition so it
/// survives inner future errors and panics until durable run settlement.
pub struct RunDriverReceiptOwner {
    store: std::sync::Arc<TaskRuntimeStore>,
    driver_token: u64,
    execution_context_id: String,
}

type BoxRunDriverFuture<T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, String>> + Send + 'static>>;

enum RunDriverStart<T> {
    Execute(BoxRunDriverFuture<T>),
    PreparationFailed(String),
    Reject(String),
}

/// Exact driver registration completed before callers mutate TaskRuntime.
/// Dropping an unstarted registration wakes the canonical owner as a rejected
/// preparation, so shutdown never waits forever for an accepted slot.
#[must_use]
pub(crate) struct RegisteredRunDriver<T: Send + 'static> {
    start_sender: Option<tokio::sync::oneshot::Sender<RunDriverStart<T>>>,
    result_receiver: Option<tokio::sync::oneshot::Receiver<Result<T, String>>>,
    receipt_owner: Option<RunDriverReceiptOwner>,
    preparation_started: bool,
    active: bool,
}

/// Exact pre-execution admission owned by the canonical TaskRuntime
/// supervisor. It is acquired before any run mutation or workspace-bound
/// memory/pool admission and consumed only when the driver is registered.
#[must_use]
pub(crate) struct RunDriverAdmissionReservation {
    store: std::sync::Arc<TaskRuntimeStore>,
    run_id: String,
    cancel: echo_agent::agent::CancellationToken,
    active: bool,
}

impl RunDriverReceiptOwner {
    const EXECUTION_CONTEXT_PREFIX: &'static str = "eko-task-driver:";

    /// Retain one driver resource. Factories passed to `spawn_run_driver` must
    /// call this from the returned future, not while constructing that future,
    /// because driver admission is serialized by the supervisor lock.
    pub fn retain<Receipt>(&mut self, receipt: Receipt)
    where
        Receipt: RunDriverExecutionReceipt + 'static,
    {
        self.store
            .run_driver_supervisor
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .execution_receipts
            .entry(self.driver_token)
            .or_default()
            .push(Box::new(receipt));
    }

    /// Opaque value-carried identity for framework-spawned tool execution.
    /// The canonical store validates it against this exact live driver.
    pub(crate) fn execution_context_id(&self) -> String {
        self.execution_context_id.clone()
    }
}

impl<T: Send + 'static> RegisteredRunDriver<T> {
    pub(crate) fn mark_preparation_started(&mut self) {
        self.preparation_started = true;
    }

    pub(crate) fn start<F, Factory>(
        mut self,
        factory: Factory,
    ) -> tokio::sync::oneshot::Receiver<Result<T, String>>
    where
        F: std::future::Future<Output = Result<T, String>> + Send + 'static,
        Factory: FnOnce(RunDriverReceiptOwner) -> F,
    {
        let receiver = self.result_receiver.take().unwrap_or_else(|| {
            let (_sender, receiver) = tokio::sync::oneshot::channel();
            receiver
        });
        let start = self
            .receipt_owner
            .take()
            .map(|owner| RunDriverStart::Execute(Box::pin(factory(owner))));
        if let (Some(sender), Some(start)) = (self.start_sender.take(), start) {
            let _start_delivered = sender.send(start);
        }
        self.active = false;
        receiver
    }

    pub(crate) fn reject(mut self, error: impl Into<String>) {
        if let Some(sender) = self.start_sender.take() {
            let _start_delivered = sender.send(RunDriverStart::Reject(error.into()));
        }
        self.active = false;
    }

    pub(crate) fn fail_preparation(mut self, error: impl Into<String>) {
        if let Some(sender) = self.start_sender.take() {
            let _start_delivered = sender.send(RunDriverStart::PreparationFailed(error.into()));
        }
        self.active = false;
    }
}

impl<T: Send + 'static> Drop for RegisteredRunDriver<T> {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        if let Some(sender) = self.start_sender.take() {
            let message =
                "TaskRun driver registration dropped before preparation completed".to_string();
            let start = if self.preparation_started {
                RunDriverStart::PreparationFailed(message)
            } else {
                RunDriverStart::Reject(message)
            };
            let _start_delivered = sender.send(start);
        }
    }
}

impl Drop for RunDriverAdmissionReservation {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let became_idle = {
            let mut supervisor = self
                .store
                .run_driver_supervisor
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            supervisor.pending_admissions = supervisor.pending_admissions.saturating_sub(1);
            supervisor.pending_admissions == 0
        };
        if became_idle {
            self.store.run_driver_admission_idle.notify_one();
        }
    }
}

struct RunSettlementDebt {
    generation_lease: WorkspaceGenerationLease,
    driver_token: Option<u64>,
    run_id: String,
    root: PathBuf,
    target: TaskRunStatus,
    note: Option<String>,
    last_error: String,
}

impl Default for RunDriverSupervisor {
    fn default() -> Self {
        Self {
            accepting: true,
            pending_admissions: 0,
            driver_cancels: std::collections::HashMap::new(),
            driver_contexts: std::collections::HashMap::new(),
            driver_settlements: tokio::task::JoinSet::new(),
            settlement_debts: Vec::new(),
            next_driver_token: 0,
            execution_receipts: std::collections::HashMap::new(),
            shutdown_result_sender: None,
            shutdown_result: None,
            shutdown_owner: None,
            shutdown_reporter: None,
            shutdown_reporter_errors: Vec::new(),
        }
    }
}

/// Exclusive workspace-generation transition. New operations receive a typed
/// busy error until this guard is dropped.
#[must_use]
pub(crate) struct TaskRuntimeWorkspaceTransition<'a> {
    store: &'a TaskRuntimeStore,
    active: bool,
}

impl TaskRuntimeWorkspaceTransition<'_> {
    #[cfg(test)]
    pub(crate) fn list_runs_in(
        &self,
        statuses: &[TaskRunStatus],
    ) -> Result<Vec<TaskRun>, StoreError> {
        super::file_store::FileTaskStore::from_root(self.store.shadow.root())
            .map_err(|error| StoreError::InvalidPlan(format!("file read: {error}")))?
            .list_runs_in(statuses)
            .map_err(|error| StoreError::InvalidPlan(format!("file read: {error}")))
    }

    pub(crate) fn rebind_shadow_root(
        &self,
        root: impl Into<PathBuf>,
        workspace_id: impl Into<String>,
    ) -> Result<(), StoreError> {
        let root = root.into();
        echo_agent::utils::fs::create_dir_all_durable(&root)
            .map_err(|error| super::file_shadow::ShadowError::Io(error.to_string()))?;
        let mut generation = self
            .store
            .shadow_generation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !generation.transitioning || generation.active_operations != 0 {
            return Err(StoreError::InvalidPlan(
                "task runtime workspace transition lost exclusive admission".to_string(),
            ));
        }
        self.store.shadow.rebind_root(root)?;
        let previous_workspace_id = generation.workspace_id.clone();
        let workspace_id = workspace_id.into();
        generation.workspace_id = workspace_id.clone();
        drop(generation);
        if let Some(runtime) = self
            .store
            .command_cell_runtime
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
        {
            runtime.rebind_store_workspace(&previous_workspace_id, &workspace_id);
        }
        Ok(())
    }
}

impl Drop for TaskRuntimeWorkspaceTransition<'_> {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let mut generation = self
            .store
            .shadow_generation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        generation.transitioning = false;
        self.active = false;
    }
}

struct ShadowFileStore<'a> {
    _operation: ShadowOperation<'a>,
    store: super::file_store::FileTaskStore,
}

impl std::ops::Deref for ShadowFileStore<'_> {
    type Target = super::file_store::FileTaskStore;

    fn deref(&self) -> &Self::Target {
        &self.store
    }
}

impl Drop for ShadowOperation<'_> {
    fn drop(&mut self) {
        let mut generation = self
            .store
            .shadow_generation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        generation.active_operations = generation.active_operations.saturating_sub(1);
    }
}

impl Drop for WorkspaceGenerationLease {
    fn drop(&mut self) {
        let mut generation = self
            .store
            .shadow_generation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        generation.active_operations = generation.active_operations.saturating_sub(1);
    }
}

/// RAII registration for one active TaskRun driver. Each guard removes only
/// its own token, so overlapping drivers can finish in either order.
pub struct RunCancellationRegistration {
    store: std::sync::Arc<TaskRuntimeStore>,
    run_id: String,
    registration_id: u64,
}

#[cfg(test)]
fn validate_runtime_plan(tasks: &[PlanTask]) -> Result<(), StoreError> {
    let runtime_tasks = tasks
        .iter()
        .map(PlanTask::to_task)
        .collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::InvalidPlan)?;
    echo_agent::tasks::PlanValidator::default()
        .validate_task_snapshot(&runtime_tasks)
        .map_err(|errors| StoreError::InvalidPlan(errors.join("; ")))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecoverableSubagentResult {
    pub(crate) result: SubagentTaskResult,
    pub(crate) full_output: String,
}

struct TaskStatusEvent<'a> {
    run_id: &'a str,
    task_id: &'a str,
    task_subject: &'a str,
    status: echo_agent::tasks::TaskStatus,
    owner_agent: Option<&'a str>,
    summary: Option<&'a str>,
    claim: Option<&'a echo_agent::tasks::TaskClaim>,
}

fn review_runtime_event(
    review: &ReviewResult,
    claim: Option<&echo_agent::tasks::TaskClaim>,
) -> RuntimeJournalEvent {
    let kind = match review.outcome {
        ReviewOutcome::Pass => RuntimeEventKind::ReviewPassed,
        ReviewOutcome::NeedsFix => RuntimeEventKind::ReviewNeedsFix,
        ReviewOutcome::Blocked => RuntimeEventKind::ReviewBlocked,
    };
    RuntimeJournalEvent::for_append(
        review.run_id.as_str(),
        Some(review.task_id.as_str()),
        None,
        kind,
        serde_json::json!({
            "review_id": review.id,
            "reviewer": review.reviewer_agent,
            "outcome": review.outcome.as_str(),
            "issues": review.issues,
            "failure_fingerprint": review.failure_fingerprint,
            "created_fix_task_id": review.created_fix_task_id,
            "created_at": echo_agent::utils::time::to_local(review.created_at).to_rfc3339(),
            "claim_id": claim.map(|claim| claim.claim_id.as_str()),
            "plan_revision": claim.map(|claim| claim.revision),
            "attempt": claim.map(|claim| claim.attempt),
            "spec_hash": claim.map(|claim| claim.spec_hash.as_str()),
        }),
    )
}

fn task_status_wire(status: &echo_agent::tasks::TaskStatus) -> (&'static str, Option<&str>) {
    match status {
        echo_agent::tasks::TaskStatus::Pending => ("pending", None),
        echo_agent::tasks::TaskStatus::Running => ("running", None),
        echo_agent::tasks::TaskStatus::Blocked(detail) => ("blocked", Some(detail.as_str())),
        echo_agent::tasks::TaskStatus::Completed => ("completed", None),
        echo_agent::tasks::TaskStatus::Failed(detail) => ("failed", Some(detail.as_str())),
        echo_agent::tasks::TaskStatus::Skipped => ("skipped", None),
        echo_agent::tasks::TaskStatus::Cancelled => ("cancelled", None),
        echo_agent::tasks::TaskStatus::TimedOut { error } => ("timed_out", Some(error.as_str())),
        echo_agent::tasks::TaskStatus::Retrying { last_error, .. } => {
            ("retrying", Some(last_error.as_str()))
        }
        echo_agent::tasks::TaskStatus::Paused(detail) => ("paused", Some(detail.as_str())),
    }
}

fn runtime_task_event_kind(status: &echo_agent::tasks::TaskStatus) -> RuntimeEventKind {
    match status {
        echo_agent::tasks::TaskStatus::Running => RuntimeEventKind::TaskStarted,
        echo_agent::tasks::TaskStatus::Completed => RuntimeEventKind::TaskCompleted,
        echo_agent::tasks::TaskStatus::Failed(_) => RuntimeEventKind::TaskFailed,
        echo_agent::tasks::TaskStatus::Cancelled => RuntimeEventKind::TaskCancelled,
        echo_agent::tasks::TaskStatus::TimedOut { .. } => RuntimeEventKind::TaskTimedOut,
        echo_agent::tasks::TaskStatus::Skipped => RuntimeEventKind::TaskSkipped,
        echo_agent::tasks::TaskStatus::Blocked(_) => RuntimeEventKind::TaskBlocked,
        echo_agent::tasks::TaskStatus::Pending
        | echo_agent::tasks::TaskStatus::Retrying { .. }
        | echo_agent::tasks::TaskStatus::Paused(_) => RuntimeEventKind::TaskStatusChanged,
    }
}

fn runtime_execution_change_event(
    run_id: &str,
    before: &echo_agent::tasks::Task,
    after: &echo_agent::tasks::Task,
    summary: Option<&str>,
) -> Result<Option<RuntimeJournalEvent>, StoreError> {
    if before.execution == after.execution {
        return Ok(None);
    }
    if before.spec != after.spec {
        return Err(StoreError::InvalidPlan(format!(
            "runtime mutation changed task specification '{}'",
            after.spec.id
        )));
    }
    let extension =
        EkoTaskSpec::from_task_spec(after.spec.clone()).map_err(StoreError::InvalidPlan)?;
    let (status, status_detail) = task_status_wire(&after.execution.status);
    let now = echo_agent::utils::time::now_local().to_rfc3339();
    let started = matches!(
        after.execution.status,
        echo_agent::tasks::TaskStatus::Running | echo_agent::tasks::TaskStatus::Retrying { .. }
    );
    let finished = after.execution.status.is_terminal();
    Ok(Some(RuntimeJournalEvent::for_append(
        run_id,
        Some(&after.spec.id),
        None,
        runtime_task_event_kind(&after.execution.status),
        serde_json::json!({
            "status": status,
            "status_detail": status_detail,
            "claim": after.execution.claim,
            "execution_id": after
                .execution
                .claim
                .as_ref()
                .map(|claim| claim.execution_id(run_id, &after.spec.id)),
            "retry_count": after.execution.retry_count,
            "failure_fingerprint": after.execution.failure_fingerprint,
            "owner_agent": extension.agent_role,
            "title": after.spec.title,
            "summary": summary,
            "started_at": if started { Some(now.as_str()) } else { None },
            "completed_at": if finished { Some(now.as_str()) } else { None },
        }),
    )))
}

fn runtime_execution_change_events(
    run_id: &str,
    before: &echo_agent::tasks::RuntimePlanSnapshot,
    after: &echo_agent::tasks::RuntimePlanSnapshot,
    summary: Option<&str>,
) -> Result<Vec<RuntimeJournalEvent>, StoreError> {
    if before.tasks.len() != after.tasks.len() {
        return Err(StoreError::InvalidPlan(
            "execution diff changed the revisioned task graph shape".to_string(),
        ));
    }
    let mut events = Vec::new();
    for after_task in &after.tasks {
        let before_task = before
            .tasks
            .iter()
            .find(|task| task.spec.id == after_task.spec.id)
            .ok_or_else(|| {
                StoreError::InvalidPlan(format!(
                    "runtime mutation introduced task '{}'",
                    after_task.spec.id
                ))
            })?;
        if let Some(event) =
            runtime_execution_change_event(run_id, before_task, after_task, summary)?
        {
            events.push(event);
        }
    }
    Ok(events)
}

pub(crate) struct SubagentReleaseRecord<'a> {
    pub run_id: &'a str,
    pub task_id: &'a str,
    pub execution_id: &'a str,
    pub agent_name: &'a str,
    pub task_subject: &'a str,
    pub plan_revision: u64,
    pub attempt: u32,
    pub status: &'a str,
    pub result: Option<&'a SubagentTaskResult>,
    pub full_output: Option<&'a str>,
    pub usage: Option<&'a SubagentRunUsage>,
    pub dispatch_hook: bool,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct RuntimeTaskProductSettlement {
    pub summary: Option<String>,
    pub execution_summary: Option<TaskExecutionSummary>,
    pub review: Option<ReviewResult>,
    pub diagnostic_note: Option<String>,
    pub typed_terminal: Option<echo_agent::error::AgentFailure>,
}

enum RunTurnClaimPreparation {
    Start(RuntimeJournalEvent),
    NotSubmitted(ContinuationNotSubmittedReason),
}

impl Drop for RunCancellationRegistration {
    fn drop(&mut self) {
        if let Ok(mut map) = self.store.run_cancel_tokens.lock() {
            let remove_run = if let Some(tokens) = map.get_mut(&self.run_id) {
                tokens.retain(|(registration_id, _)| registration_id != &self.registration_id);
                tokens.is_empty()
            } else {
                false
            };
            if remove_run {
                map.remove(&self.run_id);
            }
        }
        self.store.run_driver_idle.notify_waiters();
    }
}

impl TaskRuntimeStore {
    /// Phase-one process shutdown: close driver admission and broadcast every
    /// accepted driver cancellation without awaiting settlement.
    pub fn begin_run_driver_shutdown(&self) -> Result<(), String> {
        let mut supervisor = self
            .run_driver_supervisor
            .lock()
            .map_err(|_| "TaskRun driver supervisor lock is poisoned".to_string())?;
        supervisor.accepting = false;
        for cancel in supervisor.driver_cancels.values() {
            cancel.cancel();
        }
        super::continuation::shutdown(self);
        Ok(())
    }

    /// Whether the process runtime still accepts new finite TaskRun drivers.
    /// Long-horizon coordinators use this to stop cleanly during application
    /// shutdown and leave durable recovery to the next process.
    pub fn is_run_driver_admission_open(&self) -> bool {
        self.run_driver_supervisor
            .lock()
            .map(|supervisor| supervisor.accepting)
            .unwrap_or(false)
    }

    /// Create the store at the default location.
    ///
    /// task/plan data lives under the file shadow root (`~/.eko/tasks/`);
    /// No database is opened. Root authority, durable directory creation, and
    /// cross-process lease failures are propagated to bootstrap.
    pub fn new() -> anyhow::Result<Self> {
        Self::open()
    }

    /// Create the store. No path is needed anymore (no SQLite); the file shadow
    /// root is the real storage location. Kept as `open()` with no args for
    /// call-site compatibility with the old `open(path)` constructor.
    pub fn open() -> anyhow::Result<Self> {
        let shadow = std::sync::Arc::new(super::file_shadow::FileTaskShadow::try_new(
            super::file_shadow::FileTaskShadow::default_root(),
        )?);
        Ok(Self::with_shadow(shadow, "global"))
    }

    /// Open one workspace-owned runtime store at its immutable task root.
    ///
    /// Unlike [`Self::rebind_shadow_root`], this constructor never changes an
    /// existing runtime generation. Independent workspace hosts therefore keep
    /// distinct cancellation, continuation, hook, and file-authority owners.
    pub fn open_for_workspace(
        shadow_root: impl Into<PathBuf>,
        workspace_id: impl Into<String>,
    ) -> anyhow::Result<Self> {
        let shadow = std::sync::Arc::new(super::file_shadow::FileTaskShadow::try_new(shadow_root)?);
        Ok(Self::with_shadow(shadow, workspace_id.into()))
    }

    fn with_shadow(
        shadow: std::sync::Arc<super::file_shadow::FileTaskShadow>,
        workspace_id: impl Into<String>,
    ) -> Self {
        Self {
            task_cancel_tokens: std::sync::Mutex::new(std::collections::HashMap::new()),
            active_subagent_controls: std::sync::Mutex::new(std::collections::HashMap::new()),
            run_cancel_tokens: std::sync::Mutex::new(std::collections::HashMap::new()),
            next_run_cancel_registration: std::sync::atomic::AtomicU64::new(0),
            run_driver_supervisor: std::sync::Mutex::new(RunDriverSupervisor::default()),
            run_driver_admission_idle: tokio::sync::Notify::new(),
            run_driver_idle: tokio::sync::Notify::new(),
            continuation_runtime: std::sync::OnceLock::new(),
            boot_reconciler: std::sync::OnceLock::new(),
            execution_target_resolver: std::sync::RwLock::new(None),
            command_cell_runtime: std::sync::RwLock::new(None),
            #[cfg(test)]
            run_driver_shutdown_started: tokio::sync::Notify::new(),
            #[cfg(test)]
            abort_next_run_driver_shutdown_reporter: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            run_driver_admission_test_barrier: std::sync::Mutex::new(None),
            #[cfg(test)]
            run_driver_registration_test_barrier: std::sync::Mutex::new(None),
            #[cfg(any(test, feature = "test-utils"))]
            fail_next_run_driver_registration: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            fail_next_recovery_commit: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            fail_next_recovery_projection: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            fail_next_cell_started: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            fail_next_cell_started_projection: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            fail_next_runtime_mutation_projection: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            fail_cell_terminal_remaining: std::sync::atomic::AtomicUsize::new(0),
            shadow,
            shadow_generation: std::sync::Mutex::new(ShadowGeneration {
                active_operations: 0,
                workspace_id: workspace_id.into(),
                transitioning: false,
            }),
            hook_event_dispatcher: std::sync::Mutex::new(None),
            plan_locks: dashmap::DashMap::new(),
            operation_supervisor: super::executor::TaskRuntimeOperationSupervisor::new(),
        }
    }

    pub(crate) fn operation_supervisor(
        &self,
    ) -> std::sync::Arc<super::executor::TaskRuntimeOperationSupervisor> {
        std::sync::Arc::clone(&self.operation_supervisor)
    }

    pub fn active_operation_count(&self) -> usize {
        self.operation_supervisor.active_count()
    }

    pub fn begin_operation_shutdown(&self) -> Result<(), String> {
        self.operation_supervisor.begin_shutdown()
    }

    pub async fn shutdown_operations(&self) -> Result<(), String> {
        self.begin_operation_shutdown()?;
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            self.operation_supervisor.join(),
        )
        .await
        .map_err(|_| "TaskRuntime operation shutdown timed out after 30 seconds".to_string())?
    }

    /// In-memory store for tests / fallback. The file shadow is backed by a
    /// per-process temp dir so every test exercises the file-authority path.
    pub fn new_in_memory() -> anyhow::Result<Self> {
        let shadow_root = std::env::temp_dir().join(format!(
            "echo-agent-task-runtime-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        Self::new_in_memory_with_shadow_root(shadow_root)
    }

    /// In-memory store whose file shadow is rooted at `shadow_root`. Tests use
    /// this (with a `tempfile::tempdir()` root) so they can read the written
    /// `events.jsonl` / projection files back directly and so runs are isolated
    /// under a known directory. Replaces the old `attach_shadow` test hook.
    pub fn new_in_memory_with_shadow_root(shadow_root: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let shadow = std::sync::Arc::new(super::file_shadow::FileTaskShadow::try_new(shadow_root)?);
        Ok(Self::with_shadow(shadow, "test"))
    }

    /// Attach the application-layer HookEventDispatcher so every event written
    /// via `append_event_line` is translated into framework HookEvents.
    ///
    /// Idempotent (first call wins). Intended to be called once during
    /// bootstrap, after the agent + bridges exist (the store is built earlier).
    /// Until attached, task/subagent events are not dispatched to hooks.
    pub fn attach_hook_event_dispatcher(
        &self,
        dispatcher: super::hook_event_dispatcher::HookEventDispatcher,
    ) -> Result<bool, StoreError> {
        let Ok(mut owned_dispatcher) = self.hook_event_dispatcher.lock() else {
            tracing::warn!("HookEventDispatcher ownership lock is poisoned");
            return Err(StoreError::LockPoisoned);
        };
        if owned_dispatcher.is_some() {
            return Ok(false);
        }
        let event_dispatcher = dispatcher.clone();
        let hook: std::sync::Arc<dyn Fn(&super::types::RuntimeTaskEvent) + Send + Sync> =
            std::sync::Arc::new(move |event| {
                if let Err(error) = event_dispatcher.dispatch(event) {
                    tracing::warn!(%error, "Failed to enqueue task hook event");
                }
            });
        let _operation = self.shadow_operation()?;
        if !self.shadow.try_attach_event_hook(hook) {
            return Ok(false);
        }
        *owned_dispatcher = Some(dispatcher);
        Ok(true)
    }

    pub fn attach_execution_target_resolver(
        &self,
        resolver: std::sync::Arc<dyn super::execution_target::TaskExecutionTargetResolver>,
    ) {
        *self
            .execution_target_resolver
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(resolver);
    }

    pub(crate) fn execution_target_resolver(
        &self,
    ) -> Option<std::sync::Arc<dyn super::execution_target::TaskExecutionTargetResolver>> {
        self.execution_target_resolver
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Wait for every persisted task/subagent hook event to finish firing.
    pub async fn flush_hook_events(&self) -> Result<(), String> {
        let dispatcher = self
            .hook_event_dispatcher
            .lock()
            .map_err(|_| "HookEventDispatcher ownership lock is poisoned".to_string())?
            .clone();
        if let Some(dispatcher) = dispatcher {
            dispatcher.flush().await
        } else {
            Ok(())
        }
    }

    /// Drain and stop the hook consumer. Repeated calls are harmless.
    pub async fn shutdown_hook_events(&self) -> Result<(), String> {
        let dispatcher = self
            .hook_event_dispatcher
            .lock()
            .map_err(|_| "HookEventDispatcher ownership lock is poisoned".to_string())?
            .clone();
        if let Some(dispatcher) = dispatcher {
            dispatcher.shutdown().await
        } else {
            Ok(())
        }
    }

    /// Stop accepting TaskRun drivers, cancel every accepted driver, and await
    /// their owned settlement before the store's hook consumer is torn down.
    pub async fn shutdown_run_drivers(
        self: &std::sync::Arc<Self>,
    ) -> Result<(), TaskRunDriverShutdownError> {
        self.begin_run_driver_shutdown()
            .map_err(|error| TaskRunDriverShutdownError {
                driver_errors: vec![error],
                abandoned_settlements: Vec::new(),
            })?;
        let (mut shutdown_result, shutdown_sender, shutdown_reporter) = {
            let mut supervisor =
                self.run_driver_supervisor
                    .lock()
                    .map_err(|_| TaskRunDriverShutdownError {
                        driver_errors: vec![
                            "TaskRun driver supervisor lock is poisoned".to_string(),
                        ],
                        abandoned_settlements: Vec::new(),
                    })?;
            if let (Some(sender), Some(result), Some(reporter)) = (
                supervisor.shutdown_result_sender.as_ref(),
                supervisor.shutdown_result.as_ref(),
                supervisor.shutdown_reporter.as_ref(),
            ) {
                (result.clone(), sender.clone(), reporter.clone())
            } else {
                supervisor.accepting = false;
                #[cfg(test)]
                self.run_driver_shutdown_started.notify_one();
                for cancel in supervisor.driver_cancels.values() {
                    cancel.cancel();
                }
                let (result_sender, result_receiver) = tokio::sync::watch::channel(None);
                supervisor.shutdown_result_sender = Some(result_sender.clone());
                supervisor.shutdown_result = Some(result_receiver.clone());
                let settlement_store = std::sync::Arc::clone(self);
                let owner = std::sync::Arc::new(tokio::sync::Mutex::new(
                    RunDriverShutdownOwner::Running(tokio::spawn(async move {
                        settlement_store.settle_run_driver_shutdown().await
                    })),
                ));
                supervisor.shutdown_owner = Some(owner.clone());
                let reporter = std::sync::Arc::new(tokio::sync::Mutex::new(
                    RunDriverShutdownReporter::Running(
                        self.spawn_run_driver_shutdown_reporter(owner, result_sender.clone()),
                    ),
                ));
                supervisor.shutdown_reporter = Some(reporter.clone());
                (result_receiver, result_sender, reporter)
            }
        };
        super::continuation::shutdown(self);

        loop {
            let observed_result = shutdown_result.borrow().clone();
            if let Some(result) = observed_result {
                return result;
            }
            tokio::select! {
                changed = shutdown_result.changed() => {
                    if changed.is_err() {
                        self.restart_run_driver_shutdown_reporter(
                            &shutdown_reporter,
                            &shutdown_sender,
                            "TaskRun driver shutdown result channel closed before publication"
                                .to_string(),
                        )
                        .await;
                    }
                }
                () = self.observe_run_driver_shutdown_reporter(
                    &shutdown_reporter,
                    &shutdown_sender,
                ) => {}
            }
        }
    }

    fn spawn_run_driver_shutdown_reporter(
        self: &std::sync::Arc<Self>,
        owner: std::sync::Arc<tokio::sync::Mutex<RunDriverShutdownOwner>>,
        result_sender: tokio::sync::watch::Sender<Option<Result<(), TaskRunDriverShutdownError>>>,
    ) -> tokio::task::JoinHandle<()> {
        let reporter_store = std::sync::Arc::clone(self);
        #[cfg(test)]
        let abort_reporter = self
            .abort_next_run_driver_shutdown_reporter
            .swap(false, std::sync::atomic::Ordering::SeqCst);
        #[cfg(not(test))]
        let abort_reporter = false;
        let reporter = tokio::spawn(async move {
            if abort_reporter {
                futures::future::pending::<()>().await;
            }
            let mut result = {
                let mut owner_state = owner.lock().await;
                match &mut *owner_state {
                    RunDriverShutdownOwner::Completed(result) => result.clone(),
                    RunDriverShutdownOwner::Running(owner_handle) => {
                        let result = match owner_handle.await {
                            Ok(result) => result,
                            Err(error) => Err(TaskRunDriverShutdownError {
                                driver_errors: vec![format!(
                                    "TaskRun driver shutdown settlement owner failed: {error}"
                                )],
                                abandoned_settlements: Vec::new(),
                            }),
                        };
                        *owner_state = RunDriverShutdownOwner::Completed(result.clone());
                        result
                    }
                }
            };
            let reporter_errors = {
                let mut supervisor = reporter_store
                    .run_driver_supervisor
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                std::mem::take(&mut supervisor.shutdown_reporter_errors)
            };
            for error in reporter_errors {
                add_shutdown_driver_error(&mut result, error);
            }
            result_sender.send_replace(Some(result));
        });
        if abort_reporter {
            reporter.abort();
        }
        reporter
    }

    async fn observe_run_driver_shutdown_reporter(
        self: &std::sync::Arc<Self>,
        reporter: &std::sync::Arc<tokio::sync::Mutex<RunDriverShutdownReporter>>,
        result_sender: &tokio::sync::watch::Sender<Option<Result<(), TaskRunDriverShutdownError>>>,
    ) {
        let mut reporter_state = reporter.lock().await;
        let RunDriverShutdownReporter::Running(reporter_handle) = &mut *reporter_state else {
            return;
        };
        match reporter_handle.await {
            Ok(()) => {
                *reporter_state = RunDriverShutdownReporter::Completed;
            }
            Err(error) => {
                let reporter_error = format!("TaskRun driver shutdown reporter failed: {error}");
                self.run_driver_supervisor
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .shutdown_reporter_errors
                    .push(reporter_error);
                let owner = self
                    .run_driver_supervisor
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .shutdown_owner
                    .clone();
                let Some(owner) = owner else {
                    return;
                };
                *reporter_state = RunDriverShutdownReporter::Running(
                    self.spawn_run_driver_shutdown_reporter(owner, result_sender.clone()),
                );
            }
        }
    }

    async fn restart_run_driver_shutdown_reporter(
        self: &std::sync::Arc<Self>,
        reporter: &std::sync::Arc<tokio::sync::Mutex<RunDriverShutdownReporter>>,
        result_sender: &tokio::sync::watch::Sender<Option<Result<(), TaskRunDriverShutdownError>>>,
        error: String,
    ) {
        let mut reporter_state = reporter.lock().await;
        self.run_driver_supervisor
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .shutdown_reporter_errors
            .push(error);
        let owner = self
            .run_driver_supervisor
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .shutdown_owner
            .clone();
        let Some(owner) = owner else {
            return;
        };
        *reporter_state = RunDriverShutdownReporter::Running(
            self.spawn_run_driver_shutdown_reporter(owner, result_sender.clone()),
        );
    }

    async fn settle_run_driver_shutdown(&self) -> Result<(), TaskRunDriverShutdownError> {
        let mut driver_settlements = loop {
            let admission_released = self.run_driver_admission_idle.notified();
            let settlements = {
                let mut supervisor = self
                    .run_driver_supervisor
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                for cancel in supervisor.driver_cancels.values() {
                    cancel.cancel();
                }
                if supervisor.pending_admissions == 0 {
                    Some(std::mem::take(&mut supervisor.driver_settlements))
                } else {
                    None
                }
            };
            if let Some(settlements) = settlements {
                break settlements;
            }
            admission_released.await;
        };
        let mut driver_errors = Vec::new();
        while let Some(driver) = driver_settlements.join_next().await {
            match driver {
                Ok((_, Ok(()))) => {}
                Ok((_, Err(error))) => driver_errors.push(error),
                Err(error) => driver_errors.push(error.to_string()),
            }
        }
        let retry_error = self.retry_run_settlement_debts().await.err();
        let abandoned_settlements = if retry_error.is_some() {
            self.abandon_run_settlement_debts().await
        } else {
            Vec::new()
        };
        if let Some(error) = retry_error
            && abandoned_settlements.is_empty()
        {
            driver_errors.push(error.to_string());
        }
        let remaining_receipts = {
            let mut supervisor = self
                .run_driver_supervisor
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            supervisor.driver_cancels.clear();
            supervisor
                .execution_receipts
                .keys()
                .copied()
                .collect::<Vec<_>>()
        };
        for driver_token in remaining_receipts {
            self.release_run_driver_receipts(driver_token).await;
        }
        if driver_errors.is_empty() && abandoned_settlements.is_empty() {
            Ok(())
        } else {
            Err(TaskRunDriverShutdownError {
                driver_errors,
                abandoned_settlements,
            })
        }
    }

    pub(crate) fn active_run_driver_count(&self) -> Result<usize, String> {
        self.run_driver_supervisor
            .lock()
            .map(|supervisor| {
                supervisor
                    .driver_cancels
                    .len()
                    .saturating_add(supervisor.pending_admissions)
                    .saturating_add(supervisor.settlement_debts.len())
            })
            .map_err(|_| "TaskRuntime run driver supervisor is unavailable".to_string())
    }

    #[cfg(test)]
    pub(crate) async fn wait_run_driver_shutdown_started(&self) {
        self.run_driver_shutdown_started.notified().await;
    }

    #[cfg(test)]
    pub(crate) fn abort_next_run_driver_shutdown_reporter_for_test(&self) {
        self.abort_next_run_driver_shutdown_reporter
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn park_next_run_driver_admission_for_test(
        &self,
    ) -> Result<
        (
            std::sync::mpsc::Receiver<()>,
            std::sync::mpsc::SyncSender<()>,
        ),
        String,
    > {
        let (reserved_tx, reserved_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let mut barrier = self
            .run_driver_admission_test_barrier
            .lock()
            .map_err(|_| "TaskRuntime admission test barrier lock is poisoned".to_string())?;
        if barrier.is_some() {
            return Err("TaskRuntime admission test barrier is already installed".to_string());
        }
        *barrier = Some(RunDriverAdmissionTestBarrier {
            reserved: reserved_tx,
            release: release_rx,
        });
        Ok((reserved_rx, release_tx))
    }

    #[cfg(test)]
    pub(crate) fn park_next_run_driver_registration_for_test(
        &self,
    ) -> Result<
        (
            std::sync::mpsc::Receiver<()>,
            std::sync::mpsc::SyncSender<()>,
        ),
        String,
    > {
        let (registered_tx, registered_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let mut barrier = self
            .run_driver_registration_test_barrier
            .lock()
            .map_err(|_| "TaskRuntime registration test barrier lock is poisoned".to_string())?;
        if barrier.is_some() {
            return Err("TaskRuntime registration test barrier is already installed".to_string());
        }
        *barrier = Some(RunDriverRegistrationTestBarrier {
            registered: registered_tx,
            release: release_rx,
        });
        Ok((registered_rx, release_tx))
    }

    #[doc(hidden)]
    #[cfg(any(test, feature = "test-utils"))]
    pub fn fail_next_run_driver_registration_for_test(&self) {
        self.fail_next_run_driver_registration
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn fail_next_recovery_commit_for_test(&self) {
        self.fail_next_recovery_commit
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn fail_next_recovery_projection_for_test(&self) {
        self.fail_next_recovery_projection
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn fail_next_cell_started_for_test(&self) {
        self.fail_next_cell_started
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn fail_next_cell_started_projection_for_test(&self) {
        self.fail_next_cell_started_projection
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn fail_next_runtime_mutation_projection_for_test(&self) {
        self.fail_next_runtime_mutation_projection
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn fail_cell_terminal_writes_for_test(&self, count: usize) {
        self.fail_cell_terminal_remaining
            .store(count, std::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn active_run_driver_receipt_count(&self) -> Result<usize, String> {
        self.run_driver_supervisor
            .lock()
            .map(|supervisor| supervisor.execution_receipts.values().map(Vec::len).sum())
            .map_err(|_| "TaskRuntime run driver supervisor is unavailable".to_string())
    }

    /// Transfer a resource acquired inside a framework-spawned tool task to
    /// the exact canonical driver. Unknown, stale, or mismatched context is
    /// rejected by returning ownership to the caller.
    pub(crate) fn retain_run_driver_receipt_from_context<Receipt>(
        &self,
        run_id: &str,
        execution_context_id: &str,
        receipt: Receipt,
    ) -> Result<(), Receipt>
    where
        Receipt: RunDriverExecutionReceipt + 'static,
    {
        if !execution_context_id.starts_with(RunDriverReceiptOwner::EXECUTION_CONTEXT_PREFIX) {
            return Err(receipt);
        }
        let mut supervisor = self
            .run_driver_supervisor
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(context) = supervisor.driver_contexts.get(execution_context_id) else {
            return Err(receipt);
        };
        let token = context.driver_token;
        if context.run_id != run_id {
            return Err(receipt);
        }
        if !supervisor.driver_cancels.contains_key(&token) {
            return Err(receipt);
        }
        supervisor
            .execution_receipts
            .entry(token)
            .or_default()
            .push(Box::new(receipt));
        Ok(())
    }

    /// Retry durable terminal writes that previously failed while retaining
    /// their generation lease. A workspace transition remains Busy until the
    /// debt is settled or the application reports shutdown degradation.
    pub(crate) async fn retry_run_settlement_debts(&self) -> Result<(), StoreError> {
        let debts = {
            let mut supervisor = self
                .run_driver_supervisor
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            std::mem::take(&mut supervisor.settlement_debts)
        };
        let mut remaining = Vec::new();
        for mut debt in debts {
            match self.finalize_run(&debt.run_id, debt.target, debt.note.as_deref()) {
                Ok(_) => {
                    if let Some(driver_token) = debt.driver_token {
                        self.release_run_driver_receipts(driver_token).await;
                    }
                    drop(debt.generation_lease);
                }
                Err(error) => {
                    debt.last_error = error.to_string();
                    remaining.push(debt);
                }
            }
        }
        if remaining.is_empty() {
            return Ok(());
        }
        let details = remaining
            .iter()
            .map(|debt| format!("{}: {}", debt.run_id, debt.last_error))
            .collect::<Vec<_>>()
            .join("; ");
        self.run_driver_supervisor
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .settlement_debts
            .extend(remaining);
        Err(StoreError::InvalidPlan(format!(
            "unsettled TaskRun terminal writes: {details}"
        )))
    }

    /// Final shutdown settlement for debts that remained after the last
    /// durable retry. Preserve typed diagnostics, release each exact driver's
    /// receipts in LIFO order, then release its workspace generation lease.
    async fn abandon_run_settlement_debts(&self) -> Vec<AbandonedRunSettlement> {
        let debts = {
            let mut supervisor = self
                .run_driver_supervisor
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            std::mem::take(&mut supervisor.settlement_debts)
        };
        let mut abandoned = Vec::with_capacity(debts.len());
        for debt in debts {
            abandoned.push(AbandonedRunSettlement {
                run_id: debt.run_id.clone(),
                driver_token: debt.driver_token,
                root: debt.root.clone(),
                target: debt.target,
                error: debt.last_error.clone(),
            });
            if let Some(driver_token) = debt.driver_token {
                self.release_run_driver_receipts(driver_token).await;
            }
            drop(debt.generation_lease);
        }
        abandoned
    }

    /// Finalize a run or quarantine the supplied generation receipt for a
    /// later retry. The receipt is never dropped on an unverified write.
    pub(crate) fn finalize_run_with_lease(
        &self,
        generation_lease: &mut Option<WorkspaceGenerationLease>,
        driver_token: Option<u64>,
        run_id: &str,
        target: TaskRunStatus,
        note: Option<&str>,
    ) -> Result<TaskRun, StoreError> {
        match self.finalize_run(run_id, target, note) {
            Ok(run) => Ok(run),
            Err(error) => {
                if let Some(generation_lease) = generation_lease.take() {
                    self.run_driver_supervisor
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .settlement_debts
                        .push(RunSettlementDebt {
                            generation_lease,
                            driver_token,
                            run_id: run_id.to_string(),
                            root: self.shadow.root(),
                            target,
                            note: note.map(str::to_string),
                            last_error: error.to_string(),
                        });
                }
                Err(error)
            }
        }
    }

    /// Reserve canonical driver admission before any run mutation or secondary
    /// workspace-bound resource is acquired. Shutdown waits for every accepted
    /// reservation to register an exact driver or be dropped.
    pub(crate) fn reserve_run_driver_admission(
        self: &std::sync::Arc<Self>,
        run_id: String,
        cancel: echo_agent::agent::CancellationToken,
    ) -> Result<RunDriverAdmissionReservation, StoreError> {
        let mut supervisor = self
            .run_driver_supervisor
            .lock()
            .map_err(|_| StoreError::LockPoisoned)?;
        if !supervisor.accepting {
            return Err(StoreError::InvalidPlan(
                "task runtime is shutting down".to_string(),
            ));
        }
        supervisor.pending_admissions =
            supervisor
                .pending_admissions
                .checked_add(1)
                .ok_or_else(|| {
                    StoreError::InvalidPlan(
                        "TaskRun driver admission reservation capacity exhausted".to_string(),
                    )
                })?;
        drop(supervisor);
        let reservation = RunDriverAdmissionReservation {
            store: std::sync::Arc::clone(self),
            run_id,
            cancel,
            active: true,
        };
        #[cfg(test)]
        if let Some(barrier) = self
            .run_driver_admission_test_barrier
            .lock()
            .map_err(|_| StoreError::LockPoisoned)?
            .take()
        {
            barrier.reserved.send(()).map_err(|_| {
                StoreError::InvalidPlan(
                    "TaskRuntime admission test observer stopped before reservation".to_string(),
                )
            })?;
            barrier
                .release
                .recv_timeout(std::time::Duration::from_secs(5))
                .map_err(|error| {
                    StoreError::InvalidPlan(format!(
                        "TaskRuntime admission test barrier was not released: {error}"
                    ))
                })?;
        }
        Ok(reservation)
    }

    /// Register the exact owned driver before its caller performs any
    /// workspace-bound preparation or TaskRuntime mutation.
    pub(crate) fn register_run_driver<T>(
        self: &std::sync::Arc<Self>,
        admission: RunDriverAdmissionReservation,
        generation_lease: WorkspaceGenerationLease,
    ) -> Result<RegisteredRunDriver<T>, StoreError>
    where
        T: Send + 'static,
    {
        self.register_run_driver_with_requirement(admission, generation_lease, true)
    }

    /// Register a turn driver whose TaskRun is created lazily by `task_create`.
    /// A Chat/Auto turn that never creates a run settles successfully, while a
    /// lazily-created run remains subject to the same durable terminal contract.
    pub(crate) fn register_optional_run_driver<T>(
        self: &std::sync::Arc<Self>,
        admission: RunDriverAdmissionReservation,
        generation_lease: WorkspaceGenerationLease,
    ) -> Result<RegisteredRunDriver<T>, StoreError>
    where
        T: Send + 'static,
    {
        self.register_run_driver_with_requirement(admission, generation_lease, false)
    }

    fn register_run_driver_with_requirement<T>(
        self: &std::sync::Arc<Self>,
        mut admission: RunDriverAdmissionReservation,
        generation_lease: WorkspaceGenerationLease,
        run_required: bool,
    ) -> Result<RegisteredRunDriver<T>, StoreError>
    where
        T: Send + 'static,
    {
        #[cfg(any(test, feature = "test-utils"))]
        if self
            .fail_next_run_driver_registration
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(StoreError::InvalidPlan(
                "injected TaskRun driver registration failure".to_string(),
            ));
        }
        if !std::sync::Arc::ptr_eq(self, &admission.store) {
            return Err(StoreError::InvalidPlan(
                "TaskRun driver admission belongs to another runtime store".to_string(),
            ));
        }
        let runtime_handle = tokio::runtime::Handle::try_current().map_err(|error| {
            StoreError::InvalidPlan(format!(
                "TaskRun driver registration requires an active Tokio runtime: {error}"
            ))
        })?;
        let run_id = admission.run_id.clone();
        let cancel = admission.cancel.clone();
        let cancellation_registration =
            self.register_run_cancellation_internal(&run_id, cancel.clone())?;
        let (start_sender, start_receiver) = tokio::sync::oneshot::channel();
        let (result_sender, result_receiver) = tokio::sync::oneshot::channel();
        let mut supervisor = self
            .run_driver_supervisor
            .lock()
            .map_err(|_| StoreError::LockPoisoned)?;
        let driver_token = supervisor.next_driver_token.checked_add(1).ok_or_else(|| {
            StoreError::InvalidPlan("TaskRun driver token capacity exhausted".to_string())
        })?;
        supervisor.next_driver_token = driver_token;
        while let Some(result) = supervisor.driver_settlements.try_join_next() {
            match result {
                Ok((completed_token, Ok(()))) => {
                    supervisor.driver_cancels.remove(&completed_token);
                }
                Ok((completed_token, Err(error))) => {
                    supervisor.driver_cancels.remove(&completed_token);
                    tracing::warn!(%error, "completed TaskRun driver owner reported an error");
                }
                Err(error) => {
                    tracing::warn!(%error, "completed TaskRun driver owner failed");
                }
            }
        }
        let settlement_store = std::sync::Arc::clone(self);
        let driver_cancel = cancel.clone();
        let execution_context_id = loop {
            let candidate = format!(
                "{}{}",
                RunDriverReceiptOwner::EXECUTION_CONTEXT_PREFIX,
                uuid::Uuid::new_v4()
            );
            if !supervisor.driver_contexts.contains_key(&candidate) {
                break candidate;
            }
        };
        let receipt_owner = RunDriverReceiptOwner {
            store: std::sync::Arc::clone(self),
            driver_token,
            execution_context_id: execution_context_id.clone(),
        };
        let operation_adapter =
            super::executor::TaskRuntimeBlockingAdapter::new(std::sync::Arc::clone(self));
        let operation_reservation =
            operation_adapter.reserve_settlement("drive registered TaskRun")?;
        admission.active = false;
        supervisor.pending_admissions = supervisor.pending_admissions.saturating_sub(1);
        let reservations_idle = supervisor.pending_admissions == 0;
        if !supervisor.accepting {
            cancel.cancel();
        }
        supervisor
            .driver_cancels
            .insert(driver_token, cancel.clone());
        supervisor.driver_contexts.insert(
            execution_context_id.clone(),
            RunDriverExecutionContext {
                driver_token,
                run_id: run_id.clone(),
            },
        );
        let operation_driver_token = driver_token;
        let driver_operation = operation_adapter.spawn_reserved_settlement(
            "drive registered TaskRun",
            operation_reservation,
            async move {
            let mut generation_lease = Some(generation_lease);
            let _cancellation_registration = cancellation_registration;
            let start = start_receiver.await;
            let (mut result, should_settle) = match start {
                Ok(RunDriverStart::Execute(future)) => {
                    let result = match tokio::spawn(future).await {
                        Ok(result) => result,
                        Err(error) => {
                            let message = format!("TaskRun driver task failed: {error}");
                            Err(message)
                        }
                    };
                    (result, true)
                }
                Ok(RunDriverStart::PreparationFailed(error)) => {
                    (Err(error), true)
                }
                Ok(RunDriverStart::Reject(error)) => (Err(error), false),
                Err(error) => (
                    Err(format!(
                        "TaskRun driver preparation channel closed before start: {error}"
                    )),
                    false,
                ),
            };
            let operation_store = settlement_store.clone();
            let operation_run_id = run_id.clone();
            let (settled_result, release_receipts) =
                super::executor::TaskRuntimeBlockingAdapter::new(settlement_store.clone())
                    .run_owned("settle registered TaskRun", move || {
                let mut release_receipts = !should_settle;
                if should_settle {
                    let settlement = match &result {
                        Ok(_) => operation_store
                            .confirm_run_settled(&operation_run_id, run_required),
                        Err(error) => match operation_store.get_run(&operation_run_id) {
                            Ok(None) if !run_required => Ok(()),
                            Ok(Some(run)) if run.status == TaskRunStatus::Paused => Ok(()),
                            Ok(_) => {
                                let target = if driver_cancel.is_cancelled() {
                                    TaskRunStatus::Cancelled
                                } else {
                                    TaskRunStatus::Failed
                                };
                                operation_store
                                    .finalize_run_with_lease(
                                        &mut generation_lease,
                                        Some(driver_token),
                                        &operation_run_id,
                                        target,
                                        Some(error),
                                    )
                                    .map(|_| ())
                            }
                            Err(read_error) => Err(read_error),
                        },
                    };
                    if let Err(settlement_error) = settlement {
                        let original = result.as_ref().err().cloned().unwrap_or_else(|| {
                            "TaskRun driver returned before durable settlement".to_string()
                        });
                        if generation_lease.is_some() {
                            match operation_store.finalize_run_with_lease(
                                &mut generation_lease,
                                Some(driver_token),
                                &operation_run_id,
                                TaskRunStatus::Failed,
                                Some(&original),
                            ) {
                                Ok(_) => {
                                    release_receipts = true;
                                    result = Err(format!(
                                        "{original}; recovered non-terminal driver result after: {settlement_error}"
                                    ));
                                }
                                Err(recovery_error) => {
                                    let combined = format!(
                                        "{original}; terminal settlement failed: {settlement_error}; fallback terminal settlement failed: {recovery_error}"
                                    );
                                    result = Err(combined);
                                }
                            }
                        } else {
                            let combined =
                                format!("{original}; terminal settlement failed: {settlement_error}");
                            result = Err(combined);
                        }
                    } else {
                        release_receipts = true;
                    }
                }
                Ok((result, release_receipts))
            })
            .await?;
            result = settled_result;
            if release_receipts {
                settlement_store
                    .release_run_driver_receipts(driver_token)
                    .await;
            }
            match result {
                Ok(value) => {
                    let _ = result_sender.send(Ok(value));
                }
                Err(error) => {
                    let _ = result_sender.send(Err(error.clone()));
                }
            }
            settlement_store
                .run_driver_supervisor
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .driver_cancels
                .remove(&driver_token);
            // A terminal write failure is owned by settlement_debts together
            // with the exact generation and execution receipts. Shutdown and
            // workspace transition retry that canonical debt and report only
            // if it remains unsettled.
            Ok((driver_token, Ok(())))
            },
        );
        supervisor.driver_settlements.spawn_on(
            async move {
                match driver_operation.await {
                    Ok(Ok(result)) => result,
                    Ok(Err(error)) => (operation_driver_token, Err(error.to_string())),
                    Err(error) => (
                        operation_driver_token,
                        Err(format!("TaskRun operation receipt was lost: {error}")),
                    ),
                }
            },
            &runtime_handle,
        );
        drop(supervisor);
        if reservations_idle {
            self.run_driver_admission_idle.notify_one();
        }
        #[cfg(test)]
        if let Some(barrier) = self
            .run_driver_registration_test_barrier
            .lock()
            .map_err(|_| StoreError::LockPoisoned)?
            .take()
        {
            barrier.registered.send(()).map_err(|_| {
                StoreError::InvalidPlan(
                    "TaskRuntime registration test observer stopped before registration"
                        .to_string(),
                )
            })?;
            barrier
                .release
                .recv_timeout(std::time::Duration::from_secs(5))
                .map_err(|error| {
                    StoreError::InvalidPlan(format!(
                        "TaskRuntime registration test barrier was not released: {error}"
                    ))
                })?;
        }
        Ok(RegisteredRunDriver {
            start_sender: Some(start_sender),
            result_receiver: Some(result_receiver),
            receipt_owner: Some(receipt_owner),
            preparation_started: false,
            active: true,
        })
    }

    /// Accept an owned TaskRun driver. The caller receives only a result
    /// waiter; cancellation of that waiter does not cancel the retained task.
    #[cfg(test)]
    pub(crate) fn spawn_run_driver<T, F, Factory>(
        self: &std::sync::Arc<Self>,
        admission: RunDriverAdmissionReservation,
        generation_lease: WorkspaceGenerationLease,
        factory: Factory,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<T, String>>, StoreError>
    where
        T: Send + 'static,
        F: std::future::Future<Output = Result<T, String>> + Send + 'static,
        Factory: FnOnce(RunDriverReceiptOwner) -> F,
    {
        let registration = self.register_run_driver(admission, generation_lease)?;
        Ok(registration.start(factory))
    }

    async fn release_run_driver_receipts(&self, driver_token: u64) {
        let receipts = {
            let mut supervisor = self
                .run_driver_supervisor
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(context_id) =
                supervisor
                    .driver_contexts
                    .iter()
                    .find_map(|(context_id, context)| {
                        (context.driver_token == driver_token).then(|| context_id.clone())
                    })
            {
                supervisor.driver_contexts.remove(&context_id);
            }
            supervisor
                .execution_receipts
                .remove(&driver_token)
                .unwrap_or_default()
        };
        // Receipts are acquired TaskRuntime -> memory -> pool. Release in the
        // inverse order so asynchronous pool settlement completes before the
        // workspace-bound memory generation can be rebound.
        for receipt in receipts.into_iter().rev() {
            receipt.release().await;
        }
    }

    /// Atomically admit a binary/UI TaskRun driver, run its synchronous
    /// preparation while the current workspace generation is pinned, and
    /// transfer that pin to the canonical owned driver supervisor.
    pub fn spawn_supervised_run_driver<T, Prepared, Context, F, Factory, Preflight, Prepare>(
        self: &std::sync::Arc<Self>,
        run_id: String,
        cancel: echo_agent::agent::CancellationToken,
        preflight: Preflight,
        prepare: Prepare,
    ) -> Result<(Prepared, tokio::sync::oneshot::Receiver<Result<T, String>>), StoreError>
    where
        T: Send + 'static,
        Context: Send + 'static,
        F: std::future::Future<Output = Result<T, String>> + Send + 'static,
        Factory: FnOnce(RunDriverReceiptOwner) -> F,
        Preflight: FnOnce() -> Result<Context, StoreError> + Send + 'static,
        Prepare: FnOnce(Context) -> Result<(Prepared, Factory), StoreError>,
    {
        let admission = self.reserve_run_driver_admission(run_id, cancel)?;
        let generation_lease = self.lease_active_workspace_generation()?;
        let mut registration = self.register_run_driver(admission, generation_lease)?;
        let context = match preflight() {
            Ok(context) => context,
            Err(error) => {
                registration.reject(error.to_string());
                return Err(error);
            }
        };
        registration.mark_preparation_started();
        let (prepared, factory) = match prepare(context) {
            Ok(prepared) => prepared,
            Err(error) => {
                registration.fail_preparation(error.to_string());
                return Err(error);
            }
        };
        let waiter = registration.start(factory);
        Ok((prepared, waiter))
    }

    /// Choose between acceptance retry and durable process-recovery retry while
    /// the caller's exact driver registration pins one TaskRuntime generation.
    fn prepare_task_retry(
        &self,
        run_id: &str,
        task_id: &str,
        has_recovery_blocker: bool,
    ) -> Result<TaskRetryPreparation, StoreError> {
        if has_recovery_blocker {
            self.resolve_recovery_task(run_id, task_id, RecoveryDecision::Retry)?;
            Ok(TaskRetryPreparation::Recovery)
        } else {
            self.retry_blocked_task(run_id, task_id)
                .map(|next_attempt| TaskRetryPreparation::Acceptance { next_attempt })
        }
    }

    /// TUI/CLI retry facade. Exact supervisor registration and generation
    /// admission complete before resource preflight and before the canonical
    /// recovery-vs-acceptance mutation is selected.
    pub fn spawn_supervised_task_retry<Context, F, Factory, Preflight>(
        self: &std::sync::Arc<Self>,
        run_id: String,
        task_id: String,
        cancel: echo_agent::agent::CancellationToken,
        preflight: Preflight,
        factory: Factory,
    ) -> Result<
        (
            TaskRetryPreparation,
            tokio::sync::oneshot::Receiver<Result<(), String>>,
        ),
        StoreError,
    >
    where
        Context: Send + 'static,
        F: std::future::Future<Output = Result<(), String>> + Send + 'static,
        Factory: FnOnce(Context, RunDriverReceiptOwner) -> F,
        Preflight: FnOnce() -> Result<Context, StoreError> + Send + 'static,
    {
        let admission = self.reserve_run_driver_admission(run_id.clone(), cancel)?;
        let generation_lease = self.lease_active_workspace_generation()?;
        let mut registration = self.register_run_driver::<()>(admission, generation_lease)?;
        let context = match preflight() {
            Ok(context) => context,
            Err(error) => {
                registration.reject(error.to_string());
                return Err(error);
            }
        };
        let has_recovery_blocker = match self.list_recovery_blockers(&run_id) {
            Ok(blockers) => blockers.iter().any(|blocker| blocker.task_id == task_id),
            Err(error) => {
                registration.reject(error.to_string());
                return Err(error);
            }
        };
        registration.mark_preparation_started();
        let preparation = match self.prepare_task_retry(&run_id, &task_id, has_recovery_blocker) {
            Ok(preparation) => preparation,
            Err(error) => {
                registration.fail_preparation(error.to_string());
                return Err(error);
            }
        };
        let waiter = match preparation {
            TaskRetryPreparation::Acceptance { .. } => {
                registration.start(move |owner| factory(context, owner))
            }
            TaskRetryPreparation::Recovery => registration.start(|_| async { Ok(()) }),
        };
        Ok((preparation, waiter))
    }

    /// Async-surface variant of [`Self::spawn_supervised_task_retry`]. Driver
    /// admission remains on the Tokio runtime, while the journal-backed retry
    /// preparation runs through the process-wide bounded blocking adapter.
    ///
    /// The registration is owned by the blocking closure once file I/O starts.
    /// Dropping the caller therefore cannot release the workspace generation
    /// while the accepted non-interruptible journal mutation is still running.
    pub async fn spawn_supervised_task_retry_async<Context, F, Factory, Preflight>(
        self: &std::sync::Arc<Self>,
        run_id: String,
        task_id: String,
        cancel: echo_agent::agent::CancellationToken,
        preflight: Preflight,
        factory: Factory,
    ) -> Result<
        (
            TaskRetryPreparation,
            tokio::sync::oneshot::Receiver<Result<(), String>>,
        ),
        StoreError,
    >
    where
        Context: Send + 'static,
        F: std::future::Future<Output = Result<(), String>> + Send + 'static,
        Factory: FnOnce(Context, RunDriverReceiptOwner) -> F,
        Preflight: FnOnce() -> Result<Context, StoreError> + Send + 'static,
    {
        let admission = self.reserve_run_driver_admission(run_id.clone(), cancel)?;
        let generation_lease = self.lease_active_workspace_generation()?;
        let registration = self.register_run_driver::<()>(admission, generation_lease)?;
        let operation_store = std::sync::Arc::clone(self);
        let (registration, preparation, context) =
            super::executor::TaskRuntimeBlockingAdapter::new(std::sync::Arc::clone(self))
                .run_owned("prepare supervised task retry", move || {
                    let mut registration = registration;
                    let context = match preflight() {
                        Ok(context) => context,
                        Err(error) => {
                            registration.reject(error.to_string());
                            return Err(error);
                        }
                    };
                    registration.mark_preparation_started();
                    let has_recovery_blocker = match operation_store.list_recovery_blockers(&run_id)
                    {
                        Ok(blockers) => blockers.iter().any(|blocker| blocker.task_id == task_id),
                        Err(error) => {
                            registration.fail_preparation(error.to_string());
                            return Err(error);
                        }
                    };
                    match operation_store.prepare_task_retry(
                        &run_id,
                        &task_id,
                        has_recovery_blocker,
                    ) {
                        Ok(preparation) => Ok((registration, preparation, context)),
                        Err(error) => {
                            registration.fail_preparation(error.to_string());
                            Err(error)
                        }
                    }
                })
                .await?;
        let waiter = match preparation {
            TaskRetryPreparation::Acceptance { .. } => {
                registration.start(move |owner| factory(context, owner))
            }
            TaskRetryPreparation::Recovery => registration.start(|_| async { Ok(()) }),
        };
        Ok((preparation, waiter))
    }

    fn confirm_run_settled(&self, run_id: &str, run_required: bool) -> Result<(), StoreError> {
        let Some(run) = self.get_run(run_id)? else {
            return if run_required {
                Err(StoreError::RunNotFound(run_id.to_string()))
            } else {
                Ok(())
            };
        };
        if matches!(
            run.status,
            TaskRunStatus::Completed
                | TaskRunStatus::Failed
                | TaskRunStatus::Cancelled
                | TaskRunStatus::Paused
        ) {
            Ok(())
        } else if run.status == TaskRunStatus::Running
            && self
                .get_run_state(run_id)?
                .and_then(|snapshot| snapshot.continuation)
                .is_some_and(|state| state.enabled && state.active_turn.is_none())
        {
            // A long-horizon Goal may intentionally be Running between finite
            // RunTurns (for deferral or queued continuation). The event-folded
            // active-turn claim, not a driver future, is the authority here.
            Ok(())
        } else {
            Err(StoreError::InvalidPlan(format!(
                "TaskRun driver returned with non-terminal status {} for {run_id}",
                run.status.as_str()
            )))
        }
    }

    /// 在持有某 run 的 plan/state 写锁期间执行闭包 (F2-1 / F3-3 / F3-4)。
    ///
    /// 用 closure 模式而非返回 Guard: std::sync::MutexGuard 借自 &Mutex, 而
    /// Mutex 在 Arc 内, Arc 作为局部变量时 Guard 跨函数返回即悬垂 (自引用
    /// struct 在 Rust 里无法直接表达)。closure 把锁的获取与释放封装在内部,
    /// 闭包体内是临界区。revision compare-and-commit / transition_run 用它包裹
    /// "读事件 → 校验 → 追加 → 重建投影"全程。
    pub(super) fn with_run_lock<R>(
        &self,
        run_id: &str,
        f: impl FnOnce() -> Result<R, StoreError>,
    ) -> Result<R, StoreError> {
        let _operation = self.shadow_operation()?;
        let arc = self
            .plan_locks
            .entry(run_id.to_string())
            .or_insert_with(|| std::sync::Arc::new(std::sync::Mutex::new(())))
            .clone();
        // 持锁调用闭包; poison 时恢复 (与 working_dir 同款 into_inner, 不 panic)。
        let _guard = arc.lock().unwrap_or_else(|e| e.into_inner());
        f()
    }

    fn shadow_operation(&self) -> Result<ShadowOperation<'_>, StoreError> {
        let mut generation = self
            .shadow_generation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if generation.transitioning {
            return Err(StoreError::WorkspaceTransitionBusy {
                active_operations: generation.active_operations,
            });
        }
        generation.active_operations = generation.active_operations.saturating_add(1);
        Ok(ShadowOperation { store: self })
    }

    /// Atomically close generation admission when no operation is active.
    /// Workspace IPC gets an observable Busy error instead of blocking a Tokio
    /// runtime thread.
    pub(crate) async fn begin_workspace_transition(
        &self,
    ) -> Result<TaskRuntimeWorkspaceTransition<'_>, StoreError> {
        self.retry_run_settlement_debts().await?;
        let mut generation = self
            .shadow_generation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if generation.transitioning {
            return Err(StoreError::WorkspaceTransitionBusy {
                active_operations: generation.active_operations,
            });
        }
        generation.transitioning = true;
        let active_operations = generation.active_operations;
        drop(generation);
        let transition = TaskRuntimeWorkspaceTransition {
            store: self,
            active: true,
        };
        if active_operations != 0 {
            return Err(StoreError::WorkspaceTransitionBusy { active_operations });
        }
        Ok(transition)
    }

    /// Pin a multi-step application operation to one workspace generation.
    /// Individual store calls already take short leases; cron and other
    /// long-running adapters use this outer lease so a rebind cannot occur
    /// between their run creation, execution, and settlement writes.
    pub(crate) fn lease_active_workspace_generation(
        self: &std::sync::Arc<Self>,
    ) -> Result<WorkspaceGenerationLease, StoreError> {
        let mut generation = self
            .shadow_generation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if generation.transitioning {
            return Err(StoreError::WorkspaceTransitionBusy {
                active_operations: generation.active_operations,
            });
        }
        generation.active_operations = generation.active_operations.saturating_add(1);
        drop(generation);
        Ok(WorkspaceGenerationLease {
            store: std::sync::Arc::clone(self),
        })
    }

    /// Pin the active TaskRuntime generation for an application foreground
    /// driver. Surfaces acquire this after foreground admission and before the
    /// memory-generation and agent-pool receipts.
    pub fn lease_foreground_generation(
        self: &std::sync::Arc<Self>,
    ) -> Result<TaskRuntimeGenerationReceipt, StoreError> {
        self.lease_active_workspace_generation()
            .map(|lease| TaskRuntimeGenerationReceipt { _lease: lease })
    }

    /// Atomically switch the file authority after all operations using the
    /// previous root have completed. The store Arc and event hook stay intact.
    pub async fn rebind_shadow_root(
        &self,
        root: impl Into<PathBuf>,
        workspace_id: impl Into<String>,
    ) -> Result<(), StoreError> {
        let transition = self.begin_workspace_transition().await?;
        transition.rebind_shadow_root(root, workspace_id)
    }

    pub fn active_workspace_id(&self) -> String {
        self.shadow_generation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .workspace_id
            .clone()
    }

    pub(crate) fn bind_command_cell_runtime(
        &self,
        runtime: std::sync::Weak<super::command_cells::CommandCellRuntimeService>,
    ) {
        *self
            .command_cell_runtime
            .write()
            .unwrap_or_else(|error| error.into_inner()) = Some(runtime);
    }

    pub(crate) fn stop_owned_command_cells(&self, run_id: &str) -> Result<usize, StoreError> {
        let runtime = self
            .command_cell_runtime
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .and_then(std::sync::Weak::upgrade);
        Ok(runtime
            .map(|runtime| runtime.stop_run(&self.active_workspace_id(), run_id))
            .unwrap_or(0))
    }

    #[cfg(test)]
    pub(crate) fn active_shadow_root(&self) -> PathBuf {
        self.shadow.root()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_run_for_active_workspace(
        &self,
        run_id: &str,
        conversation_id: &str,
        root_message_id: &str,
        domain_profile: DomainProfile,
        goal: &str,
        route: &str,
        attended_mode: AttendedMode,
    ) -> Result<TaskRun, StoreError> {
        let _operation = self.shadow_operation()?;
        let workspace_id = self.active_workspace_id();
        self.create_run(
            run_id,
            &workspace_id,
            conversation_id,
            root_message_id,
            domain_profile,
            goal,
            route,
            attended_mode,
        )
    }

    /// Construct a pending run bound to the active workspace without making it
    /// visible. The caller must publish it with a framework-validated revision
    /// through `compare_and_publish_initial_revisioned_task_graph`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn prepare_run_for_active_workspace(
        &self,
        run_id: &str,
        conversation_id: &str,
        root_message_id: &str,
        domain_profile: DomainProfile,
        goal: &str,
        route: &str,
        attended_mode: AttendedMode,
    ) -> Result<TaskRun, StoreError> {
        let _operation = self.shadow_operation()?;
        Ok(new_pending_run(
            run_id,
            &self.active_workspace_id(),
            conversation_id,
            root_message_id,
            domain_profile,
            goal,
            route,
            attended_mode,
        ))
    }

    /// Build a `FileTaskStore` over the shadow, for read delegation.
    fn file_store(&self) -> Result<ShadowFileStore<'_>, StoreError> {
        let operation = self.shadow_operation()?;
        Ok(ShadowFileStore {
            _operation: operation,
            store: super::file_store::FileTaskStore::new((*self.shadow).clone()),
        })
    }

    pub(crate) fn completion_gate_projection(
        &self,
        run_id: &str,
    ) -> Result<Option<super::event_rebuild::CompletionGateProjection>, StoreError> {
        let _operation = self.shadow_operation()?;
        self.shadow
            .read_completion_gate_projection(run_id)
            .map_err(StoreError::Shadow)
    }

    // ── Runs ────────────────────────────────────────────────────────────

    /// Create a new run in `Pending` and emit `RunCreated`. Returns the
    /// existing run when `run_id` is already present.
    #[allow(clippy::too_many_arguments)] // run identity + routing fields all thread through
    pub fn create_run(
        &self,
        run_id: &str,
        workspace_id: &str,
        conversation_id: &str,
        root_message_id: &str,
        domain_profile: DomainProfile,
        goal: &str,
        route: &str,
        attended_mode: AttendedMode,
    ) -> Result<TaskRun, StoreError> {
        self.with_run_lock(run_id, || {
            if let Some(existing) = self.get_run(run_id)? {
                if existing.workspace_id != workspace_id
                    || existing.conversation_id != conversation_id
                    || existing.root_message_id != root_message_id
                    || existing.domain_profile != domain_profile
                    || existing.route != route
                    || existing.attended_mode != attended_mode
                {
                    return Err(StoreError::InvalidPlan(format!(
                        "TaskRun '{run_id}' already exists with a different immutable identity"
                    )));
                }
                return Ok(existing);
            }

            let run = new_pending_run(
                run_id,
                workspace_id,
                conversation_id,
                root_message_id,
                domain_profile,
                goal,
                route,
                attended_mode,
            );

            // U1c phase-0/0bc step-2: file is the write authority. Append the
            // RunCreated event to events.jsonl and rebuild plan.json — no SQL
            // write.
            self.commit_runtime_event(RuntimeJournalEvent::for_append(
                run.run_id.as_str(),
                None,
                None,
                RuntimeEventKind::RunCreated,
                serde_json::json!({
                    "goal": goal,
                    "goal_revision": run.goal_revision,
                    "goal_sha256": run.goal_sha256,
                    "domain_profile": domain_profile.as_str(),
                    "workspace_id": run.workspace_id,
                    "conversation_id": run.conversation_id,
                    "root_message_id": run.root_message_id,
                    "route": run.route,
                    "attended_mode": attended_mode.as_str(),
                    "created_at": echo_agent::utils::time::to_local(run.created_at).to_rfc3339(),
                }),
            ))?;
            Ok(run)
        })
    }

    /// Replace the sole authoritative Goal after an explicit local-user action.
    ///
    /// The event append is the transaction: its fold updates the Goal and keeps
    /// continuation deferred until a new Plan revision binds the new Goal.
    pub fn update_run_goal(
        &self,
        run_id: &str,
        expected_goal_revision: u64,
        new_goal: &str,
        reason: &str,
        actor_source: RunGoalActorSource,
    ) -> Result<TaskRun, StoreError> {
        let actor_user_id = crate::infra::load_or_create_cache_user_id();
        self.with_run_lock(run_id, || {
            if new_goal.trim().is_empty() {
                return Err(StoreError::GoalUpdateRejected {
                    run_id: run_id.to_string(),
                    reason: "new goal must not be empty".to_string(),
                });
            }
            if reason.trim().is_empty() {
                return Err(StoreError::GoalUpdateRejected {
                    run_id: run_id.to_string(),
                    reason: "update reason must not be empty".to_string(),
                });
            }
            if actor_user_id.trim().is_empty() {
                return Err(StoreError::GoalUpdateRejected {
                    run_id: run_id.to_string(),
                    reason: "local user identity is unavailable".to_string(),
                });
            }

            let run = self
                .get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            if run.goal_revision != expected_goal_revision {
                return Err(StoreError::GoalConflict {
                    run_id: run_id.to_string(),
                    expected: expected_goal_revision,
                    current: run.goal_revision,
                });
            }
            if run.status != TaskRunStatus::Paused {
                return Err(StoreError::GoalUpdateRejected {
                    run_id: run_id.to_string(),
                    reason: format!(
                        "run must be paused, current status is {}",
                        run.status.as_str()
                    ),
                });
            }
            if self.is_run_active(run_id) {
                return Err(StoreError::GoalUpdateRejected {
                    run_id: run_id.to_string(),
                    reason: "run still has an active driver".to_string(),
                });
            }

            let new_goal_sha256 = task_goal_sha256(new_goal);
            if new_goal_sha256 == run.goal_sha256 {
                return Err(StoreError::GoalUpdateRejected {
                    run_id: run_id.to_string(),
                    reason: "new goal is unchanged".to_string(),
                });
            }
            let new_goal_revision =
                run.goal_revision
                    .checked_add(1)
                    .ok_or_else(|| StoreError::GoalUpdateRejected {
                        run_id: run_id.to_string(),
                        reason: "goal revision overflow".to_string(),
                    })?;

            if self
                .get_run_state(run_id)?
                .and_then(|state| state.continuation)
                .and_then(|state| state.active_turn)
                .is_some()
            {
                return Err(StoreError::GoalUpdateRejected {
                    run_id: run_id.to_string(),
                    reason: "run still has an active RunTurn".to_string(),
                });
            }
            if !self.active_subagent_boundaries(run_id)?.is_empty() {
                return Err(StoreError::GoalUpdateRejected {
                    run_id: run_id.to_string(),
                    reason: "run still has an active Subagent attempt".to_string(),
                });
            }
            if self
                .list_background_cells(run_id)?
                .iter()
                .any(BackgroundCellState::is_active)
            {
                return Err(StoreError::GoalUpdateRejected {
                    run_id: run_id.to_string(),
                    reason: "run still has an active command cell".to_string(),
                });
            }

            let updated_at = Utc::now();
            let old_requirements = self
                .get_plan(run_id)?
                .as_ref()
                .map(super::completion_gate::requirements_for_plan)
                .unwrap_or_default();
            let mut events = vec![RuntimeJournalEvent::for_append(
                run_id,
                None,
                None,
                RuntimeEventKind::RunGoalUpdated,
                serde_json::json!({
                    "old_goal_revision": run.goal_revision,
                    "new_goal_revision": new_goal_revision,
                    "old_goal_sha256": run.goal_sha256,
                    "new_goal_sha256": new_goal_sha256,
                    "new_goal": new_goal,
                    "reason": reason,
                    "actor_source": actor_source.as_str(),
                    "actor_user_id": actor_user_id,
                    "updated_at": echo_agent::utils::time::to_local(updated_at).to_rfc3339(),
                    "continuation_deferred_reason": "goal_revision_unbound",
                }),
            )];
            for requirement in old_requirements {
                events.push(RuntimeJournalEvent::for_append(
                    run_id,
                    Some(requirement.task_id.as_str()),
                    None,
                    RuntimeEventKind::RequirementEvidenceInvalidated,
                    serde_json::json!({
                        "requirement_id": requirement.requirement_id,
                        "requirement_sha256": requirement.requirement_sha256,
                        "old_goal_revision": run.goal_revision,
                        "new_goal_revision": new_goal_revision,
                        "old_plan_revision": requirement.plan_revision,
                        "reason": reason,
                    }),
                ));
            }
            self.commit_runtime_events(run_id, events)?;
            self.get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))
        })
    }

    /// Record an explicit local-user decision to skip one exact requirement.
    /// The task must already be Skipped through the canonical revisioned graph.
    pub fn skip_goal_requirement(
        &self,
        run_id: &str,
        expected_goal_revision: u64,
        requirement_id: &str,
        reason: &str,
        actor_source: RunGoalActorSource,
    ) -> Result<CompletionGateReport, StoreError> {
        let actor_user_id = crate::infra::load_or_create_cache_user_id();
        self.with_run_lock(run_id, || {
            if reason.trim().is_empty() || requirement_id.trim().is_empty() {
                return Err(StoreError::RequirementSkipRejected {
                    run_id: run_id.to_string(),
                    reason: "requirement id and Skip reason must not be empty".to_string(),
                });
            }
            let run = self
                .get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            if run.goal_revision != expected_goal_revision {
                return Err(StoreError::GoalConflict {
                    run_id: run_id.to_string(),
                    expected: expected_goal_revision,
                    current: run.goal_revision,
                });
            }
            let plan = self
                .get_plan(run_id)?
                .ok_or_else(|| StoreError::PlanNotFound(run_id.to_string()))?;
            validate_plan_goal_binding(&run, &plan)?;
            let requirement = super::completion_gate::requirements_for_plan(&plan)
                .into_iter()
                .find(|item| item.requirement_id == requirement_id)
                .ok_or_else(|| StoreError::RequirementSkipRejected {
                    run_id: run_id.to_string(),
                    reason: format!("unknown requirement '{requirement_id}'"),
                })?;
            let task = plan
                .tasks
                .iter()
                .find(|item| item.id == requirement.task_id)
                .ok_or_else(|| StoreError::TaskNotFound(requirement.task_id.clone()))?;
            if task.status != echo_agent::tasks::TaskStatus::Skipped {
                return Err(StoreError::RequirementSkipRejected {
                    run_id: run_id.to_string(),
                    reason: format!(
                        "task '{}' must first be skipped through task_update(base_revision)",
                        task.id
                    ),
                });
            }
            // Audit allowlist: requirement acceptance is evidence history, not
            // operational hot state; exact Goal/hash correlation needs the
            // complete append-only evidence stream.
            let duplicate = self.list_events(run_id, 0)?.into_iter().any(|event| {
                event.event_type == RuntimeEventKind::RequirementSkipped
                    && event
                        .payload
                        .get("requirement_id")
                        .and_then(serde_json::Value::as_str)
                        == Some(requirement.requirement_id.as_str())
                    && event
                        .payload
                        .get("requirement_sha256")
                        .and_then(serde_json::Value::as_str)
                        == Some(requirement.requirement_sha256.as_str())
                    && event
                        .payload
                        .get("goal_revision")
                        .and_then(serde_json::Value::as_u64)
                        == Some(run.goal_revision)
            });
            if !duplicate {
                self.commit_runtime_event(RuntimeJournalEvent::for_append(
                    run_id,
                    Some(task.id.as_str()),
                    None,
                    RuntimeEventKind::RequirementSkipped,
                    serde_json::json!({
                        "requirement_id": requirement.requirement_id,
                        "requirement_sha256": requirement.requirement_sha256,
                        "goal_revision": run.goal_revision,
                        "plan_revision": plan.revision,
                        "reason": reason,
                        "actor_source": actor_source.as_str(),
                        "actor_user_id": actor_user_id,
                    }),
                ))?;
            }
            self.completion_gate_report(run_id)
        })
    }

    /// Bind user-uploaded attachments to a run so plan-level subagents see the
    /// same images/files as the main agent.
    ///
    /// Follows the event-sourcing pattern: append a `RunAttachmentsUpdated`
    /// event then rewrite plan.json so subsequent `get_run` reads reflect it.
    pub fn set_run_attachments(
        &self,
        run_id: &str,
        attachments: &[crate::attachments::AttachmentRef],
    ) -> Result<(), StoreError> {
        self.with_run_lock(run_id, || {
            // Validate the run exists (mirrors set_task_status / transition_run).
            self.get_run(run_id)?
                .ok_or(StoreError::RunNotFound(run_id.to_string()))?;
            self.commit_runtime_event(RuntimeJournalEvent::for_append(
                run_id,
                None,
                None,
                RuntimeEventKind::RunAttachmentsUpdated,
                serde_json::json!({ "attachments": attachments }),
            ))?;
            Ok(())
        })
    }

    /// Serialize a run transition and durably append `RunStatusChanged`.
    /// Projection refresh is self-healing, not I/O failure-atomic with append.
    /// Rejects illegal transitions (see [`TaskRunStatus::can_transition_to`]).
    pub fn transition_run(&self, run_id: &str, next: TaskRunStatus) -> Result<TaskRun, StoreError> {
        // F3-3/F3-4: 串行化"读→验证→写", 防并发 transition 丢更新 + 崩溃中态。
        // 用 closure 包裹临界区 (见 with_run_lock 说明)。
        self.with_run_lock(run_id, || {
            // U1c phase-0/0bc step-2: file is the read/write authority. Read the
            // current run from the file to validate the transition, then append the
            // status-changed event + rewrite plan.json. No SQL write.
            let run = self
                .get_run(run_id)?
                .ok_or(StoreError::RunNotFound(run_id.to_string()))?;
            let current = run.status;
            if !current.can_transition_to(next) {
                return Err(StoreError::IllegalTransition {
                    run_id: run_id.to_string(),
                    from: current.as_str().to_string(),
                    to: next.as_str().to_string(),
                });
            }
            let now = Utc::now();
            let mut events = vec![RuntimeJournalEvent::for_append(
                run_id,
                None,
                None,
                RuntimeEventKind::RunStatusChanged,
                serde_json::json!({ "from": current.as_str(), "to": next.as_str() }),
            )];
            if next == TaskRunStatus::Cancelled {
                events.push(RuntimeJournalEvent::for_append(
                    run_id,
                    None,
                    None,
                    RuntimeEventKind::RunCancelled,
                    serde_json::json!({}),
                ));
            }
            self.commit_runtime_events(run_id, events)?;
            let mut run = run;
            run.status = next;
            run.updated_at = now;
            Ok(run)
        })
    }

    /// Persist and verify a terminal TaskRun status before execution receipts
    /// may be released. Existing completed/cancelled truth wins over a late
    /// driver failure.
    pub(crate) fn finalize_run(
        &self,
        run_id: &str,
        target: TaskRunStatus,
        note: Option<&str>,
    ) -> Result<TaskRun, StoreError> {
        self.finalize_run_with_note_task(run_id, target, None, note)
    }

    pub(crate) fn finalize_run_with_note_task(
        &self,
        run_id: &str,
        target: TaskRunStatus,
        note_task_id: Option<&str>,
        note: Option<&str>,
    ) -> Result<TaskRun, StoreError> {
        if !matches!(
            target,
            TaskRunStatus::Completed | TaskRunStatus::Failed | TaskRunStatus::Cancelled
        ) {
            return Err(StoreError::InvalidPlan(format!(
                "finalize_run requires a terminal status, got {}",
                target.as_str()
            )));
        }
        self.with_run_lock(run_id, || {
            let current = self
                .get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            let mut events = Vec::new();
            if let Some(note) = note {
                events.push(RuntimeJournalEvent::for_append(
                    run_id,
                    note_task_id,
                    None,
                    RuntimeEventKind::Note,
                    serde_json::json!({ "message": note }),
                ));
            }
            if matches!(
                current.status,
                TaskRunStatus::Completed | TaskRunStatus::Cancelled
            ) || current.status == target
            {
                if !events.is_empty() {
                    self.commit_runtime_events(run_id, events)?;
                }
                return Ok(current);
            }
            let mut from = current.status;
            if target != TaskRunStatus::Cancelled && from != TaskRunStatus::Running {
                if !from.can_transition_to(TaskRunStatus::Running) {
                    return Err(StoreError::IllegalTransition {
                        run_id: run_id.to_string(),
                        from: from.as_str().to_string(),
                        to: TaskRunStatus::Running.as_str().to_string(),
                    });
                }
                events.push(RuntimeJournalEvent::for_append(
                    run_id,
                    None,
                    None,
                    RuntimeEventKind::RunStatusChanged,
                    serde_json::json!({
                        "from": from.as_str(),
                        "to": TaskRunStatus::Running.as_str(),
                    }),
                ));
                from = TaskRunStatus::Running;
            }
            if !from.can_transition_to(target) {
                return Err(StoreError::IllegalTransition {
                    run_id: run_id.to_string(),
                    from: from.as_str().to_string(),
                    to: target.as_str().to_string(),
                });
            }
            events.push(RuntimeJournalEvent::for_append(
                run_id,
                None,
                None,
                RuntimeEventKind::RunStatusChanged,
                serde_json::json!({ "from": from.as_str(), "to": target.as_str() }),
            ));
            if target == TaskRunStatus::Cancelled {
                events.push(RuntimeJournalEvent::for_append(
                    run_id,
                    None,
                    None,
                    RuntimeEventKind::RunCancelled,
                    serde_json::json!({}),
                ));
            }
            self.commit_runtime_events(run_id, events)?;

            let settled = self
                .get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            if settled.status != target {
                return Err(StoreError::InvalidPlan(format!(
                    "run {run_id} terminal write was not durable: expected {}, read back {}",
                    target.as_str(),
                    settled.status.as_str()
                )));
            }
            Ok(settled)
        })
    }

    pub(crate) fn finalize_cancelled_tasks_and_run(&self, run_id: &str) -> Result<(), StoreError> {
        self.with_run_lock(run_id, || {
            let run = self
                .get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            if run.status == TaskRunStatus::Completed {
                return Ok(());
            }
            let mut events = if let Some(graph) = self.load_revisioned_task_graph(run_id)? {
                let before = graph.snapshot;
                let mut after = before.clone();
                let settlement = echo_agent::tasks::settle_runtime_interruption(
                    &mut after,
                    before.revision,
                    echo_agent::tasks::RuntimeInterruptionDisposition::Cancelled,
                )?;
                if settlement
                    == echo_agent::tasks::RuntimeInterruptionSettlementOutcome::ReloadSnapshot
                {
                    return Err(StoreError::InvalidPlan(format!(
                        "run {run_id} changed while holding its cancellation lock"
                    )));
                }
                runtime_execution_change_events(
                    run_id,
                    &before,
                    &after,
                    Some("cancelled with parent run"),
                )?
            } else {
                Vec::new()
            };
            if run.status != TaskRunStatus::Cancelled {
                if !run.status.can_transition_to(TaskRunStatus::Cancelled) {
                    return Err(StoreError::IllegalTransition {
                        run_id: run_id.to_string(),
                        from: run.status.as_str().to_string(),
                        to: TaskRunStatus::Cancelled.as_str().to_string(),
                    });
                }
                events.push(RuntimeJournalEvent::for_append(
                    run_id,
                    None,
                    None,
                    RuntimeEventKind::RunStatusChanged,
                    serde_json::json!({
                        "from": run.status.as_str(),
                        "to": TaskRunStatus::Cancelled.as_str(),
                    }),
                ));
                events.push(RuntimeJournalEvent::for_append(
                    run_id,
                    None,
                    None,
                    RuntimeEventKind::RunCancelled,
                    serde_json::json!({}),
                ));
            }
            if !events.is_empty() {
                self.commit_runtime_events(run_id, events)?;
            }
            Ok(())
        })
    }

    /// Resume a paused run: `Paused → Running`.
    ///
    /// Test-only convenience for state-machine fixtures. Production callers
    /// must carry a journal-bound [`TaskRunResumeIdentity`].
    #[cfg(test)]
    pub fn resume_task_run(&self, run_id: &str) -> Result<TaskRun, StoreError> {
        self.with_run_lock(run_id, || {
            let mut run = self.validate_resume_locked(run_id, None)?;
            self.append_resume_events(run_id, &run, true)?;
            run.status = TaskRunStatus::Running;
            run.updated_at = Utc::now();
            Ok(run)
        })
    }

    /// Resume one exact paused TaskRun epoch without claiming a continuation
    /// turn. Ordinary planned-run executors use this path; long-horizon chat
    /// uses [`Self::resume_and_claim_run_turn_expected`].
    pub fn resume_task_run_expected(
        &self,
        expected: &TaskRunResumeIdentity,
    ) -> Result<TaskRun, StoreError> {
        let run_id = expected.run_id.as_str();
        self.with_run_lock(run_id, || {
            let mut run = self.validate_resume_locked(run_id, Some(expected))?;
            if let Err(error) = self.append_resume_events(run_id, &run, true) {
                let reconciled = self.get_run_state(run_id).map_err(|reconcile_error| {
                    StoreError::ResumeOutcomeUnknown {
                        run_id: run_id.to_string(),
                        details: format!(
                            "resume failed and journal reconciliation also failed: {error}; {reconcile_error}"
                        ),
                    }
                })?;
                match reconciled {
                    Some(snapshot)
                        if snapshot.run.status == TaskRunStatus::Running
                            && snapshot.journal_sequence > expected.journal_sequence =>
                    {
                        return Ok(snapshot.run);
                    }
                    Some(snapshot) if expected.validate_resumable(&snapshot).is_ok() => {
                        return Err(error);
                    }
                    Some(snapshot) => {
                        return Err(StoreError::ResumeOutcomeUnknown {
                            run_id: run_id.to_string(),
                            details: format!(
                                "resume could not be reconciled safely after {error}; current status is {} at journal sequence {}",
                                snapshot.run.status.as_str(),
                                snapshot.journal_sequence
                            ),
                        });
                    }
                    None => {
                        return Err(StoreError::ResumeOutcomeUnknown {
                            run_id: run_id.to_string(),
                            details: format!(
                                "resume could not be reconciled after {error}; TaskRun disappeared"
                            ),
                        });
                    }
                }
            }
            run.status = TaskRunStatus::Running;
            run.updated_at = Utc::now();
            Ok(run)
        })
    }

    fn validate_resume_locked(
        &self,
        run_id: &str,
        expected: Option<&TaskRunResumeIdentity>,
    ) -> Result<TaskRun, StoreError> {
        let blockers = self.list_recovery_blockers(run_id)?;
        if !blockers.is_empty() {
            let details = blockers
                .iter()
                .map(|blocker| format!("{}: {}", blocker.task_id, blocker.reason))
                .collect::<Vec<_>>()
                .join("; ");
            return Err(StoreError::RecoveryBlocked {
                run_id: run_id.to_string(),
                details,
            });
        }
        let snapshot = self
            .get_run_state(run_id)?
            .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
        let run = snapshot.run.clone();
        if let Some(expected) = expected
            && let Err(error) = expected.validate_resumable(&snapshot)
        {
            // Audit allowlist: exact resume inspects only its post-capture
            // suffix. Execution-path Notes are diagnostic and can be
            // published just after driver idle; every other fact remains
            // identity-changing and is rejected.
            let after_sequence = i64::try_from(expected.journal_sequence).map_err(|_| {
                StoreError::InvalidPlan("TaskRuntime sequence exceeds EKO cursor".to_string())
            })?;
            let suffix = self.list_events(run_id, after_sequence)?;
            let diagnostic_only = !suffix.is_empty()
                && suffix.iter().all(|event| {
                    event.event_type == RuntimeEventKind::Note
                        && event
                            .payload
                            .get("kind")
                            .and_then(serde_json::Value::as_str)
                            == Some("execution_path")
                });
            if diagnostic_only {
                let mut semantic_snapshot = snapshot.clone();
                semantic_snapshot.journal_sequence = expected.journal_sequence;
                expected
                    .validate_resumable(&semantic_snapshot)
                    .map_err(StoreError::InvalidPlan)?;
            } else {
                let latest = suffix
                    .last()
                    .map(|event| format!("{} at sequence {}", event.event_type.as_str(), event.seq))
                    .unwrap_or_else(|| "unavailable".to_string());
                return Err(StoreError::InvalidPlan(format!(
                    "{error}; latest event after queued identity: {latest}"
                )));
            }
        }
        let plan = self
            .get_plan(run_id)?
            .ok_or_else(|| StoreError::PlanNotFound(run_id.to_string()))?;
        validate_plan_goal_binding(&run, &plan)?;
        if let Some(continuation) = snapshot.continuation {
            if continuation.active_turn.is_some() {
                return Err(StoreError::InvalidPlan(format!(
                    "run {run_id} still has an active RunTurn; wait for exact driver settlement before resume"
                )));
            }
            if continuation
                .token_budget
                .is_some_and(|budget| continuation.tokens_used >= budget)
            {
                return Err(StoreError::InvalidPlan(format!(
                    "run {run_id} exhausted its continuation token budget"
                )));
            }
            if continuation
                .time_budget_seconds
                .is_some_and(|budget| continuation.time_used_seconds >= budget)
            {
                return Err(StoreError::InvalidPlan(format!(
                    "run {run_id} exhausted its continuation time budget"
                )));
            }
        }
        if !run.status.can_transition_to(TaskRunStatus::Running) {
            return Err(StoreError::IllegalTransition {
                run_id: run_id.to_string(),
                from: run.status.as_str().to_string(),
                to: TaskRunStatus::Running.as_str().to_string(),
            });
        }
        Ok(run)
    }

    /// Evaluate cold-start admission without changing state. The actual
    /// transition re-runs this policy under the per-run lock.
    pub fn boot_auto_resume_decision(
        &self,
        run_id: &str,
        launcher_ready: bool,
        interactive_owner_ready: bool,
    ) -> Result<BootAutoResumeDecision, StoreError> {
        self.boot_auto_resume_decision_at(
            run_id,
            launcher_ready,
            interactive_owner_ready,
            Utc::now(),
        )
    }

    /// Atomically re-check and resume a boot-recovered run. This preserves a
    /// persisted provider retry schedule; explicit user resume uses
    /// `resume_task_run` and resets it instead.
    pub fn resume_task_run_after_boot(
        &self,
        run_id: &str,
        launcher_ready: bool,
        interactive_owner_ready: bool,
    ) -> Result<BootAutoResumeOutcome, StoreError> {
        self.with_run_lock(run_id, || {
            let now = Utc::now();
            match self.boot_auto_resume_decision_at(
                run_id,
                launcher_ready,
                interactive_owner_ready,
                now,
            )? {
                BootAutoResumeDecision::Blocked(blockers) => {
                    Ok(BootAutoResumeOutcome::Blocked(blockers))
                }
                BootAutoResumeDecision::Ready {
                    retry_not_before: Some(deadline),
                } if deadline > now => Ok(BootAutoResumeOutcome::WaitingUntil(deadline)),
                BootAutoResumeDecision::Ready { .. } => {
                    let mut run = self
                        .get_run(run_id)?
                        .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
                    self.append_resume_events(run_id, &run, false)?;
                    run.status = TaskRunStatus::Running;
                    run.updated_at = now;
                    Ok(BootAutoResumeOutcome::Resumed(Box::new(run)))
                }
            }
        })
    }

    fn append_resume_events(
        &self,
        run_id: &str,
        run: &TaskRun,
        reset_provider_retry: bool,
    ) -> Result<(), StoreError> {
        let events = self.prepare_resume_events(run_id, run, reset_provider_retry)?;
        self.commit_runtime_events(run_id, events)
    }

    fn prepare_resume_events(
        &self,
        run_id: &str,
        run: &TaskRun,
        reset_provider_retry: bool,
    ) -> Result<Vec<RuntimeJournalEvent>, StoreError> {
        let mut events = if let Some(graph) = self.load_revisioned_task_graph(run_id)? {
            let before = graph.snapshot;
            let mut after = before.clone();
            let paused_tasks = before
                .tasks
                .iter()
                .filter(|task| {
                    matches!(
                        task.execution.status,
                        echo_agent::tasks::TaskStatus::Paused(_)
                    )
                })
                .cloned()
                .collect::<Vec<_>>();
            for paused in paused_tasks {
                match echo_agent::tasks::resume_runtime_task(&mut after, &paused, before.revision)?
                {
                    echo_agent::tasks::RuntimeTaskResumeOutcome::Resumed => {}
                    echo_agent::tasks::RuntimeTaskResumeOutcome::Superseded => {
                        return Err(StoreError::InvalidPlan(format!(
                            "paused task '{}' changed while holding its resume lock",
                            paused.spec.id
                        )));
                    }
                }
            }
            runtime_execution_change_events(
                run_id,
                &before,
                &after,
                Some("resumed without consuming retry budget"),
            )?
        } else {
            Vec::new()
        };
        events.extend([
            RuntimeJournalEvent::for_append(
                run_id,
                None,
                None,
                RuntimeEventKind::RunStatusChanged,
                serde_json::json!({
                    "from": run.status.as_str(),
                    "to": TaskRunStatus::Running.as_str(),
                }),
            ),
            RuntimeJournalEvent::for_append(
                run_id,
                None,
                None,
                RuntimeEventKind::RunPauseReasonChanged,
                serde_json::json!({ "reason": serde_json::Value::Null }),
            ),
            RuntimeJournalEvent::for_append(
                run_id,
                None,
                None,
                RuntimeEventKind::RunContinuationResumed,
                serde_json::json!({
                    "deferred": false,
                    "reset_blocker_audit": true,
                    "reset_provider_retry": reset_provider_retry,
                }),
            ),
        ]);
        Ok(events)
    }

    fn boot_auto_resume_decision_at(
        &self,
        run_id: &str,
        launcher_ready: bool,
        interactive_owner_ready: bool,
        now: DateTime<Utc>,
    ) -> Result<BootAutoResumeDecision, StoreError> {
        let run = self
            .get_run(run_id)?
            .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
        let state = self
            .get_run_state(run_id)?
            .and_then(|snapshot| snapshot.continuation);
        let plan = self.get_plan(run_id)?;
        let mut blockers = Vec::new();
        if run.status != TaskRunStatus::Paused {
            blockers.push(BootAutoResumeBlocker::RunNotPaused);
        }
        if state
            .as_ref()
            .and_then(|state| state.pause.as_ref())
            .map(|pause| pause.reason)
            != Some(RunPauseReason::BootRecovery)
        {
            blockers.push(BootAutoResumeBlocker::NotBootRecovery);
        }
        if !state.as_ref().is_some_and(|state| state.enabled) {
            blockers.push(BootAutoResumeBlocker::ContinuationDisabled);
        }
        if !state
            .as_ref()
            .is_some_and(|state| state.auto_resume_after_restart)
        {
            blockers.push(BootAutoResumeBlocker::AutoResumeDisabled);
        }
        if !launcher_ready {
            blockers.push(BootAutoResumeBlocker::LauncherUnavailable);
        }
        if run.attended_mode == AttendedMode::Attended && !interactive_owner_ready {
            blockers.push(BootAutoResumeBlocker::InteractiveOwnerUnavailable);
        }
        if run.workspace_id != self.active_workspace_id() {
            blockers.push(BootAutoResumeBlocker::WorkspaceMismatch);
        }
        match plan.as_ref() {
            None => blockers.push(BootAutoResumeBlocker::PlanUnavailable),
            Some(plan)
                if plan.goal_revision != run.goal_revision
                    || plan.goal_sha256 != run.goal_sha256 =>
            {
                blockers.push(BootAutoResumeBlocker::GoalPlanMismatch);
            }
            Some(_) => {}
        }
        if let Some(state) = state.as_ref() {
            if state
                .token_budget
                .is_some_and(|budget| state.tokens_used >= budget)
            {
                blockers.push(BootAutoResumeBlocker::TokenBudgetExhausted);
            }
            if state
                .time_budget_seconds
                .is_some_and(|budget| state.time_used_seconds >= budget)
            {
                blockers.push(BootAutoResumeBlocker::TimeBudgetExhausted);
            }
            if state.active_turn.is_some() {
                blockers.push(BootAutoResumeBlocker::ActiveRunTurn);
            }
        }
        if !self.active_subagent_boundaries(run_id)?.is_empty() {
            blockers.push(BootAutoResumeBlocker::ActiveSubagent);
        }
        if self
            .list_background_cells(run_id)?
            .iter()
            .any(BackgroundCellState::is_active)
        {
            blockers.push(BootAutoResumeBlocker::ActiveCommandCell);
        }
        if !self.list_recovery_blockers(run_id)?.is_empty() {
            blockers.push(BootAutoResumeBlocker::RecoveryBlocker);
        }
        if !blockers.is_empty() {
            return Ok(BootAutoResumeDecision::Blocked(blockers));
        }
        let retry_not_before = state
            .and_then(|state| state.provider_retry)
            .filter(|retry| !retry.exhausted && retry.next_retry_at > now)
            .map(|retry| retry.next_retry_at);
        Ok(BootAutoResumeDecision::Ready { retry_not_before })
    }

    /// Atomically mark a running run completed only when the latest committed
    /// revision is quiescent. A concurrent plan patch wins the same run lock
    /// and makes this return `false`, causing the executor to drain again.
    pub fn complete_run_if_quiescent(&self, run_id: &str) -> Result<bool, StoreError> {
        self.with_run_lock(run_id, || {
            let run = self
                .get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            if run.status == TaskRunStatus::Completed {
                return Ok(true);
            }
            if run.status != TaskRunStatus::Running {
                return Ok(false);
            }
            let report = self.completion_gate_report(run_id)?;
            if !report.ready {
                return Ok(false);
            }
            let active_turn = self
                .get_run_state(run_id)?
                .and_then(|state| state.continuation)
                .and_then(|continuation| continuation.active_turn);
            if active_turn.is_some() {
                // The owning RunTurn commits Goal completion in the same
                // journal batch as RunTurnFinished. Returning true tells the
                // task executor the graph is quiescent without opening a
                // Completed + active-turn crash window.
                return Ok(true);
            }
            self.commit_runtime_event(RuntimeJournalEvent::for_append(
                run_id,
                None,
                None,
                RuntimeEventKind::RunStatusChanged,
                serde_json::json!({
                    "from": TaskRunStatus::Running.as_str(),
                    "to": TaskRunStatus::Completed.as_str(),
                    "plan_revision": report.plan_revision,
                    "goal_revision": report.goal_revision,
                    "requirement_count": report.requirements.len(),
                }),
            ))?;
            Ok(true)
        })
    }

    // ── Task-level cancellation ────────────────────────────────────────────
    // These in-memory tokens let runtime control actions stop one Subagent
    // promptly without changing the immutable task specification.

    /// Register a cancellation token for a task that is about to start running.
    /// Called by the executor before dispatching the subagent. The token is a
    /// child of the run-level cancel, so run cancel still propagates.
    pub fn register_task_cancel_token(
        &self,
        run_id: &str,
        task_id: &str,
        token: echo_agent::agent::CancellationToken,
    ) {
        let key = format!("{run_id}::{task_id}");
        if let Ok(mut map) = self.task_cancel_tokens.lock() {
            map.insert(key, token);
        }
    }

    /// Remove a task's cancellation token after it completes (success/fail).
    /// Called by the executor when execute_task returns.
    pub fn unregister_task_cancel_token(&self, run_id: &str, task_id: &str) {
        let key = format!("{run_id}::{task_id}");
        if let Ok(mut map) = self.task_cancel_tokens.lock() {
            map.remove(&key);
        }
    }

    /// Cancel a specific task's Subagent if one is currently running.
    pub fn cancel_task(&self, run_id: &str, task_id: &str) {
        let key = format!("{run_id}::{task_id}");
        if let Ok(mut map) = self.task_cancel_tokens.lock() {
            #[allow(clippy::collapsible_if)]
            // nested let-Ok/let-Some reads clearer than a let-chain
            if let Some(token) = map.remove(&key) {
                token.cancel();
            }
        }
    }

    /// Register the active driver token and automatically restore/remove it
    /// when the returned guard is dropped.
    pub fn register_run_cancellation(
        self: &std::sync::Arc<Self>,
        run_id: &str,
        token: echo_agent::agent::CancellationToken,
    ) -> Result<RunCancellationRegistration, StoreError> {
        self.register_run_cancellation_internal(run_id, token)
    }

    fn register_run_cancellation_internal(
        self: &std::sync::Arc<Self>,
        run_id: &str,
        token: echo_agent::agent::CancellationToken,
    ) -> Result<RunCancellationRegistration, StoreError> {
        let registration_id = self
            .next_run_cancel_registration
            .fetch_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |current| current.checked_add(1),
            )
            .map(|previous| previous.saturating_add(1))
            .map_err(|_| {
                StoreError::InvalidPlan(
                    "TaskRun cancellation registration capacity exhausted".to_string(),
                )
            })?;
        self.run_cancel_tokens
            .lock()
            .map_err(|_| StoreError::LockPoisoned)?
            .entry(run_id.to_string())
            .or_default()
            .push((registration_id, token));
        Ok(RunCancellationRegistration {
            store: self.clone(),
            run_id: run_id.to_string(),
            registration_id,
        })
    }

    /// Whether this process currently owns a live driver for `run_id`.
    /// Persisted `Running` alone is insufficient because a killed/restarted
    /// process can leave that status behind; cleanup uses this in-memory fact
    /// to avoid touching a worktree that an active run still owns.
    pub fn is_run_active(&self, run_id: &str) -> bool {
        self.run_cancel_tokens
            .lock()
            .map(|map| map.contains_key(run_id))
            .unwrap_or(false)
    }

    /// Wait until no live driver owns this run. The notification is armed
    /// before the state check so a release between the two cannot be missed.
    pub async fn wait_for_run_driver_idle(&self, run_id: &str) {
        loop {
            let released = self.run_driver_idle.notified();
            if !self.is_run_active(run_id) {
                return;
            }
            released.await;
        }
    }

    fn active_run_cancel_tokens(&self, run_id: &str) -> Vec<echo_agent::agent::CancellationToken> {
        self.run_cancel_tokens
            .lock()
            .ok()
            .and_then(|map| map.get(run_id).cloned())
            .map(|entries| entries.into_iter().map(|(_, token)| token).collect())
            .unwrap_or_default()
    }

    /// Request cancellation through the single TaskRuntime control path.
    /// Active runs are stopped through their driver token so the executor owns
    /// the terminal transition. Runs without a driver may only be cancelled
    /// directly when they are not executing.
    pub fn request_cancel(&self, run_id: &str) -> Result<bool, StoreError> {
        let _operation = self.shadow_operation()?;
        let continuation_cut = super::continuation::capture_generation_cut(self, run_id);
        let tokens = self.active_run_cancel_tokens(run_id);
        if !tokens.is_empty() {
            // Durable Cancelled is committed before signalling the driver. It
            // therefore wins a concurrent Paused intent at the controller cut.
            self.finalize_cancelled_tasks_and_run(run_id)?;
            for token in tokens {
                token.cancel();
            }
            super::continuation::clear_launcher_at_cut(self, run_id, continuation_cut);
            self.stop_owned_command_cells(run_id)?;
            return Ok(true);
        }
        let Some(run) = self.get_run(run_id)? else {
            return Ok(false);
        };
        match run.status {
            TaskRunStatus::Pending
            | TaskRunStatus::Running
            | TaskRunStatus::Paused
            | TaskRunStatus::Failed => {
                self.finalize_cancelled_tasks_and_run(run_id)?;
                super::continuation::clear_launcher_at_cut(self, run_id, continuation_cut);
                self.stop_owned_command_cells(run_id)?;
                Ok(true)
            }
            TaskRunStatus::Cancelled | TaskRunStatus::Completed => Ok(false),
        }
    }

    /// Pause an actively driven run. The status changes first, then the same
    /// run-scoped token used for cancellation stops in-flight Subagents. The
    /// executor observes the durable Paused status and leaves the run resumable.
    pub fn request_pause(&self, run_id: &str) -> Result<bool, StoreError> {
        self.request_pause_with_reason(run_id, RunPauseReason::User, None)
    }

    /// Pause an active driver while persisting the structured reason in the
    /// same durable transition event. Background command cells intentionally keep
    /// running; explicit cancellation is the only path that stops them.
    pub fn request_pause_with_reason(
        &self,
        run_id: &str,
        reason: RunPauseReason,
        detail: Option<&str>,
    ) -> Result<bool, StoreError> {
        let continuation_cut = super::continuation::capture_generation_cut(self, run_id);
        let tokens = self.active_run_cancel_tokens(run_id);
        let transition = self.with_run_lock(run_id, || {
            let run = self
                .get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            if run.status != TaskRunStatus::Running {
                return Ok(false);
            }
            self.commit_runtime_events(
                run_id,
                vec![
                    RuntimeJournalEvent::for_append(
                        run_id,
                        None,
                        None,
                        RuntimeEventKind::RunPauseReasonChanged,
                        serde_json::json!({
                            "reason": reason.as_str(),
                            "detail": detail.map(|text| text.chars().take(600).collect::<String>()),
                        }),
                    ),
                    RuntimeJournalEvent::for_append(
                        run_id,
                        None,
                        None,
                        RuntimeEventKind::RunStatusChanged,
                        serde_json::json!({
                            "from": TaskRunStatus::Running.as_str(),
                            "to": TaskRunStatus::Paused.as_str(),
                        }),
                    ),
                ],
            )?;
            Ok(true)
        });
        let transitioned = transition?;
        if !transitioned {
            return Ok(false);
        }
        for token in tokens {
            token.cancel();
        }
        super::continuation::clear_launcher_at_cut(self, run_id, continuation_cut);
        Ok(true)
    }

    /// Unit-test fixture helper for committing a prepared initial plan.
    #[cfg(test)]
    pub(crate) fn attach_plan_for_test(&self, plan: &TaskPlan) -> Result<(), StoreError> {
        self.with_run_lock(&plan.run_id, || {
            let run = self
                .get_run(&plan.run_id)?
                .ok_or_else(|| StoreError::RunNotFound(plan.run_id.clone()))?;
            if matches!(
                run.status,
                TaskRunStatus::Completed | TaskRunStatus::Cancelled
            ) {
                return Err(StoreError::InvalidPlan(format!(
                    "cannot create a plan for terminal run {} ({:?})",
                    plan.run_id, run.status
                )));
            }
            if self.get_plan(&plan.run_id)?.is_some() {
                return Err(StoreError::InvalidPlan(
                    "plan already exists; submit a revisioned task_update".to_string(),
                ));
            }
            if plan.tasks.iter().any(|task| {
                task.status != echo_agent::tasks::TaskStatus::Pending
                    || task.retry_count != 0
                    || task.failure_fingerprint.is_some()
            }) {
                return Err(StoreError::InvalidPlan(
                    "initial plan tasks must have pending execution state".to_string(),
                ));
            }
            validate_runtime_plan(&plan.tasks)?;
            let mut committed = plan.clone();
            committed.revision = 1;
            committed.goal_revision = run.goal_revision;
            committed.goal_sha256 = run.goal_sha256;
            self.commit_runtime_event(RuntimeJournalEvent::for_append(
                plan.run_id.as_str(),
                None,
                None,
                RuntimeEventKind::PlanRevisionCommitted,
                serde_json::json!({
                    "base_revision": 0,
                    "reason": "initial complete plan",
                    "created_task_ids": committed.tasks.iter().map(|task| task.id.clone()).collect::<Vec<_>>(),
                    "plan": committed.specification(),
                }),
            ))?;
            Ok(())
        })
    }

    /// Load the product-neutral framework graph without projecting rich task
    /// execution states through EKO's smaller UI status enum.
    pub(crate) fn load_revisioned_task_graph(
        &self,
        run_id: &str,
    ) -> Result<Option<echo_agent::tasks::RevisionedTaskGraph>, StoreError> {
        let _operation = self.shadow_operation()?;
        self.shadow.ensure_projections_current(run_id)?;
        let Some(plan) = self.shadow.read_plan(run_id)? else {
            return Ok(None);
        };
        let state = self
            .shadow
            .read_run_state(run_id)?
            .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
        let run = state.run.clone();
        let mut executions = state
            .tasks
            .into_iter()
            .map(|execution| (execution.task_id.clone(), execution))
            .collect::<std::collections::HashMap<_, _>>();
        let mut tasks = Vec::with_capacity(plan.tasks.len());
        for spec in plan.tasks {
            let execution = executions
                .remove(&spec.id)
                .unwrap_or_else(|| EkoTaskExecution::pending(spec.id.clone()));
            let framework_spec = spec.to_task_spec().map_err(StoreError::InvalidPlan)?;
            tasks.push(echo_agent::tasks::Task {
                spec: framework_spec,
                execution: echo_agent::tasks::TaskExecution {
                    task_id: execution.task_id,
                    status: execution.status,
                    retry_count: execution.retry_count,
                    failure_fingerprint: execution.failure_fingerprint,
                    claim: execution.claim,
                },
            });
        }
        let context_metadata = serde_json::to_value(EkoPlanMetadata {
            plan_id: plan.plan_id,
            domain_profile: plan.domain_profile,
            goal_revision: plan.goal_revision,
            goal_sha256: plan.goal_sha256,
        })?;
        Ok(Some(echo_agent::tasks::RevisionedTaskGraph {
            snapshot: echo_agent::tasks::RuntimePlanSnapshot {
                revision: plan.revision,
                tasks,
            },
            context: echo_agent::tasks::TaskGraphContext {
                goal: run.goal,
                assumptions: plan.assumptions,
                risks: plan.risks,
                execution_mode: match plan.execution_mode {
                    ExecutionMode::Sequential => {
                        echo_agent::tasks::TaskGraphExecutionMode::Sequential
                    }
                    ExecutionMode::Parallel => echo_agent::tasks::TaskGraphExecutionMode::Parallel,
                },
                metadata: context_metadata,
            },
        }))
    }

    /// Persist one framework-computed graph candidate with optimistic
    /// concurrency. Patch semantics and DAG validation have already run in
    /// `TaskRevisionService`; this adapter only validates the EKO extension and
    /// serializes the optimistic commit and all evidence revalidation as one
    /// atomic journal batch.
    pub(crate) fn compare_and_commit_revisioned_task_graph(
        &self,
        run_id: &str,
        commit: echo_agent::tasks::TaskGraphCommit,
    ) -> Result<echo_agent::tasks::RevisionedTaskGraph, StoreError> {
        self.with_run_lock(run_id, || {
            let run = self
                .get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            if matches!(
                run.status,
                TaskRunStatus::Completed | TaskRunStatus::Cancelled
            ) {
                return Err(StoreError::InvalidPlan(format!(
                    "cannot modify terminal run {} ({:?})",
                    run_id, run.status
                )));
            }
            let previous_plan = self.get_plan(run_id)?;
            let previous_requirements = previous_plan
                .as_ref()
                .map(super::completion_gate::requirements_for_plan)
                .unwrap_or_default();
            let current = self.load_revisioned_task_graph(run_id)?;
            let prepared = prepare_revisioned_graph_commit(run_id, &run, current.as_ref(), commit)?;
            let revalidated_requirements = previous_plan
                .as_ref()
                .filter(|previous| previous.goal_revision != run.goal_revision)
                .map(|_previous| {
                    super::completion_gate::requirements_for_revision(&prepared.plan)
                        .into_iter()
                        .filter_map(|requirement| {
                            previous_requirements
                                .iter()
                                .find(|old| {
                                    old.requirement_id == requirement.requirement_id
                                        && old.requirement_sha256 == requirement.requirement_sha256
                                })
                                .map(|old| (old.clone(), requirement))
                        })
                        .map(|(old, requirement)| {
                            (old.goal_revision, old.plan_revision, requirement)
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let mut events = vec![RuntimeJournalEvent::for_append(
                run_id,
                None,
                None,
                RuntimeEventKind::PlanRevisionCommitted,
                prepared.payload,
            )];
            for (old_goal_revision, old_plan_revision, requirement) in revalidated_requirements {
                events.push(RuntimeJournalEvent::for_append(
                    run_id,
                    Some(requirement.task_id.as_str()),
                    None,
                    RuntimeEventKind::RequirementEvidenceRevalidated,
                    serde_json::json!({
                        "requirement_id": requirement.requirement_id,
                        "requirement_sha256": requirement.requirement_sha256,
                        "old_goal_revision": old_goal_revision,
                        "new_goal_revision": run.goal_revision,
                        "old_plan_revision": old_plan_revision,
                        "new_plan_revision": requirement.plan_revision,
                    }),
                ));
            }
            self.commit_runtime_events(run_id, events)?;
            Ok(prepared.next)
        })
    }

    pub(crate) fn compare_and_commit_direct_completion(
        &self,
        run_id: &str,
        commit: echo_agent::tasks::TaskGraphCommit,
        summary: &TaskExecutionSummary,
        task_summary: &str,
    ) -> Result<echo_agent::tasks::RevisionedTaskGraph, StoreError> {
        self.with_run_lock(run_id, || {
            let run = self
                .get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            if run.status != TaskRunStatus::Running {
                return Err(StoreError::InvalidPlan(format!(
                    "direct completion requires a Running TaskRun, got {}",
                    run.status.as_str()
                )));
            }
            let active_turn = self
                .get_run_state(run_id)?
                .and_then(|state| state.continuation)
                .and_then(|continuation| continuation.active_turn);
            if active_turn.is_none() {
                return Err(StoreError::InvalidPlan(
                    "direct completion requires an active canonical RunTurn".to_string(),
                ));
            }
            let current = self.load_revisioned_task_graph(run_id)?;
            let prepared = prepare_revisioned_graph_commit(run_id, &run, current.as_ref(), commit)?;
            let task = prepared.plan.tasks.first().ok_or_else(|| {
                StoreError::InvalidPlan("direct completion contains no task".to_string())
            })?;
            if prepared.plan.tasks.len() != 1
                || task.id != summary.task_id
                || summary.run_id != run_id
                || task.kind != PlanTaskKind::Summary
                || !task.required_artifacts.is_empty()
                || !task.execution_checks.is_empty()
                || !task.acceptance_criteria.is_empty()
                || !task.allowed_tools.is_empty()
                || task.execution_target.is_some()
                || summary.result.status != SubagentRunStatus::Completed
                || summary.result.summary.trim().is_empty()
                || !summary.result.remaining_work.is_empty()
                || summary.result.evidence.iter().any(|evidence| {
                    evidence.kind == "file_write"
                        && evidence.outcome.as_deref() == Some("succeeded")
                })
                || !summary.result.touched_files.written.is_empty()
                || task_summary.trim().is_empty()
            {
                return Err(StoreError::InvalidPlan(
                    "direct completion does not satisfy the fixed single-summary evidence contract"
                        .to_string(),
                ));
            }
            if !self.active_subagent_boundaries(run_id)?.is_empty()
                || self
                    .list_background_cells(run_id)?
                    .iter()
                    .any(BackgroundCellState::is_active)
                || !self.list_recovery_blockers(run_id)?.is_empty()
            {
                return Err(StoreError::InvalidPlan(
                    "direct completion is blocked by active or unresolved runtime work".to_string(),
                ));
            }
            let events = vec![
                RuntimeJournalEvent::for_append(
                    run_id,
                    None,
                    None,
                    RuntimeEventKind::PlanRevisionCommitted,
                    prepared.payload,
                ),
                task_status_runtime_event(TaskStatusEvent {
                    run_id,
                    task_id: &task.id,
                    task_subject: &task.title,
                    status: echo_agent::tasks::TaskStatus::Running,
                    owner_agent: Some(&task.agent_role),
                    summary: None,
                    claim: None,
                }),
                RuntimeJournalEvent::for_append(
                    run_id,
                    Some(&task.id),
                    None,
                    RuntimeEventKind::Note,
                    serde_json::json!({
                        "kind": "summary_persisted",
                        "summary": summary,
                    }),
                ),
                task_status_runtime_event(TaskStatusEvent {
                    run_id,
                    task_id: &task.id,
                    task_subject: &task.title,
                    status: echo_agent::tasks::TaskStatus::Completed,
                    owner_agent: Some(&task.agent_role),
                    summary: Some(task_summary),
                    claim: None,
                }),
            ];
            self.commit_runtime_events(run_id, events)?;
            Ok(prepared.next)
        })
    }

    /// Publish a pending run and revision 1 as one visible file generation.
    /// A process failure before the final rename leaves only a hidden staging
    /// directory, which startup removes without exposing a partial TaskRun.
    pub(crate) fn compare_and_publish_initial_revisioned_task_graph(
        &self,
        run: &TaskRun,
        trigger: &InitialRunTriggerMetadata,
        continuation: Option<(bool, bool, Option<u64>, Option<u64>)>,
        commit: echo_agent::tasks::TaskGraphCommit,
    ) -> Result<echo_agent::tasks::RevisionedTaskGraph, StoreError> {
        self.with_run_lock(&run.run_id, || {
            if self.get_run(&run.run_id)?.is_some() {
                return Err(StoreError::PlanConflict {
                    run_id: run.run_id.clone(),
                    expected: 0,
                    current: self
                        .load_revisioned_task_graph(&run.run_id)?
                        .map(|graph| graph.snapshot.revision)
                        .unwrap_or_default(),
                });
            }
            if run.status != TaskRunStatus::Pending || run.plan_id.is_some() {
                return Err(StoreError::InvalidPlan(
                    "initial task publication requires an uncommitted pending run".to_string(),
                ));
            }
            let prepared = prepare_revisioned_graph_commit(&run.run_id, run, None, commit)?;
            let timestamp = Utc::now();
            let mut events = vec![
                super::run_authority::RuntimeJournalEvent::new(
                    run.run_id.clone(),
                    None,
                    None,
                    RuntimeEventKind::RunCreated,
                    serde_json::json!({
                        "goal": run.goal,
                        "goal_revision": run.goal_revision,
                        "goal_sha256": run.goal_sha256,
                        "domain_profile": run.domain_profile.as_str(),
                        "workspace_id": run.workspace_id,
                        "conversation_id": run.conversation_id,
                        "root_message_id": run.root_message_id,
                        "route": run.route,
                        "attended_mode": run.attended_mode.as_str(),
                        "attachments": run.attachments,
                        "created_at": echo_agent::utils::time::to_local(run.created_at).to_rfc3339(),
                    }),
                    timestamp,
                ),
                super::run_authority::RuntimeJournalEvent::new(
                    run.run_id.clone(),
                    None,
                    None,
                    RuntimeEventKind::PlanRevisionCommitted,
                    prepared.payload,
                    timestamp,
                ),
            ];
            if let Some((enabled, auto_resume, token_budget, time_budget_seconds)) = continuation {
                events.push(super::run_authority::RuntimeJournalEvent::new(
                    run.run_id.clone(),
                    None,
                    None,
                    RuntimeEventKind::RunContinuationConfigured,
                    serde_json::json!({
                        "enabled": enabled,
                        "auto_resume_after_restart": auto_resume,
                        "token_budget": token_budget,
                        "time_budget_seconds": time_budget_seconds,
                    }),
                    timestamp,
                ));
            }
            events.push(super::run_authority::RuntimeJournalEvent::new(
                run.run_id.clone(),
                None,
                None,
                RuntimeEventKind::Note,
                serde_json::json!({
                    "kind": "trigger_metadata",
                    "source": trigger.source,
                    "task_kind": trigger.kind,
                    "prompt": trigger.prompt,
                    "priority": trigger.priority.min(10),
                    "dependencies": trigger.dependencies,
                }),
                timestamp,
            ));
            self.shadow
                .publish_initial_event_batch(&run.run_id, events)?;
            Ok(prepared.next)
        })
    }

    #[cfg(test)]
    pub(crate) fn fail_next_initial_publish_before_rename(&self) {
        self.shadow.fail_next_initial_publish_before_rename();
    }

    /// Unit-test convenience for exercising the canonical framework patch
    /// engine through EKO's file commit adapter.
    #[cfg(test)]
    pub(crate) fn apply_task_patch_for_test(
        &self,
        run_id: &str,
        request: &TaskUpdateRequest,
    ) -> Result<TaskPlan, StoreError> {
        self.get_run(run_id)?
            .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
        let current = self
            .load_revisioned_task_graph(run_id)?
            .ok_or_else(|| StoreError::PlanNotFound(run_id.to_string()))?;
        if current.snapshot.revision != request.base_revision {
            return Err(StoreError::PlanConflict {
                run_id: run_id.to_string(),
                expected: request.base_revision,
                current: current.snapshot.revision,
            });
        }
        if request.reason.trim().is_empty() {
            return Err(StoreError::InvalidPlan(
                "task_update requires a non-empty reason".to_string(),
            ));
        }
        let patch = request
            .to_task_plan_patch()
            .map_err(StoreError::InvalidPlan)?;
        let application = echo_agent::tasks::TaskPatchEngine::apply_operations(
            &current.snapshot.tasks,
            patch.operations,
            false,
        )
        .map_err(|error| StoreError::InvalidPlan(error.to_string()))?;
        echo_agent::tasks::PlanValidator::default()
            .validate_task_snapshot(&application.tasks)
            .map_err(|errors| StoreError::InvalidPlan(errors.join("; ")))?;
        let next_revision = current
            .snapshot
            .revision
            .checked_add(1)
            .ok_or_else(|| StoreError::InvalidPlan("plan revision overflow".to_string()))?;
        self.compare_and_commit_revisioned_task_graph(
            run_id,
            echo_agent::tasks::TaskGraphCommit {
                expected_revision: Some(current.snapshot.revision),
                next: echo_agent::tasks::RevisionedTaskGraph {
                    snapshot: echo_agent::tasks::RuntimePlanSnapshot {
                        revision: next_revision,
                        tasks: application.tasks,
                    },
                    context: current.context,
                },
                reason: patch.reason,
                effects: application.effects,
            },
        )?;
        self.get_plan(run_id)?
            .ok_or_else(|| StoreError::PlanNotFound(run_id.to_string()))
    }

    // ── Task / todo mutations ───────────────────────────────────────────

    /// Test-only fixture helper. Production task transitions are exclusively
    /// owned by the framework runtime and revision services above.
    #[cfg(any(test, feature = "test-utils"))]
    #[doc(hidden)]
    pub fn set_task_status(
        &self,
        run_id: &str,
        task_id: &str,
        status: echo_agent::tasks::TaskStatus,
        owner_agent: Option<&str>,
        summary: Option<&str>,
    ) -> Result<(), StoreError> {
        self.with_run_lock(run_id, || {
            let status = match status {
                echo_agent::tasks::TaskStatus::Failed(detail) if detail.is_empty() => {
                    echo_agent::tasks::TaskStatus::Failed(
                        summary.unwrap_or("task failed").to_string(),
                    )
                }
                echo_agent::tasks::TaskStatus::Blocked(detail) if detail.is_empty() => {
                    echo_agent::tasks::TaskStatus::Blocked(
                        summary.unwrap_or("task blocked").to_string(),
                    )
                }
                echo_agent::tasks::TaskStatus::TimedOut { error } if error.is_empty() => {
                    echo_agent::tasks::TaskStatus::TimedOut {
                        error: summary.unwrap_or("task timed out").to_string(),
                    }
                }
                other => other,
            };
            // U1c phase-0/0bc step-2: file authority. Validate the task exists
            // (read plan from file), then append the canonical task status event with
            // explicit started_at/completed_at and rewrite plan.json. No SQL write.
            let plan = self
                .get_plan(run_id)?
                .ok_or(StoreError::PlanNotFound(run_id.to_string()))?;
            let task = plan
                .tasks
                .iter()
                .find(|task| task.id == task_id)
                .ok_or_else(|| StoreError::TaskNotFound(task_id.to_string()))?;
            self.append_task_status_event(TaskStatusEvent {
                run_id,
                task_id,
                task_subject: &task.title,
                status,
                owner_agent,
                summary,
                claim: None,
            })
        })
    }

    /// Load one coherent framework snapshot under the run mutation lock.
    pub(crate) fn load_runtime_plan_snapshot(
        &self,
        run_id: &str,
    ) -> Result<echo_agent::tasks::RuntimePlanSnapshot, StoreError> {
        self.with_run_lock(run_id, || {
            self.load_revisioned_task_graph(run_id)?
                .map(|graph| graph.snapshot)
                .ok_or_else(|| StoreError::PlanNotFound(run_id.to_string()))
        })
    }

    fn persist_runtime_execution_changes(
        &self,
        run_id: &str,
        before: &echo_agent::tasks::RuntimePlanSnapshot,
        after: &echo_agent::tasks::RuntimePlanSnapshot,
        summary: Option<&str>,
        mut product_events: Vec<RuntimeJournalEvent>,
    ) -> Result<(), StoreError> {
        let mut events = runtime_execution_change_events(run_id, before, after, summary)?;
        events.append(&mut product_events);
        if events.is_empty() {
            return Ok(());
        }
        self.commit_runtime_events(run_id, events)
    }

    fn commit_runtime_event(&self, event: RuntimeJournalEvent) -> Result<(), StoreError> {
        let run_id = event.run_id().to_string();
        self.commit_runtime_events(&run_id, vec![event])
    }

    pub(super) fn commit_runtime_events(
        &self,
        run_id: &str,
        events: Vec<RuntimeJournalEvent>,
    ) -> Result<(), StoreError> {
        let receipt = self.commit_runtime_events_with_receipt(run_id, events)?;
        self.observe_projection_receipt(run_id, &receipt);
        Ok(())
    }

    fn commit_runtime_events_with_receipt(
        &self,
        run_id: &str,
        events: Vec<RuntimeJournalEvent>,
    ) -> Result<ProjectionCommitReceipt, StoreError> {
        let committed = self.shadow.append_event_batch(run_id, events)?;
        let sequence = i64::try_from(committed.apply.last_sequence).map_err(|_| {
            StoreError::InvalidPlan("TaskRuntime sequence exceeds EKO cursor".to_string())
        })?;
        let projection = self.refresh_committed_projection(run_id, sequence);
        Ok(Self::classify_committed_projection(
            sequence,
            committed.apply.journal,
            committed.apply.checkpoint,
            committed.history,
            projection,
        ))
    }

    fn observe_projection_receipt(&self, run_id: &str, receipt: &ProjectionCommitReceipt) {
        if let ProjectionCommitReceipt::CommittedProjectionDegraded { seq, detail } = receipt {
            tracing::warn!(
                run_id,
                seq,
                %detail,
                "TaskRuntime event committed; derived projection will self-heal on read"
            );
        }
    }

    fn refresh_committed_projection(
        &self,
        run_id: &str,
        last_committed_seq: i64,
    ) -> ProjectionCommitReceipt {
        #[cfg(test)]
        if self
            .fail_next_runtime_mutation_projection
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return ProjectionCommitReceipt::CommittedProjectionDegraded {
                seq: last_committed_seq,
                detail: "injected runtime mutation projection degradation".to_string(),
            };
        }
        match self.shadow.rewrite_plan(run_id) {
            Ok(()) => ProjectionCommitReceipt::Durable {
                seq: last_committed_seq,
            },
            Err(super::file_shadow::ShadowError::CommittedProjectionDegraded { seq, detail }) => {
                ProjectionCommitReceipt::CommittedProjectionDegraded { seq, detail }
            }
            Err(error) => ProjectionCommitReceipt::CommittedProjectionDegraded {
                seq: last_committed_seq,
                detail: error.to_string(),
            },
        }
    }

    fn classify_committed_projection(
        sequence: i64,
        journal: JournalDurabilityStatus,
        checkpoint: CheckpointApplyStatus,
        history: HistoryProjectionApplyStatus,
        projection: ProjectionCommitReceipt,
    ) -> ProjectionCommitReceipt {
        let mut degraded = Vec::new();
        match journal {
            JournalDurabilityStatus::Confirmed => {}
            JournalDurabilityStatus::Unconfirmed => {
                degraded.push("journal durability unconfirmed".to_string());
            }
            JournalDurabilityStatus::Degraded { error } => {
                degraded.push(format!("journal durability degraded: {error}"));
            }
        }
        if let CheckpointApplyStatus::Degraded { error } = checkpoint {
            degraded.push(format!("checkpoint durability degraded: {error}"));
        }
        if let HistoryProjectionApplyStatus::Degraded { error } = history {
            degraded.push(format!("history projection degraded: {error}"));
        }
        if let ProjectionCommitReceipt::CommittedProjectionDegraded { detail, .. } = projection {
            degraded.push(format!("projection refresh degraded: {detail}"));
        }
        if degraded.is_empty() {
            ProjectionCommitReceipt::Durable { seq: sequence }
        } else {
            ProjectionCommitReceipt::CommittedProjectionDegraded {
                seq: sequence,
                detail: degraded.join("; "),
            }
        }
    }

    /// Atomically claim a Pending task through the framework's canonical
    /// compare-and-set transformation.
    pub fn claim_runtime_task(
        &self,
        run_id: &str,
        expected_task: &echo_agent::tasks::Task,
        expected_revision: u64,
    ) -> Result<echo_agent::tasks::RuntimeTaskClaimOutcome, StoreError> {
        self.with_run_lock(run_id, || {
            let mut graph = self
                .load_revisioned_task_graph(run_id)?
                .ok_or_else(|| StoreError::PlanNotFound(run_id.to_string()))?;
            let before = graph.snapshot.clone();
            let outcome = echo_agent::tasks::claim_runtime_task(
                &mut graph.snapshot,
                expected_task,
                expected_revision,
            )?;
            self.persist_runtime_execution_changes(
                run_id,
                &before,
                &graph.snapshot,
                None,
                Vec::new(),
            )?;
            Ok(outcome)
        })
    }

    /// Verify one physical claim against the exact persisted framework graph.
    pub fn runtime_task_claim_is_current(
        &self,
        run_id: &str,
        task_id: &str,
        claim: &echo_agent::tasks::TaskClaim,
    ) -> Result<bool, StoreError> {
        self.with_run_lock(run_id, || {
            let graph = self
                .load_revisioned_task_graph(run_id)?
                .ok_or_else(|| StoreError::PlanNotFound(run_id.to_string()))?;
            echo_agent::tasks::runtime_claim_is_current(&graph.snapshot, task_id, claim)
                .map_err(StoreError::from)
        })
    }

    /// Atomically commit one framework-owned dispatch resolution.
    pub(crate) fn settle_runtime_task_resolution(
        &self,
        run_id: &str,
        task_id: &str,
        claim: &echo_agent::tasks::TaskClaim,
        request: echo_agent::tasks::RuntimeTaskResolutionRequest,
        product: RuntimeTaskProductSettlement,
    ) -> Result<echo_agent::tasks::RuntimeTaskResolution, StoreError> {
        self.with_run_lock(run_id, || {
            let mut graph = self
                .load_revisioned_task_graph(run_id)?
                .ok_or_else(|| StoreError::PlanNotFound(run_id.to_string()))?;
            let before = graph.snapshot.clone();
            let outcome = echo_agent::tasks::settle_runtime_resolution(
                &mut graph.snapshot,
                task_id,
                claim,
                request,
            )?;
            let RuntimeTaskProductSettlement {
                summary,
                execution_summary,
                review,
                diagnostic_note,
                typed_terminal: _,
            } = product;
            let mut product_events = Vec::new();
            if outcome != echo_agent::tasks::RuntimeTaskResolution::Superseded {
                if let Some(summary) = execution_summary {
                    product_events.push(RuntimeJournalEvent::for_append(
                        run_id,
                        Some(task_id),
                        None,
                        RuntimeEventKind::Note,
                        serde_json::json!({
                            "kind": "summary_persisted",
                            "summary": summary,
                        }),
                    ));
                }
                if let Some(review) = review {
                    product_events.push(review_runtime_event(&review, Some(claim)));
                }
                if let Some(message) = diagnostic_note {
                    product_events.push(RuntimeJournalEvent::for_append(
                        run_id,
                        Some(task_id),
                        None,
                        RuntimeEventKind::Note,
                        serde_json::json!({
                            "message": message,
                            "claim_id": claim.claim_id,
                            "plan_revision": claim.revision,
                            "attempt": claim.attempt,
                            "spec_hash": claim.spec_hash,
                        }),
                    ));
                }
            }
            self.persist_runtime_execution_changes(
                run_id,
                &before,
                &graph.snapshot,
                summary.as_deref(),
                product_events,
            )?;
            Ok(outcome)
        })
    }

    /// Settle one exact claim to a terminal or product-blocked state.
    pub fn settle_runtime_task_claim(
        &self,
        run_id: &str,
        task_id: &str,
        claim: &echo_agent::tasks::TaskClaim,
        status: echo_agent::tasks::TaskStatus,
        summary: Option<String>,
    ) -> Result<echo_agent::tasks::RuntimeTaskSettlementOutcome, StoreError> {
        self.with_run_lock(run_id, || {
            let mut graph = self
                .load_revisioned_task_graph(run_id)?
                .ok_or_else(|| StoreError::PlanNotFound(run_id.to_string()))?;
            let before = graph.snapshot.clone();
            let outcome = echo_agent::tasks::settle_runtime_claim(
                &mut graph.snapshot,
                task_id,
                claim,
                status,
            )?;
            self.persist_runtime_execution_changes(
                run_id,
                &before,
                &graph.snapshot,
                summary.as_deref(),
                Vec::new(),
            )?;
            Ok(outcome)
        })
    }

    /// Settle all unfinished tasks for one exact run-level interruption.
    pub fn settle_runtime_task_interruption(
        &self,
        run_id: &str,
        expected_revision: u64,
        disposition: echo_agent::tasks::RuntimeInterruptionDisposition,
    ) -> Result<echo_agent::tasks::RuntimeInterruptionSettlementOutcome, StoreError> {
        self.with_run_lock(run_id, || {
            let mut graph = self
                .load_revisioned_task_graph(run_id)?
                .ok_or_else(|| StoreError::PlanNotFound(run_id.to_string()))?;
            let before = graph.snapshot.clone();
            let summary = match &disposition {
                echo_agent::tasks::RuntimeInterruptionDisposition::Cancelled => {
                    "run cancelled".to_string()
                }
                echo_agent::tasks::RuntimeInterruptionDisposition::Paused { reason } => {
                    reason.clone()
                }
            };
            let outcome = echo_agent::tasks::settle_runtime_interruption(
                &mut graph.snapshot,
                expected_revision,
                disposition,
            )?;
            self.persist_runtime_execution_changes(
                run_id,
                &before,
                &graph.snapshot,
                Some(&summary),
                Vec::new(),
            )?;
            Ok(outcome)
        })
    }

    /// Explicitly retry one exact unclaimed task. Run restart policy remains
    /// outside this framework mutation.
    pub fn retry_runtime_task(
        &self,
        run_id: &str,
        expected_task: &echo_agent::tasks::Task,
        expected_revision: u64,
    ) -> Result<echo_agent::tasks::RuntimeTaskRetryOutcome, StoreError> {
        self.with_run_lock(run_id, || {
            let mut graph = self
                .load_revisioned_task_graph(run_id)?
                .ok_or_else(|| StoreError::PlanNotFound(run_id.to_string()))?;
            let before = graph.snapshot.clone();
            let outcome = echo_agent::tasks::retry_runtime_task(
                &mut graph.snapshot,
                expected_task,
                expected_revision,
            )?;
            let summary = match outcome {
                echo_agent::tasks::RuntimeTaskRetryOutcome::Retried { retry_count } => {
                    Some(format!("explicit retry (retry_count {retry_count})"))
                }
                echo_agent::tasks::RuntimeTaskRetryOutcome::Exhausted { .. }
                | echo_agent::tasks::RuntimeTaskRetryOutcome::Superseded => None,
            };
            self.persist_runtime_execution_changes(
                run_id,
                &before,
                &graph.snapshot,
                summary.as_deref(),
                Vec::new(),
            )?;
            Ok(outcome)
        })
    }

    #[cfg(any(test, feature = "test-utils"))]
    fn append_task_status_event(&self, event: TaskStatusEvent<'_>) -> Result<(), StoreError> {
        let run_id = event.run_id;
        self.commit_runtime_events(run_id, vec![task_status_runtime_event(event)])
    }

    /// Atomically retry a Blocked/Failed task in a Paused/Failed run.
    ///
    /// Framework retry state and the EKO run restart event are appended in one
    /// journal batch. Dependency blockers remain a framework DAG projection;
    /// this product transaction never discovers descendants from summary text.
    pub fn retry_blocked_task(&self, run_id: &str, task_id: &str) -> Result<u32, StoreError> {
        self.with_run_lock(run_id, || {
            let run = self
                .get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            if !matches!(run.status, TaskRunStatus::Paused | TaskRunStatus::Failed) {
                return Err(StoreError::InvalidPlan(format!(
                    "run {} is {:?}; retry requires Paused or Failed",
                    run_id, run.status
                )));
            }
            if !run.status.can_transition_to(TaskRunStatus::Running) {
                return Err(StoreError::IllegalTransition {
                    run_id: run_id.to_string(),
                    from: run.status.as_str().to_string(),
                    to: TaskRunStatus::Running.as_str().to_string(),
                });
            }
            let mut graph = self
                .load_revisioned_task_graph(run_id)?
                .ok_or_else(|| StoreError::PlanNotFound(run_id.to_string()))?;
            let expected_task = graph
                .snapshot
                .tasks
                .iter()
                .find(|task| task.spec.id == task_id)
                .cloned()
                .ok_or_else(|| StoreError::TaskNotFound(task_id.to_string()))?;
            let before = graph.snapshot.clone();
            let outcome = echo_agent::tasks::retry_runtime_task(
                &mut graph.snapshot,
                &expected_task,
                before.revision,
            )?;
            let next = match outcome {
                echo_agent::tasks::RuntimeTaskRetryOutcome::Retried { retry_count } => retry_count,
                echo_agent::tasks::RuntimeTaskRetryOutcome::Exhausted {
                    retry_count,
                    max_retries,
                } => {
                    return Err(StoreError::InvalidPlan(format!(
                        "task {task_id} retry budget exhausted ({retry_count}/{max_retries})"
                    )));
                }
                echo_agent::tasks::RuntimeTaskRetryOutcome::Superseded => {
                    return Err(StoreError::InvalidPlan(format!(
                        "task {task_id} changed before retry"
                    )));
                }
            };
            let product_events = vec![
                RuntimeJournalEvent::for_append(
                    run_id,
                    None,
                    None,
                    RuntimeEventKind::Note,
                    serde_json::json!({
                        "message": format!("user retried blocked task {task_id} (attempt {next})"),
                    }),
                ),
                RuntimeJournalEvent::for_append(
                    run_id,
                    None,
                    None,
                    RuntimeEventKind::RunStatusChanged,
                    serde_json::json!({
                        "from": run.status.as_str(),
                        "to": TaskRunStatus::Running.as_str(),
                    }),
                ),
            ];
            let summary = format!("user-initiated retry (attempt {next})");
            self.persist_runtime_execution_changes(
                run_id,
                &before,
                &graph.snapshot,
                Some(&summary),
                product_events,
            )?;
            Ok(next)
        })
    }

    #[cfg(test)]
    pub(crate) fn add_review(&self, r: &ReviewResult) -> Result<(), StoreError> {
        self.with_run_lock(&r.run_id, || {
            self.commit_runtime_events(&r.run_id, vec![review_runtime_event(r, None)])
        })
    }

    pub fn add_artifact(&self, a: &Artifact) -> Result<(), StoreError> {
        self.with_run_lock(&a.run_id, || {
            // U1c phase-0/0bc step-2: file authority. ArtifactProduced carries the
            // full artifact so FileTaskStore.list_artifacts can derive it. No SQL.
            self.commit_runtime_event(RuntimeJournalEvent::for_append(
                a.run_id.as_str(),
                a.task_id.as_deref(),
                None,
                RuntimeEventKind::ArtifactProduced,
                serde_json::json!({
                    "artifact_id": a.id,
                    "kind": a.kind.as_str(),
                    "title": a.title,
                    "task_id": a.task_id,
                    "path": a.path,
                    "metadata": a.metadata,
                }),
            ))
        })
    }

    /// Persist or overwrite the per-task execution summary. Primary key is
    /// `(run_id, task_id)` so a re-execution replaces the prior summary. The
    /// write is transactional and appends a `Note` event so the GUI and the
    /// recovery path can tell when a summary was updated (consistent with the
    /// "every state-relevant change writes a TaskEvent" invariant).
    pub fn put_summary(&self, s: &TaskExecutionSummary) -> Result<(), StoreError> {
        self.with_run_lock(&s.run_id, || {
            // U1c phase-0/0bc step-2: file authority. Note{summary_persisted}
            // carries the full summary so FileTaskStore.get_summary can derive it.
            self.commit_runtime_event(RuntimeJournalEvent::for_append(
                s.run_id.as_str(),
                Some(s.task_id.as_str()),
                None,
                RuntimeEventKind::Note,
                serde_json::json!({
                    "kind": "summary_persisted",
                    // Full summary so events.jsonl can rebuild plan.json task summaries.
                    "summary": s,
                }),
            ))
        })
    }

    // ── Read paths (used by Tauri query commands + recovery) ────────────

    pub fn get_run(&self, run_id: &str) -> Result<Option<TaskRun>, StoreError> {
        // U1c phase-0/0bc step-2: read delegates to the file store (file authority).
        self.file_store()?
            .get_run(run_id)
            .map_err(|e| StoreError::InvalidPlan(format!("file read: {e}")))
    }

    /// Read just the `route` column for a given run. Returns `None` when the
    /// run does not exist.
    pub fn get_run_route(&self, run_id: &str) -> Result<Option<String>, StoreError> {
        // U1c phase-0/0bc step-2: delegate to file store, project the route field.
        self.file_store()?
            .get_run(run_id)
            .map_err(|e| StoreError::InvalidPlan(format!("file read: {e}")))
            .map(|r| r.map(|r| r.route))
    }

    /// Latest run for a conversation (used by GUI to bind a chat to its run).
    pub fn latest_run_for_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<Option<TaskRun>, StoreError> {
        self.file_store()?
            .latest_run_for_conversation(conversation_id)
            .map_err(|e| StoreError::InvalidPlan(format!("file read: {e}")))
    }

    /// Find an in-progress (Running or Paused) run for a conversation, if any.
    /// Used by the interrupt-detection logic: if a user sends a new message
    /// while a run is still executing, the system should prompt them rather
    /// than silently starting a second run.
    pub fn find_in_progress_run_by_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<Option<TaskRun>, StoreError> {
        self.file_store()?
            .find_in_progress_run_by_conversation(conversation_id)
            .map_err(|e| StoreError::InvalidPlan(format!("file read: {e}")))
    }

    pub fn list_runs_in(&self, statuses: &[TaskRunStatus]) -> Result<Vec<TaskRun>, StoreError> {
        self.file_store()?
            .list_runs_in(statuses)
            .map_err(|e| StoreError::InvalidPlan(format!("file read: {e}")))
    }

    /// Rebuild all Subagent execution instances for a run from lifecycle and
    /// usage events. `SubagentReleased.usage` is the terminal aggregate when
    /// available; usage events provide the live projection while it is running.
    pub fn list_subagent_runs(&self, run_id: &str) -> Result<Vec<SubagentRun>, StoreError> {
        self.list_subagent_run_snapshots(run_id, usize::MAX)
            .map(|snapshots| snapshots.into_iter().map(|snapshot| snapshot.run).collect())
    }

    /// Rebuild at most `limit` Subagent attempts while scanning the journal in
    /// fixed-size pages. The retained map is bounded even when the TaskRun has
    /// a long history; ordering matches [`Self::list_subagent_runs`].
    pub fn list_subagent_run_snapshots(
        &self,
        run_id: &str,
        limit: usize,
    ) -> Result<Vec<SubagentRunSnapshot>, StoreError> {
        self.project_subagent_runs(run_id, limit, None)
            .map(|runs| runs.into_values().collect())
    }

    /// Rebuild one exact Subagent attempt without first constructing the full
    /// run history vector.
    pub fn get_subagent_run_snapshot(
        &self,
        run_id: &str,
        execution_id: &str,
    ) -> Result<Option<SubagentRunSnapshot>, StoreError> {
        self.project_subagent_runs(run_id, 1, Some(execution_id))
            .map(|mut runs| runs.remove(execution_id))
    }

    fn project_subagent_runs(
        &self,
        run_id: &str,
        limit: usize,
        exact_execution_id: Option<&str>,
    ) -> Result<std::collections::BTreeMap<String, SubagentRunSnapshot>, StoreError> {
        const SCAN_PAGE_SIZE: usize = 256;

        if limit == 0 {
            return Ok(std::collections::BTreeMap::new());
        }
        let mut runs = std::collections::BTreeMap::new();
        let mut after_sequence = 0_i64;
        loop {
            let events = self.query_events_bounded(
                run_id,
                RuntimeEventQuery::new(after_sequence, SCAN_PAGE_SIZE),
            )?;
            let event_count = events.len();
            if event_count == 0 {
                break;
            }
            for event in events {
                after_sequence = event.seq;
                apply_subagent_projection_event(
                    &mut runs,
                    run_id,
                    limit,
                    exact_execution_id,
                    event,
                );
            }
            if event_count < SCAN_PAGE_SIZE {
                break;
            }
        }
        Ok(runs)
    }

    pub fn list_runs_for_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<Vec<TaskRun>, StoreError> {
        self.file_store()?
            .list_runs()
            .map_err(|error| StoreError::InvalidPlan(format!("file read: {error}")))
            .map(|runs| {
                runs.into_iter()
                    .filter(|run| run.conversation_id == conversation_id)
                    .collect()
            })
    }

    /// Remove every TaskRun owned by one conversation after its drivers have
    /// settled. The outer conversation deletion transaction owns retries; this
    /// primitive owns only TaskRuntime files and process-local projections.
    pub fn remove_conversation(&self, conversation_id: &str) -> Result<(), StoreError> {
        let _operation = self.shadow_operation()?;
        let runs = super::file_store::FileTaskStore::new((*self.shadow).clone())
            .list_runs()
            .map_err(|error| StoreError::InvalidPlan(format!("file read: {error}")))?
            .into_iter()
            .filter(|run| run.conversation_id == conversation_id)
            .collect::<Vec<_>>();
        let active_run_ids = runs
            .iter()
            .filter(|run| self.is_run_active(&run.run_id))
            .map(|run| run.run_id.clone())
            .collect::<Vec<_>>();
        if !active_run_ids.is_empty() {
            return Err(StoreError::ConversationHasActiveRuns {
                conversation_id: conversation_id.to_string(),
                run_ids: active_run_ids,
            });
        }

        let run_ids = runs.into_iter().map(|run| run.run_id).collect::<Vec<_>>();
        for run_id in &run_ids {
            self.stop_owned_command_cells(run_id)?;
        }
        let removal = self.shadow.remove_runs(&run_ids);
        let committed_degraded = matches!(
            &removal,
            Err(super::file_shadow::ShadowError::CommittedDeletionDegraded { .. })
        );
        if removal.is_err() && !committed_degraded {
            return removal.map_err(Into::into);
        }
        for run_id in &run_ids {
            super::continuation::clear_launcher(self, run_id);
            self.plan_locks.remove(run_id);
        }
        if let Ok(mut tokens) = self.run_cancel_tokens.lock() {
            for run_id in &run_ids {
                tokens.remove(run_id);
            }
        }
        if let Ok(mut tokens) = self.task_cancel_tokens.lock() {
            tokens.retain(|key, _| {
                !run_ids
                    .iter()
                    .any(|run_id| key.starts_with(&format!("{run_id}::")))
            });
        }
        if let Ok(mut controls) = self.active_subagent_controls.lock() {
            controls
                .retain(|_, target| !run_ids.iter().any(|run_id| target.belongs_to_run(run_id)));
        }
        removal.map_err(Into::into)
    }

    pub(crate) fn active_subagent_boundaries(
        &self,
        run_id: &str,
    ) -> Result<Vec<ActiveSubagentBoundary>, StoreError> {
        Ok(self
            .get_run_state(run_id)?
            .map(|state| state.event_index.active_subagents)
            .unwrap_or_default())
    }

    fn active_tool_boundaries(&self, run_id: &str) -> Result<Vec<ActiveToolBoundary>, StoreError> {
        Ok(self
            .get_run_state(run_id)?
            .map(|state| state.event_index.active_tools)
            .unwrap_or_default())
    }

    #[cfg(test)]
    fn record_recovery_blocker(
        &self,
        run_id: &str,
        task_id: &str,
        execution_id: Option<&str>,
        call_id: Option<&str>,
        tool_name: Option<&str>,
        reason: &str,
    ) -> Result<(), StoreError> {
        let _operation = self.shadow_operation()?;
        self.commit_runtime_event(RuntimeJournalEvent::for_append(
            run_id,
            Some(task_id),
            execution_id,
            RuntimeEventKind::RecoveryBlocked,
            serde_json::json!({
                "execution_id": execution_id,
                "call_id": call_id,
                "tool_name": tool_name,
                "reason": reason,
            }),
        ))
    }

    /// Recover every run whose process-scoped driver disappeared at restart.
    ///
    /// One `RunStatusChanged` event contains the complete recovery generation.
    /// Failure before that append leaves `Running` as the retry marker. Failure
    /// after it can only leave derived files stale; the next canonical read
    /// repairs those files from the event tail without appending a duplicate.
    pub fn recover_incomplete(&self) -> Result<usize, StoreError> {
        let _operation = self.shadow_operation()?;
        const INTERRUPTED: &[TaskRunStatus] = &[TaskRunStatus::Running, TaskRunStatus::Paused];
        let zombies = self.list_runs_in(INTERRUPTED)?;
        let mut recovered = 0_usize;
        for run in &zombies {
            let changed = match run.status {
                TaskRunStatus::Running => self.recover_interrupted_run(run)?,
                TaskRunStatus::Paused => self.recover_paused_orphan_cells(run)?,
                _ => false,
            };
            if changed {
                recovered = recovered.saturating_add(1);
            }
        }
        super::subagent_control::reconcile_subagent_guidance_at_boot(self)?;
        Ok(recovered)
    }

    fn recover_paused_orphan_cells(&self, run: &TaskRun) -> Result<bool, StoreError> {
        let active = self
            .list_background_cells(&run.run_id)?
            .into_iter()
            .filter(BackgroundCellState::is_active)
            .collect::<Vec<_>>();
        if active.is_empty() {
            return Ok(false);
        }
        for cell in active {
            let artifact_interrupted =
                cell.artifact_status == BackgroundCellArtifactStatus::Writing;
            self.record_background_cell_finished(
                &run.run_id,
                &cell.cell_id,
                &cell.name,
                BackgroundCellPhase::Failed,
                Some(BackgroundCellTerminalCause::Interrupted),
                Some("command cell was interrupted by process restart"),
                None,
                if artifact_interrupted {
                    BackgroundCellArtifactStatus::Failed
                } else {
                    cell.artifact_status
                },
                if artifact_interrupted {
                    Some("artifact finalization was interrupted by process restart")
                } else {
                    cell.artifact_message.as_deref()
                },
                cell.total_output_bytes,
                cell.output_truncated,
                cell.output_excerpt.as_deref(),
                cell.artifact_path.as_deref(),
                cell.artifact_sha256.as_deref(),
                cell.call_id.as_deref(),
            )?;
        }
        Ok(true)
    }

    fn recover_interrupted_run(&self, run: &TaskRun) -> Result<bool, StoreError> {
        self.with_run_lock(&run.run_id, || {
            let state = self
                .get_run_state(&run.run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run.run_id.clone()))?;
            if state.run.status != TaskRunStatus::Running {
                let was_boot_recovered = state
                    .continuation
                    .as_ref()
                    .and_then(|continuation| continuation.pause.as_ref())
                    .is_some_and(|pause| pause.reason == RunPauseReason::BootRecovery);
                self.shadow.rewrite_plan(&run.run_id)?;
                return Ok(was_boot_recovered);
            }

            let active_turn = state
                .continuation
                .as_ref()
                .and_then(|continuation| continuation.active_turn.as_ref())
                .map(|turn| serde_json::json!({ "turn_id": turn.turn_id }));
            let retention = echo_agent::utils::retention::ContentRetentionPolicy {
                max_string_chars: 1_200,
                ..Default::default()
            };
            let orphan_cells = state
                .background_cells
                .iter()
                .filter(|cell| cell.is_active())
                .map(|cell| {
                    serde_json::json!({
                        "cell_id": cell.cell_id,
                        "name": cell.name,
                        "call_id": cell.call_id,
                        "total_output_bytes": cell.total_output_bytes,
                        "output_truncated": cell.output_truncated,
                        "output_excerpt": retention.sanitize_text(
                            "cell process ended with the previous application process"
                        ),
                        "artifact_path": cell.artifact_path,
                        "artifact_sha256": cell.artifact_sha256,
                    })
                })
                .collect::<Vec<_>>();
            let plan = self.get_plan(&run.run_id)?;
            let active_subagents = self.active_subagent_boundaries(&run.run_id)?;
            let active_tools = self.active_tool_boundaries(&run.run_id)?;
            let running_task_ids = state
                .tasks
                .iter()
                .filter(|task| task.status.is_running())
                .map(|task| task.task_id.clone())
                .collect::<Vec<_>>();
            let mut recovered_tasks = Vec::with_capacity(running_task_ids.len());
            for task_id in running_task_ids {
                let task = plan
                    .as_ref()
                    .and_then(|plan| plan.tasks.iter().find(|task| task.id == task_id));
                let execution_id = task.and_then(|task| {
                    task.claim
                        .as_ref()
                        .map(|claim| claim.execution_id(&run.run_id, &task.id))
                });
                let completed_subagent = match task.and_then(|task| task.claim.as_ref()) {
                    Some(claim) => self.recoverable_subagent_result_for_attempt(
                        &run.run_id,
                        &task_id,
                        &claim.execution_id(&run.run_id, &task_id),
                        claim.revision,
                        claim.attempt,
                    )?,
                    None => None,
                };
                let active_tool = active_tools
                    .iter()
                    .find(|boundary| boundary.task_id == task_id && !boundary.replay_safe)
                    .cloned();
                let active_subagent = active_subagents
                    .iter()
                    .find(|boundary| boundary.task_id == task_id && !boundary.replay_safe)
                    .cloned();
                let (next_status, summary) = if completed_subagent.is_some() {
                    (
                        echo_agent::tasks::TaskStatus::Pending,
                        "Subagent completed before interruption; pending review",
                    )
                } else if active_tool.is_some() || active_subagent.is_some() {
                    (
                        echo_agent::tasks::TaskStatus::Blocked(
                            "mutating side effect is indeterminate after restart".to_string(),
                        ),
                        "mutating side effect is indeterminate after restart",
                    )
                } else {
                    (
                        echo_agent::tasks::TaskStatus::Pending,
                        "interrupted; pending resume",
                    )
                };
                let blocker = if matches!(&next_status, echo_agent::tasks::TaskStatus::Blocked(_)) {
                    let (boundary_execution_id, call_id, tool_name) =
                        if let Some(tool) = active_tool {
                            (tool.execution_id, Some(tool.call_id), Some(tool.tool_name))
                        } else if let Some(subagent) = active_subagent {
                            (Some(subagent.execution_id), None, None)
                        } else {
                            (execution_id, None, None)
                        };
                    Some(serde_json::json!({
                        "execution_id": boundary_execution_id,
                        "call_id": call_id,
                        "tool_name": tool_name,
                        "reason": summary,
                    }))
                } else {
                    None
                };
                let (status_name, status_detail) = task_status_wire(&next_status);
                recovered_tasks.push(serde_json::json!({
                    "task_id": task_id,
                    "status": status_name,
                    "status_detail": status_detail,
                    "summary": summary,
                    "blocker": blocker,
                }));
            }
            let recovered_subagents = active_subagents
                .iter()
                .map(|boundary| {
                    serde_json::json!({
                        "task_id": boundary.task_id,
                        "execution_id": boundary.execution_id,
                        "status": SubagentRunStatus::Failed.as_str(),
                        "terminal_cause": "process_interrupted",
                    })
                })
                .collect::<Vec<_>>();
            let recovered_tools = active_tools
                .iter()
                .map(|boundary| {
                    serde_json::json!({
                        "task_id": boundary.task_id,
                        "execution_id": boundary.execution_id,
                        "call_id": boundary.call_id,
                        "tool_name": boundary.tool_name,
                    })
                })
                .collect::<Vec<_>>();

            #[cfg(test)]
            if self
                .fail_next_recovery_commit
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                return Err(StoreError::InvalidPlan(
                    "injected recovery commit failure".to_string(),
                ));
            }
            let recovery_event = RuntimeJournalEvent::for_append(
                &run.run_id,
                None,
                None,
                RuntimeEventKind::RunStatusChanged,
                serde_json::json!({
                    "from": TaskRunStatus::Running.as_str(),
                    "to": TaskRunStatus::Paused.as_str(),
                    "recovery": {
                        "kind": "boot_recovery",
                        "message": "recovered from running (interrupted by process restart)",
                        "active_turn": active_turn,
                        "pause": {
                            "reason": RunPauseReason::BootRecovery.as_str(),
                            "detail": "the application process ended while this run was active",
                        },
                        "cells": orphan_cells,
                        "tasks": recovered_tasks,
                        "subagents": recovered_subagents,
                        "tools": recovered_tools,
                    },
                }),
            );
            #[cfg(test)]
            let inject_projection_degradation = self
                .fail_next_recovery_projection
                .swap(false, std::sync::atomic::Ordering::SeqCst);
            #[cfg(not(test))]
            let inject_projection_degradation = false;
            let receipt = if inject_projection_degradation {
                let committed = self
                    .shadow
                    .append_event_batch(&run.run_id, vec![recovery_event])?;
                let sequence = i64::try_from(committed.apply.last_sequence).map_err(|_| {
                    StoreError::InvalidPlan("TaskRuntime sequence exceeds EKO cursor".to_string())
                })?;
                Self::classify_committed_projection(
                    sequence,
                    committed.apply.journal,
                    committed.apply.checkpoint,
                    committed.history,
                    ProjectionCommitReceipt::CommittedProjectionDegraded {
                        seq: sequence,
                        detail: "injected recovery projection degradation".to_string(),
                    },
                )
            } else {
                self.commit_runtime_events_with_receipt(&run.run_id, vec![recovery_event])?
            };
            self.observe_projection_receipt(&run.run_id, &receipt);
            tracing::info!(
                run_id = %run.run_id,
                from = %run.status.as_str(),
                "recovered interrupted run -> Paused at boot"
            );
            Ok(true)
        })
    }

    pub fn get_plan(&self, run_id: &str) -> Result<Option<TaskPlan>, StoreError> {
        self.file_store()?
            .get_plan(run_id)
            .map_err(|e| StoreError::InvalidPlan(format!("file read: {e}")))
    }

    /// Return the immutable revision artifact without joining execution state.
    /// Surface callers combine this with the read-only Todo projection.
    pub fn get_plan_revision(&self, run_id: &str) -> Result<Option<PlanRevision>, StoreError> {
        self.file_store()?
            .get_plan_revision(run_id)
            .map_err(|error| StoreError::InvalidPlan(format!("file read: {error}")))
    }

    pub fn list_todos(&self, run_id: &str) -> Result<Vec<TodoItem>, StoreError> {
        self.file_store()?
            .list_todos(run_id)
            .map_err(|e| StoreError::InvalidPlan(format!("file read: {e}")))
    }

    pub fn list_events(
        &self,
        run_id: &str,
        since_seq: i64,
    ) -> Result<Vec<RuntimeTaskEvent>, StoreError> {
        self.file_store()?
            .list_events(run_id, since_seq)
            .map_err(|e| StoreError::InvalidPlan(format!("file read: {e}")))
    }

    /// Execute a bounded event query at the journal boundary. Filtering is
    /// applied while scanning fixed-size pages, so unrelated events cannot
    /// consume the caller's result limit and the complete suffix is never
    /// materialized as an intermediate vector.
    pub fn query_events_bounded(
        &self,
        run_id: &str,
        query: RuntimeEventQuery,
    ) -> Result<Vec<RuntimeTaskEvent>, StoreError> {
        const SCAN_PAGE_SIZE: usize = 256;

        if query.limit == 0 {
            return Ok(Vec::new());
        }
        let store = self.file_store()?;
        let mut after_sequence = query.after_sequence;
        let mut matched = Vec::with_capacity(query.limit.min(SCAN_PAGE_SIZE));
        loop {
            let events = store
                .list_events_bounded(run_id, after_sequence, SCAN_PAGE_SIZE)
                .map_err(|error| StoreError::InvalidPlan(format!("file read: {error}")))?;
            let event_count = events.len();
            if event_count == 0 {
                break;
            }
            for event in events {
                after_sequence = event.seq;
                let execution_matches = query
                    .execution_id
                    .as_deref()
                    .is_none_or(|execution_id| event.step_id.as_deref() == Some(execution_id));
                let type_matches =
                    query.event_types.is_empty() || query.event_types.contains(&event.event_type);
                if execution_matches && type_matches {
                    matched.push(event);
                    if matched.len() >= query.limit {
                        return Ok(matched);
                    }
                }
            }
            if event_count < SCAN_PAGE_SIZE {
                break;
            }
        }
        Ok(matched)
    }

    /// Read the deterministic checkpoint-backed run-state projection.
    pub fn get_run_state(&self, run_id: &str) -> Result<Option<RunStateSnapshot>, StoreError> {
        self.file_store()?
            .get_run_state(run_id)
            .map_err(|error| StoreError::InvalidPlan(format!("file read: {error}")))
    }

    /// Rebuild one diagnostic projection from the complete journal with an
    /// empty in-memory checkpoint. The production authority remains unchanged.
    pub fn diagnose_full_journal_projection(
        &self,
        run_id: &str,
    ) -> Result<Option<RunStateSnapshot>, StoreError> {
        let _operation = self.shadow_operation()?;
        self.shadow
            .diagnostic_full_replay(run_id)
            .map_err(|error| StoreError::InvalidPlan(format!("journal diagnostic: {error}")))
    }

    /// Configure long-horizon execution without introducing a second Goal store.
    pub fn configure_run_continuation(
        &self,
        run_id: &str,
        enabled: bool,
        auto_resume_after_restart: bool,
        token_budget: Option<u64>,
        time_budget_seconds: Option<u64>,
    ) -> Result<RunContinuationState, StoreError> {
        self.with_run_lock(run_id, || {
            self.get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            let current = self
                .get_run_state(run_id)?
                .and_then(|state| state.continuation);
            let unchanged = current.as_ref().is_some_and(|state| {
                state.enabled == enabled
                    && state.auto_resume_after_restart == auto_resume_after_restart
                    && state.token_budget == token_budget
                    && state.time_budget_seconds == time_budget_seconds
            });
            if !unchanged {
                self.commit_runtime_event(RuntimeJournalEvent::for_append(
                    run_id,
                    None,
                    None,
                    RuntimeEventKind::RunContinuationConfigured,
                    serde_json::json!({
                        "enabled": enabled,
                        "auto_resume_after_restart": auto_resume_after_restart,
                        "token_budget": token_budget,
                        "time_budget_seconds": time_budget_seconds,
                    }),
                ))?;
            }
            self.get_run_state(run_id)?
                .and_then(|state| state.continuation)
                .ok_or_else(|| {
                    StoreError::InvalidPlan(format!(
                        "continuation projection missing after configuration for {run_id}"
                    ))
                })
        })
    }

    /// Persist a deterministic cross-RunTurn retry schedule for one typed
    /// transient provider failure. Provider display text is deliberately not
    /// stored here; callers pass a stable, non-sensitive fingerprint.
    pub fn schedule_provider_retry(
        &self,
        run_id: &str,
        error_fingerprint: &str,
    ) -> Result<ProviderRetryDisposition, StoreError> {
        self.schedule_provider_retry_at(run_id, error_fingerprint, Utc::now())
    }

    #[cfg(test)]
    pub(crate) fn schedule_provider_retry_at_for_test(
        &self,
        run_id: &str,
        error_fingerprint: &str,
        now: DateTime<Utc>,
    ) -> Result<ProviderRetryDisposition, StoreError> {
        self.schedule_provider_retry_at(run_id, error_fingerprint, now)
    }

    fn schedule_provider_retry_at(
        &self,
        run_id: &str,
        error_fingerprint: &str,
        now: DateTime<Utc>,
    ) -> Result<ProviderRetryDisposition, StoreError> {
        if error_fingerprint.trim().is_empty() {
            return Err(StoreError::InvalidPlan(
                "provider retry fingerprint must not be empty".to_string(),
            ));
        }
        let continuation_cut = super::continuation::capture_generation_cut(self, run_id);
        let tokens = self.active_run_cancel_tokens(run_id);
        let disposition = self.with_run_lock(run_id, || {
            let run = self
                .get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            let continuation = self
                .get_run_state(run_id)?
                .and_then(|snapshot| snapshot.continuation)
                .filter(|state| state.enabled)
                .ok_or_else(|| {
                    StoreError::InvalidPlan(format!(
                        "run {run_id} is not configured for long-horizon continuation"
                    ))
                })?;
            let budget_pause = continuation.pause.as_ref().is_some_and(|pause| {
                matches!(
                    pause.reason,
                    RunPauseReason::TokenBudget | RunPauseReason::TimeBudget
                )
            });
            if run.status != TaskRunStatus::Running
                && !(run.status == TaskRunStatus::Paused && budget_pause)
            {
                return Err(StoreError::InvalidPlan(format!(
                    "provider retry requires a Running or budget-paused run, current status is {}",
                    run.status.as_str()
                )));
            }
            if continuation.active_turn.is_some() {
                return Err(StoreError::InvalidPlan(format!(
                    "provider retry cannot be scheduled while run {run_id} has an active RunTurn"
                )));
            }
            let previous_retry = continuation.provider_retry.as_ref();
            let attempt_count = previous_retry
                .map(|retry| retry.attempt_count.saturating_add(1))
                .unwrap_or(1);
            let first_failure_at = previous_retry
                .map(|retry| retry.first_failure_at)
                .unwrap_or(now);
            let delay_millis = stable_provider_retry_delay_millis(
                run_id,
                error_fingerprint,
                attempt_count,
            );
            let delay_i64 = i64::try_from(delay_millis).unwrap_or(i64::MAX);
            let next_retry_at = now
                .checked_add_signed(chrono::Duration::milliseconds(delay_i64))
                .ok_or_else(|| {
                    StoreError::InvalidPlan("provider retry deadline overflow".to_string())
                })?;
            let attempts_exhausted = attempt_count >= MAX_PROVIDER_RETRY_ATTEMPTS;
            let token_budget_exhausted = continuation
                .token_budget
                .is_some_and(|budget| continuation.tokens_used >= budget);
            let time_budget_exhausted = continuation
                .time_budget_seconds
                .is_some_and(|budget| continuation.time_used_seconds >= budget);
            let exhausted =
                attempts_exhausted || token_budget_exhausted || time_budget_exhausted;
            let pause_detail = exhausted.then(|| {
                if attempts_exhausted {
                    format!(
                        "provider remained unavailable after {attempt_count} durable attempts"
                    )
                } else if token_budget_exhausted {
                    "provider retry stopped because the TaskRun token budget is exhausted"
                        .to_string()
                } else {
                    "provider retry stopped because the TaskRun time budget is exhausted"
                        .to_string()
                }
            });
            self.commit_runtime_event(RuntimeJournalEvent::for_append(
                run_id,
                None,
                None,
                RuntimeEventKind::RunProviderRetryScheduled,
                serde_json::json!({
                    "error_fingerprint": error_fingerprint,
                    "attempt_count": attempt_count,
                    "delay_millis": delay_millis,
                    "next_retry_at": echo_agent::utils::time::to_local(next_retry_at).to_rfc3339(),
                    "first_failure_at": echo_agent::utils::time::to_local(first_failure_at).to_rfc3339(),
                    "exhausted": exhausted,
                    "pause_reason": exhausted.then(|| RunPauseReason::ProviderUnavailable.as_str()),
                    "pause_detail": pause_detail,
                }),
            ))?;
            let state = self
                .get_run_state(run_id)?
                .and_then(|snapshot| snapshot.continuation)
                .and_then(|state| state.provider_retry)
                .ok_or_else(|| {
                    StoreError::InvalidPlan(format!(
                        "provider retry projection missing after schedule for {run_id}"
                    ))
                })?;
            Ok(if exhausted {
                ProviderRetryDisposition::Exhausted(state)
            } else {
                ProviderRetryDisposition::Scheduled(state)
            })
        })?;
        if matches!(disposition, ProviderRetryDisposition::Exhausted(_)) {
            for token in tokens {
                token.cancel();
            }
            super::continuation::clear_launcher_at_cut(self, run_id, continuation_cut);
        }
        Ok(disposition)
    }

    /// Update only the budgets of an already-enabled continuation. Product
    /// surfaces use this instead of the bootstrap configuration API so a typo
    /// cannot silently turn an ordinary one-shot run into a long-horizon run.
    pub fn update_run_continuation_budgets(
        &self,
        run_id: &str,
        token_budget: Option<u64>,
        time_budget_seconds: Option<u64>,
    ) -> Result<RunContinuationState, StoreError> {
        if token_budget == Some(0) || time_budget_seconds == Some(0) {
            return Err(StoreError::InvalidPlan(
                "continuation budgets must be positive or omitted".to_string(),
            ));
        }
        let continuation_cut = super::continuation::capture_generation_cut(self, run_id);
        let tokens = self.active_run_cancel_tokens(run_id);
        let (updated, paused) = self.with_run_lock(run_id, || {
            let run = self
                .get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            let current = self
                .get_run_state(run_id)?
                .and_then(|snapshot| snapshot.continuation)
                .filter(|continuation| continuation.enabled)
                .ok_or_else(|| {
                    StoreError::InvalidPlan(format!(
                        "run {run_id} is not configured for long-horizon continuation"
                    ))
                })?;
            let pause_reason = if run.status == TaskRunStatus::Running
                && token_budget.is_some_and(|budget| current.tokens_used >= budget)
            {
                Some(RunPauseReason::TokenBudget)
            } else if run.status == TaskRunStatus::Running
                && time_budget_seconds.is_some_and(|budget| current.time_used_seconds >= budget)
            {
                Some(RunPauseReason::TimeBudget)
            } else {
                None
            };
            let pause_detail = pause_reason.map(|reason| match reason {
                RunPauseReason::TokenBudget => {
                    "the lowered continuation token budget is already exhausted"
                }
                RunPauseReason::TimeBudget => {
                    "the lowered continuation time budget is already exhausted"
                }
                _ => "the lowered continuation budget is already exhausted",
            });
            self.commit_runtime_event(RuntimeJournalEvent::for_append(
                run_id,
                None,
                None,
                RuntimeEventKind::RunContinuationConfigured,
                serde_json::json!({
                    "enabled": true,
                    "auto_resume_after_restart": current.auto_resume_after_restart,
                    "token_budget": token_budget,
                    "time_budget_seconds": time_budget_seconds,
                    "pause_reason": pause_reason.map(RunPauseReason::as_str),
                    "pause_detail": pause_detail,
                }),
            ))?;
            let updated = self
                .get_run_state(run_id)?
                .and_then(|state| state.continuation)
                .ok_or_else(|| {
                    StoreError::InvalidPlan(format!(
                        "continuation projection missing after budget update for {run_id}"
                    ))
                })?;
            Ok((updated, pause_reason.is_some()))
        })?;
        if paused {
            for token in tokens {
                token.cancel();
            }
            super::continuation::clear_launcher_at_cut(self, run_id, continuation_cut);
        }
        Ok(updated)
    }

    /// Atomically claim the next RunTurn ordinal when this run is eligible.
    pub fn claim_run_turn(
        &self,
        run_id: &str,
        turn_id: &str,
        origin: RunTurnOrigin,
        transcript_visibility: TurnVisibility,
    ) -> Result<RunTurnClaimOutcome, StoreError> {
        self.with_run_lock(run_id, || {
            let run = self
                .get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            let snapshot = self
                .get_run_state(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            let event = match self.prepare_run_turn_start(
                run_id,
                turn_id,
                origin,
                transcript_visibility,
                run.status,
                &snapshot,
            )? {
                RunTurnClaimPreparation::Start(event) => event,
                RunTurnClaimPreparation::NotSubmitted(reason) => {
                    return Ok(RunTurnClaimOutcome::NotSubmitted(reason));
                }
            };
            self.commit_runtime_events(run_id, vec![event])?;
            self.read_claimed_run_turn(run_id, turn_id)
        })
    }

    /// Atomically validate one queued resume identity, resume the exact paused
    /// run, and claim its first resumed RunTurn in one journal batch.
    pub fn resume_and_claim_run_turn_expected(
        &self,
        expected: &TaskRunResumeIdentity,
        turn_id: &str,
        origin: RunTurnOrigin,
        transcript_visibility: TurnVisibility,
    ) -> Result<RunTurnClaimOutcome, StoreError> {
        let run_id = expected.run_id.as_str();
        self.with_run_lock(run_id, || {
            if origin != RunTurnOrigin::Resume {
                return Err(StoreError::InvalidPlan(
                    "expected TaskRun resume identity requires RunTurn origin resume".to_string(),
                ));
            }
            let run = self.validate_resume_locked(run_id, Some(expected))?;
            let mut snapshot = self
                .get_run_state(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            let continuation = snapshot.continuation.get_or_insert_with(Default::default);
            continuation.deferred = false;
            continuation.deferred_reason = None;
            continuation.provider_retry = None;
            continuation.blocker_audit = None;
            let start = match self.prepare_run_turn_start(
                run_id,
                turn_id,
                origin,
                transcript_visibility,
                TaskRunStatus::Running,
                &snapshot,
            )? {
                RunTurnClaimPreparation::Start(event) => event,
                RunTurnClaimPreparation::NotSubmitted(reason) => {
                    return Ok(RunTurnClaimOutcome::NotSubmitted(reason));
                }
            };
            let mut events = self.prepare_resume_events(run_id, &run, true)?;
            events.push(start);
            self.commit_runtime_events(run_id, events)?;
            self.read_claimed_run_turn(run_id, turn_id)
        })
    }

    fn prepare_run_turn_start(
        &self,
        run_id: &str,
        turn_id: &str,
        origin: RunTurnOrigin,
        transcript_visibility: TurnVisibility,
        run_status: TaskRunStatus,
        snapshot: &RunStateSnapshot,
    ) -> Result<RunTurnClaimPreparation, StoreError> {
        if turn_id.trim().is_empty() {
            return Err(StoreError::InvalidPlan(
                "RunTurn id must not be empty".to_string(),
            ));
        }
        if run_status != TaskRunStatus::Running {
            return Ok(RunTurnClaimPreparation::NotSubmitted(
                ContinuationNotSubmittedReason::RunNotRunning,
            ));
        }
        let state = snapshot.continuation.clone().unwrap_or_default();
        let rejected = if !state.enabled {
            Some(ContinuationNotSubmittedReason::Disabled)
        } else if state.deferred {
            Some(ContinuationNotSubmittedReason::Deferred)
        } else if state
            .provider_retry
            .as_ref()
            .is_some_and(|retry| retry.exhausted || retry.next_retry_at > Utc::now())
        {
            Some(ContinuationNotSubmittedReason::ProviderRetryBackoff)
        } else if state.active_turn.is_some() {
            Some(ContinuationNotSubmittedReason::AlreadyRunning)
        } else if state
            .token_budget
            .is_some_and(|budget| state.tokens_used >= budget)
        {
            Some(ContinuationNotSubmittedReason::TokenBudgetExhausted)
        } else if state
            .time_budget_seconds
            .is_some_and(|budget| state.time_used_seconds >= budget)
        {
            Some(ContinuationNotSubmittedReason::TimeBudgetExhausted)
        } else {
            None
        };
        if let Some(reason) = rejected {
            return Ok(RunTurnClaimPreparation::NotSubmitted(reason));
        }
        if snapshot.event_index.started_turns.contains(turn_id) {
            return Err(StoreError::InvalidPlan(format!(
                "RunTurn id {turn_id} was already used by {run_id}"
            )));
        }
        Ok(RunTurnClaimPreparation::Start(
            RuntimeJournalEvent::for_append(
                run_id,
                None,
                None,
                RuntimeEventKind::RunTurnStarted,
                serde_json::json!({
                    "event_id": format!("{run_id}:{turn_id}:started"),
                    "turn_id": turn_id,
                    "ordinal": state.next_turn_ordinal.max(1),
                    "origin": origin.as_str(),
                    "transcript_visibility": transcript_visibility.as_str(),
                }),
            ),
        ))
    }

    fn read_claimed_run_turn(
        &self,
        run_id: &str,
        turn_id: &str,
    ) -> Result<RunTurnClaimOutcome, StoreError> {
        self.get_run_state(run_id)?
            .and_then(|snapshot| snapshot.continuation)
            .and_then(|state| state.active_turn)
            .map(RunTurnClaimOutcome::Started)
            .ok_or_else(|| {
                StoreError::InvalidPlan(format!(
                    "active RunTurn missing after claim for {run_id}:{turn_id}"
                ))
            })
    }

    /// Account a provider usage envelope exactly once. Returns true once the
    /// optional user token budget is exhausted.
    pub fn account_run_turn_usage(
        &self,
        run_id: &str,
        turn_id: &str,
        provider_event_id: &str,
        input_tokens: u64,
        output_tokens: u64,
    ) -> Result<bool, StoreError> {
        let continuation_cut = super::continuation::capture_generation_cut(self, run_id);
        let tokens = self.active_run_cancel_tokens(run_id);
        let exhausted = self.with_run_lock(run_id, || {
            let active_turn_id = self
                .get_run_state(run_id)?
                .and_then(|snapshot| snapshot.continuation)
                .and_then(|state| state.active_turn)
                .map(|turn| turn.turn_id);
            if active_turn_id.as_deref() != Some(turn_id) {
                return Err(StoreError::InvalidPlan(format!(
                    "usage event targets inactive RunTurn {turn_id} in {run_id}"
                )));
            }
            let event_id = format!("{run_id}:{turn_id}:usage:{provider_event_id}");
            let current = self
                .get_run_state(run_id)?
                .and_then(|snapshot| snapshot.continuation)
                .unwrap_or_default();
            let already_recorded = self
                .get_run_state(run_id)?
                .is_some_and(|state| state.event_index.accounted_usage.contains(&event_id));
            let added_tokens = input_tokens.saturating_add(output_tokens);
            let will_exhaust = !already_recorded
                && current.token_budget.is_some_and(|budget| {
                    current.tokens_used.saturating_add(added_tokens) >= budget
                });
            if !already_recorded {
                self.commit_runtime_event(RuntimeJournalEvent::for_append(
                    run_id,
                    None,
                    None,
                    RuntimeEventKind::RunTurnUsageAccounted,
                    serde_json::json!({
                        "event_id": event_id,
                        "turn_id": turn_id,
                        "provider_event_id": provider_event_id,
                        "input_tokens": input_tokens,
                        "output_tokens": output_tokens,
                        "source_scope": "primary_turn",
                        "pause_reason": will_exhaust.then_some(RunPauseReason::TokenBudget.as_str()),
                        "pause_detail": will_exhaust.then_some("the configured token budget was reached at a provider usage boundary"),
                    }),
                ))?;
            }
            let state = self
                .get_run_state(run_id)?
                .and_then(|snapshot| snapshot.continuation)
                .unwrap_or_default();
            Ok(state
                .token_budget
                .is_some_and(|budget| state.tokens_used >= budget))
        })?;
        if exhausted {
            for token in tokens {
                token.cancel();
            }
            super::continuation::clear_launcher_at_cut(self, run_id, continuation_cut);
        }
        Ok(exhausted)
    }

    /// Fold one PlanTask Subagent usage source into the owning Goal budget.
    /// Duration is charged only without an active parent RunTurn; otherwise
    /// that RunTurn's wall clock already includes the Subagent execution.
    #[allow(clippy::too_many_arguments)]
    pub fn account_subagent_usage(
        &self,
        run_id: &str,
        execution_id: &str,
        source_event_id: &str,
        input_tokens: u64,
        output_tokens: u64,
        duration_ms: u64,
    ) -> Result<bool, StoreError> {
        let continuation_cut = super::continuation::capture_generation_cut(self, run_id);
        let tokens = self.active_run_cancel_tokens(run_id);
        let exhausted = self.with_run_lock(run_id, || {
            let Some(current) = self
                .get_run_state(run_id)?
                .and_then(|snapshot| snapshot.continuation)
                .filter(|continuation| continuation.enabled)
            else {
                return Ok(false);
            };
            let state = self.get_run_state(run_id)?;
            let assigned = state
                .as_ref()
                .is_some_and(|state| state.event_index.assigned_subagents.contains(execution_id));
            if !assigned {
                return Err(StoreError::InvalidPlan(format!(
                    "usage event targets unknown Subagent execution {execution_id} in {run_id}"
                )));
            }
            let event_id =
                format!("{run_id}:subagent:{execution_id}:usage:{source_event_id}");
            let already_recorded = state
                .as_ref()
                .is_some_and(|state| state.event_index.accounted_usage.contains(&event_id));
            let active_turn_id = current
                .active_turn
                .as_ref()
                .map(|turn| turn.turn_id.clone());
            let elapsed_seconds = if active_turn_id.is_some() || duration_ms == 0 {
                0
            } else {
                duration_ms.saturating_add(999) / 1_000
            };
            let added_tokens = input_tokens.saturating_add(output_tokens);
            let token_exhausted = !already_recorded
                && current.token_budget.is_some_and(|budget| {
                    current.tokens_used.saturating_add(added_tokens) >= budget
                });
            let time_exhausted = !already_recorded
                && !token_exhausted
                && current.time_budget_seconds.is_some_and(|budget| {
                    current.time_used_seconds.saturating_add(elapsed_seconds) >= budget
                });
            let pause_reason = if token_exhausted {
                Some(RunPauseReason::TokenBudget)
            } else if time_exhausted {
                Some(RunPauseReason::TimeBudget)
            } else {
                None
            };
            if !already_recorded {
                self.commit_runtime_event(RuntimeJournalEvent::for_append(
                    run_id,
                    None,
                    Some(execution_id),
                    RuntimeEventKind::RunTurnUsageAccounted,
                    serde_json::json!({
                        "event_id": event_id,
                        "turn_id": active_turn_id,
                        "source_scope": "subagent",
                        "source_event_id": source_event_id,
                        "execution_id": execution_id,
                        "input_tokens": input_tokens,
                        "output_tokens": output_tokens,
                        "duration_ms": duration_ms,
                        "elapsed_seconds": elapsed_seconds,
                        "pause_reason": pause_reason.map(RunPauseReason::as_str),
                        "pause_detail": pause_reason.map(|reason| match reason {
                            RunPauseReason::TokenBudget => "a PlanTask Subagent reached the configured token budget",
                            RunPauseReason::TimeBudget => "a PlanTask Subagent reached the configured time budget",
                            _ => "a PlanTask Subagent reached a configured budget",
                        }),
                    }),
                ))?;
            }
            let state = self
                .get_run_state(run_id)?
                .and_then(|snapshot| snapshot.continuation)
                .unwrap_or_default();
            Ok(state
                .token_budget
                .is_some_and(|budget| state.tokens_used >= budget)
                || state
                    .time_budget_seconds
                    .is_some_and(|budget| state.time_used_seconds >= budget))
        })?;
        if exhausted {
            for token in tokens {
                token.cancel();
            }
            super::continuation::clear_launcher_at_cut(self, run_id, continuation_cut);
        }
        Ok(exhausted)
    }

    pub fn record_run_turn_compaction(
        &self,
        run_id: &str,
        turn_id: &str,
        provider_event_id: &str,
    ) -> Result<(), StoreError> {
        self.with_run_lock(run_id, || {
            let active_turn_id = self
                .get_run_state(run_id)?
                .and_then(|snapshot| snapshot.continuation)
                .and_then(|state| state.active_turn)
                .map(|turn| turn.turn_id);
            if active_turn_id.as_deref() != Some(turn_id) {
                return Err(StoreError::InvalidPlan(format!(
                    "compaction event targets inactive RunTurn {turn_id} in {run_id}"
                )));
            }
            let event_id = format!("{run_id}:{turn_id}:compact:{provider_event_id}");
            let already_recorded = self
                .get_run_state(run_id)?
                .is_some_and(|state| state.event_index.accounted_compactions.contains(&event_id));
            if already_recorded {
                return Ok(());
            }
            self.commit_runtime_event(RuntimeJournalEvent::for_append(
                run_id,
                None,
                None,
                RuntimeEventKind::RunTurnCompacted,
                serde_json::json!({
                    "event_id": event_id,
                    "turn_id": turn_id,
                }),
            ))?;
            Ok(())
        })
    }

    /// Finish the active RunTurn exactly once and return the rebuilt state.
    pub fn finish_run_turn(
        &self,
        run_id: &str,
        completion: RunTurnCompletion<'_>,
    ) -> Result<RunContinuationState, StoreError> {
        self.finish_run_turn_with_agent_failure(run_id, completion, None)
    }

    pub(crate) fn finish_run_turn_with_agent_failure(
        &self,
        run_id: &str,
        completion: RunTurnCompletion<'_>,
        agent_failure: Option<&echo_agent::error::AgentFailure>,
    ) -> Result<RunContinuationState, StoreError> {
        self.with_run_lock(run_id, || {
            self.get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            // Audit allowlist: the completion progress fingerprint summarizes
            // task/result history beyond the operational RunStateSnapshot.
            let events = self.list_events(run_id, 0)?;
            let already_recorded = self.get_run_state(run_id)?.is_some_and(|state| {
                state
                    .event_index
                    .finished_turns
                    .contains(completion.turn_id)
            });
            if already_recorded {
                return self
                    .get_run_state(run_id)?
                    .and_then(|snapshot| snapshot.continuation)
                    .ok_or_else(|| {
                        StoreError::InvalidPlan(format!(
                            "continuation projection missing after finishing {}",
                            completion.turn_id
                        ))
                    });
            }
            let active_turn_id = self
                .get_run_state(run_id)?
                .and_then(|snapshot| snapshot.continuation)
                .and_then(|state| state.active_turn)
                .map(|turn| turn.turn_id);
            if active_turn_id.as_deref() != Some(completion.turn_id) {
                return Err(StoreError::InvalidPlan(format!(
                    "finish targets inactive RunTurn {} in {run_id}",
                    completion.turn_id
                )));
            }
            {
                let progress_fingerprint = run_progress_fingerprint(&events);
                let made_progress = run_turn_made_progress(&events, completion.turn_id);
                let blocker_fingerprint = (!made_progress).then(|| {
                    blocker_fingerprint(completion.error_fingerprint, &progress_fingerprint)
                });
                let mut terminal_events = vec![RuntimeJournalEvent::for_append(
                    run_id,
                    None,
                    None,
                    RuntimeEventKind::RunTurnFinished,
                    serde_json::json!({
                        "event_id": format!("{run_id}:{}:finished", completion.turn_id),
                        "turn_id": completion.turn_id,
                        "status": completion.status.as_str(),
                        "elapsed_seconds": completion.elapsed_seconds,
                        "final_message_id": completion.final_message_id,
                        "error_fingerprint": completion.error_fingerprint,
                        "progress_fingerprint": progress_fingerprint,
                        "made_progress": made_progress,
                        "blocker_fingerprint": blocker_fingerprint,
                        "agent_failure": agent_failure,
                    }),
                )];
                let run = self
                    .get_run(run_id)?
                    .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
                if completion.status == RunTurnStatus::Ended && run.status == TaskRunStatus::Running
                {
                    let report = self.completion_gate_report(run_id)?;
                    if report.ready {
                        terminal_events.push(RuntimeJournalEvent::for_append(
                            run_id,
                            None,
                            None,
                            RuntimeEventKind::RunStatusChanged,
                            serde_json::json!({
                                "from": TaskRunStatus::Running.as_str(),
                                "to": TaskRunStatus::Completed.as_str(),
                                "plan_revision": report.plan_revision,
                                "goal_revision": report.goal_revision,
                                "requirement_count": report.requirements.len(),
                                "completed_with_run_turn": completion.turn_id,
                            }),
                        ));
                    }
                }
                self.commit_runtime_events(run_id, terminal_events)?;
            }
            self.get_run_state(run_id)?
                .and_then(|snapshot| snapshot.continuation)
                .ok_or_else(|| {
                    StoreError::InvalidPlan(format!(
                        "continuation projection missing after finishing {}",
                        completion.turn_id
                    ))
                })
        })
    }

    pub fn set_continuation_deferred(
        &self,
        run_id: &str,
        deferred: bool,
    ) -> Result<(), StoreError> {
        self.with_run_lock(run_id, || {
            self.get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            let current = self
                .get_run_state(run_id)?
                .and_then(|snapshot| snapshot.continuation)
                .is_some_and(|state| state.deferred);
            if current == deferred {
                return Ok(());
            }
            self.commit_runtime_event(RuntimeJournalEvent::for_append(
                run_id,
                None,
                None,
                if deferred {
                    RuntimeEventKind::RunContinuationDeferred
                } else {
                    RuntimeEventKind::RunContinuationResumed
                },
                serde_json::json!({ "deferred": deferred }),
            ))?;
            Ok(())
        })
    }

    /// Atomically observe active cells and defer continuation under the same
    /// run lock used by terminal cell persistence.
    pub fn defer_continuation_for_active_cells(&self, run_id: &str) -> Result<usize, StoreError> {
        self.with_run_lock(run_id, || {
            self.get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            let active_cells = self
                .list_background_cells(run_id)?
                .into_iter()
                .filter(BackgroundCellState::is_active)
                .count();
            if active_cells == 0 {
                return Ok(0);
            }
            let deferred = self
                .get_run_state(run_id)?
                .and_then(|snapshot| snapshot.continuation)
                .is_some_and(|state| state.deferred);
            if !deferred {
                self.commit_runtime_event(RuntimeJournalEvent::for_append(
                    run_id,
                    None,
                    None,
                    RuntimeEventKind::RunContinuationDeferred,
                    serde_json::json!({
                        "deferred": true,
                        "reason": "background_cells_active",
                    }),
                ))?;
            }
            Ok(active_cells)
        })
    }

    pub fn record_run_pause_reason(
        &self,
        run_id: &str,
        reason: RunPauseReason,
        detail: Option<&str>,
    ) -> Result<(), StoreError> {
        self.with_run_lock(run_id, || {
            self.get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            self.commit_runtime_event(RuntimeJournalEvent::for_append(
                run_id,
                None,
                None,
                RuntimeEventKind::RunPauseReasonChanged,
                serde_json::json!({
                    "reason": reason.as_str(),
                    "detail": detail.map(|text| text.chars().take(600).collect::<String>()),
                }),
            ))?;
            Ok(())
        })
    }

    /// Fold the append-only cell lifecycle events for one run.
    pub fn list_background_cells(
        &self,
        run_id: &str,
    ) -> Result<Vec<BackgroundCellState>, StoreError> {
        Ok(self
            .get_run_state(run_id)?
            .map(|state| state.background_cells)
            .unwrap_or_default())
    }

    /// Persist one cell launch exactly once. The framework registry remains
    /// the execution authority; this event is the EKO recovery/UI projection.
    #[allow(clippy::too_many_arguments)]
    pub fn record_background_cell_started(
        &self,
        run_id: &str,
        cell_id: &str,
        name: &str,
        command_hash: &str,
        turn_id: Option<&str>,
        execution_id: Option<&str>,
        call_id: Option<&str>,
    ) -> Result<ProjectionCommitReceipt, StoreError> {
        self.with_run_lock(run_id, || {
            self.get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            #[cfg(test)]
            if self
                .fail_next_cell_started
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                return Err(StoreError::InvalidPlan(
                    "injected BackgroundCellStarted append failure".to_string(),
                ));
            }
            let retention = echo_agent::utils::retention::ContentRetentionPolicy {
                max_string_chars: 240,
                ..Default::default()
            };
            let payload = serde_json::json!({
                "cell_id": cell_id,
                "name": retention.sanitize_text(name),
                "command_hash": command_hash,
                "turn_id": turn_id,
                "execution_id": execution_id,
                "call_id": call_id,
                "phase": BackgroundCellPhase::Prepared,
                "artifact_status": BackgroundCellArtifactStatus::NotRequested,
            });
            let current = self
                .get_run_state(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            let existing = current
                .background_cells
                .into_iter()
                .find(|cell| cell.cell_id == cell_id);
            let current_seq = i64::try_from(current.journal_sequence).map_err(|_| {
                StoreError::InvalidPlan("TaskRuntime sequence exceeds EKO cursor".to_string())
            })?;
            let append_receipt;
            if let Some(existing) = existing {
                if existing.name != retention.sanitize_text(name)
                    || existing.command_hash != command_hash
                    || existing.turn_id.as_deref() != turn_id
                    || existing.execution_id.as_deref() != execution_id
                    || existing.call_id.as_deref() != call_id
                {
                    return Err(StoreError::InvalidPlan(format!(
                        "conflicting BackgroundCellStarted fact for cell {cell_id}"
                    )));
                }
                append_receipt = None;
            } else {
                append_receipt = Some(self.shadow.append_event_line_with_receipt(
                    run_id,
                    None,
                    call_id,
                    RuntimeEventKind::BackgroundCellStarted,
                    payload,
                )?);
            }
            let committed_seq = append_receipt
                .as_ref()
                .map_or(current_seq, |(event, _, _)| event.seq);
            #[cfg(test)]
            let inject_projection_degradation = self
                .fail_next_cell_started_projection
                .swap(false, std::sync::atomic::Ordering::SeqCst);
            #[cfg(not(test))]
            let inject_projection_degradation = false;
            let projection = if inject_projection_degradation {
                ProjectionCommitReceipt::CommittedProjectionDegraded {
                    seq: committed_seq,
                    detail: "injected BackgroundCellStarted projection failure".to_string(),
                }
            } else {
                self.refresh_committed_projection(run_id, committed_seq)
            };
            let Some((_, apply, history)) = append_receipt else {
                let (journal, history) = self.shadow.settle_event_state(run_id)?;
                return Ok(Self::classify_committed_projection(
                    committed_seq,
                    journal,
                    CheckpointApplyStatus::NotDue,
                    history,
                    projection,
                ));
            };
            Ok(Self::classify_committed_projection(
                committed_seq,
                apply.journal,
                apply.checkpoint,
                history,
                projection,
            ))
        })
    }

    /// Persist one terminal cell result exactly once. Durable excerpts are
    /// redacted and bounded before they enter events.jsonl.
    #[allow(clippy::too_many_arguments)]
    pub fn record_background_cell_finished(
        &self,
        run_id: &str,
        cell_id: &str,
        name: &str,
        phase: BackgroundCellPhase,
        terminal_cause: Option<BackgroundCellTerminalCause>,
        terminal_message: Option<&str>,
        exit_code: Option<i32>,
        artifact_status: BackgroundCellArtifactStatus,
        artifact_message: Option<&str>,
        total_output_bytes: u64,
        output_truncated: bool,
        output_excerpt: Option<&str>,
        artifact_path: Option<&str>,
        artifact_sha256: Option<&str>,
        call_id: Option<&str>,
    ) -> Result<(), StoreError> {
        self.with_run_lock(run_id, || {
            self.get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            #[cfg(test)]
            if self
                .fail_cell_terminal_remaining
                .fetch_update(
                    std::sync::atomic::Ordering::SeqCst,
                    std::sync::atomic::Ordering::SeqCst,
                    |remaining| remaining.checked_sub(1),
                )
                .is_ok()
            {
                return Err(StoreError::InvalidPlan(
                    "injected BackgroundCellFinished append failure".to_string(),
                ));
            }
            let retention = echo_agent::utils::retention::ContentRetentionPolicy {
                max_string_chars: 1_200,
                ..Default::default()
            };
            let payload = serde_json::json!({
                "cell_id": cell_id,
                "name": retention.sanitize_text(name),
                "phase": phase,
                "terminal_cause": terminal_cause,
                "terminal_message": terminal_message.map(|text| retention.sanitize_text(text)),
                "exit_code": exit_code,
                "artifact_status": artifact_status,
                "artifact_message": artifact_message.map(|text| retention.sanitize_text(text)),
                "total_output_bytes": total_output_bytes,
                "output_truncated": output_truncated,
                "output_excerpt": output_excerpt.map(|text| retention.sanitize_text(text)),
                "artifact_path": artifact_path,
                "artifact_sha256": artifact_sha256,
                "call_id": call_id,
            });
            let existing = self
                .list_background_cells(run_id)?
                .into_iter()
                .find(|cell| cell.cell_id == cell_id && !cell.is_active());
            if let Some(existing) = existing {
                if existing.phase != phase
                    || existing.terminal_cause != terminal_cause
                    || existing.terminal_message.as_deref()
                        != terminal_message
                            .map(|text| retention.sanitize_text(text))
                            .as_deref()
                    || existing.exit_code != exit_code
                    || existing.artifact_status != artifact_status
                    || existing.artifact_message.as_deref()
                        != artifact_message
                            .map(|text| retention.sanitize_text(text))
                            .as_deref()
                    || existing.total_output_bytes != total_output_bytes
                    || existing.output_truncated != output_truncated
                    || existing.output_excerpt.as_deref()
                        != output_excerpt
                            .map(|text| retention.sanitize_text(text))
                            .as_deref()
                    || existing.artifact_path.as_deref() != artifact_path
                    || existing.artifact_sha256.as_deref() != artifact_sha256
                    || existing.call_id.as_deref() != call_id
                {
                    return Err(StoreError::InvalidPlan(format!(
                        "conflicting BackgroundCellFinished fact for cell {cell_id}"
                    )));
                }
            } else {
                self.commit_runtime_event(RuntimeJournalEvent::for_append(
                    run_id,
                    None,
                    call_id,
                    RuntimeEventKind::BackgroundCellFinished,
                    payload,
                ))?;
            }
            Ok(())
        })
    }

    pub fn list_artifacts(&self, run_id: &str) -> Result<Vec<Artifact>, StoreError> {
        self.file_store()?
            .list_artifacts(run_id)
            .map_err(|e| StoreError::InvalidPlan(format!("file read: {e}")))
    }

    pub fn list_reviews(
        &self,
        run_id: &str,
        task_id: &str,
    ) -> Result<Vec<ReviewResult>, StoreError> {
        self.file_store()?
            .list_reviews(run_id, task_id)
            .map_err(|e| StoreError::InvalidPlan(format!("file read: {e}")))
    }

    pub fn get_summary(
        &self,
        run_id: &str,
        task_id: &str,
    ) -> Result<Option<TaskExecutionSummary>, StoreError> {
        self.file_store()?
            .get_summary(run_id, task_id)
            .map_err(|e| StoreError::InvalidPlan(format!("file read: {e}")))
    }

    /// Append a free-form `Note` event for diagnostics / trace breadcrumbs.
    pub fn note(
        &self,
        run_id: &str,
        task_id: Option<&str>,
        message: &str,
    ) -> Result<(), StoreError> {
        let _operation = self.shadow_operation()?;
        self.commit_runtime_event(RuntimeJournalEvent::for_append(
            run_id,
            task_id,
            None,
            RuntimeEventKind::Note,
            serde_json::json!({ "message": message }),
        ))
    }

    /// Persist trigger/scheduling metadata without expanding the TaskRun state
    /// model. Consumers may rebuild this projection from the append-only event.
    pub fn record_trigger_metadata(
        &self,
        run_id: &str,
        source: &str,
        kind: &str,
        prompt: &str,
        priority: u8,
        dependencies: &[String],
    ) -> Result<(), StoreError> {
        let _operation = self.shadow_operation()?;
        self.commit_runtime_event(RuntimeJournalEvent::for_append(
            run_id,
            None,
            None,
            RuntimeEventKind::Note,
            serde_json::json!({
                "kind": "trigger_metadata",
                "source": source,
                "task_kind": kind,
                "prompt": prompt,
                "priority": priority.min(10),
                "dependencies": dependencies,
            }),
        ))
    }

    pub fn record_execution_path(
        &self,
        run_id: &str,
        observed_path: &str,
    ) -> Result<(), StoreError> {
        let _operation = self.shadow_operation()?;
        self.commit_runtime_event(RuntimeJournalEvent::for_append(
            run_id,
            None,
            None,
            RuntimeEventKind::Note,
            serde_json::json!({
                "kind": "execution_path",
                "observed_path": observed_path,
            }),
        ))
    }

    /// Persist the boundary immediately before a task Subagent starts model/tool
    /// execution. A matching [`record_subagent_released`](Self::record_subagent_released)
    /// makes the Subagent result recoverable without dispatching it again.
    #[allow(clippy::too_many_arguments)]
    pub fn record_subagent_assigned(
        &self,
        run_id: &str,
        task_id: &str,
        execution_id: &str,
        agent_name: &str,
        task_subject: &str,
        plan_revision: u64,
        attempt: u32,
        replay_safe: bool,
        dispatch_hook: bool,
    ) -> Result<(), StoreError> {
        let _operation = self.shadow_operation()?;
        self.commit_runtime_event(RuntimeJournalEvent::for_append(
            run_id,
            Some(task_id),
            Some(execution_id),
            RuntimeEventKind::SubagentAssigned,
            serde_json::json!({
                "execution_id": execution_id,
                "agent_name": agent_name,
                "title": task_subject,
                "plan_revision": plan_revision,
                "attempt": attempt,
                "replay_safe": replay_safe,
                "dispatch_hook": dispatch_hook,
            }),
        ))
    }

    /// Persist a Subagent terminal fact with the structured result needed for resume.
    pub(crate) fn record_subagent_released(
        &self,
        record: SubagentReleaseRecord<'_>,
    ) -> Result<(), StoreError> {
        let _operation = self.shadow_operation()?;
        let SubagentReleaseRecord {
            run_id,
            task_id,
            execution_id,
            agent_name,
            task_subject,
            plan_revision,
            attempt,
            status,
            result,
            full_output,
            usage,
            dispatch_hook,
        } = record;
        self.with_run_lock(run_id, || {
            let summary = result.map(|value| bounded_event_text(&value.summary, 2_000));
            let mut events = vec![RuntimeJournalEvent::for_append(
                run_id,
                Some(task_id),
                Some(execution_id),
                RuntimeEventKind::SubagentReleased,
                serde_json::json!({
                    "execution_id": execution_id,
                    "agent_name": agent_name,
                    "title": task_subject,
                    "plan_revision": plan_revision,
                    "attempt": attempt,
                    "status": status,
                    "summary": summary,
                    "result": result,
                    "full_output": full_output,
                    "usage": usage,
                    "dispatch_hook": dispatch_hook,
                }),
            )];
            events.extend(
                super::subagent_control::control_settlements_for_subagent_release(
                    self,
                    run_id,
                    task_id,
                    execution_id,
                    plan_revision,
                    attempt,
                    status,
                )?,
            );
            self.commit_runtime_events(run_id, events).map(|_| ())
        })
    }

    /// Persist a tool dispatch before execution. Raw arguments are deliberately
    /// excluded from the durable event to avoid leaking secrets or inflating
    /// the run file; `call_id` is the idempotency/correlation key.
    pub fn record_tool_started(
        &self,
        run_id: &str,
        task_id: &str,
        execution_id: &str,
        call_id: &str,
        tool_name: &str,
        replay_safe: bool,
    ) -> Result<(), StoreError> {
        let _operation = self.shadow_operation()?;
        self.commit_runtime_event(RuntimeJournalEvent::for_append(
            run_id,
            Some(task_id),
            Some(call_id),
            RuntimeEventKind::ToolStarted,
            serde_json::json!({
                "execution_id": execution_id,
                "call_id": call_id,
                "tool_name": tool_name,
                "replay_safe": replay_safe,
            }),
        ))
    }

    /// Persist a tool terminal fact. The result preview is diagnostic only;
    /// canonical tool output remains in the agent checkpoint/transcript.
    #[allow(clippy::too_many_arguments)]
    pub fn record_tool_finished(
        &self,
        run_id: &str,
        task_id: &str,
        execution_id: &str,
        call_id: &str,
        tool_name: &str,
        success: bool,
        result: &str,
        failure: Option<&echo_agent::tools::ToolFailure>,
    ) -> Result<(), StoreError> {
        let _operation = self.shadow_operation()?;
        let event_type = if success {
            RuntimeEventKind::ToolCompleted
        } else {
            RuntimeEventKind::ToolFailed
        };
        self.commit_runtime_event(RuntimeJournalEvent::for_append(
            run_id,
            Some(task_id),
            Some(call_id),
            event_type,
            serde_json::json!({
                "execution_id": execution_id,
                "call_id": call_id,
                "tool_name": tool_name,
                "success": success,
                "result_preview": bounded_event_text(result, 500),
                "result_chars": result.chars().count(),
                "failure": failure,
            }),
        ))
    }

    /// Return a completed Subagent result for a stable logical attempt.
    ///
    /// A physical claim gets a fresh execution id when an interrupted task is
    /// reclaimed. Revision and attempt remain stable across that reclaim, so
    /// they form the durable idempotency key. A later assignment for the same
    /// logical attempt clears the terminal fact, while a retry or edited task
    /// has a different attempt or revision and cannot reuse stale output.
    pub(crate) fn recoverable_subagent_result_for_attempt(
        &self,
        run_id: &str,
        task_id: &str,
        execution_id: &str,
        plan_revision: u64,
        attempt: u32,
    ) -> Result<Option<RecoverableSubagentResult>, StoreError> {
        let events = self.list_events(run_id, 0)?;
        let current_claim_index = events.iter().position(|event| {
            event.task_id.as_deref() == Some(task_id)
                && event.event_type == RuntimeEventKind::TaskStarted
                && json_string(&event.payload, "execution_id").as_deref() == Some(execution_id)
        });
        let mut current_result = None;
        let mut prior_result = None;
        // Audit allowlist: exact physical-attempt recovery needs the complete
        // assignment/release sequence to enforce its TaskStarted cut.
        // The same physical execution may recover its own completed release.
        // A reclaimed execution may reuse only a release committed before its
        // TaskStarted claim event; a late old release cannot cross that cut.
        for (index, event) in events.into_iter().enumerate() {
            let matches_attempt = event.task_id.as_deref() == Some(task_id)
                && event
                    .payload
                    .get("plan_revision")
                    .and_then(serde_json::Value::as_u64)
                    == Some(plan_revision)
                && event
                    .payload
                    .get("attempt")
                    .and_then(serde_json::Value::as_u64)
                    == Some(u64::from(attempt));
            if !matches_attempt {
                continue;
            }
            let event_execution_id = json_string(&event.payload, "execution_id");
            let targets_current = event_execution_id.as_deref() == Some(execution_id);
            let targets_prior = current_claim_index.is_some_and(|claim_index| index < claim_index);
            match event.event_type {
                RuntimeEventKind::SubagentAssigned if targets_current => current_result = None,
                RuntimeEventKind::SubagentAssigned if targets_prior => prior_result = None,
                RuntimeEventKind::SubagentReleased => {
                    let candidate =
                        if json_string(&event.payload, "status").as_deref() == Some("completed") {
                            event
                                .payload
                                .get("result")
                                .cloned()
                                .and_then(|value| {
                                    serde_json::from_value::<SubagentTaskResult>(value).ok()
                                })
                                .map(|result| RecoverableSubagentResult {
                                    full_output: json_string(&event.payload, "full_output")
                                        .filter(|output| !output.trim().is_empty())
                                        .unwrap_or_else(|| result.summary.clone()),
                                    result,
                                })
                        } else {
                            None
                        };
                    if targets_current {
                        current_result = candidate;
                    } else if targets_prior {
                        prior_result = candidate;
                    }
                }
                _ => {}
            }
        }
        Ok(current_result.or(prior_result))
    }

    /// Current unresolved recovery barriers, folded from append-only events.
    pub fn list_recovery_blockers(&self, run_id: &str) -> Result<Vec<RecoveryBlocker>, StoreError> {
        let _operation = self.shadow_operation()?;
        let mut blockers = self
            .get_run_state(run_id)?
            .map(|state| {
                state
                    .event_index
                    .recovery_blockers
                    .into_iter()
                    .map(|blocker| (blocker.task_id.clone(), blocker))
                    .collect::<std::collections::BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        // If the dedicated RecoveryBlocked append was interrupted after the
        // canonical TaskStatus landed, synthesize the barrier so resume fails closed.
        let tasks = self
            .get_plan(run_id)?
            .map(|plan| plan.tasks)
            .unwrap_or_default();
        for task in tasks.into_iter().filter(|task| {
            matches!(
                &task.status,
                echo_agent::tasks::TaskStatus::Blocked(detail)
                    if detail == "mutating side effect is indeterminate after restart"
            )
        }) {
            blockers
                .entry(task.id.clone())
                .or_insert_with(|| RecoveryBlocker {
                    run_id: run_id.to_string(),
                    task_id: task.id,
                    execution_id: None,
                    call_id: None,
                    tool_name: None,
                    reason: "mutating side effect is indeterminate after restart".to_string(),
                });
        }
        Ok(blockers.into_values().collect())
    }

    /// Resolve one recovery barrier after the user inspects the workspace.
    pub fn resolve_recovery_task(
        &self,
        run_id: &str,
        task_id: &str,
        decision: RecoveryDecision,
    ) -> Result<(), StoreError> {
        let _operation = self.shadow_operation()?;
        self.with_run_lock(run_id, || {
            let blocker = self
                .list_recovery_blockers(run_id)?
                .into_iter()
                .find(|blocker| blocker.task_id == task_id)
                .ok_or_else(|| {
                    StoreError::InvalidPlan(format!(
                        "task {task_id} has no unresolved recovery barrier"
                    ))
                })?;
            let run = self
                .get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            let current = self
                .load_revisioned_task_graph(run_id)?
                .ok_or_else(|| StoreError::PlanNotFound(run_id.to_string()))?;
            let expected_task = current
                .snapshot
                .tasks
                .iter()
                .find(|task| task.spec.id == task_id)
                .cloned()
                .ok_or_else(|| StoreError::TaskNotFound(task_id.to_string()))?;
            let recovery_event = RuntimeJournalEvent::for_append(
                run_id,
                Some(task_id),
                blocker.execution_id.as_deref(),
                RuntimeEventKind::RecoveryResolved,
                serde_json::json!({
                    "decision": decision.as_str(),
                    "previous_reason": blocker.reason,
                }),
            );
            let events = match decision {
                RecoveryDecision::Retry => {
                    let before = current.snapshot;
                    let mut after = before.clone();
                    match echo_agent::tasks::retry_runtime_task(
                        &mut after,
                        &expected_task,
                        before.revision,
                    )? {
                        echo_agent::tasks::RuntimeTaskRetryOutcome::Retried { .. } => {}
                        echo_agent::tasks::RuntimeTaskRetryOutcome::Exhausted {
                            retry_count,
                            max_retries,
                        } => {
                            return Err(StoreError::InvalidPlan(format!(
                                "task {task_id} recovery retry budget exhausted ({retry_count}/{max_retries})"
                            )));
                        }
                        echo_agent::tasks::RuntimeTaskRetryOutcome::Superseded => {
                            return Err(StoreError::InvalidPlan(format!(
                                "task {task_id} changed before recovery retry"
                            )));
                        }
                    }
                    let mut events = vec![recovery_event];
                    events.extend(runtime_execution_change_events(
                        run_id,
                        &before,
                        &after,
                        Some("recovery retry confirmed by user"),
                    )?);
                    events
                }
                RecoveryDecision::Skip => {
                    let application = echo_agent::tasks::TaskPatchEngine::apply_operations(
                        &current.snapshot.tasks,
                        vec![echo_agent::tasks::TaskPlanPatchOp::Skip {
                            task_id: task_id.to_string(),
                        }],
                        false,
                    )
                    .map_err(|error| StoreError::InvalidPlan(error.to_string()))?;
                    echo_agent::tasks::PlanValidator::default()
                        .validate_task_snapshot(&application.tasks)
                        .map_err(|errors| StoreError::InvalidPlan(errors.join("; ")))?;
                    let next_revision = current
                        .snapshot
                        .revision
                        .checked_add(1)
                        .ok_or_else(|| {
                            StoreError::InvalidPlan("plan revision overflow".to_string())
                        })?;
                    let prepared = prepare_revisioned_graph_commit(
                        run_id,
                        &run,
                        Some(&current),
                        echo_agent::tasks::TaskGraphCommit {
                            expected_revision: Some(current.snapshot.revision),
                            next: echo_agent::tasks::RevisionedTaskGraph {
                                snapshot: echo_agent::tasks::RuntimePlanSnapshot {
                                    revision: next_revision,
                                    tasks: application.tasks,
                                },
                                context: current.context.clone(),
                            },
                            reason: format!(
                                "resolve recovery barrier for task {task_id} by skipping it"
                            ),
                            effects: application.effects,
                        },
                    )?;
                    let mut events = vec![
                        RuntimeJournalEvent::for_append(
                            run_id,
                            None,
                            None,
                            RuntimeEventKind::PlanRevisionCommitted,
                            prepared.payload,
                        ),
                        recovery_event,
                    ];
                    events.extend(runtime_execution_change_events(
                        run_id,
                        &current.snapshot,
                        &prepared.next.snapshot,
                        Some("recovery skip confirmed by user"),
                    )?);
                    events
                }
            };
            self.commit_runtime_events(run_id, events)?;
            Ok(())
        })
    }
}

fn task_status_runtime_event(event: TaskStatusEvent<'_>) -> RuntimeJournalEvent {
    let TaskStatusEvent {
        run_id,
        task_id,
        task_subject,
        status,
        owner_agent,
        summary,
        claim,
    } = event;
    let now = echo_agent::utils::time::now_local().to_rfc3339();
    let started = status.is_running();
    let finished = status.is_terminal();
    let kind = runtime_task_event_kind(&status);
    let (status_name, status_detail) = task_status_wire(&status);
    RuntimeJournalEvent::for_append(
        run_id,
        Some(task_id),
        None,
        kind,
        serde_json::json!({
            "status": status_name,
            "status_detail": status_detail,
            "owner_agent": owner_agent,
            "title": task_subject,
            "summary": summary,
            "claim": claim,
            "started_at": if started { Some(now.as_str()) } else { None },
            "completed_at": if finished { Some(now.as_str()) } else { None },
        }),
    )
}

fn apply_subagent_projection_event(
    runs: &mut std::collections::BTreeMap<String, SubagentRunSnapshot>,
    run_id: &str,
    limit: usize,
    exact_execution_id: Option<&str>,
    event: RuntimeTaskEvent,
) {
    if let Some(recovery) = boot_recovery_payload(&event)
        && let Some(subagents) = recovery
            .get("subagents")
            .and_then(serde_json::Value::as_array)
    {
        for recovered in subagents {
            let Some(execution_id) = json_string(recovered, "execution_id") else {
                continue;
            };
            let Some(snapshot) = runs.get_mut(&execution_id) else {
                continue;
            };
            snapshot.run.status = json_string(recovered, "status")
                .as_deref()
                .and_then(SubagentRunStatus::from_str)
                .unwrap_or(SubagentRunStatus::Failed);
        }
    }
    let Some(execution_id) = event.step_id.clone() else {
        return;
    };
    if exact_execution_id.is_some_and(|wanted| wanted != execution_id) {
        return;
    }
    if event.event_type == RuntimeEventKind::SubagentAssigned {
        let Some(task_id) = event.task_id.clone() else {
            return;
        };
        let Some(subagent_name) = json_string(&event.payload, "agent_name") else {
            return;
        };
        if exact_execution_id.is_none() && !runs.contains_key(&execution_id) && runs.len() >= limit
        {
            let Some(last_key) = runs.last_key_value().map(|(key, _)| key.clone()) else {
                return;
            };
            if execution_id >= last_key {
                return;
            }
            runs.remove(&last_key);
        }
        let attempt = event
            .payload
            .get("attempt")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(1);
        let plan_revision = event
            .payload
            .get("plan_revision")
            .and_then(serde_json::Value::as_u64);
        runs.insert(
            execution_id.clone(),
            SubagentRunSnapshot {
                run: SubagentRun::new(execution_id, run_id, task_id, subagent_name, attempt),
                plan_revision,
                latest_event: event,
            },
        );
        return;
    }
    let Some(snapshot) = runs.get_mut(&execution_id) else {
        return;
    };
    snapshot.latest_event = event.clone();
    match event.event_type {
        RuntimeEventKind::RunTurnUsageAccounted
            if json_string(&event.payload, "source_scope").as_deref() == Some("subagent") =>
        {
            let tokens = event
                .payload
                .get("input_tokens")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0)
                .saturating_add(
                    event
                        .payload
                        .get("output_tokens")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0),
                );
            snapshot.run.usage.tokens_used = Some(
                snapshot
                    .run
                    .usage
                    .tokens_used
                    .unwrap_or(0)
                    .saturating_add(tokens),
            );
            let duration_ms = event
                .payload
                .get("duration_ms")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            snapshot.run.usage.duration_ms = Some(
                snapshot
                    .run
                    .usage
                    .duration_ms
                    .unwrap_or(0)
                    .saturating_add(duration_ms),
            );
        }
        RuntimeEventKind::SubagentReleased => {
            if let Some(status) = json_string(&event.payload, "status")
                .as_deref()
                .and_then(SubagentRunStatus::from_str)
            {
                snapshot.run.status = status;
            }
            if let Some(result) = event
                .payload
                .get("result")
                .cloned()
                .and_then(|value| serde_json::from_value::<SubagentTaskResult>(value).ok())
            {
                snapshot.run.result = Some(result);
            }
            if let Some(usage) = event
                .payload
                .get("usage")
                .cloned()
                .and_then(|value| serde_json::from_value::<SubagentRunUsage>(value).ok())
            {
                snapshot.run.usage = usage;
            }
        }
        _ => {}
    }
}

fn boot_recovery_payload(event: &RuntimeTaskEvent) -> Option<&serde_json::Value> {
    event.payload.get("recovery").filter(|recovery| {
        recovery.get("kind").and_then(serde_json::Value::as_str) == Some("boot_recovery")
    })
}

fn json_string(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string())
}

fn bounded_event_text(value: &str, max_chars: usize) -> String {
    let mut text = value.chars().take(max_chars).collect::<String>();
    if value.chars().count() > max_chars {
        text.push_str("...");
    }
    text
}

fn validate_plan_goal_binding(run: &TaskRun, plan: &TaskPlan) -> Result<(), StoreError> {
    if plan.goal_revision == run.goal_revision && plan.goal_sha256 == run.goal_sha256 {
        return Ok(());
    }
    Err(StoreError::PlanGoalMismatch {
        run_id: run.run_id.clone(),
        plan_revision: plan.revision,
        plan_goal_revision: plan.goal_revision,
        run_goal_revision: run.goal_revision,
    })
}

// The compile-time test that proves the transaction invariant:
// a state change without an event would leave the DB inconsistent.
// We assert both rows land together.
#[cfg(test)]
#[allow(clippy::items_after_test_module)] // usage-record impls below are production code kept here for locality with their tests; reordering is pure churn
mod tests {
    use super::*;

    struct DropFlag(std::sync::Arc<std::sync::atomic::AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    impl RunDriverExecutionReceipt for DropFlag {
        fn release(self: Box<Self>) -> futures::future::BoxFuture<'static, ()> {
            Box::pin(async move {
                drop(self);
            })
        }
    }

    struct ReleaseOrder(
        std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>,
        &'static str,
    );

    impl RunDriverExecutionReceipt for ReleaseOrder {
        fn release(self: Box<Self>) -> futures::future::BoxFuture<'static, ()> {
            Box::pin(async move {
                if let Ok(mut order) = self.0.lock() {
                    order.push(self.1);
                }
            })
        }
    }

    fn fresh() -> Result<TaskRuntimeStore, StoreError> {
        TaskRuntimeStore::new_in_memory()
            .map_err(|error| StoreError::InvalidPlan(error.to_string()))
    }

    #[test]
    fn bounded_agent_queries_scan_growth_without_unbounded_results() -> Result<(), StoreError> {
        let store = fresh()?;
        store.create_run(
            "bounded-run",
            "global",
            "conversation",
            "message",
            DomainProfile::General,
            "goal",
            "task",
            AttendedMode::Attended,
        )?;
        let mut events = (0..600)
            .map(|index| {
                RuntimeJournalEvent::for_append(
                    "bounded-run",
                    None,
                    None,
                    RuntimeEventKind::Note,
                    serde_json::json!({ "message": format!("before-{index}") }),
                )
            })
            .collect::<Vec<_>>();
        events.push(RuntimeJournalEvent::for_append(
            "bounded-run",
            Some("task-z"),
            Some("execution-z"),
            RuntimeEventKind::SubagentAssigned,
            serde_json::json!({
                "execution_id": "execution-z",
                "agent_name": "reviewer",
                "plan_revision": 7,
                "attempt": 2,
            }),
        ));
        events.extend((0..400).map(|index| {
            RuntimeJournalEvent::for_append(
                "bounded-run",
                None,
                None,
                RuntimeEventKind::Note,
                serde_json::json!({ "message": format!("middle-{index}") }),
            )
        }));
        events.push(RuntimeJournalEvent::for_append(
            "bounded-run",
            Some("task-a"),
            Some("execution-a"),
            RuntimeEventKind::SubagentAssigned,
            serde_json::json!({
                "execution_id": "execution-a",
                "agent_name": "implementer",
                "plan_revision": 9,
                "attempt": 3,
            }),
        ));
        events.extend((0..300).map(|index| {
            RuntimeJournalEvent::for_append(
                "bounded-run",
                None,
                None,
                RuntimeEventKind::Note,
                serde_json::json!({ "message": format!("after-{index}") }),
            )
        }));
        let result = SubagentTaskResult::terminal(
            SubagentRunStatus::Completed,
            "完成 ✅ bounded result",
            Vec::new(),
        );
        let usage = SubagentRunUsage {
            duration_ms: Some(41),
            tokens_used: Some(73),
            iterations: Some(5),
        };
        events.push(RuntimeJournalEvent::for_append(
            "bounded-run",
            Some("task-a"),
            Some("execution-a"),
            RuntimeEventKind::SubagentReleased,
            serde_json::json!({
                "status": "completed",
                "result": result,
                "usage": usage,
            }),
        ));
        store.commit_runtime_events("bounded-run", events)?;

        let released = store.query_events_bounded(
            "bounded-run",
            RuntimeEventQuery::new(0, 1)
                .for_execution("execution-a")
                .with_event_types(vec![RuntimeEventKind::SubagentReleased]),
        )?;
        assert_eq!(released.len(), 1);
        assert_eq!(
            released.first().map(|event| event.event_type),
            Some(RuntimeEventKind::SubagentReleased)
        );

        let snapshots = store.list_subagent_run_snapshots("bounded-run", 1)?;
        let snapshot = snapshots
            .first()
            .ok_or_else(|| StoreError::InvalidPlan("bounded snapshot missing".to_string()))?;
        assert_eq!(snapshot.run.subagent_run_id, "execution-a");
        assert_eq!(snapshot.plan_revision, Some(9));
        assert_eq!(snapshot.run.attempt, 3);
        assert_eq!(snapshot.run.usage.tokens_used, Some(73));
        assert_eq!(snapshot.run.usage.iterations, Some(5));
        assert_eq!(
            snapshot
                .run
                .result
                .as_ref()
                .map(|value| value.summary.as_str()),
            Some("完成 ✅ bounded result")
        );
        assert_eq!(
            snapshot.latest_event.event_type,
            RuntimeEventKind::SubagentReleased
        );

        let exact = store
            .get_subagent_run_snapshot("bounded-run", "execution-z")?
            .ok_or_else(|| StoreError::InvalidPlan("exact snapshot missing".to_string()))?;
        assert_eq!(exact.run.task_id, "task-z");
        assert_eq!(exact.plan_revision, Some(7));
        let encoded = serde_json::to_value(&exact.run)?;
        let decoded = serde_json::from_value::<SubagentRun>(encoded.clone())?;
        assert_eq!(serde_json::to_value(decoded)?, encoded);
        Ok(())
    }

    #[test]
    fn public_run_state_and_cell_queries_do_not_request_full_journal_replay()
    -> Result<(), StoreError> {
        let store = fresh()?;
        store.create_run(
            "checkpoint-read-run",
            "global",
            "conversation",
            "message",
            DomainProfile::General,
            "read checkpointed state",
            "task",
            AttendedMode::Attended,
        )?;
        store.record_background_cell_started(
            "checkpoint-read-run",
            "cell-1",
            "probe",
            "sha256:probe",
            Some("turn-1"),
            None,
            None,
        )?;
        store
            .shadow
            .reset_full_replay_requests_for_test("checkpoint-read-run")?;

        assert!(store.get_run_state("checkpoint-read-run")?.is_some());
        assert_eq!(store.list_background_cells("checkpoint-read-run")?.len(), 1);
        assert_eq!(
            store
                .shadow
                .full_replay_requests_for_test("checkpoint-read-run")?,
            0
        );

        assert!(!store.list_events("checkpoint-read-run", 0)?.is_empty());
        assert_eq!(
            store
                .shadow
                .full_replay_requests_for_test("checkpoint-read-run")?,
            1,
            "the replay probe did not observe an explicit sequence-zero query"
        );
        Ok(())
    }

    #[test]
    fn create_run_rejects_same_id_with_different_identity() -> Result<(), String> {
        let store = TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?;
        store
            .create_run(
                "same-run",
                "global",
                "conversation-a",
                "root-a",
                DomainProfile::General,
                "goal-a",
                "task",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        let duplicate = store.create_run(
            "same-run",
            "global",
            "conversation-b",
            "root-b",
            DomainProfile::General,
            "goal-b",
            "task",
            AttendedMode::Attended,
        );
        assert!(duplicate.is_err_and(|error| error.to_string().contains("different immutable")));
        Ok(())
    }

    #[test]
    fn create_run_retry_remains_idempotent_after_goal_update() -> Result<(), String> {
        let store = TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?;
        store
            .create_run(
                "goal-run",
                "global",
                "conversation",
                "root",
                DomainProfile::General,
                "goal A",
                "task",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run("goal-run", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store
            .transition_run("goal-run", TaskRunStatus::Paused)
            .map_err(|error| error.to_string())?;
        store
            .update_run_goal(
                "goal-run",
                1,
                "goal B",
                "user refined goal",
                RunGoalActorSource::Cli,
            )
            .map_err(|error| error.to_string())?;

        let existing = store
            .create_run(
                "goal-run",
                "global",
                "conversation",
                "root",
                DomainProfile::General,
                "goal A",
                "task",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        assert_eq!(existing.goal, "goal B");
        assert_eq!(existing.goal_revision, 2);
        Ok(())
    }

    fn last_frame_event_types(
        store: &TaskRuntimeStore,
        run_id: &str,
    ) -> Result<Vec<String>, String> {
        let contents =
            std::fs::read_to_string(store.active_shadow_root().join(run_id).join("events.jsonl"))
                .map_err(|error| error.to_string())?;
        let line = contents
            .lines()
            .last()
            .ok_or_else(|| "TaskRuntime journal has no frame".to_string())?;
        let frame: serde_json::Value =
            serde_json::from_str(line).map_err(|error| error.to_string())?;
        let batch_id = frame
            .get("batch_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "TaskRuntime journal frame has no batch id".to_string())?;
        let first_sequence = frame
            .get("first_sequence")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| "TaskRuntime journal frame has no first sequence".to_string())?;
        let records = frame
            .get("records")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "TaskRuntime journal frame has no records".to_string())?;
        for (index, record) in records.iter().enumerate() {
            let expected = first_sequence
                .checked_add(u64::try_from(index).map_err(|error| error.to_string())?)
                .ok_or_else(|| "TaskRuntime test sequence overflow".to_string())?;
            if record.get("batch_id").and_then(serde_json::Value::as_str) != Some(batch_id)
                || record.get("sequence").and_then(serde_json::Value::as_u64) != Some(expected)
            {
                return Err("TaskRuntime batch record identity is not contiguous".to_string());
            }
        }
        records
            .iter()
            .map(|record| {
                record
                    .get("event")
                    .and_then(|event| event.get("event_type"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
                    .ok_or_else(|| "TaskRuntime record has no event type".to_string())
            })
            .collect()
    }

    fn seed_public_state_fixture(
        event_count: usize,
    ) -> Result<(tempfile::TempDir, TaskRuntimeStore, String), String> {
        if event_count == 0 {
            return Err("performance fixture requires at least one event".to_string());
        }
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("tasks");
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(&root)
            .map_err(|error| error.to_string())?;
        let run_id = format!("public-state-{event_count}");
        let mut pending = Vec::with_capacity(512);
        for index in 0..event_count {
            let (task_id, step_id, event_type, payload) = match index {
                0 => (
                    None,
                    None,
                    RuntimeEventKind::RunCreated,
                    serde_json::json!({
                        "goal": "public state performance",
                        "goal_revision": 1,
                        "goal_sha256": task_goal_sha256("public state performance"),
                        "domain_profile": "general",
                        "workspace_id": "test",
                        "conversation_id": "ordinary-conversation",
                        "root_message_id": "root-message",
                        "route": "task",
                        "attended_mode": "unattended",
                    }),
                ),
                1 => (
                    None,
                    None,
                    RuntimeEventKind::RunContinuationConfigured,
                    serde_json::json!({"enabled": true}),
                ),
                2 => (
                    None,
                    None,
                    RuntimeEventKind::RunTurnStarted,
                    serde_json::json!({
                        "turn_id": "fixture-turn",
                        "ordinal": 1,
                        "origin": "continuation",
                        "transcript_visibility": "internal",
                    }),
                ),
                3 => (
                    None,
                    None,
                    RuntimeEventKind::RunTurnUsageAccounted,
                    serde_json::json!({
                        "event_id": "fixture-usage",
                        "turn_id": "fixture-turn",
                        "input_tokens": 2,
                        "output_tokens": 3,
                        "elapsed_seconds": 1,
                    }),
                ),
                4 => (
                    None,
                    None,
                    RuntimeEventKind::RunTurnCompacted,
                    serde_json::json!({
                        "event_id": "fixture-compaction",
                        "turn_id": "fixture-turn",
                    }),
                ),
                5 => (
                    None,
                    None,
                    RuntimeEventKind::RunTurnFinished,
                    serde_json::json!({
                        "turn_id": "fixture-turn",
                        "status": "ended",
                        "elapsed_seconds": 1,
                        "made_progress": true,
                    }),
                ),
                6 => (
                    Some("fixture-task-a".to_string()),
                    Some("fixture-execution-a".to_string()),
                    RuntimeEventKind::SubagentAssigned,
                    serde_json::json!({"replay_safe": false}),
                ),
                7 => (
                    Some("fixture-task-a".to_string()),
                    Some("fixture-call-a".to_string()),
                    RuntimeEventKind::ToolStarted,
                    serde_json::json!({
                        "execution_id": "fixture-execution-a",
                        "call_id": "fixture-call-a",
                        "tool_name": "write_file",
                        "replay_safe": false,
                    }),
                ),
                8 => (
                    Some("fixture-task-a".to_string()),
                    None,
                    RuntimeEventKind::RecoveryBlocked,
                    serde_json::json!({
                        "execution_id": "fixture-execution-a",
                        "call_id": "fixture-call-a",
                        "tool_name": "write_file",
                        "reason": "fixture uncertain side effect",
                    }),
                ),
                9 => (
                    None,
                    Some("fixture-call-a".to_string()),
                    RuntimeEventKind::BackgroundCellStarted,
                    serde_json::json!({
                        "cell_id": "fixture-cell",
                        "name": "fixture command",
                        "command_hash": "fixture-hash",
                        "phase": "running",
                        "artifact_status": "not_requested",
                    }),
                ),
                10 => (
                    Some("fixture-task-a".to_string()),
                    Some("fixture-call-a".to_string()),
                    RuntimeEventKind::ToolCompleted,
                    serde_json::json!({"call_id": "fixture-call-a"}),
                ),
                11 => (
                    Some("fixture-task-a".to_string()),
                    Some("fixture-execution-a".to_string()),
                    RuntimeEventKind::SubagentReleased,
                    serde_json::json!({}),
                ),
                12 => (
                    Some("fixture-task-a".to_string()),
                    None,
                    RuntimeEventKind::RecoveryResolved,
                    serde_json::json!({}),
                ),
                13 => (
                    None,
                    Some("fixture-call-a".to_string()),
                    RuntimeEventKind::BackgroundCellFinished,
                    serde_json::json!({
                        "cell_id": "fixture-cell",
                        "phase": "succeeded",
                        "terminal_cause": "exited",
                        "exit_code": 0,
                        "artifact_status": "not_requested",
                    }),
                ),
                14 => (
                    Some("fixture-task-b".to_string()),
                    Some("fixture-execution-b".to_string()),
                    RuntimeEventKind::SubagentAssigned,
                    serde_json::json!({"replay_safe": false}),
                ),
                15 => (
                    Some("fixture-task-b".to_string()),
                    Some("fixture-call-b".to_string()),
                    RuntimeEventKind::ToolStarted,
                    serde_json::json!({
                        "execution_id": "fixture-execution-b",
                        "call_id": "fixture-call-b",
                        "tool_name": "shell",
                        "replay_safe": false,
                    }),
                ),
                16 => (
                    Some("fixture-task-b".to_string()),
                    None,
                    RuntimeEventKind::RecoveryBlocked,
                    serde_json::json!({
                        "execution_id": "fixture-execution-b",
                        "call_id": "fixture-call-b",
                        "tool_name": "shell",
                        "reason": "fixture active recovery blocker",
                    }),
                ),
                _ => (
                    None,
                    None,
                    RuntimeEventKind::Note,
                    serde_json::json!({
                        "kind": "performance_fixture",
                        "ordinal": index,
                        "detail": "fixed diagnostic payload for public checkpoint-backed state reads",
                    }),
                ),
            };
            pending.push(RuntimeJournalEvent::for_append(
                &run_id,
                task_id.as_deref(),
                step_id.as_deref(),
                event_type,
                payload,
            ));
            if pending.len() == 512 || index.saturating_add(1) == event_count {
                store
                    .shadow
                    .append_event_batch(&run_id, std::mem::take(&mut pending))
                    .map_err(|error| error.to_string())?;
            }
        }
        store
            .shadow
            .rewrite_plan(&run_id)
            .map_err(|error| error.to_string())?;
        Ok((temp, store, run_id))
    }

    fn median_duration(samples: &mut [std::time::Duration]) -> Option<std::time::Duration> {
        samples.sort_unstable();
        samples.get(samples.len() / 2).copied()
    }

    fn seed_public_query_fixture(
        event_count: usize,
    ) -> Result<(tempfile::TempDir, TaskRuntimeStore, String), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("tasks");
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(&root)
            .map_err(|error| error.to_string())?;
        let run_id = format!("public-query-{event_count}");
        store
            .create_run(
                &run_id,
                "test",
                "conversation",
                "root-message",
                DomainProfile::General,
                "bounded public query",
                "task",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        let plan = TaskPlan {
            plan_id: format!("plan-{event_count}"),
            run_id: run_id.clone(),
            revision: 1,
            domain_profile: DomainProfile::General,
            goal_revision: 1,
            goal_sha256: task_goal_sha256("bounded public query"),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Sequential,
            tasks: vec![PlanTask {
                id: "task-a".to_string(),
                title: "Project bounded state".to_string(),
                description: "Exercise the production Todo and completion queries".to_string(),
                kind: PlanTaskKind::Investigation,
                agent_role: "subagent".to_string(),
                domain_profile: DomainProfile::General,
                sort_order: 1,
                ..Default::default()
            }],
        };
        store
            .attach_plan_for_test(&plan)
            .map_err(|error| error.to_string())?;
        store
            .set_task_status(
                &run_id,
                "task-a",
                echo_agent::tasks::TaskStatus::Completed,
                Some("subagent"),
                Some("projected"),
            )
            .map_err(|error| error.to_string())?;
        store
            .put_summary(&TaskExecutionSummary {
                run_id: run_id.clone(),
                task_id: "task-a".to_string(),
                subagent_name: "subagent".to_string(),
                result: SubagentTaskResult::terminal(
                    SubagentRunStatus::Completed,
                    "bounded query complete",
                    Vec::new(),
                ),
                decisions: Vec::new(),
                next_implications: Vec::new(),
                suggested_tasks: Vec::new(),
                created_at: Utc::now(),
            })
            .map_err(|error| error.to_string())?;
        store
            .add_artifact(&Artifact {
                id: "artifact-a".to_string(),
                run_id: run_id.clone(),
                task_id: Some("task-a".to_string()),
                kind: ArtifactKind::Report,
                title: "Bounded report".to_string(),
                path: None,
                metadata: serde_json::json!({"source": "fixture"}),
            })
            .map_err(|error| error.to_string())?;
        store
            .add_review(&ReviewResult {
                id: "review-a".to_string(),
                run_id: run_id.clone(),
                task_id: "task-a".to_string(),
                reviewer_agent: "reviewer".to_string(),
                outcome: ReviewOutcome::Pass,
                issues: Vec::new(),
                failure_fingerprint: None,
                created_fix_task_id: None,
                created_at: Utc::now(),
            })
            .map_err(|error| error.to_string())?;
        let current = store
            .list_events(&run_id, 0)
            .map_err(|error| error.to_string())?
            .len();
        if current > event_count {
            return Err(format!(
                "query fixture needs {current} semantic events, target was {event_count}"
            ));
        }
        let mut pending = Vec::with_capacity(512);
        for ordinal in current..event_count {
            pending.push(RuntimeJournalEvent::for_append(
                &run_id,
                None,
                None,
                RuntimeEventKind::Note,
                serde_json::json!({"kind": "bounded_query_fixture", "ordinal": ordinal}),
            ));
            if pending.len() == 512 || ordinal.saturating_add(1) == event_count {
                store
                    .shadow
                    .append_event_batch(&run_id, std::mem::take(&mut pending))
                    .map_err(|error| error.to_string())?;
            }
        }
        store
            .shadow
            .rewrite_plan(&run_id)
            .map_err(|error| error.to_string())?;
        Ok((temp, store, run_id))
    }

    fn seed_history_plan(store: &TaskRuntimeStore, run_id: &str) -> Result<(), String> {
        store
            .create_run(
                run_id,
                "test",
                "conversation",
                "root",
                DomainProfile::General,
                "history projection",
                "task",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .attach_plan_for_test(&TaskPlan {
                plan_id: format!("plan-{run_id}"),
                run_id: run_id.to_string(),
                revision: 1,
                domain_profile: DomainProfile::General,
                goal_revision: 1,
                goal_sha256: task_goal_sha256("history projection"),
                assumptions: Vec::new(),
                risks: Vec::new(),
                execution_mode: ExecutionMode::Sequential,
                tasks: ["other-task", "target-task"]
                    .into_iter()
                    .enumerate()
                    .map(|(index, task_id)| PlanTask {
                        id: task_id.to_string(),
                        title: task_id.to_string(),
                        description: "history scale".to_string(),
                        kind: PlanTaskKind::Investigation,
                        agent_role: "subagent".to_string(),
                        domain_profile: DomainProfile::General,
                        sort_order: i64::try_from(index).unwrap_or_default(),
                        ..Default::default()
                    })
                    .collect(),
            })
            .map_err(|error| error.to_string())
    }

    fn history_artifact(run_id: &str, id: &str) -> Artifact {
        Artifact {
            id: id.to_string(),
            run_id: run_id.to_string(),
            task_id: Some("target-task".to_string()),
            kind: ArtifactKind::Report,
            title: id.to_string(),
            path: None,
            metadata: serde_json::json!({"fixture": true}),
        }
    }

    fn history_review(run_id: &str, task_id: &str, id: &str) -> ReviewResult {
        ReviewResult {
            id: id.to_string(),
            run_id: run_id.to_string(),
            task_id: task_id.to_string(),
            reviewer_agent: "reviewer".to_string(),
            outcome: ReviewOutcome::Pass,
            issues: Vec::new(),
            failure_fingerprint: None,
            created_fix_task_id: None,
            created_at: Utc::now(),
        }
    }

    fn artifact_history_event(run_id: &str, id: &str) -> RuntimeJournalEvent {
        let artifact = history_artifact(run_id, id);
        RuntimeJournalEvent::for_append(
            run_id,
            artifact.task_id.as_deref(),
            None,
            RuntimeEventKind::ArtifactProduced,
            serde_json::json!({
                "artifact_id": artifact.id,
                "kind": artifact.kind.as_str(),
                "title": artifact.title,
                "task_id": artifact.task_id,
                "path": artifact.path,
                "metadata": artifact.metadata,
            }),
        )
    }

    fn review_history_event(run_id: &str, task_id: &str, id: &str) -> RuntimeJournalEvent {
        review_runtime_event(&history_review(run_id, task_id, id), None)
    }

    fn line_count(path: &std::path::Path) -> Result<usize, String> {
        Ok(std::fs::read(path)
            .map_err(|error| error.to_string())?
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .count())
    }

    fn retain_first_jsonl_record(path: &std::path::Path) -> Result<(), String> {
        let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
        let first = bytes
            .split_inclusive(|byte| *byte == b'\n')
            .next()
            .ok_or_else(|| "history segment has no first record".to_string())?;
        std::fs::write(path, first).map_err(|error| error.to_string())
    }

    fn history_cursor_sequence(path: &std::path::Path) -> Result<u64, String> {
        serde_json::from_slice::<serde_json::Value>(
            &std::fs::read(path).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?
        .get("through_sequence")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| "history cursor has no through_sequence".to_string())
    }

    fn flush_history_batch(
        store: &TaskRuntimeStore,
        run_id: &str,
        pending: &mut Vec<RuntimeJournalEvent>,
        samples: &mut Vec<std::time::Duration>,
    ) -> Result<(), String> {
        if pending.is_empty() {
            return Ok(());
        }
        let started = std::time::Instant::now();
        store
            .commit_runtime_events(run_id, std::mem::take(pending))
            .map_err(|error| error.to_string())?;
        samples.push(started.elapsed());
        Ok(())
    }

    type HistoryScaleFixture = (
        tempfile::TempDir,
        TaskRuntimeStore,
        String,
        std::time::Duration,
        std::time::Duration,
    );

    fn seed_history_scale_fixture(review_count: usize) -> Result<HistoryScaleFixture, String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("tasks");
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(&root)
            .map_err(|error| error.to_string())?;
        let run_id = format!("history-scale-{review_count}");
        seed_history_plan(&store, &run_id)?;

        let split = review_count / 2;
        let mut pending = vec![
            artifact_history_event(&run_id, "only-artifact"),
            review_history_event(&run_id, "target-task", "only-target-review"),
        ];
        let mut first_half = Vec::new();
        let mut second_half = Vec::new();
        for ordinal in 0..review_count {
            pending.push(review_history_event(
                &run_id,
                "other-task",
                &format!("other-review-{ordinal}"),
            ));
            if pending.len() == 512 {
                let samples = if ordinal < split {
                    &mut first_half
                } else {
                    &mut second_half
                };
                flush_history_batch(&store, &run_id, &mut pending, samples)?;
            }
        }
        flush_history_batch(&store, &run_id, &mut pending, &mut second_half)?;
        let first_median = median_duration(&mut first_half)
            .ok_or_else(|| "history first-half append samples are empty".to_string())?;
        let second_median = median_duration(&mut second_half)
            .ok_or_else(|| "history second-half append samples are empty".to_string())?;
        Ok((temp, store, run_id, first_median, second_median))
    }

    type ArtifactScaleFixture = (
        tempfile::TempDir,
        TaskRuntimeStore,
        String,
        std::time::Duration,
        std::time::Duration,
        usize,
        u64,
    );

    fn seed_artifact_scale_fixture(artifact_count: usize) -> Result<ArtifactScaleFixture, String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("tasks");
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(&root)
            .map_err(|error| error.to_string())?;
        let run_id = format!("artifact-scale-{artifact_count}");
        seed_history_plan(&store, &run_id)?;
        let (scans_before, bytes_before) = store
            .shadow
            .history_stats_for_test(&run_id)
            .map_err(|error| error.to_string())?;
        let split = artifact_count / 2;
        let mut pending = Vec::with_capacity(512);
        let mut first_half = Vec::new();
        let mut second_half = Vec::new();
        for ordinal in 0..artifact_count {
            pending.push(artifact_history_event(
                &run_id,
                &format!("artifact-{ordinal}"),
            ));
            if pending.len() == 512 {
                let samples = if ordinal < split {
                    &mut first_half
                } else {
                    &mut second_half
                };
                flush_history_batch(&store, &run_id, &mut pending, samples)?;
            }
        }
        flush_history_batch(&store, &run_id, &mut pending, &mut second_half)?;
        let first_median = median_duration(&mut first_half)
            .ok_or_else(|| "artifact first-half append samples are empty".to_string())?;
        let second_median = median_duration(&mut second_half)
            .ok_or_else(|| "artifact second-half append samples are empty".to_string())?;
        let (scans_after, bytes_after) = store
            .shadow
            .history_stats_for_test(&run_id)
            .map_err(|error| error.to_string())?;
        let scan_delta = scans_after.saturating_sub(scans_before);
        let byte_delta = bytes_after.saturating_sub(bytes_before);
        Ok((
            temp,
            store,
            run_id,
            first_median,
            second_median,
            scan_delta,
            byte_delta,
        ))
    }

    fn time_history_target_queries(
        store: &TaskRuntimeStore,
        run_id: &str,
    ) -> Result<std::time::Duration, String> {
        let started = std::time::Instant::now();
        let artifacts = store
            .list_artifacts(run_id)
            .map_err(|error| error.to_string())?;
        let reviews = store
            .list_reviews(run_id, "target-task")
            .map_err(|error| error.to_string())?;
        if artifacts.len() != 1 || reviews.len() != 1 {
            return Err("targeted history projection returned incomplete facts".to_string());
        }
        Ok(started.elapsed())
    }

    fn time_public_queries(
        store: &TaskRuntimeStore,
        run_id: &str,
    ) -> Result<std::time::Duration, String> {
        let started = std::time::Instant::now();
        let todos = store
            .list_todos(run_id)
            .map_err(|error| error.to_string())?;
        let artifacts = store
            .list_artifacts(run_id)
            .map_err(|error| error.to_string())?;
        let completion = store
            .completion_gate_report(run_id)
            .map_err(|error| error.to_string())?;
        let reviews = store
            .list_reviews(run_id, "task-a")
            .map_err(|error| error.to_string())?;
        let summary = store
            .get_summary(run_id, "task-a")
            .map_err(|error| error.to_string())?;
        if todos.len() != 1
            || artifacts.len() != 1
            || reviews.len() != 1
            || summary.is_none()
            || !completion.ready
        {
            return Err("production TaskRuntime query projection is incomplete".to_string());
        }
        Ok(started.elapsed())
    }

    fn boot_recovery_event_count(store: &TaskRuntimeStore) -> Result<usize, StoreError> {
        Ok(store
            .list_events("r1", 0)?
            .iter()
            .filter(|event| boot_recovery_payload(event).is_some())
            .count())
    }

    fn create_paused_run(store: &TaskRuntimeStore, run_id: &str) -> Result<TaskRun, StoreError> {
        store.create_run(
            run_id,
            "ws",
            "conversation",
            "message",
            DomainProfile::General,
            "original goal",
            "",
            AttendedMode::Attended,
        )?;
        store.transition_run(run_id, TaskRunStatus::Running)?;
        store.transition_run(run_id, TaskRunStatus::Paused)
    }

    fn test_driver_admission(
        store: &std::sync::Arc<TaskRuntimeStore>,
        run_id: &str,
    ) -> Result<RunDriverAdmissionReservation, String> {
        store
            .reserve_run_driver_admission(
                run_id.to_string(),
                echo_agent::agent::CancellationToken::new(),
            )
            .map_err(|error| error.to_string())
    }

    fn prepare_retryable_run(
        store: &TaskRuntimeStore,
        run_id: &str,
        task_id: &str,
    ) -> Result<(), StoreError> {
        store.create_run(
            run_id,
            "workspace-a",
            "conversation",
            "message",
            DomainProfile::General,
            "retry through the TUI facade",
            "",
            AttendedMode::Attended,
        )?;
        store.attach_plan_for_test(&TaskPlan {
            plan_id: format!("{run_id}-plan"),
            run_id: run_id.to_string(),
            revision: 1,
            domain_profile: DomainProfile::General,
            goal_revision: 1,
            goal_sha256: task_goal_sha256("retry through the TUI facade"),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Sequential,
            tasks: vec![PlanTask {
                id: task_id.to_string(),
                title: "Retry task".to_string(),
                max_retries: 2,
                ..PlanTask::default()
            }],
        })?;
        store.transition_run(run_id, TaskRunStatus::Running)?;
        store.set_task_status(
            run_id,
            task_id,
            echo_agent::tasks::TaskStatus::Failed(String::new()),
            None,
            Some("acceptance failed"),
        )?;
        store.transition_run(run_id, TaskRunStatus::Failed)?;
        Ok(())
    }

    fn retry_state_snapshot(
        store: &TaskRuntimeStore,
        run_id: &str,
    ) -> Result<(serde_json::Value, serde_json::Value, serde_json::Value), String> {
        Ok((
            serde_json::to_value(
                store
                    .list_events(run_id, 0)
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?,
            serde_json::to_value(store.get_run(run_id).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?,
            serde_json::to_value(store.get_plan(run_id).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?,
        ))
    }

    #[test]
    fn tui_retry_registration_failure_leaves_events_run_and_plan_unchanged() -> Result<(), String> {
        let store = std::sync::Arc::new(
            TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?,
        );
        prepare_retryable_run(&store, "registration-failure", "retry-task")
            .map_err(|error| error.to_string())?;
        let before = retry_state_snapshot(&store, "registration-failure")?;
        store.fail_next_run_driver_registration_for_test();

        let error = store
            .spawn_supervised_task_retry(
                "registration-failure".to_string(),
                "retry-task".to_string(),
                echo_agent::agent::CancellationToken::new(),
                || Ok(()),
                |(), _receipt_owner| async { Ok(()) },
            )
            .err()
            .ok_or_else(|| "injected driver registration unexpectedly succeeded".to_string())?;
        assert!(
            error
                .to_string()
                .contains("injected TaskRun driver registration failure")
        );
        assert_eq!(
            before,
            retry_state_snapshot(&store, "registration-failure")?
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tui_retry_registration_pins_generation_before_recovery_classification()
    -> Result<(), String> {
        let store = std::sync::Arc::new(
            TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?,
        );
        prepare_retryable_run(&store, "generation-race", "retry-task")
            .map_err(|error| error.to_string())?;
        let before = retry_state_snapshot(&store, "generation-race")?;
        let (registered, release) = store.park_next_run_driver_registration_for_test()?;
        let retry_store = std::sync::Arc::clone(&store);
        let retry = tokio::spawn(async move {
            retry_store.spawn_supervised_task_retry(
                "generation-race".to_string(),
                "retry-task".to_string(),
                echo_agent::agent::CancellationToken::new(),
                || Ok(()),
                |(), _receipt_owner| async { Ok(()) },
            )
        });
        tokio::task::spawn_blocking(move || {
            registered
                .recv_timeout(std::time::Duration::from_secs(2))
                .map_err(|error| format!("retry registration was not parked: {error}"))
        })
        .await
        .map_err(|error| error.to_string())??;

        let transition_error = store
            .begin_workspace_transition()
            .await
            .err()
            .ok_or_else(|| "workspace transition overtook registered TUI retry".to_string())?;
        assert!(matches!(
            transition_error,
            StoreError::WorkspaceTransitionBusy { .. }
        ));
        assert_eq!(before, retry_state_snapshot(&store, "generation-race")?);

        release
            .send(())
            .map_err(|_| "retry registration release receiver closed".to_string())?;
        let (preparation, waiter) = retry
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert_eq!(
            preparation,
            TaskRetryPreparation::Acceptance { next_attempt: 1 }
        );
        let _driver_result = waiter.await.map_err(|error| error.to_string())?;
        store
            .shutdown_run_drivers()
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    #[tokio::test]
    async fn terminal_write_debt_retains_execution_receipt_until_retry() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("tasks");
        let blocked_root = temp.path().join("tasks-blocked");
        let store = std::sync::Arc::new(
            TaskRuntimeStore::new_in_memory_with_shadow_root(root.clone())
                .map_err(|error| error.to_string())?,
        );
        store
            .create_run(
                "receipt-debt",
                "workspace-a",
                "conversation",
                "message",
                DomainProfile::General,
                "retain execution receipt",
                "",
                AttendedMode::Unattended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run("receipt-debt", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        let generation_lease = store
            .lease_active_workspace_generation()
            .map_err(|error| error.to_string())?;
        let dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let dropped_for_driver = std::sync::Arc::clone(&dropped);
        let admission = test_driver_admission(&store, "receipt-debt")?;
        let waiter = store
            .spawn_run_driver(
                admission,
                generation_lease,
                move |mut receipt_owner| async move {
                    receipt_owner.retain(DropFlag(dropped_for_driver));
                    std::fs::rename(&root, &blocked_root)
                        .map_err(|error| format!("block task root: {error}"))?;
                    std::fs::write(&root, b"block directory recreation")
                        .map_err(|error| format!("replace task root: {error}"))?;
                    Err::<(), String>("injected driver failure".to_string())
                },
            )
            .map_err(|error| error.to_string())?;
        let driver_error = waiter
            .await
            .map_err(|error| error.to_string())?
            .err()
            .ok_or_else(|| "driver failure was not reported".to_string())?;
        assert!(driver_error.contains("terminal settlement failed"));
        assert!(!dropped.load(std::sync::atomic::Ordering::SeqCst));
        assert!(store.begin_workspace_transition().await.is_err());

        std::fs::remove_file(temp.path().join("tasks")).map_err(|error| error.to_string())?;
        std::fs::rename(temp.path().join("tasks-blocked"), temp.path().join("tasks"))
            .map_err(|error| error.to_string())?;
        store
            .retry_run_settlement_debts()
            .await
            .map_err(|error| error.to_string())?;
        assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
        let transition = store
            .begin_workspace_transition()
            .await
            .map_err(|error| error.to_string())?;
        drop(transition);
        store
            .shutdown_run_drivers()
            .await
            .map_err(|error| error.to_string())
    }

    #[tokio::test]
    async fn dropped_waiter_shutdown_settles_run_and_releases_execution_receipt()
    -> Result<(), String> {
        let store = std::sync::Arc::new(
            TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?,
        );
        store
            .create_run(
                "dropped-waiter",
                "workspace-a",
                "conversation",
                "message",
                DomainProfile::General,
                "settle dropped waiter",
                "",
                AttendedMode::Unattended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run("dropped-waiter", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        let generation_lease = store
            .lease_active_workspace_generation()
            .map_err(|error| error.to_string())?;
        let cancel = echo_agent::agent::CancellationToken::new();
        let driver_cancel = cancel.clone();
        let admission = store
            .reserve_run_driver_admission("dropped-waiter".to_string(), cancel)
            .map_err(|error| error.to_string())?;
        let dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let dropped_for_driver = std::sync::Arc::clone(&dropped);
        let waiter = store
            .spawn_run_driver(
                admission,
                generation_lease,
                move |mut receipt_owner| async move {
                    receipt_owner.retain(DropFlag(dropped_for_driver));
                    driver_cancel.cancelled().await;
                    Err::<(), String>("driver cancelled during shutdown".to_string())
                },
            )
            .map_err(|error| error.to_string())?;
        drop(waiter);

        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            store.shutdown_run_drivers(),
        )
        .await
        .map_err(|_| "TaskRun driver shutdown timed out".to_string())?
        .map_err(|error| error.to_string())?;
        let run = store
            .get_run("dropped-waiter")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "settled run disappeared".to_string())?;
        assert_eq!(run.status, TaskRunStatus::Cancelled);
        assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_reporter_failure_is_published_once_to_all_waiters() -> Result<(), String> {
        let store = std::sync::Arc::new(
            TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?,
        );
        store.abort_next_run_driver_shutdown_reporter_for_test();

        let first_store = std::sync::Arc::clone(&store);
        let second_store = std::sync::Arc::clone(&store);
        let (first, second) = tokio::time::timeout(std::time::Duration::from_secs(2), async move {
            tokio::join!(
                first_store.shutdown_run_drivers(),
                second_store.shutdown_run_drivers()
            )
        })
        .await
        .map_err(|_| "concurrent TaskRun shutdown waiters timed out".to_string())?;
        let first = first.err().ok_or_else(|| {
            "first shutdown waiter did not observe the reporter failure".to_string()
        })?;
        let second = second.err().ok_or_else(|| {
            "second shutdown waiter did not observe the reporter failure".to_string()
        })?;
        assert_eq!(first, second);
        assert!(
            first
                .driver_errors
                .iter()
                .any(|error| error.contains("shutdown reporter failed"))
        );

        let repeated = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            store.shutdown_run_drivers(),
        )
        .await
        .map_err(|_| "repeated TaskRun shutdown waiter timed out".to_string())?
        .err()
        .ok_or_else(|| "repeated shutdown waiter lost the reporter failure".to_string())?;
        assert_eq!(first, repeated);
        assert_eq!(store.active_run_driver_count()?, 0);
        assert_eq!(store.active_run_driver_receipt_count()?, 0);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_waits_for_parked_prepare_and_reports_its_permanent_debt() -> Result<(), String>
    {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("tasks");
        let blocked_root = temp.path().join("tasks-blocked");
        let store = std::sync::Arc::new(
            TaskRuntimeStore::new_in_memory_with_shadow_root(root.clone())
                .map_err(|error| error.to_string())?,
        );
        let canonical_root = std::fs::canonicalize(temp.path())
            .map_err(|error| error.to_string())?
            .join("tasks");
        store
            .create_run(
                "parked-prepare",
                "workspace-a",
                "conversation",
                "message",
                DomainProfile::General,
                "settle an accepted preparation",
                "",
                AttendedMode::Unattended,
            )
            .map_err(|error| error.to_string())?;
        store
            .attach_plan_for_test(&TaskPlan {
                plan_id: "parked-prepare-plan".to_string(),
                run_id: "parked-prepare".to_string(),
                revision: 1,
                domain_profile: DomainProfile::General,
                goal_revision: 1,
                goal_sha256: task_goal_sha256("settle an accepted preparation"),
                assumptions: Vec::new(),
                risks: Vec::new(),
                execution_mode: ExecutionMode::Sequential,
                tasks: vec![PlanTask {
                    id: "parked-prepare-task".to_string(),
                    title: "Settle preparation".to_string(),
                    ..Default::default()
                }],
            })
            .map_err(|error| error.to_string())?;
        store
            .transition_run("parked-prepare", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store
            .transition_run("parked-prepare", TaskRunStatus::Paused)
            .map_err(|error| error.to_string())?;

        let (prepare_started_tx, prepare_started_rx) = tokio::sync::oneshot::channel::<()>();
        let (continue_prepare_tx, continue_prepare_rx) = std::sync::mpsc::channel::<()>();
        let preparation_store = std::sync::Arc::clone(&store);
        let run_store = std::sync::Arc::clone(&store);
        let root_for_driver = root.clone();
        let blocked_root_for_driver = blocked_root.clone();
        let preparation = tokio::task::spawn_blocking(move || {
            preparation_store.spawn_supervised_run_driver(
                "parked-prepare".to_string(),
                echo_agent::agent::CancellationToken::new(),
                || Ok(()),
                move |()| {
                    prepare_started_tx.send(()).map_err(|_| {
                        StoreError::InvalidPlan(
                            "shutdown test stopped before prepare admission".to_string(),
                        )
                    })?;
                    continue_prepare_rx
                        .recv_timeout(std::time::Duration::from_secs(2))
                        .map_err(|error| {
                            StoreError::InvalidPlan(format!(
                                "parked prepare was not released: {error}"
                            ))
                        })?;
                    run_store.resume_task_run("parked-prepare")?;
                    Ok(((), move |_receipt_owner| async move {
                        std::fs::rename(&root_for_driver, &blocked_root_for_driver)
                            .map_err(|error| format!("block task root: {error}"))?;
                        std::fs::write(&root_for_driver, b"block directory recreation")
                            .map_err(|error| format!("replace task root: {error}"))?;
                        Err::<(), String>("injected prepared driver failure".to_string())
                    }))
                },
            )
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), prepare_started_rx)
            .await
            .map_err(|_| "parked prepare did not start".to_string())?
            .map_err(|_| "parked prepare start sender closed".to_string())?;

        let shutdown_store = std::sync::Arc::clone(&store);
        let shutdown = tokio::spawn(async move { shutdown_store.shutdown_run_drivers().await });
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            store.wait_run_driver_shutdown_started(),
        )
        .await
        .map_err(|_| "TaskRun shutdown did not close driver admission".to_string())?;
        if store
            .reserve_run_driver_admission(
                "late-after-shutdown".to_string(),
                echo_agent::agent::CancellationToken::new(),
            )
            .is_ok()
        {
            return Err("TaskRun shutdown accepted a late driver reservation".to_string());
        }
        if shutdown.is_finished() {
            return Err("shutdown overtook an accepted parked preparation".to_string());
        }

        continue_prepare_tx
            .send(())
            .map_err(|_| "parked prepare receiver closed".to_string())?;
        let (_, result_waiter) =
            tokio::time::timeout(std::time::Duration::from_secs(2), preparation)
                .await
                .map_err(|_| "parked prepare did not register its driver".to_string())?
                .map_err(|error| error.to_string())?
                .map_err(|error| error.to_string())?;
        drop(result_waiter);

        let shutdown_error = tokio::time::timeout(std::time::Duration::from_secs(2), shutdown)
            .await
            .map_err(|_| "TaskRun shutdown did not settle parked preparation".to_string())?
            .map_err(|error| error.to_string())?
            .err()
            .ok_or_else(|| "permanent prepared driver debt was not reported".to_string())?;
        assert_eq!(shutdown_error.abandoned_settlements.len(), 1);
        let abandoned = shutdown_error
            .abandoned_settlements
            .first()
            .ok_or_else(|| "prepared driver abandonment is missing".to_string())?;
        assert_eq!(abandoned.run_id, "parked-prepare");
        assert_eq!(abandoned.target, TaskRunStatus::Cancelled);
        assert_eq!(abandoned.root, canonical_root);
        assert!(abandoned.driver_token.is_some());
        assert!(!abandoned.error.is_empty());
        assert_eq!(store.active_run_driver_count()?, 0);
        assert_eq!(store.active_run_driver_receipt_count()?, 0);

        std::fs::remove_file(temp.path().join("tasks")).map_err(|error| error.to_string())?;
        std::fs::rename(blocked_root, temp.path().join("tasks"))
            .map_err(|error| error.to_string())?;
        let run = store
            .get_run("parked-prepare")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "parked prepared run disappeared".to_string())?;
        assert_eq!(run.status, TaskRunStatus::Running);
        Ok(())
    }

    #[tokio::test]
    async fn overlapping_same_run_drivers_release_only_their_exact_receipts() -> Result<(), String>
    {
        let store = std::sync::Arc::new(
            TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?,
        );
        store
            .create_run(
                "overlap",
                "workspace-a",
                "conversation",
                "message",
                DomainProfile::General,
                "exact driver receipt",
                "",
                AttendedMode::Unattended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run("overlap", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store
            .transition_run("overlap", TaskRunStatus::Paused)
            .map_err(|error| error.to_string())?;

        let first_dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let second_dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (first_tx, first_rx) = tokio::sync::oneshot::channel::<()>();
        let (second_tx, second_rx) = tokio::sync::oneshot::channel::<()>();
        let first_flag = std::sync::Arc::clone(&first_dropped);
        let first_admission = test_driver_admission(&store, "overlap")?;
        let first_waiter = store
            .spawn_run_driver(
                first_admission,
                store
                    .lease_active_workspace_generation()
                    .map_err(|error| error.to_string())?,
                move |mut receipt_owner| async move {
                    receipt_owner.retain(DropFlag(first_flag));
                    first_rx.await.map_err(|error| error.to_string())
                },
            )
            .map_err(|error| error.to_string())?;
        let second_flag = std::sync::Arc::clone(&second_dropped);
        let second_admission = test_driver_admission(&store, "overlap")?;
        let second_waiter = store
            .spawn_run_driver(
                second_admission,
                store
                    .lease_active_workspace_generation()
                    .map_err(|error| error.to_string())?,
                move |mut receipt_owner| async move {
                    receipt_owner.retain(DropFlag(second_flag));
                    second_rx.await.map_err(|error| error.to_string())
                },
            )
            .map_err(|error| error.to_string())?;

        first_tx
            .send(())
            .map_err(|_| "first driver receiver closed".to_string())?;
        first_waiter.await.map_err(|error| error.to_string())??;
        assert!(first_dropped.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!second_dropped.load(std::sync::atomic::Ordering::SeqCst));

        second_tx
            .send(())
            .map_err(|_| "second driver receiver closed".to_string())?;
        second_waiter.await.map_err(|error| error.to_string())??;
        assert!(second_dropped.load(std::sync::atomic::Ordering::SeqCst));
        store.wait_for_run_driver_idle("overlap").await;
        assert!(!store.is_run_active("overlap"));
        store
            .shutdown_run_drivers()
            .await
            .map_err(|error| error.to_string())
    }

    #[test]
    fn overlapping_run_cancellation_tokens_release_in_any_order_and_cancel_together()
    -> Result<(), String> {
        let store = std::sync::Arc::new(
            TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?,
        );
        store
            .create_run(
                "cancel-overlap",
                "workspace-a",
                "conversation",
                "message",
                DomainProfile::General,
                "cancel every active driver",
                "",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run("cancel-overlap", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        let first = echo_agent::agent::CancellationToken::new();
        let second = echo_agent::agent::CancellationToken::new();
        let first_guard = store
            .register_run_cancellation("cancel-overlap", first.clone())
            .map_err(|error| error.to_string())?;
        let second_guard = store
            .register_run_cancellation("cancel-overlap", second.clone())
            .map_err(|error| error.to_string())?;
        assert!(
            store
                .request_pause("cancel-overlap")
                .map_err(|error| error.to_string())?
        );
        assert!(first.is_cancelled());
        assert!(second.is_cancelled());

        drop(first_guard);
        assert!(store.is_run_active("cancel-overlap"));
        drop(second_guard);
        assert!(!store.is_run_active("cancel-overlap"));

        let shared = echo_agent::agent::CancellationToken::new();
        let third_guard = store
            .register_run_cancellation("cancel-overlap", shared.clone())
            .map_err(|error| error.to_string())?;
        let fourth_guard = store
            .register_run_cancellation("cancel-overlap", shared)
            .map_err(|error| error.to_string())?;
        drop(fourth_guard);
        assert!(store.is_run_active("cancel-overlap"));
        drop(third_guard);
        assert!(!store.is_run_active("cancel-overlap"));
        Ok(())
    }

    #[tokio::test]
    async fn opaque_driver_context_rejects_forged_wrong_stale_and_cross_run_receipts()
    -> Result<(), String> {
        let store = std::sync::Arc::new(
            TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?,
        );
        for run_id in ["context-overlap", "context-other"] {
            store
                .create_run(
                    run_id,
                    "workspace-a",
                    "conversation",
                    "message",
                    DomainProfile::General,
                    "opaque driver execution context",
                    "",
                    AttendedMode::Unattended,
                )
                .map_err(|error| error.to_string())?;
            store
                .transition_run(run_id, TaskRunStatus::Running)
                .map_err(|error| error.to_string())?;
        }

        let (first_context_tx, first_context_rx) = std::sync::mpsc::sync_channel(1);
        let (finish_first_tx, finish_first_rx) = tokio::sync::oneshot::channel::<()>();
        let first_store = std::sync::Arc::clone(&store);
        let first_waiter = store
            .spawn_run_driver(
                test_driver_admission(&store, "context-overlap")?,
                store
                    .lease_active_workspace_generation()
                    .map_err(|error| error.to_string())?,
                move |receipt_owner| {
                    let context_sent = first_context_tx
                        .send(receipt_owner.execution_context_id())
                        .map_err(|_| "first context receiver closed".to_string());
                    async move {
                        context_sent?;
                        finish_first_rx.await.map_err(|error| error.to_string())?;
                        first_store
                            .finalize_run("context-overlap", TaskRunStatus::Completed, None)
                            .map(|_| ())
                            .map_err(|error| error.to_string())
                    }
                },
            )
            .map_err(|error| error.to_string())?;
        let first_context = first_context_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .map_err(|error| format!("first driver context was not published: {error}"))?;

        let (second_context_tx, second_context_rx) = std::sync::mpsc::sync_channel(1);
        let (finish_second_tx, finish_second_rx) = tokio::sync::oneshot::channel::<()>();
        let second_waiter = store
            .spawn_run_driver(
                test_driver_admission(&store, "context-overlap")?,
                store
                    .lease_active_workspace_generation()
                    .map_err(|error| error.to_string())?,
                move |receipt_owner| {
                    let context_sent = second_context_tx
                        .send(receipt_owner.execution_context_id())
                        .map_err(|_| "second context receiver closed".to_string());
                    async move {
                        context_sent?;
                        finish_second_rx.await.map_err(|error| error.to_string())
                    }
                },
            )
            .map_err(|error| error.to_string())?;
        let second_context = second_context_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .map_err(|error| format!("second driver context was not published: {error}"))?;
        assert_ne!(first_context, second_context);

        let (other_context_tx, other_context_rx) = std::sync::mpsc::sync_channel(1);
        let (finish_other_tx, finish_other_rx) = tokio::sync::oneshot::channel::<()>();
        let other_store = std::sync::Arc::clone(&store);
        let other_waiter = store
            .spawn_run_driver(
                test_driver_admission(&store, "context-other")?,
                store
                    .lease_active_workspace_generation()
                    .map_err(|error| error.to_string())?,
                move |receipt_owner| {
                    let context_sent = other_context_tx
                        .send(receipt_owner.execution_context_id())
                        .map_err(|_| "other context receiver closed".to_string());
                    async move {
                        context_sent?;
                        finish_other_rx.await.map_err(|error| error.to_string())?;
                        other_store
                            .finalize_run("context-other", TaskRunStatus::Completed, None)
                            .map(|_| ())
                            .map_err(|error| error.to_string())
                    }
                },
            )
            .map_err(|error| error.to_string())?;
        let other_context = other_context_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .map_err(|error| format!("other driver context was not published: {error}"))?;

        let first_released = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        store
            .retain_run_driver_receipt_from_context(
                "context-overlap",
                &first_context,
                DropFlag(std::sync::Arc::clone(&first_released)),
            )
            .map_err(|_| "first exact context was rejected".to_string())?;
        let second_released = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        store
            .retain_run_driver_receipt_from_context(
                "context-overlap",
                &second_context,
                DropFlag(std::sync::Arc::clone(&second_released)),
            )
            .map_err(|_| "second exact context was rejected".to_string())?;

        for (label, run_id, context_id) in [
            (
                "forged sequential token",
                "context-overlap",
                "eko-task-driver:2".to_string(),
            ),
            (
                "wrong nonce",
                "context-overlap",
                format!("{first_context}-wrong"),
            ),
            (
                "cross-run context",
                "context-overlap",
                other_context.clone(),
            ),
        ] {
            let rejected_released = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let rejected = store
                .retain_run_driver_receipt_from_context(
                    run_id,
                    &context_id,
                    DropFlag(std::sync::Arc::clone(&rejected_released)),
                )
                .err()
                .ok_or_else(|| format!("{label} unexpectedly retained a receipt"))?;
            drop(rejected);
            assert!(rejected_released.load(std::sync::atomic::Ordering::SeqCst));
        }
        assert_eq!(store.active_run_driver_receipt_count()?, 2);

        finish_first_tx
            .send(())
            .map_err(|_| "first driver receiver closed".to_string())?;
        first_waiter.await.map_err(|error| error.to_string())??;
        assert!(first_released.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!second_released.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(store.active_run_driver_receipt_count()?, 1);

        let stale_released = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stale = store
            .retain_run_driver_receipt_from_context(
                "context-overlap",
                &first_context,
                DropFlag(std::sync::Arc::clone(&stale_released)),
            )
            .err()
            .ok_or_else(|| "stale driver context unexpectedly retained a receipt".to_string())?;
        drop(stale);
        assert!(stale_released.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!second_released.load(std::sync::atomic::Ordering::SeqCst));

        finish_second_tx
            .send(())
            .map_err(|_| "second driver receiver closed".to_string())?;
        second_waiter.await.map_err(|error| error.to_string())??;
        assert!(second_released.load(std::sync::atomic::Ordering::SeqCst));

        finish_other_tx
            .send(())
            .map_err(|_| "other driver receiver closed".to_string())?;
        other_waiter.await.map_err(|error| error.to_string())??;
        assert_eq!(store.active_run_driver_receipt_count()?, 0);
        store
            .shutdown_run_drivers()
            .await
            .map_err(|error| error.to_string())
    }

    #[tokio::test]
    async fn abandoned_same_run_driver_releases_only_its_exact_receipt() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("tasks");
        let blocked_root = temp.path().join("tasks-blocked");
        let store = std::sync::Arc::new(
            TaskRuntimeStore::new_in_memory_with_shadow_root(root.clone())
                .map_err(|error| error.to_string())?,
        );
        store
            .create_run(
                "overlap-abandon",
                "workspace-a",
                "conversation",
                "message",
                DomainProfile::General,
                "exact abandoned driver receipt",
                "",
                AttendedMode::Unattended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run("overlap-abandon", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;

        let first_dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let second_dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (first_started_tx, first_started_rx) = tokio::sync::oneshot::channel::<()>();
        let (finish_first_tx, finish_first_rx) = tokio::sync::oneshot::channel::<()>();
        let first_flag = std::sync::Arc::clone(&first_dropped);
        let first_admission = test_driver_admission(&store, "overlap-abandon")?;
        let first_waiter = store
            .spawn_run_driver(
                first_admission,
                store
                    .lease_active_workspace_generation()
                    .map_err(|error| error.to_string())?,
                move |mut receipt_owner| async move {
                    receipt_owner.retain(DropFlag(first_flag));
                    first_started_tx
                        .send(())
                        .map_err(|_| "first driver start receiver closed".to_string())?;
                    finish_first_rx.await.map_err(|error| error.to_string())?;
                    std::fs::rename(&root, &blocked_root)
                        .map_err(|error| format!("block task root: {error}"))?;
                    std::fs::write(&root, b"block directory recreation")
                        .map_err(|error| format!("replace task root: {error}"))?;
                    Err::<(), String>("injected first driver failure".to_string())
                },
            )
            .map_err(|error| error.to_string())?;
        first_started_rx.await.map_err(|error| error.to_string())?;

        let second_flag = std::sync::Arc::clone(&second_dropped);
        let store_for_second = std::sync::Arc::clone(&store);
        let second_admission = test_driver_admission(&store, "overlap-abandon")?;
        let second_waiter = store
            .spawn_run_driver(
                second_admission,
                store
                    .lease_active_workspace_generation()
                    .map_err(|error| error.to_string())?,
                move |mut receipt_owner| async move {
                    receipt_owner.retain(DropFlag(second_flag));
                    store_for_second
                        .finalize_run("overlap-abandon", TaskRunStatus::Completed, None)
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                },
            )
            .map_err(|error| error.to_string())?;
        second_waiter.await.map_err(|error| error.to_string())??;
        assert!(second_dropped.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!first_dropped.load(std::sync::atomic::Ordering::SeqCst));

        finish_first_tx
            .send(())
            .map_err(|_| "first driver receiver closed".to_string())?;
        let first_error = first_waiter
            .await
            .map_err(|error| error.to_string())?
            .err()
            .ok_or_else(|| "first driver failure was not reported".to_string())?;
        assert!(first_error.contains("terminal settlement failed"));
        assert_eq!(store.active_run_driver_receipt_count()?, 1);

        let shutdown_error = store
            .shutdown_run_drivers()
            .await
            .err()
            .ok_or_else(|| "abandoned first driver debt was not reported".to_string())?;
        assert_eq!(shutdown_error.abandoned_settlements.len(), 1);
        let diagnostic = shutdown_error
            .abandoned_settlements
            .first()
            .ok_or_else(|| "first driver abandonment diagnostic is missing".to_string())?;
        assert_eq!(diagnostic.run_id, "overlap-abandon");
        assert_eq!(diagnostic.driver_token, Some(1));
        assert!(first_dropped.load(std::sync::atomic::Ordering::SeqCst));
        assert!(second_dropped.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(store.active_run_driver_count()?, 0);
        assert_eq!(store.active_run_driver_receipt_count()?, 0);

        std::fs::remove_file(temp.path().join("tasks")).map_err(|error| error.to_string())?;
        std::fs::rename(temp.path().join("tasks-blocked"), temp.path().join("tasks"))
            .map_err(|error| error.to_string())?;
        let run = store
            .get_run("overlap-abandon")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "overlap run disappeared".to_string())?;
        assert_eq!(run.status, TaskRunStatus::Completed);
        Ok(())
    }

    #[tokio::test]
    async fn exact_driver_releases_pool_before_memory_generation() -> Result<(), String> {
        let store = std::sync::Arc::new(
            TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?,
        );
        store
            .create_run(
                "lifo-receipts",
                "workspace-a",
                "conversation",
                "message",
                DomainProfile::General,
                "release pool before memory",
                "",
                AttendedMode::Unattended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run("lifo-receipts", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store
            .transition_run("lifo-receipts", TaskRunStatus::Paused)
            .map_err(|error| error.to_string())?;

        let release_order = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let memory_order = std::sync::Arc::clone(&release_order);
        let pool_order = std::sync::Arc::clone(&release_order);
        let admission = test_driver_admission(&store, "lifo-receipts")?;
        let waiter = store
            .spawn_run_driver(
                admission,
                store
                    .lease_active_workspace_generation()
                    .map_err(|error| error.to_string())?,
                move |mut receipt_owner| async move {
                    receipt_owner.retain(ReleaseOrder(memory_order, "memory"));
                    receipt_owner.retain(ReleaseOrder(pool_order, "pool"));
                    Ok::<(), String>(())
                },
            )
            .map_err(|error| error.to_string())?;
        waiter.await.map_err(|error| error.to_string())??;
        let observed = release_order
            .lock()
            .map_err(|_| "release order lock is poisoned".to_string())?
            .clone();
        assert_eq!(observed, ["pool", "memory"]);
        store
            .shutdown_run_drivers()
            .await
            .map_err(|error| error.to_string())
    }

    #[test]
    fn create_run_emits_run_created_event() -> Result<(), StoreError> {
        let s = fresh()?;
        let run = s.create_run(
            "r1",
            "ws",
            "c1",
            "m1",
            DomainProfile::AiCoding,
            "review runtime",
            "",
            AttendedMode::Attended,
        )?;
        assert_eq!(run.status, TaskRunStatus::Pending);
        let evs = s.list_events("r1", 0)?;
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].event_type, RuntimeEventKind::RunCreated);
        Ok(())
    }

    #[test]
    fn artifact_round_trip_preserves_path_and_metadata() -> std::result::Result<(), String> {
        let store = TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?;
        store
            .create_run(
                "artifact-run",
                "ws",
                "conversation",
                "message",
                DomainProfile::General,
                "artifact round trip",
                "",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        let artifact = Artifact {
            id: "artifact-1".to_string(),
            run_id: "artifact-run".to_string(),
            task_id: None,
            kind: ArtifactKind::Trace,
            title: "Complete tool output".to_string(),
            path: Some("/tmp/tool-output.log".to_string()),
            metadata: serde_json::json!({
                "sha256": "abcdef",
                "retention": "conversation_or_30d",
            }),
        };
        store
            .add_artifact(&artifact)
            .map_err(|error| error.to_string())?;

        let artifacts = store
            .list_artifacts("artifact-run")
            .map_err(|error| error.to_string())?;
        let restored = artifacts
            .first()
            .ok_or_else(|| "artifact was not restored".to_string())?;
        assert_eq!(restored.path, artifact.path);
        assert_eq!(restored.metadata, artifact.metadata);
        Ok(())
    }

    #[test]
    fn transition_run_appends_status_event_atomically() -> Result<(), StoreError> {
        let s = fresh()?;
        s.create_run(
            "r1",
            "ws",
            "c1",
            "m1",
            DomainProfile::General,
            "g",
            "",
            AttendedMode::Attended,
        )?;
        let run = s.transition_run("r1", TaskRunStatus::Running)?;
        assert_eq!(run.status, TaskRunStatus::Running);
        let evs = s.list_events("r1", 0)?;
        // RunCreated + RunStatusChanged
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[1].event_type, RuntimeEventKind::RunStatusChanged);
        Ok(())
    }

    #[test]
    fn run_goal_update_is_revisioned_audited_and_deferred() -> Result<(), String> {
        let store = TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?;
        let created = store
            .create_run(
                "goal-run",
                "ws",
                "conversation",
                "message",
                DomainProfile::General,
                "original goal",
                "",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        assert_eq!(created.goal_revision, 1);
        assert_eq!(created.goal_sha256, task_goal_sha256("original goal"));
        store
            .attach_plan_for_test(&TaskPlan {
                plan_id: "goal-plan".to_string(),
                run_id: "goal-run".to_string(),
                revision: 1,
                domain_profile: DomainProfile::General,
                goal_revision: 1,
                goal_sha256: task_goal_sha256("original goal"),
                assumptions: Vec::new(),
                risks: Vec::new(),
                execution_mode: ExecutionMode::Sequential,
                tasks: vec![PlanTask {
                    id: "goal-task".to_string(),
                    title: "Satisfy the original goal".to_string(),
                    description: "Produce traceable evidence".to_string(),
                    ..PlanTask::default()
                }],
            })
            .map_err(|error| error.to_string())?;
        store
            .transition_run("goal-run", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store
            .transition_run("goal-run", TaskRunStatus::Paused)
            .map_err(|error| error.to_string())?;

        let updated = store
            .update_run_goal(
                "goal-run",
                1,
                "revised goal",
                "user narrowed the requested scope",
                RunGoalActorSource::Cli,
            )
            .map_err(|error| error.to_string())?;
        assert_eq!(updated.goal, "revised goal");
        assert_eq!(updated.goal_revision, 2);
        assert_eq!(updated.goal_sha256, task_goal_sha256("revised goal"));

        let event = store
            .list_events("goal-run", 0)
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|event| event.event_type == RuntimeEventKind::RunGoalUpdated)
            .ok_or_else(|| "RunGoalUpdated was not persisted".to_string())?;
        assert_eq!(event.payload["old_goal_revision"], 1);
        assert_eq!(event.payload["new_goal_revision"], 2);
        assert_eq!(
            event.payload["old_goal_sha256"],
            task_goal_sha256("original goal")
        );
        assert_eq!(
            event.payload["new_goal_sha256"],
            task_goal_sha256("revised goal")
        );
        assert_eq!(event.payload["actor_source"], "cli");
        assert!(
            event
                .payload
                .get("actor_user_id")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|value| !value.is_empty())
        );

        let continuation = store
            .get_run_state("goal-run")
            .map_err(|error| error.to_string())?
            .and_then(|state| state.continuation)
            .ok_or_else(|| "continuation projection was not created".to_string())?;
        assert!(continuation.deferred);
        assert_eq!(
            continuation.deferred_reason.as_deref(),
            Some("goal_revision_unbound")
        );
        assert_eq!(
            last_frame_event_types(&store, "goal-run")?,
            ["run_goal_updated", "requirement_evidence_invalidated"]
        );
        Ok(())
    }

    #[test]
    fn run_goal_update_rejects_stale_revision_without_an_event() -> Result<(), String> {
        let store = TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?;
        create_paused_run(&store, "goal-conflict").map_err(|error| error.to_string())?;
        let before = store
            .list_events("goal-conflict", 0)
            .map_err(|error| error.to_string())?
            .len();

        let error = store
            .update_run_goal(
                "goal-conflict",
                9,
                "revised goal",
                "explicit correction",
                RunGoalActorSource::Tui,
            )
            .err()
            .ok_or_else(|| "stale goal revision was accepted".to_string())?;
        assert!(matches!(error, StoreError::GoalConflict { .. }));
        assert_eq!(
            store
                .list_events("goal-conflict", 0)
                .map_err(|error| error.to_string())?
                .len(),
            before
        );
        Ok(())
    }

    #[test]
    fn run_goal_update_requires_a_quiescent_paused_run() -> Result<(), String> {
        let active_turn = TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?;
        active_turn
            .create_run(
                "active-turn",
                "ws",
                "conversation",
                "message",
                DomainProfile::General,
                "original goal",
                "",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        active_turn
            .configure_run_continuation("active-turn", true, false, None, None)
            .map_err(|error| error.to_string())?;
        active_turn
            .transition_run("active-turn", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        let claim = active_turn
            .claim_run_turn(
                "active-turn",
                "turn-1",
                RunTurnOrigin::Continuation,
                TurnVisibility::Internal,
            )
            .map_err(|error| error.to_string())?;
        assert!(matches!(claim, RunTurnClaimOutcome::Started(_)));
        active_turn
            .transition_run("active-turn", TaskRunStatus::Paused)
            .map_err(|error| error.to_string())?;

        let active_subagent =
            TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?;
        create_paused_run(&active_subagent, "active-subagent")
            .map_err(|error| error.to_string())?;
        active_subagent
            .record_subagent_assigned(
                "active-subagent",
                "task-1",
                "execution-1",
                "researcher",
                "research",
                1,
                1,
                false,
                false,
            )
            .map_err(|error| error.to_string())?;

        let active_cell = TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?;
        create_paused_run(&active_cell, "active-cell").map_err(|error| error.to_string())?;
        active_cell
            .record_background_cell_started(
                "active-cell",
                "cell-1",
                "test cell",
                "command-hash",
                None,
                None,
                None,
            )
            .map_err(|error| error.to_string())?;

        let active_driver = std::sync::Arc::new(
            TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?,
        );
        create_paused_run(active_driver.as_ref(), "active-driver")
            .map_err(|error| error.to_string())?;
        let _driver_registration = active_driver
            .register_run_cancellation("active-driver", echo_agent::agent::CancellationToken::new())
            .map_err(|error| error.to_string())?;

        for (store, run_id, expected_reason) in [
            (&active_turn, "active-turn", "active RunTurn"),
            (&active_subagent, "active-subagent", "active Subagent"),
            (&active_cell, "active-cell", "active command cell"),
            (active_driver.as_ref(), "active-driver", "active driver"),
        ] {
            let error = store
                .update_run_goal(
                    run_id,
                    1,
                    "revised goal",
                    "explicit correction",
                    RunGoalActorSource::Gui,
                )
                .err()
                .ok_or_else(|| format!("goal update was accepted for {run_id}"))?;
            assert!(matches!(
                error,
                StoreError::GoalUpdateRejected { reason, .. }
                    if reason.contains(expected_reason)
            ));
        }
        Ok(())
    }

    #[test]
    fn task_update_rebinds_plan_before_goal_updated_run_can_resume() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.transition_run("r1", TaskRunStatus::Paused)?;
        store.update_run_goal(
            "r1",
            1,
            "revised goal",
            "user changed the acceptance target",
            RunGoalActorSource::Tui,
        )?;

        let stale_plan = store
            .get_plan("r1")?
            .ok_or_else(|| StoreError::PlanNotFound("r1".to_string()))?;
        assert_eq!(stale_plan.goal_revision, 1);
        let resume_error = store
            .resume_task_run("r1")
            .err()
            .ok_or_else(|| StoreError::InvalidPlan("stale plan resumed".to_string()))?;
        assert!(
            matches!(
                &resume_error,
                StoreError::PlanGoalMismatch {
                    plan_goal_revision: 1,
                    run_goal_revision: 2,
                    ..
                }
            ),
            "unexpected resume error: {resume_error}"
        );

        let rebound = store.apply_task_patch_for_test(
            "r1",
            &TaskUpdateRequest {
                base_revision: 1,
                reason: "align the task graph with goal revision 2".to_string(),
                operations: vec![TaskUpdateOperation::Update {
                    task_id: "t1".to_string(),
                    patch: TaskPatch {
                        title: Some("Review revised runtime scope".to_string()),
                        ..Default::default()
                    },
                }],
            },
        )?;
        assert_eq!(rebound.revision, 2);
        assert_eq!(rebound.goal_revision, 2);
        assert_eq!(rebound.goal_sha256, task_goal_sha256("revised goal"));
        assert_eq!(store.resume_task_run("r1")?.status, TaskRunStatus::Running);

        let latest_plan_event = store
            .list_events("r1", 0)?
            .into_iter()
            .rev()
            .find(|event| event.event_type == RuntimeEventKind::PlanRevisionCommitted)
            .ok_or_else(|| StoreError::PlanNotFound("r1".to_string()))?;
        assert!(latest_plan_event.payload["plan"].get("goal").is_none());
        Ok(())
    }

    #[test]
    fn illegal_transition_is_rejected_and_leaves_no_event() -> Result<(), StoreError> {
        let s = fresh()?;
        s.create_run(
            "r1",
            "ws",
            "c1",
            "m1",
            DomainProfile::General,
            "g",
            "",
            AttendedMode::Attended,
        )?;
        // First transition to Running (was Pending → now legal).
        s.transition_run("r1", TaskRunStatus::Running)?;
        // Running → Completed is legal. Now test that Completed → Running is
        // illegal (terminal state → non-terminal is always rejected).
        let _before = s.list_events("r1", 0)?.len();
        s.transition_run("r1", TaskRunStatus::Completed)?;
        let before_terminal = s.list_events("r1", 0)?.len();
        let err = s
            .transition_run("r1", TaskRunStatus::Running)
            .err()
            .ok_or_else(|| {
                StoreError::InvalidPlan("illegal transition unexpectedly succeeded".to_string())
            })?;
        assert!(matches!(err, StoreError::IllegalTransition { .. }));
        // No new event was appended — the tx rolled back.
        assert_eq!(s.list_events("r1", 0)?.len(), before_terminal);
        Ok(())
    }

    #[test]
    fn attach_plan_creates_tasks_and_todos() -> Result<(), StoreError> {
        let s = fresh()?;
        s.create_run(
            "r1",
            "ws",
            "c1",
            "m1",
            DomainProfile::General,
            "g",
            "",
            AttendedMode::Attended,
        )?;
        // attach_plan no longer changes the run status; caller decides.
        let plan = TaskPlan {
            plan_id: "p1".into(),
            run_id: "r1".into(),
            revision: 1,
            domain_profile: DomainProfile::General,
            goal_revision: 1,
            goal_sha256: task_goal_sha256("g"),
            assumptions: vec!["a".into()],
            risks: vec![],
            execution_mode: ExecutionMode::Parallel,
            tasks: vec![PlanTask {
                id: "t1".into(),
                title: "Review runtime".into(),
                kind: PlanTaskKind::ReadOnlyReview,
                agent_role: "code_reviewer".into(),
                ..Default::default()
            }],
        };
        s.attach_plan_for_test(&plan)?;

        let loaded = s
            .get_plan("r1")?
            .ok_or_else(|| StoreError::PlanNotFound("r1".to_string()))?;
        assert_eq!(loaded.tasks.len(), 1);
        assert_eq!(loaded.tasks[0].id, "t1");

        let todos = s.list_todos("r1")?;
        assert_eq!(todos.len(), 1);
        assert_eq!(todos[0].task_id, "t1");
        assert_eq!(todos[0].status, TodoStatus::Pending);

        let run = s
            .get_run("r1")?
            .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?;
        // attach_plan no longer transitions status; run stays Pending.
        assert_eq!(run.status, TaskRunStatus::Pending);
        assert_eq!(run.plan_id.as_deref(), Some("p1"));
        Ok(())
    }

    #[test]
    fn set_task_status_updates_task_todo_and_event_together() -> Result<(), StoreError> {
        let s = fresh()?;
        seed_plan(&s)?;
        s.set_task_status(
            "r1",
            "t1",
            echo_agent::tasks::TaskStatus::Running,
            Some("code_reviewer"),
            None,
        )?;
        let todos = s.list_todos("r1")?;
        assert_eq!(todos[0].status, TodoStatus::Running);
        assert_eq!(todos[0].owner_agent.as_deref(), Some("code_reviewer"));
        assert!(todos[0].started_at.is_some());

        let evs = s.list_events("r1", 0)?;
        assert!(
            evs.iter()
                .any(|e| e.event_type == RuntimeEventKind::TaskStarted)
        );
        Ok(())
    }

    #[test]
    fn task_terminal_events_follow_typed_status_not_detail_text() -> Result<(), StoreError> {
        let failed = fresh()?;
        seed_plan(&failed)?;
        failed.set_task_status(
            "r1",
            "t1",
            echo_agent::tasks::TaskStatus::Failed(String::new()),
            Some("code_reviewer"),
            Some("the report mentions timeout and cancelled behavior"),
        )?;
        let failed_events = failed.list_events("r1", 0)?;
        assert!(
            failed_events
                .iter()
                .any(|event| event.event_type == RuntimeEventKind::TaskFailed)
        );
        assert!(failed_events.iter().all(|event| {
            !matches!(
                event.event_type,
                RuntimeEventKind::TaskTimedOut | RuntimeEventKind::TaskCancelled
            )
        }));

        let timed_out = fresh()?;
        seed_plan(&timed_out)?;
        timed_out.set_task_status(
            "r1",
            "t1",
            echo_agent::tasks::TaskStatus::TimedOut {
                error: String::new(),
            },
            Some("code_reviewer"),
            Some("provider deadline elapsed"),
        )?;
        assert!(
            timed_out
                .list_events("r1", 0)?
                .iter()
                .any(|event| event.event_type == RuntimeEventKind::TaskTimedOut)
        );

        let cancelled = fresh()?;
        seed_plan(&cancelled)?;
        cancelled.set_task_status(
            "r1",
            "t1",
            echo_agent::tasks::TaskStatus::Cancelled,
            Some("code_reviewer"),
            Some("stopped by parent run"),
        )?;
        assert!(
            cancelled
                .list_events("r1", 0)?
                .iter()
                .any(|event| event.event_type == RuntimeEventKind::TaskCancelled)
        );
        Ok(())
    }

    #[test]
    fn task_todo_characterization_tracks_dynamic_graph_run_state_todo_and_recovery()
    -> Result<(), StoreError> {
        let temp =
            tempfile::tempdir().map_err(|error| StoreError::InvalidPlan(error.to_string()))?;
        let root = temp.path().join("tasks");
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(&root)
            .map_err(|error| StoreError::InvalidPlan(error.to_string()))?;
        let run_id = "task-todo-characterization";
        store.create_run(
            run_id,
            "ws",
            "conversation",
            "message",
            DomainProfile::General,
            "characterize TaskRuntime projections",
            "task",
            AttendedMode::Attended,
        )?;

        let mut upstream = sample_task_body("upstream");
        upstream.max_retries = 2;
        let mut dependent = sample_task_body("dependent");
        dependent.depends_on = vec![upstream.id.clone()];
        let blocked = sample_task_body("blocked");
        let timed_out = sample_task_body("timed-out");
        let skipped = sample_task_body("skipped");
        store.attach_plan_for_test(&TaskPlan {
            plan_id: "task-todo-characterization-plan".to_string(),
            run_id: run_id.to_string(),
            revision: 1,
            domain_profile: DomainProfile::General,
            goal_revision: 1,
            goal_sha256: task_goal_sha256("characterize TaskRuntime projections"),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Parallel,
            tasks: vec![upstream, dependent, blocked, timed_out, skipped],
        })?;
        store.transition_run(run_id, TaskRunStatus::Running)?;

        let assert_graph_and_projections =
            |store: &TaskRuntimeStore, expected_revision: u64| -> Result<(), StoreError> {
                let plan = store
                    .get_plan(run_id)?
                    .ok_or_else(|| StoreError::PlanNotFound(run_id.to_string()))?;
                let snapshot = store
                    .get_run_state(run_id)?
                    .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
                let runtime = store.load_runtime_plan_snapshot(run_id)?;
                let todos = store.list_todos(run_id)?;
                assert_eq!(plan.revision, expected_revision);
                assert_eq!(runtime.revision, expected_revision);
                assert_eq!(plan.tasks.len(), snapshot.tasks.len());
                assert_eq!(plan.tasks.len(), runtime.tasks.len());
                assert_eq!(plan.tasks.len(), todos.len());
                for task in &plan.tasks {
                    if !snapshot.tasks.iter().any(|state| state.task_id == task.id) {
                        return Err(StoreError::TaskNotFound(format!(
                            "run-state task {} missing",
                            task.id
                        )));
                    }
                    if !runtime.tasks.iter().any(|state| state.spec.id == task.id) {
                        return Err(StoreError::TaskNotFound(format!(
                            "runtime graph task {} missing",
                            task.id
                        )));
                    }
                    if !todos.iter().any(|todo| todo.task_id == task.id) {
                        return Err(StoreError::TaskNotFound(format!(
                            "Todo projection {} missing",
                            task.id
                        )));
                    }
                }
                Ok(())
            };

        assert_graph_and_projections(&store, 1)?;

        let mut patched = sample_task_body("patched");
        patched.title = "Inserted during characterization".to_string();
        let patched_plan = store.apply_task_patch_for_test(
            run_id,
            &TaskUpdateRequest {
                base_revision: 1,
                reason: "exercise dynamic graph insertion".to_string(),
                operations: vec![TaskUpdateOperation::Insert {
                    after_task_id: Some("upstream".to_string()),
                    task: patched.spec(),
                }],
            },
        )?;
        assert_eq!(patched_plan.revision, 2);
        assert_graph_and_projections(&store, 2)?;
        assert_eq!(
            store
                .list_todos(run_id)?
                .iter()
                .find(|todo| todo.task_id == "patched")
                .map(|todo| todo.status),
            Some(TodoStatus::Pending)
        );

        store.set_task_status(
            run_id,
            "upstream",
            echo_agent::tasks::TaskStatus::Failed(String::new()),
            Some("implementer"),
            Some("upstream failed"),
        )?;
        store.set_task_status(
            run_id,
            "blocked",
            echo_agent::tasks::TaskStatus::Blocked(String::new()),
            Some("reviewer"),
            Some("awaiting explicit decision"),
        )?;
        store.set_task_status(
            run_id,
            "timed-out",
            echo_agent::tasks::TaskStatus::TimedOut {
                error: String::new(),
            },
            Some("reviewer"),
            Some("deadline elapsed"),
        )?;
        let skipped_plan = store.apply_task_patch_for_test(
            run_id,
            &TaskUpdateRequest {
                base_revision: 2,
                reason: "exercise explicit skip projection".to_string(),
                operations: vec![TaskUpdateOperation::Skip {
                    task_id: "skipped".to_string(),
                }],
            },
        )?;
        assert_eq!(skipped_plan.revision, 3);
        assert_graph_and_projections(&store, 3)?;

        let runtime = store.load_runtime_plan_snapshot(run_id)?;
        let runtime_status = |task_id: &str| {
            runtime
                .tasks
                .iter()
                .find(|task| task.spec.id == task_id)
                .map(|task| &task.execution.status)
        };
        assert!(matches!(
            runtime_status("upstream"),
            Some(echo_agent::tasks::TaskStatus::Failed(detail)) if detail == "upstream failed"
        ));
        assert_eq!(
            runtime_status("dependent"),
            Some(&echo_agent::tasks::TaskStatus::Pending)
        );
        assert!(matches!(
            runtime_status("blocked"),
            Some(echo_agent::tasks::TaskStatus::Blocked(detail)) if detail == "awaiting explicit decision"
        ));
        assert!(matches!(
            runtime_status("timed-out"),
            Some(echo_agent::tasks::TaskStatus::TimedOut { error }) if error == "deadline elapsed"
        ));
        assert_eq!(
            runtime_status("skipped"),
            Some(&echo_agent::tasks::TaskStatus::Skipped)
        );
        let todos = store.list_todos(run_id)?;
        assert_eq!(
            todos
                .iter()
                .find(|todo| todo.task_id == "dependent")
                .map(|todo| todo.status),
            Some(TodoStatus::Blocked)
        );
        assert_eq!(
            todos
                .iter()
                .find(|todo| todo.task_id == "timed-out")
                .map(|todo| todo.status),
            Some(TodoStatus::TimedOut)
        );
        assert_eq!(
            todos
                .iter()
                .find(|todo| todo.task_id == "skipped")
                .map(|todo| todo.status),
            Some(TodoStatus::Skipped)
        );

        store.transition_run(run_id, TaskRunStatus::Failed)?;
        assert_eq!(store.retry_blocked_task(run_id, "upstream")?, 1);
        assert_graph_and_projections(&store, 3)?;
        let retried = store.load_runtime_plan_snapshot(run_id)?;
        let retried_upstream = retried
            .tasks
            .iter()
            .find(|task| task.spec.id == "upstream")
            .ok_or_else(|| StoreError::TaskNotFound("upstream".to_string()))?;
        assert_eq!(
            &retried_upstream.execution.status,
            &echo_agent::tasks::TaskStatus::Pending
        );
        assert_eq!(retried_upstream.execution.retry_count, 1);
        assert_eq!(
            store
                .get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?
                .status,
            TaskRunStatus::Running
        );
        assert_eq!(
            store
                .list_todos(run_id)?
                .iter()
                .find(|todo| todo.task_id == "dependent")
                .map(|todo| todo.status),
            Some(TodoStatus::Pending)
        );

        assert!(store.request_cancel(run_id)?);
        let cancelled_runtime = store.load_runtime_plan_snapshot(run_id)?;
        for task in &cancelled_runtime.tasks {
            if task.spec.id == "timed-out" || task.spec.id == "skipped" {
                continue;
            }
            assert_eq!(
                &task.execution.status,
                &echo_agent::tasks::TaskStatus::Cancelled
            );
        }
        assert_eq!(
            store
                .get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?
                .status,
            TaskRunStatus::Cancelled
        );
        let cancelled_todos = store.list_todos(run_id)?;
        assert_eq!(
            cancelled_todos
                .iter()
                .find(|todo| todo.task_id == "upstream")
                .map(|todo| todo.status),
            Some(TodoStatus::Cancelled)
        );
        assert_eq!(
            cancelled_todos
                .iter()
                .find(|todo| todo.task_id == "blocked")
                .map(|todo| todo.status),
            Some(TodoStatus::Cancelled)
        );
        assert_eq!(
            cancelled_todos
                .iter()
                .find(|todo| todo.task_id == "timed-out")
                .map(|todo| todo.status),
            Some(TodoStatus::TimedOut)
        );
        assert_eq!(
            cancelled_todos
                .iter()
                .find(|todo| todo.task_id == "skipped")
                .map(|todo| todo.status),
            Some(TodoStatus::Skipped)
        );

        let recovery_id = "restart-recovery-characterization";
        store.create_run(
            recovery_id,
            "ws",
            "conversation",
            "recovery-message",
            DomainProfile::General,
            "characterize restart recovery",
            "task",
            AttendedMode::Attended,
        )?;
        store.attach_plan_for_test(&TaskPlan {
            plan_id: "restart-recovery-characterization-plan".to_string(),
            run_id: recovery_id.to_string(),
            revision: 1,
            domain_profile: DomainProfile::General,
            goal_revision: 1,
            goal_sha256: task_goal_sha256("characterize restart recovery"),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Sequential,
            tasks: vec![sample_task_body("recovery-task")],
        })?;
        store.transition_run(recovery_id, TaskRunStatus::Running)?;
        store.set_task_status(
            recovery_id,
            "recovery-task",
            echo_agent::tasks::TaskStatus::Running,
            Some("subagent"),
            Some("interrupted before completion"),
        )?;
        drop(store);

        let restarted = TaskRuntimeStore::new_in_memory_with_shadow_root(&root)
            .map_err(|error| StoreError::InvalidPlan(error.to_string()))?;
        assert_eq!(
            restarted
                .get_run(recovery_id)?
                .ok_or_else(|| StoreError::RunNotFound(recovery_id.to_string()))?
                .status,
            TaskRunStatus::Running
        );
        assert_eq!(
            restarted
                .list_todos(recovery_id)?
                .iter()
                .find(|todo| todo.task_id == "recovery-task")
                .map(|todo| todo.status),
            Some(TodoStatus::Running)
        );
        assert_eq!(restarted.recover_incomplete()?, 1);
        let recovered_state = restarted
            .get_run_state(recovery_id)?
            .ok_or_else(|| StoreError::RunNotFound(recovery_id.to_string()))?;
        assert_eq!(recovered_state.run.status, TaskRunStatus::Paused);
        let recovered_task = recovered_state
            .tasks
            .iter()
            .find(|task| task.task_id == "recovery-task")
            .ok_or_else(|| StoreError::TaskNotFound("recovery-task".to_string()))?;
        // Current recovery pauses the Run but resets an interrupted task to
        // Pending so it can be reclaimed explicitly on resume. Todo is the
        // matching read-only projection of that canonical task status.
        assert_eq!(
            recovered_task.status,
            echo_agent::tasks::TaskStatus::Pending
        );
        assert_eq!(
            restarted
                .list_todos(recovery_id)?
                .iter()
                .find(|todo| todo.task_id == "recovery-task")
                .map(|todo| todo.status),
            Some(TodoStatus::Pending)
        );
        Ok(())
    }

    #[test]
    fn list_todos_is_read_only_and_does_not_append_journal() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        let events_path = store.active_shadow_root().join("r1").join("events.jsonl");
        let before_events = std::fs::read(&events_path)
            .map_err(|error| StoreError::InvalidPlan(error.to_string()))?;
        let before_count = store.list_events("r1", 0)?.len();
        let first = store.list_todos("r1")?;
        let second = store.list_todos("r1")?;
        let first_value = serde_json::to_value(&first)
            .map_err(|error| StoreError::InvalidPlan(error.to_string()))?;
        let second_value = serde_json::to_value(&second)
            .map_err(|error| StoreError::InvalidPlan(error.to_string()))?;
        assert_eq!(first_value, second_value);
        assert_eq!(store.list_events("r1", 0)?.len(), before_count);
        let after_events = std::fs::read(&events_path)
            .map_err(|error| StoreError::InvalidPlan(error.to_string()))?;
        assert_eq!(before_events, after_events);
        Ok(())
    }

    #[test]
    fn put_summary_upserts_and_get_summary_reads() -> Result<(), StoreError> {
        let s = fresh()?;
        seed_plan(&s)?;
        let sum = TaskExecutionSummary {
            run_id: "r1".into(),
            task_id: "t1".into(),
            subagent_name: "code_reviewer".into(),
            result: SubagentTaskResult {
                contract_version: 1,
                status: SubagentRunStatus::Completed,
                summary: "read chat.rs".into(),
                artifacts: Vec::new(),
                evidence: Vec::new(),
                verification: vec![SubagentVerificationResult {
                    check: "cargo check".into(),
                    status: SubagentVerificationStatus::Passed,
                    details: String::new(),
                    source: SubagentVerificationSource::Observed,
                }],
                remaining_work: Vec::new(),
                touched_files: SubagentTouchedFiles {
                    read: vec!["chat.rs".into()],
                    written: Vec::new(),
                },
            },
            decisions: vec!["route via TaskRuntime".into()],
            next_implications: vec!["implement router".into()],
            suggested_tasks: vec![],
            created_at: Utc::now(),
        };
        s.put_summary(&sum)?;
        let got = s
            .get_summary("r1", "t1")?
            .ok_or_else(|| StoreError::TaskNotFound("t1 summary".to_string()))?;
        assert_eq!(got.result.summary, "read chat.rs");
        assert_eq!(got.next_implications.len(), 1);
        Ok(())
    }

    #[test]
    fn latest_run_for_conversation_orders_by_created_desc() -> Result<(), StoreError> {
        let s = fresh()?;
        s.create_run(
            "r1",
            "ws",
            "c1",
            "m1",
            DomainProfile::General,
            "g1",
            "",
            AttendedMode::Attended,
        )?;
        std::thread::sleep(std::time::Duration::from_millis(10));
        s.create_run(
            "r2",
            "ws",
            "c1",
            "m2",
            DomainProfile::General,
            "g2",
            "",
            AttendedMode::Attended,
        )?;
        let latest = s
            .latest_run_for_conversation("c1")?
            .ok_or_else(|| StoreError::RunNotFound("latest run for c1".to_string()))?;
        assert_eq!(latest.run_id, "r2");
        Ok(())
    }

    fn seed_plan(s: &TaskRuntimeStore) -> Result<(), StoreError> {
        s.create_run(
            "r1",
            "ws",
            "c1",
            "m1",
            DomainProfile::General,
            "g",
            "",
            AttendedMode::Attended,
        )?;
        let plan = TaskPlan {
            plan_id: "p1".into(),
            run_id: "r1".into(),
            revision: 1,
            domain_profile: DomainProfile::General,
            goal_revision: 1,
            goal_sha256: task_goal_sha256("g"),
            assumptions: vec![],
            risks: vec![],
            execution_mode: ExecutionMode::Parallel,
            tasks: vec![PlanTask {
                id: "t1".into(),
                title: "Review runtime".into(),
                kind: PlanTaskKind::ReadOnlyReview,
                agent_role: "code_reviewer".into(),
                ..Default::default()
            }],
        };
        s.attach_plan_for_test(&plan)?;
        s.transition_run("r1", TaskRunStatus::Running)?;
        Ok(())
    }

    #[test]
    fn budget_update_requires_existing_continuation_and_preserves_policy() -> Result<(), StoreError>
    {
        let store = fresh()?;
        seed_plan(&store)?;
        assert!(
            store
                .update_run_continuation_budgets("r1", Some(100), Some(60))
                .is_err()
        );
        store.configure_run_continuation("r1", true, true, None, None)?;
        let updated = store.update_run_continuation_budgets("r1", Some(100), Some(60))?;
        assert_eq!(updated.token_budget, Some(100));
        assert_eq!(updated.time_budget_seconds, Some(60));
        assert!(updated.auto_resume_after_restart);
        assert!(
            store
                .update_run_continuation_budgets("r1", Some(0), None)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn lowering_budget_atomically_pauses_and_cancels_active_driver() -> Result<(), StoreError> {
        let store = std::sync::Arc::new(fresh()?);
        seed_plan(&store)?;
        store.configure_run_continuation("r1", true, true, None, None)?;
        assert!(matches!(
            store.claim_run_turn("r1", "turn-1", RunTurnOrigin::User, TurnVisibility::Visible)?,
            RunTurnClaimOutcome::Started(_)
        ));
        assert!(!store.account_run_turn_usage("r1", "turn-1", "usage-1", 40, 20)?);
        let token = echo_agent::agent::CancellationToken::new();
        let registration = store.register_run_cancellation("r1", token.clone())?;

        let updated = store.update_run_continuation_budgets("r1", Some(60), None)?;

        assert!(token.is_cancelled());
        assert_eq!(updated.token_budget, Some(60));
        assert_eq!(
            updated.pause.as_ref().map(|pause| pause.reason),
            Some(RunPauseReason::TokenBudget)
        );
        assert_eq!(
            store
                .get_run("r1")?
                .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?
                .status,
            TaskRunStatus::Paused
        );
        let budget_event = store
            .list_events("r1", 0)?
            .into_iter()
            .rev()
            .find(|event| event.event_type == RuntimeEventKind::RunContinuationConfigured)
            .ok_or_else(|| StoreError::InvalidPlan("budget event missing".to_string()))?;
        assert_eq!(
            budget_event
                .payload
                .get("pause_reason")
                .and_then(serde_json::Value::as_str),
            Some("token_budget")
        );
        drop(registration);
        Ok(())
    }

    #[test]
    fn subagent_usage_is_idempotent_and_parent_turn_owns_wall_clock() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.configure_run_continuation("r1", true, false, Some(100), Some(20))?;
        assert!(matches!(
            store.claim_run_turn("r1", "turn-1", RunTurnOrigin::User, TurnVisibility::Visible)?,
            RunTurnClaimOutcome::Started(_)
        ));
        store.record_subagent_assigned(
            "r1",
            "t1",
            "execution-1",
            "code_reviewer",
            "Review runtime",
            1,
            1,
            true,
            false,
        )?;

        assert!(!store.account_subagent_usage(
            "r1",
            "execution-1",
            "provider-total",
            12,
            8,
            2_500,
        )?);
        assert!(!store.account_subagent_usage(
            "r1",
            "execution-1",
            "provider-total",
            12,
            8,
            2_500,
        )?);
        let during_turn = store
            .get_run_state("r1")?
            .and_then(|state| state.continuation)
            .ok_or_else(|| StoreError::InvalidPlan("continuation missing".to_string()))?;
        assert_eq!(during_turn.tokens_used, 20);
        assert_eq!(during_turn.time_used_seconds, 0);
        let subagent_runs = store.list_subagent_runs("r1")?;
        let subagent_run = subagent_runs
            .first()
            .ok_or_else(|| StoreError::InvalidPlan("SubagentRun projection missing".to_string()))?;
        assert_eq!(subagent_run.subagent_run_id, "execution-1");
        assert_eq!(subagent_run.usage.tokens_used, Some(20));
        assert_eq!(subagent_run.usage.duration_ms, Some(2_500));
        let result = SubagentTaskResult::terminal(
            SubagentRunStatus::Completed,
            "review complete",
            Vec::new(),
        );
        let terminal_usage = SubagentRunUsage {
            duration_ms: Some(2_500),
            tokens_used: Some(20),
            iterations: Some(2),
        };
        store.record_subagent_released(SubagentReleaseRecord {
            run_id: "r1",
            task_id: "t1",
            execution_id: "execution-1",
            agent_name: "code_reviewer",
            task_subject: "Review runtime",
            plan_revision: 1,
            attempt: 1,
            status: "completed",
            result: Some(&result),
            full_output: Some("review complete"),
            usage: Some(&terminal_usage),
            dispatch_hook: false,
        })?;
        let settled_runs = store.list_subagent_runs("r1")?;
        let settled = settled_runs.first().ok_or_else(|| {
            StoreError::InvalidPlan("settled SubagentRun projection missing".to_string())
        })?;
        assert_eq!(settled.status, SubagentRunStatus::Completed);
        assert_eq!(settled.result.as_ref(), Some(&result));
        assert_eq!(settled.usage, terminal_usage);
        assert_eq!(
            store
                .list_events("r1", 0)?
                .iter()
                .filter(|event| {
                    event.event_type == RuntimeEventKind::RunTurnUsageAccounted
                        && event
                            .payload
                            .get("source_scope")
                            .and_then(serde_json::Value::as_str)
                            == Some("subagent")
                })
                .count(),
            1
        );

        let finished = store.finish_run_turn(
            "r1",
            RunTurnCompletion {
                turn_id: "turn-1",
                status: RunTurnStatus::Ended,
                elapsed_seconds: 3,
                final_message_id: None,
                error_fingerprint: None,
            },
        )?;
        assert_eq!(finished.tokens_used, 20);
        assert_eq!(finished.time_used_seconds, 3);
        Ok(())
    }

    #[test]
    fn cell_terminal_and_defer_race_cannot_leave_lost_wakeup() -> Result<(), String> {
        let store = std::sync::Arc::new(fresh().map_err(|error| error.to_string())?);
        seed_plan(&store).map_err(|error| error.to_string())?;
        store
            .configure_run_continuation("r1", true, false, None, None)
            .map_err(|error| error.to_string())?;
        store
            .record_background_cell_started(
                "r1",
                "cell-1",
                "cargo test",
                "hash",
                Some("turn-1"),
                None,
                Some("call-1"),
            )
            .map_err(|error| error.to_string())?;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));

        let defer_store = store.clone();
        let defer_barrier = barrier.clone();
        let defer = std::thread::spawn(move || {
            defer_barrier.wait();
            defer_store.defer_continuation_for_active_cells("r1")
        });
        let terminal_store = store.clone();
        let terminal = std::thread::spawn(move || {
            barrier.wait();
            terminal_store.record_background_cell_finished(
                "r1",
                "cell-1",
                "cargo test",
                BackgroundCellPhase::Succeeded,
                Some(BackgroundCellTerminalCause::Exited),
                None,
                Some(0),
                BackgroundCellArtifactStatus::NotRequested,
                None,
                2,
                false,
                Some("ok"),
                None,
                None,
                Some("call-1"),
            )?;
            super::super::continuation::wake_after_cell_terminal(&terminal_store, "r1");
            Ok::<(), StoreError>(())
        });

        defer
            .join()
            .map_err(|_| "defer thread panicked".to_string())?
            .map_err(|error| error.to_string())?;
        terminal
            .join()
            .map_err(|_| "terminal thread panicked".to_string())?
            .map_err(|error| error.to_string())?;
        let continuation = store
            .get_run_state("r1")
            .map_err(|error| error.to_string())?
            .and_then(|state| state.continuation)
            .ok_or_else(|| "continuation missing".to_string())?;
        assert!(!continuation.deferred);
        Ok(())
    }

    #[test]
    fn cell_terminal_retry_uses_the_exact_checkpointed_terminal_fact() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.record_background_cell_started(
            "r1",
            "cell-retry",
            "prepared name",
            "hash",
            None,
            None,
            Some("start-call"),
        )?;

        for _ in 0..2 {
            store.record_background_cell_finished(
                "r1",
                "cell-retry",
                "terminal name",
                BackgroundCellPhase::Succeeded,
                Some(BackgroundCellTerminalCause::Exited),
                None,
                Some(0),
                BackgroundCellArtifactStatus::NotRequested,
                None,
                2,
                false,
                Some("ok"),
                None,
                None,
                None,
            )?;
        }

        let cells = store.list_background_cells("r1")?;
        let cell = cells
            .iter()
            .find(|cell| cell.cell_id == "cell-retry")
            .ok_or_else(|| StoreError::InvalidPlan("terminal cell missing".to_string()))?;
        assert_eq!(cell.name, "terminal name");
        assert_eq!(cell.call_id, None);
        assert_eq!(cell.phase, BackgroundCellPhase::Succeeded);
        assert_eq!(
            store
                .list_events("r1", 0)?
                .iter()
                .filter(|event| {
                    event.event_type == RuntimeEventKind::BackgroundCellFinished
                        && event
                            .payload
                            .get("cell_id")
                            .and_then(serde_json::Value::as_str)
                            == Some("cell-retry")
                })
                .count(),
            1
        );
        Ok(())
    }

    #[test]
    fn concurrent_run_turn_claim_has_one_authoritative_winner() -> Result<(), String> {
        let store = std::sync::Arc::new(fresh().map_err(|error| error.to_string())?);
        seed_plan(&store).map_err(|error| error.to_string())?;
        store
            .configure_run_continuation("r1", true, false, None, None)
            .map_err(|error| error.to_string())?;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(16));
        let mut threads = Vec::new();
        for index in 0..16 {
            let store = store.clone();
            let barrier = barrier.clone();
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                store.claim_run_turn(
                    "r1",
                    &format!("turn-{index}"),
                    RunTurnOrigin::Continuation,
                    TurnVisibility::Internal,
                )
            }));
        }
        let mut started = Vec::new();
        let mut already_running = 0_usize;
        for thread in threads {
            let outcome = thread
                .join()
                .map_err(|_| "RunTurn claim thread panicked".to_string())?
                .map_err(|error| error.to_string())?;
            match outcome {
                RunTurnClaimOutcome::Started(summary) => started.push(summary),
                RunTurnClaimOutcome::NotSubmitted(
                    ContinuationNotSubmittedReason::AlreadyRunning,
                ) => already_running = already_running.saturating_add(1),
                other => return Err(format!("unexpected claim outcome: {other:?}")),
            }
        }
        assert_eq!(started.len(), 1);
        assert_eq!(already_running, 15);
        assert_eq!(started.first().map(|turn| turn.ordinal), Some(1));
        assert_eq!(
            store
                .get_run_state("r1")
                .map_err(|error| error.to_string())?
                .and_then(|state| state.continuation)
                .and_then(|state| state.active_turn)
                .map(|turn| turn.ordinal),
            Some(1)
        );
        Ok(())
    }

    #[test]
    fn run_turn_accounting_is_idempotent_and_rejects_cross_turn_events() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.configure_run_continuation("r1", true, false, Some(100), None)?;
        assert!(matches!(
            store.claim_run_turn("r1", "turn-1", RunTurnOrigin::User, TurnVisibility::Visible,)?,
            RunTurnClaimOutcome::Started(_)
        ));
        assert!(
            store
                .account_run_turn_usage("r1", "wrong-turn", "usage-1", 10, 20)
                .is_err()
        );
        assert!(
            store
                .record_run_turn_compaction("r1", "wrong-turn", "compact-1")
                .is_err()
        );
        assert!(
            store
                .finish_run_turn(
                    "r1",
                    RunTurnCompletion {
                        turn_id: "wrong-turn",
                        status: RunTurnStatus::Ended,
                        elapsed_seconds: 7,
                        final_message_id: None,
                        error_fingerprint: None,
                    },
                )
                .is_err()
        );
        assert!(!store.account_run_turn_usage("r1", "turn-1", "usage-1", 10, 20)?);
        assert!(!store.account_run_turn_usage("r1", "turn-1", "usage-1", 10, 20)?);
        store.record_run_turn_compaction("r1", "turn-1", "compact-1")?;
        store.record_run_turn_compaction("r1", "turn-1", "compact-1")?;
        let first = store.finish_run_turn(
            "r1",
            RunTurnCompletion {
                turn_id: "turn-1",
                status: RunTurnStatus::Ended,
                elapsed_seconds: 7,
                final_message_id: Some("message-1"),
                error_fingerprint: None,
            },
        )?;
        let replay = store.finish_run_turn(
            "r1",
            RunTurnCompletion {
                turn_id: "turn-1",
                status: RunTurnStatus::Failed,
                elapsed_seconds: 99,
                final_message_id: None,
                error_fingerprint: Some("must-not-overwrite"),
            },
        )?;
        assert_eq!(first, replay);
        assert_eq!(replay.tokens_used, 30);
        assert_eq!(replay.time_used_seconds, 7);
        assert_eq!(replay.compaction_count, 1);
        let last = replay
            .last_turn
            .ok_or_else(|| StoreError::InvalidPlan("finished RunTurn missing".to_string()))?;
        assert_eq!(last.input_tokens, 10);
        assert_eq!(last.output_tokens, 20);
        assert_eq!(last.compaction_count, 1);
        assert_eq!(last.status, RunTurnStatus::Ended);
        assert_eq!(last.final_message_id.as_deref(), Some("message-1"));
        assert!(matches!(
            store.claim_run_turn(
                "r1",
                "turn-1",
                RunTurnOrigin::Continuation,
                TurnVisibility::Internal,
            ),
            Err(StoreError::InvalidPlan(_))
        ));
        Ok(())
    }

    #[test]
    fn time_budget_stops_at_exact_boundary_and_cannot_be_bypassed_by_resume()
    -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.configure_run_continuation("r1", true, false, None, Some(7))?;
        assert!(matches!(
            store.claim_run_turn("r1", "turn-1", RunTurnOrigin::User, TurnVisibility::Visible)?,
            RunTurnClaimOutcome::Started(_)
        ));
        let state = store.finish_run_turn(
            "r1",
            RunTurnCompletion {
                turn_id: "turn-1",
                status: RunTurnStatus::Ended,
                elapsed_seconds: 7,
                final_message_id: None,
                error_fingerprint: None,
            },
        )?;
        assert_eq!(state.time_budget_seconds, Some(7));
        assert_eq!(state.time_used_seconds, 7);
        assert!(matches!(
            store.claim_run_turn(
                "r1",
                "turn-2",
                RunTurnOrigin::Continuation,
                TurnVisibility::Internal,
            )?,
            RunTurnClaimOutcome::NotSubmitted(ContinuationNotSubmittedReason::TimeBudgetExhausted)
        ));

        assert!(store.request_pause_with_reason(
            "r1",
            RunPauseReason::TimeBudget,
            Some("configured time budget exhausted"),
        )?);
        let error = store
            .resume_task_run("r1")
            .err()
            .ok_or_else(|| StoreError::InvalidPlan("resume unexpectedly succeeded".to_string()))?;
        assert!(error.to_string().contains("time budget"));
        Ok(())
    }

    #[test]
    fn one_hundred_turns_and_compactions_replay_without_double_accounting() -> Result<(), StoreError>
    {
        let store = fresh()?;
        seed_plan(&store)?;
        let initial_goal_sha256 = store
            .get_run("r1")?
            .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?
            .goal_sha256;
        store.configure_run_continuation("r1", true, false, None, None)?;
        for ordinal in 1..=100_u64 {
            let turn_id = format!("soak-turn-{ordinal}");
            assert!(matches!(
                store.claim_run_turn(
                    "r1",
                    &turn_id,
                    RunTurnOrigin::Continuation,
                    TurnVisibility::Internal,
                )?,
                RunTurnClaimOutcome::Started(_)
            ));
            let provider_event_id = format!("usage-{ordinal}");
            assert!(!store.account_run_turn_usage("r1", &turn_id, &provider_event_id, 1, 2,)?);
            assert!(!store.account_run_turn_usage("r1", &turn_id, &provider_event_id, 1, 2,)?);
            let compaction_event_id = format!("compact-{ordinal}");
            store.record_run_turn_compaction("r1", &turn_id, &compaction_event_id)?;
            store.record_run_turn_compaction("r1", &turn_id, &compaction_event_id)?;
            store.finish_run_turn(
                "r1",
                RunTurnCompletion {
                    turn_id: &turn_id,
                    status: RunTurnStatus::Ended,
                    elapsed_seconds: 1,
                    final_message_id: None,
                    error_fingerprint: None,
                },
            )?;
        }

        let events = store.list_events("r1", 0)?;
        let journal_sequence = events
            .last()
            .and_then(|event| u64::try_from(event.seq).ok())
            .ok_or_else(|| StoreError::InvalidPlan("replay sequence is unavailable".to_string()))?;
        let replayed = super::super::event_rebuild::fold_fixture_for_test(&events)
            .map_err(|error| StoreError::InvalidPlan(error.to_string()))?
            .run_state_with_sequence(journal_sequence)
            .continuation
            .ok_or_else(|| StoreError::InvalidPlan("soak continuation missing".to_string()))?;
        assert_eq!(replayed.tokens_used, 300);
        assert_eq!(replayed.time_used_seconds, 100);
        assert_eq!(replayed.compaction_count, 100);
        assert_eq!(replayed.next_turn_ordinal, 101);
        assert!(replayed.active_turn.is_none());
        assert_eq!(
            store
                .get_run("r1")?
                .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?
                .goal_sha256,
            initial_goal_sha256
        );
        Ok(())
    }

    #[test]
    fn provider_retry_schedule_rebuilds_and_counts_across_fingerprints() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.configure_run_continuation("r1", true, false, None, None)?;
        let base = Utc::now() - chrono::Duration::hours(1);

        for (turn_id, fingerprint, expected_attempt, offset) in [
            ("retry-turn-1", "provider-a", 1_u32, 0_i64),
            ("retry-turn-2", "provider-a", 2_u32, 1_i64),
            ("retry-turn-3", "provider-b", 3_u32, 2_i64),
        ] {
            assert!(matches!(
                store.claim_run_turn(
                    "r1",
                    turn_id,
                    RunTurnOrigin::Continuation,
                    TurnVisibility::Internal,
                )?,
                RunTurnClaimOutcome::Started(_)
            ));
            store.finish_run_turn(
                "r1",
                RunTurnCompletion {
                    turn_id,
                    status: RunTurnStatus::Failed,
                    elapsed_seconds: 1,
                    final_message_id: None,
                    error_fingerprint: Some(fingerprint),
                },
            )?;
            let scheduled = store.schedule_provider_retry_at(
                "r1",
                fingerprint,
                base + chrono::Duration::seconds(offset),
            )?;
            assert_eq!(scheduled.state().attempt_count, expected_attempt);
            assert_eq!(scheduled.state().error_fingerprint, fingerprint);
        }

        let events = store.list_events("r1", 0)?;
        let journal_sequence = events
            .last()
            .and_then(|event| u64::try_from(event.seq).ok())
            .ok_or_else(|| StoreError::InvalidPlan("replay sequence is unavailable".to_string()))?;
        let replayed = super::super::event_rebuild::fold_fixture_for_test(&events)
            .map_err(|error| StoreError::InvalidPlan(error.to_string()))?
            .run_state_with_sequence(journal_sequence)
            .continuation
            .and_then(|state| state.provider_retry)
            .ok_or_else(|| StoreError::InvalidPlan("provider retry did not rebuild".to_string()))?;
        assert_eq!(replayed.attempt_count, 3);
        assert_eq!(replayed.error_fingerprint, "provider-b");
        assert_eq!(replayed.first_failure_at, base);
        assert_eq!(
            stable_provider_retry_delay_millis("r1", "provider-b", 1),
            stable_provider_retry_delay_millis("r1", "provider-b", 1)
        );
        Ok(())
    }

    #[test]
    fn provider_retry_claim_waits_then_success_clears_schedule() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.configure_run_continuation("r1", true, false, None, None)?;
        assert!(matches!(
            store.claim_run_turn(
                "r1",
                "failed-turn",
                RunTurnOrigin::Continuation,
                TurnVisibility::Internal,
            )?,
            RunTurnClaimOutcome::Started(_)
        ));
        store.finish_run_turn(
            "r1",
            RunTurnCompletion {
                turn_id: "failed-turn",
                status: RunTurnStatus::Failed,
                elapsed_seconds: 1,
                final_message_id: None,
                error_fingerprint: Some("provider-a"),
            },
        )?;
        store.schedule_provider_retry_at("r1", "provider-a", Utc::now())?;
        assert!(matches!(
            store.claim_run_turn(
                "r1",
                "too-early",
                RunTurnOrigin::Continuation,
                TurnVisibility::Internal,
            )?,
            RunTurnClaimOutcome::NotSubmitted(ContinuationNotSubmittedReason::ProviderRetryBackoff)
        ));

        let past = Utc::now() - chrono::Duration::hours(1);
        store.schedule_provider_retry_at("r1", "provider-b", past)?;
        assert!(matches!(
            store.claim_run_turn(
                "r1",
                "successful-retry",
                RunTurnOrigin::Continuation,
                TurnVisibility::Internal,
            )?,
            RunTurnClaimOutcome::Started(_)
        ));
        let state = store.finish_run_turn(
            "r1",
            RunTurnCompletion {
                turn_id: "successful-retry",
                status: RunTurnStatus::Ended,
                elapsed_seconds: 1,
                final_message_id: None,
                error_fingerprint: None,
            },
        )?;
        assert!(state.provider_retry.is_none());
        Ok(())
    }

    #[test]
    fn fifth_provider_failure_atomically_pauses_and_explicit_resume_resets_retry()
    -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.configure_run_continuation("r1", true, true, None, None)?;
        let base = Utc::now() - chrono::Duration::hours(1);
        for attempt in 1..=MAX_PROVIDER_RETRY_ATTEMPTS {
            let turn_id = format!("retry-exhaustion-{attempt}");
            assert!(matches!(
                store.claim_run_turn(
                    "r1",
                    &turn_id,
                    RunTurnOrigin::Continuation,
                    TurnVisibility::Internal,
                )?,
                RunTurnClaimOutcome::Started(_)
            ));
            store.finish_run_turn(
                "r1",
                RunTurnCompletion {
                    turn_id: &turn_id,
                    status: RunTurnStatus::Failed,
                    elapsed_seconds: 1,
                    final_message_id: None,
                    error_fingerprint: Some("provider-a"),
                },
            )?;
            let disposition = store.schedule_provider_retry_at(
                "r1",
                "provider-a",
                base + chrono::Duration::seconds(i64::from(attempt)),
            )?;
            assert_eq!(disposition.state().attempt_count, attempt);
        }
        let snapshot = store
            .get_run_state("r1")?
            .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?;
        assert_eq!(snapshot.run.status, TaskRunStatus::Paused);
        let continuation = snapshot
            .continuation
            .ok_or_else(|| StoreError::InvalidPlan("continuation missing".to_string()))?;
        assert!(
            continuation
                .provider_retry
                .as_ref()
                .is_some_and(|retry| retry.exhausted)
        );
        assert_eq!(
            continuation.pause.map(|pause| pause.reason),
            Some(RunPauseReason::ProviderUnavailable)
        );

        store.resume_task_run("r1")?;
        let resumed = store
            .get_run_state("r1")?
            .and_then(|snapshot| snapshot.continuation)
            .ok_or_else(|| StoreError::InvalidPlan("resumed continuation missing".to_string()))?;
        assert!(resumed.provider_retry.is_none());
        Ok(())
    }

    fn prepare_boot_auto_resume_run(
        store: &TaskRuntimeStore,
        run_id: &str,
        attended_mode: AttendedMode,
    ) -> Result<(), StoreError> {
        let workspace_id = store.active_workspace_id();
        store.create_run(
            run_id,
            &workspace_id,
            &format!("background:test:{run_id}"),
            "root",
            DomainProfile::General,
            "boot goal",
            "bg:kind:test",
            attended_mode,
        )?;
        store.attach_plan_for_test(&TaskPlan {
            plan_id: format!("{run_id}-plan"),
            run_id: run_id.to_string(),
            revision: 1,
            domain_profile: DomainProfile::General,
            goal_revision: 1,
            goal_sha256: task_goal_sha256("boot goal"),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Sequential,
            tasks: vec![PlanTask {
                id: format!("{run_id}-task"),
                title: "Resume safely".to_string(),
                ..PlanTask::default()
            }],
        })?;
        store.transition_run(run_id, TaskRunStatus::Running)?;
        store.configure_run_continuation(run_id, true, true, None, None)?;
        store.record_run_pause_reason(
            run_id,
            RunPauseReason::BootRecovery,
            Some("test process interruption"),
        )?;
        store.transition_run(run_id, TaskRunStatus::Paused)?;
        Ok(())
    }

    #[test]
    fn boot_auto_resume_admission_rejects_missing_owner_workspace_and_unsafe_boundary()
    -> Result<(), StoreError> {
        let store = fresh()?;
        prepare_boot_auto_resume_run(&store, "attended", AttendedMode::Attended)?;
        let attended = store.boot_auto_resume_decision("attended", true, false)?;
        assert!(matches!(
            attended,
            BootAutoResumeDecision::Blocked(blockers)
                if blockers.contains(&BootAutoResumeBlocker::InteractiveOwnerUnavailable)
        ));

        prepare_boot_auto_resume_run(&store, "disabled", AttendedMode::Unattended)?;
        store.configure_run_continuation("disabled", true, false, None, None)?;
        assert!(matches!(
            store.boot_auto_resume_decision("disabled", true, false)?,
            BootAutoResumeDecision::Blocked(blockers)
                if blockers.contains(&BootAutoResumeBlocker::AutoResumeDisabled)
        ));

        prepare_boot_auto_resume_run(&store, "launcher", AttendedMode::Unattended)?;
        assert!(matches!(
            store.boot_auto_resume_decision("launcher", false, false)?,
            BootAutoResumeDecision::Blocked(blockers)
                if blockers.contains(&BootAutoResumeBlocker::LauncherUnavailable)
        ));

        prepare_boot_auto_resume_run(&store, "unsafe", AttendedMode::Unattended)?;
        store.record_recovery_blocker(
            "unsafe",
            "unsafe-task",
            Some("execution"),
            Some("call"),
            Some("shell"),
            "indeterminate side effect",
        )?;
        let unsafe_decision = store.boot_auto_resume_decision("unsafe", true, false)?;
        assert!(matches!(
            unsafe_decision,
            BootAutoResumeDecision::Blocked(blockers)
                if blockers.contains(&BootAutoResumeBlocker::RecoveryBlocker)
        ));

        let mismatched = fresh()?;
        mismatched.create_run(
            "mismatch",
            "different-workspace",
            "background:test:mismatch",
            "root",
            DomainProfile::General,
            "boot goal",
            "bg:kind:test",
            AttendedMode::Unattended,
        )?;
        mismatched.attach_plan_for_test(&TaskPlan {
            plan_id: "mismatch-plan".to_string(),
            run_id: "mismatch".to_string(),
            revision: 1,
            domain_profile: DomainProfile::General,
            goal_revision: 1,
            goal_sha256: task_goal_sha256("boot goal"),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Sequential,
            tasks: vec![PlanTask {
                id: "mismatch-task".to_string(),
                title: "Stay paused".to_string(),
                ..PlanTask::default()
            }],
        })?;
        mismatched.transition_run("mismatch", TaskRunStatus::Running)?;
        mismatched.configure_run_continuation("mismatch", true, true, None, None)?;
        mismatched.record_run_pause_reason(
            "mismatch",
            RunPauseReason::BootRecovery,
            Some("test process interruption"),
        )?;
        mismatched.transition_run("mismatch", TaskRunStatus::Paused)?;
        assert!(matches!(
            mismatched.boot_auto_resume_decision("mismatch", true, false)?,
            BootAutoResumeDecision::Blocked(blockers)
                if blockers.contains(&BootAutoResumeBlocker::WorkspaceMismatch)
        ));
        Ok(())
    }

    #[test]
    fn competing_boot_launchers_have_one_atomic_resume_winner() -> Result<(), String> {
        let store = std::sync::Arc::new(
            TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?,
        );
        prepare_boot_auto_resume_run(&store, "race", AttendedMode::Unattended)
            .map_err(|error| error.to_string())?;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let mut threads = Vec::new();
        for _ in 0..2 {
            let thread_store = std::sync::Arc::clone(&store);
            let thread_barrier = std::sync::Arc::clone(&barrier);
            threads.push(std::thread::spawn(move || {
                thread_barrier.wait();
                thread_store.resume_task_run_after_boot("race", true, false)
            }));
        }
        barrier.wait();
        let mut resumed = 0_usize;
        for thread in threads {
            let outcome = thread
                .join()
                .map_err(|_| "boot resume thread panicked".to_string())?
                .map_err(|error| error.to_string())?;
            if matches!(outcome, BootAutoResumeOutcome::Resumed(_)) {
                resumed = resumed.saturating_add(1);
            }
        }
        assert_eq!(resumed, 1);
        Ok(())
    }

    #[test]
    fn resume_task_run_transitions_paused_to_running() -> Result<(), String> {
        let s = fresh().map_err(|error| error.to_string())?;
        seed_plan(&s).map_err(|error| error.to_string())?;
        // Simulate user interrupt: Running -> Paused.
        s.transition_run("r1", TaskRunStatus::Paused)
            .map_err(|error| error.to_string())?;
        let run = s
            .get_run("r1")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "resumable TaskRun missing".to_string())?;
        assert_eq!(run.status, TaskRunStatus::Paused);

        // Resume: Paused -> Running.
        let run = s.resume_task_run("r1").map_err(|error| error.to_string())?;
        assert_eq!(run.status, TaskRunStatus::Running);

        // Event log contains the Paused and Running transitions.
        let evs = s.list_events("r1", 0).map_err(|error| error.to_string())?;
        let status_changes: Vec<_> = evs
            .iter()
            .filter(|e| e.event_type == RuntimeEventKind::RunStatusChanged)
            .collect();
        assert!(status_changes.len() >= 2);
        assert_eq!(
            last_frame_event_types(&s, "r1")?,
            [
                "run_status_changed",
                "run_pause_reason_changed",
                "run_continuation_resumed",
            ]
        );
        Ok(())
    }

    #[test]
    fn expected_resume_and_run_turn_claim_commit_in_one_frame() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.configure_run_continuation("r1", true, false, None, None)?;
        store.request_pause("r1")?;
        let expected = TaskRunResumeIdentity::capture(
            &store
                .get_run_state("r1")?
                .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?,
        );
        let before_events = store.list_events("r1", 0)?.len();
        assert!(
            store
                .resume_and_claim_run_turn_expected(
                    &expected,
                    "invalid-origin-turn",
                    RunTurnOrigin::Continuation,
                    TurnVisibility::Internal,
                )
                .is_err()
        );
        assert_eq!(store.list_events("r1", 0)?.len(), before_events);

        assert!(matches!(
            store.resume_and_claim_run_turn_expected(
                &expected,
                "resume-turn",
                RunTurnOrigin::Resume,
                TurnVisibility::Visible,
            )?,
            RunTurnClaimOutcome::Started(ref turn) if turn.turn_id == "resume-turn"
        ));
        assert_eq!(
            last_frame_event_types(&store, "r1").map_err(StoreError::InvalidPlan)?,
            [
                "run_status_changed",
                "run_pause_reason_changed",
                "run_continuation_resumed",
                "run_turn_started",
            ]
        );
        assert_eq!(
            store
                .get_run("r1")?
                .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?
                .status,
            TaskRunStatus::Running
        );
        Ok(())
    }

    #[test]
    fn expected_resume_allows_only_execution_path_diagnostic_suffix() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.configure_run_continuation("r1", true, false, None, None)?;
        store.request_pause("r1")?;
        let expected = TaskRunResumeIdentity::capture(
            &store
                .get_run_state("r1")?
                .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?,
        );
        store.record_execution_path("r1", "formal_plan")?;

        assert!(matches!(
            store.resume_and_claim_run_turn_expected(
                &expected,
                "diagnostic-race-resume",
                RunTurnOrigin::Resume,
                TurnVisibility::Visible,
            )?,
            RunTurnClaimOutcome::Started(ref turn)
                if turn.turn_id == "diagnostic-race-resume"
        ));
        Ok(())
    }

    #[test]
    fn expected_resume_sequence_rejects_status_attachment_and_continuation_aba()
    -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.configure_run_continuation("r1", true, false, None, None)?;
        store.request_pause("r1")?;
        let first = store
            .get_run_state("r1")?
            .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?;
        let status_epoch = TaskRunResumeIdentity::capture(&first);
        store.resume_task_run_expected(&status_epoch)?;
        store.request_pause("r1")?;
        let before_status_retry = store.list_events("r1", 0)?.len();
        assert!(store.resume_task_run_expected(&status_epoch).is_err());
        assert_eq!(store.list_events("r1", 0)?.len(), before_status_retry);

        let attachment_epoch = TaskRunResumeIdentity::capture(
            &store
                .get_run_state("r1")?
                .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?,
        );
        store.set_run_attachments(
            "r1",
            &[crate::attachments::AttachmentRef {
                path: std::path::PathBuf::from("/tmp/resume-epoch.txt"),
                name: "resume-epoch.txt".to_string(),
                mime_type: "text/plain".to_string(),
                source: crate::types::AttachmentSource::default(),
            }],
        )?;
        let before_attachment_retry = store.list_events("r1", 0)?.len();
        assert!(store.resume_task_run_expected(&attachment_epoch).is_err());
        assert_eq!(store.list_events("r1", 0)?.len(), before_attachment_retry);

        let continuation_epoch = TaskRunResumeIdentity::capture(
            &store
                .get_run_state("r1")?
                .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?,
        );
        store.configure_run_continuation("r1", true, false, Some(100), None)?;
        let before_continuation_retry = store.list_events("r1", 0)?.len();
        assert!(store.resume_task_run_expected(&continuation_epoch).is_err());
        assert_eq!(store.list_events("r1", 0)?.len(), before_continuation_retry);
        Ok(())
    }

    #[test]
    fn stale_expected_resume_cannot_mutate_recreated_run() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.configure_run_continuation("r1", true, false, None, None)?;
        store.request_pause("r1")?;
        let stale = TaskRunResumeIdentity::capture(
            &store
                .get_run_state("r1")?
                .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?,
        );

        store.shadow.remove_runs(&["r1".to_string()])?;
        seed_plan(&store)?;
        store.configure_run_continuation("r1", true, false, None, None)?;
        store.request_pause("r1")?;
        let before_events = store.list_events("r1", 0)?.len();
        let before = store
            .get_run_state("r1")?
            .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?;

        let error = store
            .resume_and_claim_run_turn_expected(
                &stale,
                "stale-resume-turn",
                RunTurnOrigin::Resume,
                TurnVisibility::Visible,
            )
            .err()
            .ok_or_else(|| StoreError::InvalidPlan("stale resume unexpectedly won".to_string()))?;
        assert!(error.to_string().contains("identity changed"));
        assert_eq!(store.list_events("r1", 0)?.len(), before_events);
        let after = store
            .get_run_state("r1")?
            .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?;
        assert_eq!(after.run.status, TaskRunStatus::Paused);
        assert!(before.run.attachments.is_empty());
        assert!(after.run.attachments.is_empty());
        assert_eq!(after.continuation, before.continuation);
        assert!(
            after
                .continuation
                .as_ref()
                .is_none_or(|continuation| continuation.active_turn.is_none())
        );
        Ok(())
    }

    #[test]
    fn inactive_cancel_terminalizes_todos_and_run_in_one_frame() -> Result<(), String> {
        let store = fresh().map_err(|error| error.to_string())?;
        seed_plan(&store).map_err(|error| error.to_string())?;
        store
            .set_task_status(
                "r1",
                "t1",
                echo_agent::tasks::TaskStatus::Running,
                Some("subagent"),
                None,
            )
            .map_err(|error| error.to_string())?;
        assert!(
            store
                .request_cancel("r1")
                .map_err(|error| error.to_string())?
        );
        assert_eq!(
            last_frame_event_types(&store, "r1")?,
            ["task_cancelled", "run_status_changed", "run_cancelled"]
        );
        let todo = store
            .list_todos("r1")
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|todo| todo.task_id == "t1")
            .ok_or_else(|| "cancelled Todo missing".to_string())?;
        assert_eq!(todo.status, TodoStatus::Cancelled);
        assert_eq!(todo.owner_agent.as_deref(), Some("code_reviewer"));
        assert_eq!(
            store
                .get_run("r1")
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "cancelled TaskRun missing".to_string())?
                .status,
            TaskRunStatus::Cancelled
        );
        Ok(())
    }

    #[test]
    fn idle_long_horizon_run_accepts_pause_resume_and_cancel() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.configure_run_continuation("r1", true, false, None, None)?;
        for ordinal in 1..=3 {
            let turn_id = format!("turn-{ordinal}");
            assert!(matches!(
                store.claim_run_turn(
                    "r1",
                    &turn_id,
                    RunTurnOrigin::Continuation,
                    TurnVisibility::Internal,
                )?,
                RunTurnClaimOutcome::Started(_)
            ));
            store.finish_run_turn(
                "r1",
                RunTurnCompletion {
                    turn_id: &turn_id,
                    status: RunTurnStatus::Ended,
                    elapsed_seconds: 1,
                    final_message_id: None,
                    error_fingerprint: None,
                },
            )?;
        }
        assert_eq!(
            store
                .get_run_state("r1")?
                .and_then(|state| state.continuation)
                .and_then(|state| state.blocker_audit)
                .map(|audit| audit.consecutive_turns),
            Some(3)
        );

        assert!(store.request_pause("r1")?);
        let paused = store
            .get_run_state("r1")?
            .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?;
        assert_eq!(paused.run.status, TaskRunStatus::Paused);
        assert_eq!(
            paused
                .continuation
                .as_ref()
                .and_then(|state| state.pause.as_ref())
                .map(|pause| pause.reason),
            Some(RunPauseReason::User)
        );

        assert_eq!(store.resume_task_run("r1")?.status, TaskRunStatus::Running);
        assert!(
            store
                .get_run_state("r1")?
                .and_then(|state| state.continuation)
                .and_then(|state| state.blocker_audit)
                .is_none()
        );
        assert!(store.request_cancel("r1")?);
        assert_eq!(
            store
                .get_run("r1")?
                .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?
                .status,
            TaskRunStatus::Cancelled
        );
        Ok(())
    }

    #[test]
    fn blocker_audit_resets_on_progress_and_distinguishes_error_fingerprints()
    -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.configure_run_continuation("r1", true, false, None, None)?;

        assert!(matches!(
            store.claim_run_turn(
                "r1",
                "stalled-before-progress",
                RunTurnOrigin::Continuation,
                TurnVisibility::Internal,
            )?,
            RunTurnClaimOutcome::Started(_)
        ));
        let stalled = store.finish_run_turn(
            "r1",
            RunTurnCompletion {
                turn_id: "stalled-before-progress",
                status: RunTurnStatus::Ended,
                elapsed_seconds: 1,
                final_message_id: None,
                error_fingerprint: None,
            },
        )?;
        assert_eq!(
            stalled
                .blocker_audit
                .as_ref()
                .map(|audit| audit.consecutive_turns),
            Some(1)
        );

        assert!(matches!(
            store.claim_run_turn(
                "r1",
                "progress-turn",
                RunTurnOrigin::Continuation,
                TurnVisibility::Internal,
            )?,
            RunTurnClaimOutcome::Started(_)
        ));
        store.set_task_status(
            "r1",
            "t1",
            echo_agent::tasks::TaskStatus::Running,
            Some("code_reviewer"),
            Some("started review"),
        )?;
        let progressed = store.finish_run_turn(
            "r1",
            RunTurnCompletion {
                turn_id: "progress-turn",
                status: RunTurnStatus::Ended,
                elapsed_seconds: 1,
                final_message_id: None,
                error_fingerprint: None,
            },
        )?;
        assert!(progressed.blocker_audit.is_none());

        for (turn_id, fingerprint, expected) in [
            ("provider-a", "provider_a", 1_u32),
            ("provider-b-1", "provider_b", 1_u32),
            ("provider-b-2", "provider_b", 2_u32),
            ("provider-b-3", "provider_b", 3_u32),
        ] {
            assert!(matches!(
                store.claim_run_turn(
                    "r1",
                    turn_id,
                    RunTurnOrigin::Continuation,
                    TurnVisibility::Internal,
                )?,
                RunTurnClaimOutcome::Started(_)
            ));
            let state = store.finish_run_turn(
                "r1",
                RunTurnCompletion {
                    turn_id,
                    status: RunTurnStatus::Failed,
                    elapsed_seconds: 1,
                    final_message_id: None,
                    error_fingerprint: Some(fingerprint),
                },
            )?;
            let audit = state.blocker_audit.ok_or_else(|| {
                StoreError::InvalidPlan(format!("blocker audit missing for {turn_id}"))
            })?;
            assert_eq!(audit.fingerprint, format!("error:{fingerprint}"));
            assert_eq!(audit.consecutive_turns, expected);
        }
        Ok(())
    }

    #[test]
    fn canonical_runtime_store_retry_keeps_dependency_blockers_derived() -> Result<(), StoreError> {
        let store = fresh()?;
        store.create_run(
            "retry-run",
            "ws",
            "c1",
            "m1",
            DomainProfile::General,
            "retry a failed dependency chain",
            "",
            AttendedMode::Attended,
        )?;
        store.attach_plan_for_test(&TaskPlan {
            plan_id: "retry-plan".to_string(),
            run_id: "retry-run".to_string(),
            revision: 1,
            domain_profile: DomainProfile::General,
            goal_revision: 1,
            goal_sha256: task_goal_sha256("retry a failed dependency chain"),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Sequential,
            tasks: vec![
                PlanTask {
                    id: "upstream".to_string(),
                    agent_role: "implementer".to_string(),
                    max_retries: 2,
                    ..sample_task_body("upstream")
                },
                PlanTask {
                    id: "child".to_string(),
                    agent_role: "reviewer".to_string(),
                    depends_on: vec!["upstream".to_string()],
                    ..sample_task_body("child")
                },
                PlanTask {
                    id: "acceptance-blocked".to_string(),
                    agent_role: "reviewer".to_string(),
                    ..sample_task_body("acceptance-blocked")
                },
            ],
        })?;
        store.transition_run("retry-run", TaskRunStatus::Running)?;
        store.set_task_status(
            "retry-run",
            "upstream",
            echo_agent::tasks::TaskStatus::Failed(String::new()),
            Some("implementer"),
            Some("execution failed"),
        )?;
        store.set_task_status(
            "retry-run",
            "acceptance-blocked",
            echo_agent::tasks::TaskStatus::Blocked(String::new()),
            Some("reviewer"),
            Some("review needs fix; awaiting explicit retry"),
        )?;
        store.transition_run("retry-run", TaskRunStatus::Failed)?;

        let before = store.load_runtime_plan_snapshot("retry-run")?;
        let expected_task = before
            .tasks
            .iter()
            .find(|task| task.spec.id == "upstream")
            .cloned()
            .ok_or_else(|| StoreError::TaskNotFound("upstream".to_string()))?;
        let mut expected = before.clone();
        let expected_outcome =
            echo_agent::tasks::retry_runtime_task(&mut expected, &expected_task, before.revision)?;
        assert_eq!(
            expected_outcome,
            echo_agent::tasks::RuntimeTaskRetryOutcome::Retried { retry_count: 1 }
        );
        assert_eq!(store.retry_blocked_task("retry-run", "upstream")?, 1);
        let stored = store.load_runtime_plan_snapshot("retry-run")?;
        assert_eq!(stored.revision, expected.revision);
        assert_eq!(stored.tasks, expected.tasks);
        let child = store
            .list_todos("retry-run")?
            .into_iter()
            .find(|todo| todo.task_id == "child")
            .ok_or_else(|| StoreError::TaskNotFound("child".to_string()))?;
        assert_eq!(child.status, TodoStatus::Pending);
        let independent = store
            .list_todos("retry-run")?
            .into_iter()
            .find(|todo| todo.task_id == "acceptance-blocked")
            .ok_or_else(|| StoreError::TaskNotFound("acceptance-blocked".to_string()))?;
        assert_eq!(independent.status, TodoStatus::Blocked);
        let upstream = store
            .list_todos("retry-run")?
            .into_iter()
            .find(|todo| todo.task_id == "upstream")
            .ok_or_else(|| StoreError::TaskNotFound("upstream".to_string()))?;
        assert_eq!(upstream.owner_agent.as_deref(), Some("implementer"));
        assert_eq!(
            store
                .get_run("retry-run")?
                .ok_or_else(|| StoreError::RunNotFound("retry-run".to_string()))?
                .status,
            TaskRunStatus::Running
        );
        assert_eq!(
            last_frame_event_types(&store, "retry-run").map_err(StoreError::InvalidPlan)?,
            ["task_status_changed", "note", "run_status_changed"]
        );
        assert!(store.list_events("retry-run", 0)?.iter().all(|event| {
            event
                .payload
                .get("summary")
                .and_then(serde_json::Value::as_str)
                .is_none_or(|summary| !summary.contains("unblocked after retrying upstream"))
        }));
        Ok(())
    }

    #[test]
    fn boot_recovery_pauses_run_and_preserves_completed_tasks() -> Result<(), StoreError> {
        let s = fresh()?;
        seed_plan(&s)?;
        s.set_task_status(
            "r1",
            "t1",
            echo_agent::tasks::TaskStatus::Completed,
            Some("explorer"),
            Some("verified"),
        )?;

        assert_eq!(s.recover_incomplete()?, 1);
        let run = s
            .get_run("r1")?
            .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?;
        assert_eq!(run.status, TaskRunStatus::Paused);
        let todos = s.list_todos("r1")?;
        let task = todos
            .iter()
            .find(|todo| todo.task_id == "t1")
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        assert_eq!(task.status, TodoStatus::Completed);
        assert_eq!(task.summary.as_deref(), Some("verified"));
        Ok(())
    }

    #[test]
    fn boot_recovery_failure_keeps_running_marker_and_is_retryable() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.set_task_status(
            "r1",
            "t1",
            echo_agent::tasks::TaskStatus::Running,
            Some("subagent"),
            None,
        )?;
        let event_count_before = store.list_events("r1", 0)?.len();
        store.fail_next_recovery_commit_for_test();

        assert!(matches!(
            store.recover_incomplete(),
            Err(StoreError::InvalidPlan(message)) if message == "injected recovery commit failure"
        ));
        assert_eq!(store.list_events("r1", 0)?.len(), event_count_before);
        assert_eq!(
            store
                .get_run("r1")?
                .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?
                .status,
            TaskRunStatus::Running
        );
        assert_eq!(
            store
                .list_todos("r1")?
                .into_iter()
                .find(|todo| todo.task_id == "t1")
                .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?
                .status,
            TodoStatus::Running
        );

        assert_eq!(store.recover_incomplete()?, 1);
        assert_eq!(
            store
                .get_run("r1")?
                .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?
                .status,
            TaskRunStatus::Paused
        );
        Ok(())
    }

    #[test]
    fn boot_recovery_repairs_projection_after_atomic_event_commit() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.set_task_status(
            "r1",
            "t1",
            echo_agent::tasks::TaskStatus::Running,
            Some("subagent"),
            None,
        )?;
        store.fail_next_recovery_projection_for_test();

        assert_eq!(store.recover_incomplete()?, 1);
        let stale_projection = store
            .shadow
            .read_run_state("r1")
            .map_err(|error| StoreError::InvalidPlan(error.to_string()))?
            .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?;
        assert_eq!(stale_projection.run.status, TaskRunStatus::Paused);
        assert_eq!(
            store
                .get_run("r1")?
                .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?
                .status,
            TaskRunStatus::Paused
        );
        let authoritative = store
            .get_run_state("r1")?
            .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?;
        assert_eq!(authoritative.run.status, TaskRunStatus::Paused);
        assert_eq!(
            authoritative
                .tasks
                .iter()
                .find(|task| task.task_id == "t1")
                .map(|task| TodoStatus::project_task_status(&task.status)),
            Some(TodoStatus::Pending)
        );
        assert_eq!(boot_recovery_event_count(&store)?, 1);

        assert_eq!(store.recover_incomplete()?, 0);
        assert_eq!(
            store
                .get_run("r1")?
                .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?
                .status,
            TaskRunStatus::Paused
        );
        let todo = store
            .list_todos("r1")?
            .into_iter()
            .find(|todo| todo.task_id == "t1")
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        assert_eq!(todo.status, TodoStatus::Pending);
        assert_eq!(todo.summary.as_deref(), Some("interrupted; pending resume"));
        assert_eq!(boot_recovery_event_count(&store)?, 1);
        Ok(())
    }

    #[test]
    fn boot_recovery_closes_orphan_turn_and_records_pause_reason() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.configure_run_continuation("r1", true, false, None, None)?;
        assert!(matches!(
            store.claim_run_turn(
                "r1",
                "turn-before-restart",
                RunTurnOrigin::User,
                TurnVisibility::Visible,
            )?,
            RunTurnClaimOutcome::Started(_)
        ));
        store.account_run_turn_usage("r1", "turn-before-restart", "usage-1", 40, 2)?;

        assert_eq!(store.recover_incomplete()?, 1);
        let state = store
            .get_run_state("r1")?
            .and_then(|state| state.continuation)
            .ok_or_else(|| StoreError::InvalidPlan("continuation missing".to_string()))?;
        assert!(state.active_turn.is_none());
        assert_eq!(state.tokens_used, 42);
        assert_eq!(
            state.last_turn.as_ref().map(|turn| turn.status),
            Some(RunTurnStatus::Failed)
        );
        assert_eq!(
            state
                .last_turn
                .as_ref()
                .and_then(|turn| turn.error_fingerprint.as_deref()),
            Some("process_interrupted")
        );
        assert_eq!(
            state.pause.as_ref().map(|pause| pause.reason),
            Some(RunPauseReason::BootRecovery)
        );
        assert_eq!(
            store
                .get_run("r1")?
                .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?
                .status,
            TaskRunStatus::Paused
        );
        Ok(())
    }

    #[test]
    fn boot_recovery_closes_orphan_cell_without_replaying_it() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.record_background_cell_started(
            "r1",
            "orphan-cell",
            "cargo test --workspace",
            "command-hash",
            Some("turn-before-restart"),
            None,
            Some("call-before-restart"),
        )?;

        assert_eq!(store.recover_incomplete()?, 1);
        let cells = store.list_background_cells("r1")?;
        let cell = cells
            .iter()
            .find(|cell| cell.cell_id == "orphan-cell")
            .ok_or_else(|| StoreError::InvalidPlan("orphan cell was not rebuilt".to_string()))?;
        assert_eq!(cell.phase, BackgroundCellPhase::Failed);
        assert_eq!(
            cell.terminal_cause,
            Some(BackgroundCellTerminalCause::Interrupted)
        );
        assert!(!cell.is_active());
        let recovered_cell_count = store
            .list_events("r1", 0)?
            .iter()
            .filter(|event| {
                boot_recovery_payload(event)
                    .and_then(|recovery| recovery.get("cells"))
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|cells| {
                        cells.iter().any(|cell| {
                            json_string(cell, "cell_id").as_deref() == Some("orphan-cell")
                        })
                    })
            })
            .count();
        assert_eq!(recovered_cell_count, 1);
        assert_eq!(store.recover_incomplete()?, 0);
        Ok(())
    }

    #[test]
    fn boot_recovery_closes_active_cell_owned_by_already_paused_run_once() -> Result<(), StoreError>
    {
        let store = fresh()?;
        seed_plan(&store)?;
        store.record_background_cell_started(
            "r1",
            "paused-orphan-cell",
            "sleep 30",
            "hash",
            Some("turn-before-pause"),
            None,
            Some("call-before-pause"),
        )?;
        store.transition_run("r1", TaskRunStatus::Paused)?;

        assert_eq!(store.recover_incomplete()?, 1);
        let cells = store.list_background_cells("r1")?;
        let cell = cells
            .iter()
            .find(|cell| cell.cell_id == "paused-orphan-cell")
            .ok_or_else(|| StoreError::InvalidPlan("paused orphan cell missing".to_string()))?;
        assert_eq!(cell.phase, BackgroundCellPhase::Failed);
        assert_eq!(
            cell.terminal_cause,
            Some(BackgroundCellTerminalCause::Interrupted)
        );
        assert_eq!(store.recover_incomplete()?, 0);
        assert_eq!(
            store
                .list_events("r1", 0)?
                .into_iter()
                .filter(|event| {
                    event.event_type == RuntimeEventKind::BackgroundCellFinished
                        && json_string(&event.payload, "cell_id").as_deref()
                            == Some("paused-orphan-cell")
                })
                .count(),
            1
        );
        Ok(())
    }

    #[test]
    fn pause_request_stops_driver_and_keeps_run_resumable() -> Result<(), StoreError> {
        let store = std::sync::Arc::new(fresh()?);
        seed_plan(&store)?;
        store.set_task_status(
            "r1",
            "t1",
            echo_agent::tasks::TaskStatus::Running,
            Some("subagent"),
            None,
        )?;
        let token = echo_agent::agent::CancellationToken::new();
        let registration = store.register_run_cancellation("r1", token.clone())?;

        assert!(store.request_pause("r1")?);
        assert!(token.is_cancelled());
        drop(registration);
        let run = store
            .get_run("r1")?
            .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?;
        assert_eq!(run.status, TaskRunStatus::Paused);
        Ok(())
    }

    #[test]
    fn active_cancel_durably_overrides_a_prior_pause_intent() -> Result<(), StoreError> {
        let store = std::sync::Arc::new(fresh()?);
        seed_plan(&store)?;
        let snapshot = store.load_runtime_plan_snapshot("r1")?;
        let task = snapshot
            .tasks
            .first()
            .cloned()
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        let _claim = match store.claim_runtime_task("r1", &task, snapshot.revision)? {
            echo_agent::tasks::RuntimeTaskClaimOutcome::Claimed(claim) => claim,
            echo_agent::tasks::RuntimeTaskClaimOutcome::ReloadSnapshot => {
                return Err(StoreError::InvalidPlan(
                    "claim unexpectedly reloaded".to_string(),
                ));
            }
        };
        let token = echo_agent::agent::CancellationToken::new();
        let registration = store.register_run_cancellation("r1", token.clone())?;

        assert!(store.request_pause("r1")?);
        assert_eq!(
            store
                .get_run("r1")?
                .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?
                .status,
            TaskRunStatus::Paused
        );
        assert!(store.request_cancel("r1")?);
        assert_eq!(
            store
                .get_run("r1")?
                .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?
                .status,
            TaskRunStatus::Cancelled
        );
        assert!(
            store
                .load_runtime_plan_snapshot("r1")?
                .tasks
                .iter()
                .all(|task| task.execution.status.is_terminal())
        );
        drop(registration);
        Ok(())
    }

    #[test]
    fn cancelled_registration_drop_only_releases_in_memory_ownership() -> Result<(), StoreError> {
        let store = std::sync::Arc::new(fresh()?);
        store.create_run(
            "cancelled-driver",
            "ws",
            "conversation",
            "message",
            DomainProfile::General,
            "cancel interrupted driver",
            "",
            AttendedMode::Unattended,
        )?;
        store.transition_run("cancelled-driver", TaskRunStatus::Running)?;
        let token = echo_agent::agent::CancellationToken::new();
        let registration = store.register_run_cancellation("cancelled-driver", token.clone())?;

        token.cancel();
        drop(registration);

        let run = store
            .get_run("cancelled-driver")?
            .ok_or_else(|| StoreError::RunNotFound("cancelled-driver".to_string()))?;
        assert_eq!(run.status, TaskRunStatus::Running);
        assert!(!store.is_run_active("cancelled-driver"));
        Ok(())
    }

    #[test]
    fn cancel_request_retains_driver_ownership_until_registration_drop() -> Result<(), StoreError> {
        let store = std::sync::Arc::new(fresh()?);
        store.create_run(
            "cancel-request-driver",
            "ws",
            "conversation",
            "message",
            DomainProfile::General,
            "cancel active driver without releasing its registry slot",
            "",
            AttendedMode::Unattended,
        )?;
        store.transition_run("cancel-request-driver", TaskRunStatus::Running)?;
        let token = echo_agent::agent::CancellationToken::new();
        let registration =
            store.register_run_cancellation("cancel-request-driver", token.clone())?;

        assert!(store.request_cancel("cancel-request-driver")?);
        assert!(token.is_cancelled());
        assert!(store.is_run_active("cancel-request-driver"));
        assert_eq!(
            store
                .get_run("cancel-request-driver")?
                .ok_or_else(|| StoreError::RunNotFound("cancel-request-driver".to_string()))?
                .status,
            TaskRunStatus::Cancelled
        );

        drop(registration);
        assert!(!store.is_run_active("cancel-request-driver"));
        assert_eq!(
            store
                .get_run("cancel-request-driver")?
                .ok_or_else(|| StoreError::RunNotFound("cancel-request-driver".to_string()))?
                .status,
            TaskRunStatus::Cancelled
        );
        Ok(())
    }

    #[test]
    fn cancelled_nested_registration_restores_outer_driver() -> Result<(), StoreError> {
        let store = std::sync::Arc::new(fresh()?);
        store.create_run(
            "nested-cancelled-driver",
            "ws",
            "conversation",
            "message",
            DomainProfile::General,
            "cancel nested driver",
            "",
            AttendedMode::Unattended,
        )?;
        store.transition_run("nested-cancelled-driver", TaskRunStatus::Running)?;
        let outer_token = echo_agent::agent::CancellationToken::new();
        let outer_registration =
            store.register_run_cancellation("nested-cancelled-driver", outer_token.clone())?;
        let inner_token = outer_token.child_token();
        let inner_registration =
            store.register_run_cancellation("nested-cancelled-driver", inner_token.clone())?;

        inner_token.cancel();
        drop(inner_registration);

        let run = store
            .get_run("nested-cancelled-driver")?
            .ok_or_else(|| StoreError::RunNotFound("nested-cancelled-driver".to_string()))?;
        assert_eq!(run.status, TaskRunStatus::Running);
        assert!(store.is_run_active("nested-cancelled-driver"));
        assert!(!outer_token.is_cancelled());

        drop(outer_registration);
        assert!(!store.is_run_active("nested-cancelled-driver"));
        Ok(())
    }

    #[test]
    fn boot_recovery_requeues_orphaned_running_task() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.set_task_status(
            "r1",
            "t1",
            echo_agent::tasks::TaskStatus::Running,
            Some("subagent"),
            None,
        )?;

        assert_eq!(store.recover_incomplete()?, 1);
        let todo = store
            .list_todos("r1")?
            .into_iter()
            .find(|todo| todo.task_id == "t1")
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        assert_eq!(todo.status, TodoStatus::Pending);
        assert_eq!(todo.summary.as_deref(), Some("interrupted; pending resume"));
        Ok(())
    }

    #[test]
    fn boot_recovery_terminalizes_replay_safe_orphan_subagent_without_blocker()
    -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        let task = store
            .get_plan("r1")?
            .and_then(|plan| plan.tasks.into_iter().next())
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        let runtime_task = task.to_task().map_err(StoreError::InvalidPlan)?;
        let claim = match store.claim_runtime_task("r1", &runtime_task, 1)? {
            echo_agent::tasks::RuntimeTaskClaimOutcome::Claimed(claim) => claim,
            echo_agent::tasks::RuntimeTaskClaimOutcome::ReloadSnapshot => {
                return Err(StoreError::InvalidPlan(
                    "fresh task claim unexpectedly required reload".to_string(),
                ));
            }
        };
        let execution_id = claim.execution_id("r1", "t1");
        store.record_subagent_assigned(
            "r1",
            "t1",
            &execution_id,
            "subagent",
            "Task 1",
            claim.revision,
            claim.attempt,
            true,
            true,
        )?;

        assert_eq!(store.recover_incomplete()?, 1);
        assert!(store.active_subagent_boundaries("r1")?.is_empty());
        assert!(store.list_recovery_blockers("r1")?.is_empty());
        let subagent = store
            .list_subagent_runs("r1")?
            .into_iter()
            .find(|run| run.subagent_run_id == execution_id)
            .ok_or_else(|| StoreError::InvalidPlan("orphan Subagent missing".to_string()))?;
        assert_eq!(subagent.status, SubagentRunStatus::Failed);
        let recovery = store
            .list_events("r1", 0)?
            .into_iter()
            .find_map(|event| boot_recovery_payload(&event).cloned())
            .ok_or_else(|| StoreError::InvalidPlan("recovery event missing".to_string()))?;
        assert_eq!(
            recovery
                .get("subagents")
                .and_then(serde_json::Value::as_array)
                .and_then(|subagents| {
                    subagents.iter().find(|subagent| {
                        json_string(subagent, "execution_id").as_deref()
                            == Some(execution_id.as_str())
                    })
                })
                .and_then(|subagent| subagent.get("terminal_cause"))
                .and_then(serde_json::Value::as_str),
            Some("process_interrupted")
        );
        Ok(())
    }

    #[test]
    fn boot_recovery_reuses_completed_subagent_without_redispatch() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        let task = store
            .get_plan("r1")?
            .ok_or_else(|| StoreError::PlanNotFound("r1".to_string()))?
            .tasks
            .first()
            .cloned()
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        let runtime_task = task.to_task().map_err(StoreError::InvalidPlan)?;
        let claim = match store.claim_runtime_task("r1", &runtime_task, 1)? {
            echo_agent::tasks::RuntimeTaskClaimOutcome::Claimed(claim) => claim,
            echo_agent::tasks::RuntimeTaskClaimOutcome::ReloadSnapshot => {
                return Err(StoreError::InvalidPlan(
                    "fresh task claim unexpectedly required reload".to_string(),
                ));
            }
        };
        let execution_id = claim.execution_id("r1", "t1");
        store.record_subagent_assigned(
            "r1",
            "t1",
            &execution_id,
            "subagent",
            "Task 1",
            claim.revision,
            claim.attempt,
            true,
            true,
        )?;
        let result = SubagentTaskResult::terminal(
            SubagentRunStatus::Completed,
            "durable result",
            Vec::new(),
        );
        store.record_subagent_released(SubagentReleaseRecord {
            run_id: "r1",
            task_id: "t1",
            execution_id: &execution_id,
            agent_name: "subagent",
            task_subject: "Task 1",
            plan_revision: claim.revision,
            attempt: claim.attempt,
            status: "completed",
            result: Some(&result),
            full_output: Some("durable full output"),
            usage: None,
            dispatch_hook: true,
        })?;

        assert_eq!(store.recover_incomplete()?, 1);
        assert_eq!(
            store.recoverable_subagent_result_for_attempt(
                "r1",
                "t1",
                &execution_id,
                claim.revision,
                claim.attempt,
            )?,
            Some(RecoverableSubagentResult {
                result,
                full_output: "durable full output".to_string(),
            })
        );
        let todo = store
            .list_todos("r1")?
            .into_iter()
            .find(|todo| todo.task_id == "t1")
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        assert_eq!(todo.status, TodoStatus::Pending);
        assert_eq!(
            todo.summary.as_deref(),
            Some("Subagent completed before interruption; pending review")
        );
        assert!(store.list_recovery_blockers("r1")?.is_empty());
        Ok(())
    }

    #[test]
    fn mutating_in_doubt_subagent_blocks_resume_until_user_decides() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.apply_task_patch_for_test(
            "r1",
            &TaskUpdateRequest {
                base_revision: 1,
                reason: "exercise mutating recovery".to_string(),
                operations: vec![TaskUpdateOperation::Update {
                    task_id: "t1".to_string(),
                    patch: TaskPatch {
                        kind: Some(PlanTaskKind::Implementation),
                        ..Default::default()
                    },
                }],
            },
        )?;
        store.set_task_status(
            "r1",
            "t1",
            echo_agent::tasks::TaskStatus::Running,
            Some("subagent"),
            None,
        )?;
        store.record_subagent_assigned(
            "r1", "t1", "t1:1", "subagent", "Task 1", 1, 1, false, true,
        )?;
        store.record_tool_started("r1", "t1", "t1:1", "call-write", "write_file", false)?;

        assert_eq!(store.recover_incomplete()?, 1);
        assert!(store.active_subagent_boundaries("r1")?.is_empty());
        let blockers = store.list_recovery_blockers("r1")?;
        assert_eq!(blockers.len(), 1);
        assert_eq!(
            blockers.first().and_then(|b| b.call_id.as_deref()),
            Some("call-write")
        );
        assert!(matches!(
            store.resume_task_run("r1"),
            Err(StoreError::RecoveryBlocked { .. })
        ));

        store.resolve_recovery_task("r1", "t1", RecoveryDecision::Retry)?;
        assert_eq!(
            last_frame_event_types(&store, "r1").map_err(StoreError::InvalidPlan)?,
            ["recovery_resolved", "task_status_changed"]
        );
        assert!(store.list_recovery_blockers("r1")?.is_empty());
        let todo = store
            .list_todos("r1")?
            .into_iter()
            .find(|todo| todo.task_id == "t1")
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        assert_eq!(todo.status, TodoStatus::Pending);
        assert_eq!(store.resume_task_run("r1")?.status, TaskRunStatus::Running);
        Ok(())
    }

    #[test]
    fn canonical_recovery_skip_commits_revision_and_resolution_in_one_batch()
    -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.apply_task_patch_for_test(
            "r1",
            &TaskUpdateRequest {
                base_revision: 1,
                reason: "exercise canonical recovery skip".to_string(),
                operations: vec![TaskUpdateOperation::Update {
                    task_id: "t1".to_string(),
                    patch: TaskPatch {
                        kind: Some(PlanTaskKind::Implementation),
                        ..Default::default()
                    },
                }],
            },
        )?;
        store.set_task_status(
            "r1",
            "t1",
            echo_agent::tasks::TaskStatus::Running,
            Some("subagent"),
            None,
        )?;
        store.record_subagent_assigned(
            "r1", "t1", "t1:1", "subagent", "Task 1", 2, 1, false, true,
        )?;
        store.record_tool_started("r1", "t1", "t1:1", "call-write", "write_file", false)?;
        assert_eq!(store.recover_incomplete()?, 1);
        let before = store.load_runtime_plan_snapshot("r1")?;
        let expected_revision = before
            .revision
            .checked_add(1)
            .ok_or_else(|| StoreError::InvalidPlan("test plan revision overflow".to_string()))?;

        store.resolve_recovery_task("r1", "t1", RecoveryDecision::Skip)?;

        let after = store.load_runtime_plan_snapshot("r1")?;
        assert_eq!(after.revision, expected_revision);
        let skipped = after
            .tasks
            .iter()
            .find(|task| task.spec.id == "t1")
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        assert_eq!(
            skipped.execution.status,
            echo_agent::tasks::TaskStatus::Skipped
        );
        assert!(skipped.execution.claim.is_none());
        assert!(store.list_recovery_blockers("r1")?.is_empty());
        assert_eq!(
            last_frame_event_types(&store, "r1").map_err(StoreError::InvalidPlan)?,
            [
                "plan_revision_committed",
                "recovery_resolved",
                "task_skipped",
            ]
        );
        Ok(())
    }

    #[test]
    fn tool_failure_boundary_persists_recovery_contract() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        let failure = echo_agent::tools::ToolFailure::new(
            echo_agent::tools::ToolFailureCategory::PartialSideEffect,
        )
        .with_postcondition("verify target hash");

        store.record_tool_started("r1", "t1", "t1:1", "call-1", "write_file", false)?;
        store.record_tool_finished(
            "r1",
            "t1",
            "t1:1",
            "call-1",
            "write_file",
            false,
            "write interrupted",
            Some(&failure),
        )?;

        let event = store
            .list_events("r1", 0)?
            .into_iter()
            .find(|event| event.event_type == RuntimeEventKind::ToolFailed)
            .ok_or_else(|| StoreError::TaskNotFound("tool failure event".to_string()))?;
        assert_eq!(
            event
                .payload
                .get("failure")
                .and_then(|failure| failure.get("category"))
                .and_then(serde_json::Value::as_str),
            Some("partial_side_effect")
        );
        assert_eq!(
            event
                .payload
                .get("failure")
                .and_then(|failure| failure.get("postcondition"))
                .and_then(serde_json::Value::as_str),
            Some("verify target hash")
        );
        Ok(())
    }

    #[test]
    fn find_in_progress_run_by_conversation_returns_running() -> Result<(), StoreError> {
        let s = fresh()?;
        seed_plan(&s)?; // run "r1" in conversation "c1" is now Running.
        let found = s
            .find_in_progress_run_by_conversation("c1")?
            .ok_or_else(|| StoreError::RunNotFound("in-progress run for c1".to_string()))?;
        assert_eq!(found.run_id, "r1");
        Ok(())
    }

    #[test]
    fn find_in_progress_run_by_conversation_returns_paused() -> Result<(), StoreError> {
        let s = fresh()?;
        seed_plan(&s)?;
        s.transition_run("r1", TaskRunStatus::Paused)?;
        let found = s
            .find_in_progress_run_by_conversation("c1")?
            .ok_or_else(|| StoreError::RunNotFound("paused run for c1".to_string()))?;
        assert_eq!(found.run_id, "r1");
        Ok(())
    }

    #[test]
    fn find_in_progress_run_by_conversation_returns_none_for_completed() -> Result<(), StoreError> {
        let s = fresh()?;
        seed_plan(&s)?;
        s.transition_run("r1", TaskRunStatus::Completed)?;
        let found = s.find_in_progress_run_by_conversation("c1")?;
        assert!(found.is_none());
        Ok(())
    }

    #[test]
    fn task_update_inserts_task_and_commits_one_revision() -> Result<(), StoreError> {
        let s = fresh()?;
        seed_plan(&s)?;
        let t2 = PlanTask {
            id: "t2".into(),
            title: "Second task".into(),
            description: "implement the second task".into(),
            kind: PlanTaskKind::Implementation,
            agent_role: "implementer".into(),
            depends_on: vec!["t1".into()],
            ..Default::default()
        };
        let before = s.list_events("r1", 0)?.len();
        let plan = s.apply_task_patch_for_test(
            "r1",
            &TaskUpdateRequest {
                base_revision: 1,
                reason: "new implementation dependency".to_string(),
                operations: vec![TaskUpdateOperation::Insert {
                    after_task_id: Some("t1".to_string()),
                    task: t2.spec(),
                }],
            },
        )?;

        assert_eq!(plan.revision, 2);
        assert_eq!(plan.tasks.len(), 2);
        assert_eq!(plan.tasks[0].id, "t1");
        assert_eq!(plan.tasks[1].id, "t2");
        let evs = s.list_events("r1", 0)?;
        assert_eq!(evs.len(), before + 1);
        assert_eq!(
            evs.last().map(|event| event.event_type),
            Some(RuntimeEventKind::PlanRevisionCommitted)
        );
        Ok(())
    }

    #[test]
    fn reorder_keeps_plan_todos_and_framework_graph_in_one_order() -> Result<(), StoreError> {
        let store = TaskRuntimeStore::new_in_memory()
            .map_err(|error| StoreError::InvalidPlan(error.to_string()))?;
        store.create_run(
            "reorder-run",
            "ws",
            "c-reorder",
            "m-reorder",
            DomainProfile::General,
            "reorder",
            "",
            AttendedMode::Attended,
        )?;
        let plan = TaskPlan {
            plan_id: "reorder-plan".to_string(),
            run_id: "reorder-run".to_string(),
            revision: 1,
            domain_profile: DomainProfile::General,
            goal_revision: 1,
            goal_sha256: task_goal_sha256("reorder"),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Sequential,
            tasks: vec![
                PlanTask {
                    id: "first".to_string(),
                    title: "First".to_string(),
                    sort_order: 0,
                    ..PlanTask::default()
                },
                PlanTask {
                    id: "second".to_string(),
                    title: "Second".to_string(),
                    sort_order: 1,
                    ..PlanTask::default()
                },
            ],
        };
        store.attach_plan_for_test(&plan)?;
        store.transition_run("reorder-run", TaskRunStatus::Running)?;

        let reordered = store.apply_task_patch_for_test(
            "reorder-run",
            &TaskUpdateRequest {
                base_revision: 1,
                reason: "surface parity reorder".to_string(),
                operations: vec![TaskUpdateOperation::Reorder {
                    task_ids: vec!["second".to_string(), "first".to_string()],
                }],
            },
        )?;
        assert_eq!(
            reordered
                .tasks
                .iter()
                .map(|task| task.id.as_str())
                .collect::<Vec<_>>(),
            vec!["second", "first"]
        );
        assert_eq!(
            reordered
                .tasks
                .iter()
                .map(|task| task.sort_order)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );

        let todos = store.list_todos("reorder-run")?;
        assert_eq!(
            todos
                .iter()
                .map(|todo| todo.task_id.as_str())
                .collect::<Vec<_>>(),
            vec!["second", "first"]
        );
        let graph = store
            .load_revisioned_task_graph("reorder-run")?
            .ok_or_else(|| StoreError::PlanNotFound("reorder-run".to_string()))?;
        assert_eq!(
            graph
                .snapshot
                .tasks
                .iter()
                .map(|task| task.spec.id.as_str())
                .collect::<Vec<_>>(),
            vec!["second", "first"]
        );
        Ok(())
    }

    #[test]
    fn task_update_rejects_missing_run() -> std::result::Result<(), String> {
        let s = TaskRuntimeStore::new_in_memory().map_err(|e| e.to_string())?;
        let err = s
            .apply_task_patch_for_test(
                "missing-run",
                &TaskUpdateRequest {
                    base_revision: 1,
                    reason: "invalid".to_string(),
                    operations: vec![TaskUpdateOperation::Reorder {
                        task_ids: Vec::new(),
                    }],
                },
            )
            .err()
            .ok_or_else(|| "task_update unexpectedly succeeded without a run".to_string())?;
        assert!(matches!(err, StoreError::RunNotFound(run_id) if run_id == "missing-run"));
        Ok(())
    }

    #[test]
    fn task_update_rejects_stale_revision_without_appending_event() -> Result<(), StoreError> {
        let s = fresh()?;
        seed_plan(&s)?;
        let before = s.list_events("r1", 0)?.len();
        let error = s
            .apply_task_patch_for_test(
                "r1",
                &TaskUpdateRequest {
                    base_revision: 0,
                    reason: "stale edit".to_string(),
                    operations: vec![TaskUpdateOperation::Skip {
                        task_id: "t1".to_string(),
                    }],
                },
            )
            .err()
            .ok_or_else(|| {
                StoreError::InvalidPlan("stale update unexpectedly succeeded".to_string())
            })?;
        assert!(matches!(error, StoreError::PlanConflict { .. }));
        assert_eq!(s.list_events("r1", 0)?.len(), before);
        Ok(())
    }

    #[test]
    fn canonical_runtime_store_claim_and_settlement_match_framework_fields()
    -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        let before = store.load_runtime_plan_snapshot("r1")?;
        let expected_task = before
            .tasks
            .first()
            .cloned()
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        let mut framework_claimed = before.clone();
        let framework_claim = match echo_agent::tasks::claim_runtime_task(
            &mut framework_claimed,
            &expected_task,
            before.revision,
        )? {
            echo_agent::tasks::RuntimeTaskClaimOutcome::Claimed(claim) => claim,
            echo_agent::tasks::RuntimeTaskClaimOutcome::ReloadSnapshot => {
                return Err(StoreError::InvalidPlan(
                    "framework unexpectedly rejected a fresh claim".to_string(),
                ));
            }
        };
        store.fail_next_runtime_mutation_projection_for_test();
        let stored_claim = match store.claim_runtime_task("r1", &expected_task, before.revision)? {
            echo_agent::tasks::RuntimeTaskClaimOutcome::Claimed(claim) => claim,
            echo_agent::tasks::RuntimeTaskClaimOutcome::ReloadSnapshot => {
                return Err(StoreError::InvalidPlan(
                    "store unexpectedly rejected a fresh claim".to_string(),
                ));
            }
        };
        assert_eq!(stored_claim.revision, framework_claim.revision);
        assert_eq!(stored_claim.attempt, framework_claim.attempt);
        assert_eq!(stored_claim.spec_hash, framework_claim.spec_hash);
        let framework_task = framework_claimed
            .tasks
            .iter_mut()
            .find(|task| task.spec.id == "t1")
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        framework_task.execution.claim = Some(stored_claim.clone());
        let stored_claimed = store.load_runtime_plan_snapshot("r1")?;
        assert_eq!(stored_claimed.revision, framework_claimed.revision);
        assert_eq!(stored_claimed.tasks, framework_claimed.tasks);

        let claim_event = store
            .list_events("r1", 0)?
            .into_iter()
            .last()
            .ok_or_else(|| StoreError::InvalidPlan("claim event missing".to_string()))?;
        for field in [
            "status",
            "status_detail",
            "claim",
            "retry_count",
            "failure_fingerprint",
            "owner_agent",
            "summary",
        ] {
            assert!(claim_event.payload.get(field).is_some(), "missing {field}");
        }

        let mut framework_settled = framework_claimed;
        assert_eq!(
            echo_agent::tasks::settle_runtime_claim(
                &mut framework_settled,
                "t1",
                &stored_claim,
                echo_agent::tasks::TaskStatus::Completed,
            )?,
            echo_agent::tasks::RuntimeTaskSettlementOutcome::Settled
        );
        store.fail_next_runtime_mutation_projection_for_test();
        assert_eq!(
            store.settle_runtime_task_claim(
                "r1",
                "t1",
                &stored_claim,
                echo_agent::tasks::TaskStatus::Completed,
                Some("reviewed completion".to_string()),
            )?,
            echo_agent::tasks::RuntimeTaskSettlementOutcome::Settled
        );
        let stored = store.load_runtime_plan_snapshot("r1")?;
        assert_eq!(stored.revision, framework_settled.revision);
        assert_eq!(stored.tasks, framework_settled.tasks);
        let terminal = stored
            .tasks
            .iter()
            .find(|task| task.spec.id == "t1")
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        assert!(terminal.execution.claim.is_none());
        assert_eq!(
            terminal.execution.status,
            echo_agent::tasks::TaskStatus::Completed
        );
        Ok(())
    }

    #[test]
    fn canonical_runtime_store_requeue_matches_framework_and_rejects_aba_claim()
    -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        let initial = store.load_runtime_plan_snapshot("r1")?;
        let expected_task = initial
            .tasks
            .first()
            .cloned()
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        let first_claim = match store.claim_runtime_task("r1", &expected_task, initial.revision)? {
            echo_agent::tasks::RuntimeTaskClaimOutcome::Claimed(claim) => claim,
            echo_agent::tasks::RuntimeTaskClaimOutcome::ReloadSnapshot => {
                return Err(StoreError::InvalidPlan(
                    "fresh requeue claim unexpectedly required reload".to_string(),
                ));
            }
        };
        let claimed = store.load_runtime_plan_snapshot("r1")?;
        let request = echo_agent::tasks::RuntimeTaskResolutionRequest::Requeue {
            failure_fingerprint: Some("compile-fingerprint".to_string()),
            error: "cargo check failed".to_string(),
            exhaustion: echo_agent::tasks::RuntimeRetryExhaustion::Failed,
        };
        let mut framework_requeued = claimed.clone();
        let framework_resolution = echo_agent::tasks::settle_runtime_resolution(
            &mut framework_requeued,
            "t1",
            &first_claim,
            request.clone(),
        )?;
        store.fail_next_runtime_mutation_projection_for_test();
        assert_eq!(
            store.settle_runtime_task_resolution(
                "r1",
                "t1",
                &first_claim,
                request,
                RuntimeTaskProductSettlement {
                    summary: Some("retry after cargo check".to_string()),
                    ..RuntimeTaskProductSettlement::default()
                },
            )?,
            framework_resolution
        );
        let stored_requeued = store.load_runtime_plan_snapshot("r1")?;
        assert_eq!(stored_requeued.revision, framework_requeued.revision);
        assert_eq!(stored_requeued.tasks, framework_requeued.tasks);
        let requeued = framework_requeued
            .tasks
            .iter()
            .find(|task| task.spec.id == "t1")
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        assert_eq!(
            requeued.execution.status,
            echo_agent::tasks::TaskStatus::Pending
        );
        assert_eq!(requeued.execution.retry_count, 1);
        assert_eq!(
            requeued.execution.failure_fingerprint.as_deref(),
            Some("compile-fingerprint")
        );
        assert!(requeued.execution.claim.is_none());

        let second_claim = match store.claim_runtime_task("r1", requeued, initial.revision)? {
            echo_agent::tasks::RuntimeTaskClaimOutcome::Claimed(claim) => claim,
            echo_agent::tasks::RuntimeTaskClaimOutcome::ReloadSnapshot => {
                return Err(StoreError::InvalidPlan(
                    "requeued task unexpectedly required reload".to_string(),
                ));
            }
        };
        assert_ne!(first_claim.claim_id, second_claim.claim_id);
        let completed_summary = TaskExecutionSummary {
            run_id: "r1".to_string(),
            task_id: "t1".to_string(),
            subagent_name: "subagent".to_string(),
            result: SubagentTaskResult::terminal(
                SubagentRunStatus::Completed,
                "new physical claim result",
                Vec::new(),
            ),
            decisions: Vec::new(),
            next_implications: Vec::new(),
            suggested_tasks: Vec::new(),
            created_at: Utc::now(),
        };
        let current_review = ReviewResult {
            id: "review-new-claim".to_string(),
            run_id: "r1".to_string(),
            task_id: "t1".to_string(),
            reviewer_agent: "reviewer".to_string(),
            outcome: ReviewOutcome::Pass,
            issues: Vec::new(),
            failure_fingerprint: None,
            created_fix_task_id: None,
            created_at: Utc::now(),
        };
        assert_eq!(
            store.settle_runtime_task_resolution(
                "r1",
                "t1",
                &second_claim,
                echo_agent::tasks::RuntimeTaskResolutionRequest::Completed,
                RuntimeTaskProductSettlement {
                    summary: Some("new physical claim result".to_string()),
                    execution_summary: Some(completed_summary.clone()),
                    review: Some(current_review),
                    diagnostic_note: Some("accepted new claim review".to_string()),
                    typed_terminal: None,
                },
            )?,
            echo_agent::tasks::RuntimeTaskResolution::Completed
        );
        let stale_summary = TaskExecutionSummary {
            result: SubagentTaskResult::terminal(
                SubagentRunStatus::Completed,
                "stale physical claim result",
                Vec::new(),
            ),
            ..completed_summary
        };
        let stale_review = ReviewResult {
            id: "review-stale-claim".to_string(),
            run_id: "r1".to_string(),
            task_id: "t1".to_string(),
            reviewer_agent: "reviewer".to_string(),
            outcome: ReviewOutcome::NeedsFix,
            issues: Vec::new(),
            failure_fingerprint: Some("stale-fingerprint".to_string()),
            created_fix_task_id: None,
            created_at: Utc::now(),
        };
        assert_eq!(
            store.settle_runtime_task_resolution(
                "r1",
                "t1",
                &first_claim,
                echo_agent::tasks::RuntimeTaskResolutionRequest::Completed,
                RuntimeTaskProductSettlement {
                    summary: Some("stale physical claim result".to_string()),
                    execution_summary: Some(stale_summary),
                    review: Some(stale_review),
                    diagnostic_note: Some("stale claim review note".to_string()),
                    typed_terminal: None,
                },
            )?,
            echo_agent::tasks::RuntimeTaskResolution::Superseded
        );
        assert_eq!(
            store
                .get_summary("r1", "t1")?
                .ok_or_else(|| StoreError::TaskNotFound("t1 summary".to_string()))?
                .result
                .summary,
            "new physical claim result"
        );
        let reviews = store.list_reviews("r1", "t1")?;
        assert_eq!(reviews.len(), 1);
        assert_eq!(
            reviews.first().map(|review| review.id.as_str()),
            Some("review-new-claim")
        );
        let notes = store.list_events("r1", 0)?;
        assert!(notes.iter().any(|event| {
            event
                .payload
                .get("message")
                .and_then(serde_json::Value::as_str)
                == Some("accepted new claim review")
        }));
        assert!(!notes.iter().any(|event| {
            event
                .payload
                .get("message")
                .and_then(serde_json::Value::as_str)
                == Some("stale claim review note")
        }));
        Ok(())
    }

    #[test]
    fn canonical_runtime_store_pause_interruption_is_lossless() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        let initial = store.load_runtime_plan_snapshot("r1")?;
        let expected_task = initial
            .tasks
            .first()
            .cloned()
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        let claim = match store.claim_runtime_task("r1", &expected_task, initial.revision)? {
            echo_agent::tasks::RuntimeTaskClaimOutcome::Claimed(claim) => claim,
            echo_agent::tasks::RuntimeTaskClaimOutcome::ReloadSnapshot => {
                return Err(StoreError::InvalidPlan(
                    "fresh interruption claim unexpectedly required reload".to_string(),
                ));
            }
        };
        let claimed = store.load_runtime_plan_snapshot("r1")?;
        let mut framework_paused = claimed.clone();
        let disposition = echo_agent::tasks::RuntimeInterruptionDisposition::Paused {
            reason: "user requested pause".to_string(),
        };
        let framework_outcome = echo_agent::tasks::settle_runtime_interruption(
            &mut framework_paused,
            claimed.revision,
            disposition.clone(),
        )?;
        store.fail_next_runtime_mutation_projection_for_test();
        assert_eq!(
            store.settle_runtime_task_interruption("r1", claimed.revision, disposition)?,
            framework_outcome
        );
        let stored_paused = store.load_runtime_plan_snapshot("r1")?;
        assert_eq!(stored_paused.revision, framework_paused.revision);
        assert!(!store.runtime_task_claim_is_current("r1", "t1", &claim)?);
        let paused = framework_paused
            .tasks
            .iter()
            .find(|task| task.spec.id == "t1")
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        assert_eq!(
            paused.execution.status,
            echo_agent::tasks::TaskStatus::Paused("user requested pause".to_string())
        );
        assert!(paused.execution.claim.is_none());
        let retry_count_before_resume = paused.execution.retry_count;
        let persisted = stored_paused
            .tasks
            .iter()
            .find(|task| task.spec.id == "t1")
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        assert_eq!(
            persisted.execution.status,
            echo_agent::tasks::TaskStatus::Paused("user requested pause".to_string())
        );
        assert_eq!(persisted.execution.retry_count, retry_count_before_resume);
        store.transition_run("r1", TaskRunStatus::Paused)?;
        store.resume_task_run("r1")?;
        let resumed = store
            .load_runtime_plan_snapshot("r1")?
            .tasks
            .into_iter()
            .find(|task| task.spec.id == "t1")
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        assert_eq!(
            resumed.execution.status,
            echo_agent::tasks::TaskStatus::Pending
        );
        assert_eq!(resumed.execution.retry_count, retry_count_before_resume);
        assert!(resumed.execution.claim.is_none());
        Ok(())
    }

    #[test]
    fn recovered_result_requires_exact_physical_claim_after_pause_resume() -> Result<(), StoreError>
    {
        let store = fresh()?;
        seed_plan(&store)?;
        let initial = store.load_runtime_plan_snapshot("r1")?;
        let expected = initial
            .tasks
            .first()
            .cloned()
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        let old_claim = match store.claim_runtime_task("r1", &expected, initial.revision)? {
            echo_agent::tasks::RuntimeTaskClaimOutcome::Claimed(claim) => claim,
            echo_agent::tasks::RuntimeTaskClaimOutcome::ReloadSnapshot => {
                return Err(StoreError::InvalidPlan(
                    "initial claim reloaded".to_string(),
                ));
            }
        };
        let old_execution_id = old_claim.execution_id("r1", "t1");
        store.record_subagent_assigned(
            "r1",
            "t1",
            &old_execution_id,
            "subagent",
            "Review runtime",
            old_claim.revision,
            old_claim.attempt,
            true,
            true,
        )?;
        store.settle_runtime_task_interruption(
            "r1",
            initial.revision,
            echo_agent::tasks::RuntimeInterruptionDisposition::Paused {
                reason: "pause for exact recovery test".to_string(),
            },
        )?;
        store.transition_run("r1", TaskRunStatus::Paused)?;
        store.resume_task_run("r1")?;
        let resumed = store.load_runtime_plan_snapshot("r1")?;
        let resumed_task = resumed
            .tasks
            .first()
            .cloned()
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        let new_claim = match store.claim_runtime_task("r1", &resumed_task, resumed.revision)? {
            echo_agent::tasks::RuntimeTaskClaimOutcome::Claimed(claim) => claim,
            echo_agent::tasks::RuntimeTaskClaimOutcome::ReloadSnapshot => {
                return Err(StoreError::InvalidPlan(
                    "resumed claim reloaded".to_string(),
                ));
            }
        };
        assert_eq!(new_claim.attempt, old_claim.attempt);
        let new_execution_id = new_claim.execution_id("r1", "t1");
        store.record_subagent_assigned(
            "r1",
            "t1",
            &new_execution_id,
            "subagent",
            "Review runtime",
            new_claim.revision,
            new_claim.attempt,
            true,
            true,
        )?;
        let stale_result = SubagentTaskResult::terminal(
            SubagentRunStatus::Completed,
            "late result from old physical claim",
            Vec::new(),
        );
        store.record_subagent_released(SubagentReleaseRecord {
            run_id: "r1",
            task_id: "t1",
            execution_id: &old_execution_id,
            agent_name: "subagent",
            task_subject: "Review runtime",
            plan_revision: old_claim.revision,
            attempt: old_claim.attempt,
            status: "completed",
            result: Some(&stale_result),
            full_output: Some("late result from old physical claim"),
            usage: None,
            dispatch_hook: true,
        })?;

        assert!(
            store
                .recoverable_subagent_result_for_attempt(
                    "r1",
                    "t1",
                    &new_execution_id,
                    new_claim.revision,
                    new_claim.attempt,
                )?
                .is_none()
        );
        assert!(
            store
                .recoverable_subagent_result_for_attempt(
                    "r1",
                    "t1",
                    &old_execution_id,
                    old_claim.revision,
                    old_claim.attempt,
                )?
                .is_some()
        );
        Ok(())
    }

    #[test]
    fn canonical_runtime_store_revision_reload_wins_task_update_race() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        let expected = store
            .get_plan("r1")?
            .ok_or_else(|| StoreError::PlanNotFound("r1".to_string()))?
            .tasks
            .first()
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?
            .to_task()
            .map_err(StoreError::InvalidPlan)?;
        store.apply_task_patch_for_test(
            "r1",
            &TaskUpdateRequest {
                base_revision: 1,
                reason: "skip before stale dispatch claims task".to_string(),
                operations: vec![TaskUpdateOperation::Skip {
                    task_id: "t1".to_string(),
                }],
            },
        )?;

        let outcome = store.claim_runtime_task("r1", &expected, 1)?;

        assert_eq!(
            outcome,
            echo_agent::tasks::RuntimeTaskClaimOutcome::ReloadSnapshot
        );
        let task = store
            .get_plan("r1")?
            .ok_or_else(|| StoreError::PlanNotFound("r1".to_string()))?
            .tasks
            .into_iter()
            .find(|task| task.id == "t1")
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        assert_eq!(task.status, echo_agent::tasks::TaskStatus::Skipped);
        assert!(task.claim.is_none());
        Ok(())
    }

    #[test]
    fn stale_claim_cannot_overwrite_cancelled_task() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        let expected = store
            .get_plan("r1")?
            .ok_or_else(|| StoreError::PlanNotFound("r1".to_string()))?
            .tasks
            .first()
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?
            .to_task()
            .map_err(StoreError::InvalidPlan)?;
        let claim = match store.claim_runtime_task("r1", &expected, 1)? {
            echo_agent::tasks::RuntimeTaskClaimOutcome::Claimed(claim) => claim,
            echo_agent::tasks::RuntimeTaskClaimOutcome::ReloadSnapshot => {
                return Err(StoreError::InvalidPlan(
                    "fresh task claim unexpectedly required reload".to_string(),
                ));
            }
        };
        store.set_task_status(
            "r1",
            "t1",
            echo_agent::tasks::TaskStatus::Skipped,
            None,
            Some("cancelled by user"),
        )?;

        let outcome = store.settle_runtime_task_claim(
            "r1",
            "t1",
            &claim,
            echo_agent::tasks::TaskStatus::Completed,
            Some("stale completion".to_string()),
        )?;

        assert_eq!(
            outcome,
            echo_agent::tasks::RuntimeTaskSettlementOutcome::Superseded
        );
        let task = store
            .get_plan("r1")?
            .ok_or_else(|| StoreError::PlanNotFound("r1".to_string()))?
            .tasks
            .into_iter()
            .find(|task| task.id == "t1")
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        assert_eq!(task.status, echo_agent::tasks::TaskStatus::Skipped);
        Ok(())
    }

    #[test]
    fn patched_spec_uses_new_execution_identity_without_retry_bump() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        let original = store
            .get_plan("r1")?
            .ok_or_else(|| StoreError::PlanNotFound("r1".to_string()))?
            .tasks
            .first()
            .cloned()
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        let original_runtime = original.to_task().map_err(StoreError::InvalidPlan)?;
        let old_claim = echo_agent::tasks::TaskClaim::new(
            1,
            1,
            original_runtime
                .spec
                .stable_hash()
                .map_err(StoreError::InvalidPlan)?,
        );
        let old_execution_id = old_claim.execution_id("r1", &original.id);
        let durable_result = SubagentTaskResult::terminal(
            SubagentRunStatus::Completed,
            "old spec result",
            Vec::new(),
        );
        store.record_subagent_assigned(
            "r1",
            "t1",
            &old_execution_id,
            "code_reviewer",
            &original.title,
            old_claim.revision,
            old_claim.attempt,
            true,
            true,
        )?;
        store.record_subagent_released(SubagentReleaseRecord {
            run_id: "r1",
            task_id: "t1",
            execution_id: &old_execution_id,
            agent_name: "code_reviewer",
            task_subject: &original.title,
            plan_revision: old_claim.revision,
            attempt: old_claim.attempt,
            status: "completed",
            result: Some(&durable_result),
            full_output: Some("old spec full output"),
            usage: None,
            dispatch_hook: true,
        })?;
        store.set_task_status(
            "r1",
            "t1",
            echo_agent::tasks::TaskStatus::Blocked(String::new()),
            Some("code_reviewer"),
            Some("requires a revised contract"),
        )?;
        let patched = store.apply_task_patch_for_test(
            "r1",
            &TaskUpdateRequest {
                base_revision: 1,
                reason: "change blocked task contract".to_string(),
                operations: vec![TaskUpdateOperation::Update {
                    task_id: "t1".to_string(),
                    patch: TaskPatch {
                        description: Some("review the revised runtime contract".to_string()),
                        ..Default::default()
                    },
                }],
            },
        )?;
        let patched_task = patched
            .tasks
            .first()
            .cloned()
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        assert_eq!(patched_task.retry_count, 0);
        let patched_runtime = patched_task.to_task().map_err(StoreError::InvalidPlan)?;
        let new_claim = match store.claim_runtime_task("r1", &patched_runtime, patched.revision)? {
            echo_agent::tasks::RuntimeTaskClaimOutcome::Claimed(claim) => claim,
            echo_agent::tasks::RuntimeTaskClaimOutcome::ReloadSnapshot => {
                return Err(StoreError::InvalidPlan(
                    "patched task claim unexpectedly required reload".to_string(),
                ));
            }
        };
        let new_execution_id = new_claim.execution_id("r1", &patched_task.id);

        assert_ne!(old_execution_id, new_execution_id);
        assert_ne!(old_claim.spec_hash, new_claim.spec_hash);
        assert!(
            store
                .recoverable_subagent_result_for_attempt(
                    "r1",
                    "t1",
                    &old_execution_id,
                    old_claim.revision,
                    old_claim.attempt,
                )?
                .is_some()
        );
        assert!(
            store
                .recoverable_subagent_result_for_attempt(
                    "r1",
                    "t1",
                    &new_execution_id,
                    new_claim.revision,
                    new_claim.attempt,
                )?
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn task_update_skip_preserves_spec_and_updates_execution() -> Result<(), StoreError> {
        let s = fresh()?;
        seed_plan(&s)?;
        let plan = s.apply_task_patch_for_test(
            "r1",
            &TaskUpdateRequest {
                base_revision: 1,
                reason: "task no longer required".to_string(),
                operations: vec![TaskUpdateOperation::Skip {
                    task_id: "t1".to_string(),
                }],
            },
        )?;
        assert_eq!(plan.revision, 2);
        assert_eq!(plan.tasks.len(), 1);
        assert_eq!(plan.tasks[0].status, echo_agent::tasks::TaskStatus::Skipped);
        Ok(())
    }

    #[test]
    fn task_update_update_requeues_blocked_task() -> Result<(), StoreError> {
        let s = fresh()?;
        seed_plan(&s)?;
        s.set_task_status(
            "r1",
            "t1",
            echo_agent::tasks::TaskStatus::Blocked(String::new()),
            Some("reviewer"),
            Some("needs a clearer brief"),
        )?;
        let plan = s.apply_task_patch_for_test(
            "r1",
            &TaskUpdateRequest {
                base_revision: 1,
                reason: "clarify the blocked task".to_string(),
                operations: vec![TaskUpdateOperation::Update {
                    task_id: "t1".to_string(),
                    patch: TaskPatch {
                        description: Some("Review the clarified runtime boundary".to_string()),
                        ..Default::default()
                    },
                }],
            },
        )?;
        assert_eq!(plan.revision, 2);
        assert_eq!(plan.tasks[0].status, echo_agent::tasks::TaskStatus::Pending);
        assert_eq!(
            plan.tasks[0].description,
            "Review the clarified runtime boundary"
        );
        Ok(())
    }

    #[test]
    fn completion_gate_rechecks_latest_plan_revision() -> Result<(), StoreError> {
        let s = fresh()?;
        seed_plan(&s)?;
        let persist_summary = |task_id: &str| {
            s.put_summary(&TaskExecutionSummary {
                run_id: "r1".to_string(),
                task_id: task_id.to_string(),
                subagent_name: "explorer".to_string(),
                result: SubagentTaskResult::terminal(
                    SubagentRunStatus::Completed,
                    "verified task result",
                    Vec::new(),
                ),
                decisions: Vec::new(),
                next_implications: Vec::new(),
                suggested_tasks: Vec::new(),
                created_at: Utc::now(),
            })
        };
        persist_summary("t1")?;
        s.set_task_status(
            "r1",
            "t1",
            echo_agent::tasks::TaskStatus::Completed,
            Some("explorer"),
            None,
        )?;
        let follow_up = PlanTask {
            id: "t2".to_string(),
            title: "Verify follow-up".to_string(),
            description: "Verify evidence discovered by t1".to_string(),
            kind: PlanTaskKind::Verification,
            agent_role: "explorer".to_string(),
            depends_on: vec!["t1".to_string()],
            ..Default::default()
        };
        s.apply_task_patch_for_test(
            "r1",
            &TaskUpdateRequest {
                base_revision: 1,
                reason: "new evidence requires verification".to_string(),
                operations: vec![TaskUpdateOperation::Insert {
                    after_task_id: Some("t1".to_string()),
                    task: follow_up.spec(),
                }],
            },
        )?;
        assert!(!s.complete_run_if_quiescent("r1")?);
        persist_summary("t2")?;
        s.set_task_status(
            "r1",
            "t2",
            echo_agent::tasks::TaskStatus::Completed,
            Some("explorer"),
            None,
        )?;
        assert!(s.complete_run_if_quiescent("r1")?);
        assert_eq!(
            s.get_run("r1")?
                .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?
                .status,
            TaskRunStatus::Completed
        );
        Ok(())
    }

    #[test]
    fn task_update_rejects_running_task_contract_change() -> Result<(), StoreError> {
        let store = fresh()?;
        seed_plan(&store)?;
        store.set_task_status(
            "r1",
            "t1",
            echo_agent::tasks::TaskStatus::Running,
            Some("subagent"),
            None,
        )?;
        let result = store.apply_task_patch_for_test(
            "r1",
            &TaskUpdateRequest {
                base_revision: 1,
                reason: "change active ownership".to_string(),
                operations: vec![TaskUpdateOperation::Update {
                    task_id: "t1".to_string(),
                    patch: TaskPatch {
                        files: Some(vec!["src/new-owner.rs".to_string()]),
                        ..Default::default()
                    },
                }],
            },
        );
        assert!(matches!(result, Err(StoreError::InvalidPlan(_))));
        Ok(())
    }

    // ── review #4: intent-visible tests that validation fires on the FILE
    //    authority path (not just transitively). Each asserts the error is
    //    returned AND no event line was appended — proving the file-path
    //    validation branch rejected before writing. ──────────────────────

    /// `transition_run` rejects an illegal transition on the file path and
    /// appends no event. (Completed → Running is always illegal.)
    #[test]
    fn file_path_rejects_illegal_transition_and_appends_no_event() -> Result<(), StoreError> {
        let s = fresh()?;
        s.create_run(
            "r1",
            "ws",
            "c1",
            "m1",
            DomainProfile::General,
            "g",
            "",
            AttendedMode::Attended,
        )?;
        s.transition_run("r1", TaskRunStatus::Running)?;
        s.transition_run("r1", TaskRunStatus::Completed)?;
        let before = s.list_events("r1", 0)?.len();
        let err = s
            .transition_run("r1", TaskRunStatus::Running)
            .err()
            .ok_or_else(|| {
                StoreError::InvalidPlan(
                    "illegal file transition unexpectedly succeeded".to_string(),
                )
            })?;
        assert!(matches!(err, StoreError::IllegalTransition { .. }));
        // No new event appended — the file-path validation rejected before writing.
        assert_eq!(s.list_events("r1", 0)?.len(), before);
        Ok(())
    }

    /// `task_update` rejects a dependency cycle and appends no revision event.
    #[test]
    fn file_path_rejects_dependency_cycle_and_appends_no_event() -> Result<(), StoreError> {
        let s = fresh()?;
        s.create_run(
            "r1",
            "ws",
            "c1",
            "m1",
            DomainProfile::General,
            "g",
            "",
            AttendedMode::Attended,
        )?;
        s.attach_plan_for_test(&TaskPlan {
            plan_id: "p1".to_string(),
            run_id: "r1".to_string(),
            revision: 1,
            domain_profile: DomainProfile::General,
            goal_revision: 1,
            goal_sha256: task_goal_sha256("g"),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Parallel,
            tasks: vec![
                PlanTask {
                    id: "t1".into(),
                    depends_on: Vec::new(),
                    ..sample_task_body("t1")
                },
                PlanTask {
                    id: "t2".into(),
                    depends_on: vec!["t1".into()],
                    ..sample_task_body("t2")
                },
            ],
        })?;
        let before = s.list_events("r1", 0)?.len();
        // Now make t1 depend on t2 → cycle.
        let err = s
            .apply_task_patch_for_test(
                "r1",
                &TaskUpdateRequest {
                    base_revision: 1,
                    reason: "introduce invalid cycle".to_string(),
                    operations: vec![TaskUpdateOperation::Update {
                        task_id: "t1".to_string(),
                        patch: TaskPatch {
                            depends_on: Some(vec!["t2".into()]),
                            ..Default::default()
                        },
                    }],
                },
            )
            .err()
            .ok_or_else(|| {
                StoreError::InvalidPlan("dependency cycle unexpectedly succeeded".to_string())
            })?;
        assert!(matches!(err, StoreError::InvalidPlan(_)));
        assert_eq!(s.list_events("r1", 0)?.len(), before);
        Ok(())
    }

    /// `set_task_status` rejects an unknown task on the file path and appends
    /// no event.
    #[test]
    fn file_path_rejects_unknown_task_and_appends_no_event() -> Result<(), StoreError> {
        let s = fresh()?;
        seed_plan(&s)?;
        let before = s.list_events("r1", 0)?.len();
        let err = s
            .set_task_status(
                "r1",
                "nope",
                echo_agent::tasks::TaskStatus::Running,
                None,
                None,
            )
            .err()
            .ok_or_else(|| {
                StoreError::InvalidPlan("unknown task unexpectedly succeeded".to_string())
            })?;
        assert!(matches!(err, StoreError::TaskNotFound(_)));
        assert_eq!(s.list_events("r1", 0)?.len(), before);
        Ok(())
    }

    #[tokio::test]
    async fn generation_rebind_rejects_active_operation_then_isolates_roots()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let first_root = temp.path().join("first");
        let second_root = temp.path().join("second");
        let store = std::sync::Arc::new(TaskRuntimeStore::new_in_memory_with_shadow_root(
            &first_root,
        )?);
        let operation = store.lease_active_workspace_generation()?;
        assert!(
            matches!(
                store.rebind_shadow_root(&second_root, "workspace-b").await,
                Err(StoreError::WorkspaceTransitionBusy {
                    active_operations: 1
                })
            ),
            "workspace transition must fail fast while a generation lease is active"
        );
        drop(operation);
        store
            .rebind_shadow_root(&second_root, "workspace-b")
            .await?;

        store.create_run_for_active_workspace(
            "run-b",
            "conversation-b",
            "message-b",
            DomainProfile::General,
            "generation isolation",
            "task",
            AttendedMode::Attended,
        )?;
        assert!(!first_root.join("run-b").exists());
        assert!(temp.path().join("second/run-b/events.jsonl").is_file());
        assert_eq!(
            store.get_run("run-b")?.map(|run| run.workspace_id),
            Some("workspace-b".to_string())
        );
        Ok(())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn workspace_transition_rejects_operations_without_blocking_single_thread_runtime()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let first_root = temp.path().join("first");
        let second_root = temp.path().join("second");
        let store = std::sync::Arc::new(TaskRuntimeStore::new_in_memory_with_shadow_root(
            &first_root,
        )?);
        let transition = store.begin_workspace_transition().await?;

        assert!(matches!(
            store.create_run_for_active_workspace(
                "run-b",
                "conversation-b",
                "message-b",
                DomainProfile::General,
                "generation admission",
                "task",
                AttendedMode::Attended,
            ),
            Err(StoreError::WorkspaceTransitionBusy { .. })
        ));
        assert!(matches!(
            store.transition_run("run-b", TaskRunStatus::Running),
            Err(StoreError::WorkspaceTransitionBusy { .. })
        ));
        assert!(matches!(
            store.set_task_status(
                "run-b",
                "task-b",
                echo_agent::tasks::TaskStatus::Running,
                None,
                None
            ),
            Err(StoreError::WorkspaceTransitionBusy { .. })
        ));
        assert!(matches!(
            store.note("run-b", None, "must not reach the old root"),
            Err(StoreError::WorkspaceTransitionBusy { .. })
        ));
        assert!(matches!(
            store.record_subagent_assigned(
                "run-b",
                "task-b",
                "execution-b",
                "subagent-b",
                "Task B",
                1,
                1,
                true,
                true,
            ),
            Err(StoreError::WorkspaceTransitionBusy { .. })
        ));
        assert!(matches!(
            store.record_tool_started(
                "run-b",
                "task-b",
                "execution-b",
                "call-b",
                "read_file",
                true,
            ),
            Err(StoreError::WorkspaceTransitionBusy { .. })
        ));
        assert!(matches!(
            store.get_run("run-b"),
            Err(StoreError::WorkspaceTransitionBusy { .. })
        ));
        assert!(matches!(
            store.lease_active_workspace_generation(),
            Err(StoreError::WorkspaceTransitionBusy { .. })
        ));

        transition.rebind_shadow_root(&second_root, "workspace-b")?;
        assert!(matches!(
            store.note("run-b", None, "must wait for generation publication"),
            Err(StoreError::WorkspaceTransitionBusy { .. })
        ));
        drop(transition);

        store.create_run_for_active_workspace(
            "run-b",
            "conversation-b",
            "message-b",
            DomainProfile::General,
            "generation admission",
            "task",
            AttendedMode::Attended,
        )?;

        assert!(!first_root.join("run-b").exists());
        assert!(second_root.join("run-b/events.jsonl").is_file());
        assert_eq!(store.active_workspace_id(), "workspace-b");
        Ok(())
    }

    #[tokio::test]
    async fn failed_generation_rebind_keeps_previous_root_and_identity()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let first_root = temp.path().join("first");
        let invalid_root = temp.path().join("not-a-directory");
        std::fs::write(&invalid_root, "file")?;
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(&first_root)?;

        assert!(
            store
                .rebind_shadow_root(&invalid_root, "workspace-b")
                .await
                .is_err()
        );
        assert_eq!(store.active_workspace_id(), "test");
        store.create_run_for_active_workspace(
            "run-a",
            "conversation-a",
            "message-a",
            DomainProfile::General,
            "failed rebind",
            "task",
            AttendedMode::Attended,
        )?;
        assert!(first_root.join("run-a/events.jsonl").is_file());
        assert_eq!(
            store.get_run("run-a")?.map(|run| run.workspace_id),
            Some("test".to_string())
        );
        Ok(())
    }

    #[test]
    fn conversation_removal_deletes_only_its_task_runs() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(temp.path().join("tasks"))?;
        for (run_id, conversation_id) in [
            ("conversation-run-a", "conversation-delete"),
            ("conversation-run-b", "conversation-delete"),
            ("retained-run", "conversation-keep"),
        ] {
            store.create_run(
                run_id,
                "workspace",
                conversation_id,
                "message",
                DomainProfile::General,
                run_id,
                "chat",
                AttendedMode::Attended,
            )?;
        }

        store.remove_conversation("conversation-delete")?;

        assert!(store.get_run("conversation-run-a")?.is_none());
        assert!(store.get_run("conversation-run-b")?.is_none());
        assert!(store.get_run("retained-run")?.is_some());
        assert!(
            store
                .list_runs_for_conversation("conversation-delete")?
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn degraded_conversation_delete_clears_local_state_before_same_id_recreate()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(temp.path().join("tasks"))?;
        store.create_run(
            "degraded-delete",
            "workspace",
            "conversation-delete",
            "message",
            DomainProfile::General,
            "degraded delete",
            "chat",
            AttendedMode::Attended,
        )?;
        store
            .task_cancel_tokens
            .lock()
            .map_err(|_| "task token lock poisoned")?
            .insert(
                "degraded-delete::task".to_string(),
                echo_agent::agent::CancellationToken::new(),
            );
        assert!(store.plan_locks.contains_key("degraded-delete"));
        store.shadow.fail_root_sync_on_call_for_test(2);
        assert!(matches!(
            store.remove_conversation("conversation-delete"),
            Err(StoreError::Shadow(
                super::super::file_shadow::ShadowError::CommittedDeletionDegraded { .. }
            ))
        ));
        assert!(!store.plan_locks.contains_key("degraded-delete"));
        assert!(
            !store
                .task_cancel_tokens
                .lock()
                .map_err(|_| "task token lock poisoned")?
                .contains_key("degraded-delete::task")
        );
        assert!(store.get_run("degraded-delete")?.is_none());
        let recreated = store.create_run(
            "degraded-delete",
            "workspace",
            "conversation-new",
            "message-new",
            DomainProfile::General,
            "recreated",
            "chat",
            AttendedMode::Attended,
        )?;
        assert_eq!(recreated.conversation_id, "conversation-new");
        Ok(())
    }

    #[test]
    fn conversation_removal_fails_closed_while_a_driver_is_active()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let store = std::sync::Arc::new(TaskRuntimeStore::new_in_memory_with_shadow_root(
            temp.path().join("tasks"),
        )?);
        store.create_run(
            "active-delete-run",
            "workspace",
            "conversation-delete",
            "message",
            DomainProfile::General,
            "active run",
            "chat",
            AttendedMode::Attended,
        )?;
        let registration = store.register_run_cancellation(
            "active-delete-run",
            echo_agent::agent::CancellationToken::new(),
        )?;

        assert!(matches!(
            store.remove_conversation("conversation-delete"),
            Err(StoreError::ConversationHasActiveRuns { .. })
        ));
        assert!(store.get_run("active-delete-run")?.is_some());
        drop(registration);
        store.remove_conversation("conversation-delete")?;
        assert!(store.get_run("active-delete-run")?.is_none());
        Ok(())
    }

    #[test]
    fn checkpoint_state_is_canonical_byte_equivalent_to_full_replay_and_repairs_corruption()
    -> Result<(), String> {
        let (temp, store, run_id) = seed_public_state_fixture(256)?;
        let events = store
            .list_events(&run_id, 0)
            .map_err(|error| error.to_string())?;
        let journal_sequence = events
            .last()
            .and_then(|event| u64::try_from(event.seq).ok())
            .ok_or_else(|| "full replay sequence is unavailable".to_string())?;
        let full = super::super::event_rebuild::fold_fixture_for_test(&events)
            .map_err(|error| error.to_string())?
            .run_state_with_sequence(journal_sequence);
        let warm = store
            .get_run_state(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "checkpoint-backed state is missing".to_string())?;
        let full_bytes = echo_agent::utils::canonical_json::canonical_json_bytes(&full)
            .map_err(|error| error.to_string())?;
        let warm_bytes = echo_agent::utils::canonical_json::canonical_json_bytes(&warm)
            .map_err(|error| error.to_string())?;
        assert_eq!(warm_bytes, full_bytes);

        let checkpoint_path = temp
            .path()
            .join("tasks")
            .join(&run_id)
            .join("checkpoint.json");
        std::fs::write(&checkpoint_path, b"{corrupt checkpoint")
            .map_err(|error| error.to_string())?;
        drop(store);
        let reopened = TaskRuntimeStore::new_in_memory_with_shadow_root(temp.path().join("tasks"))
            .map_err(|error| error.to_string())?;
        let repaired = reopened
            .get_run_state(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "repaired state is missing".to_string())?;
        let repaired_bytes = echo_agent::utils::canonical_json::canonical_json_bytes(&repaired)
            .map_err(|error| error.to_string())?;
        assert_eq!(repaired_bytes, full_bytes);
        use echo_agent::state::journal::{CheckpointStore, FileCheckpointStore};
        let decoded = FileCheckpointStore::<serde_json::Value>::open(&checkpoint_path)
            .load()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "repaired checkpoint is missing".to_string())?;
        assert_eq!(decoded.sequence, 256);
        assert!(decoded.state.is_object());
        Ok(())
    }

    #[test]
    #[ignore = "LH5 release performance gate; run with --release --ignored --nocapture"]
    fn performance_public_get_run_state_10k_100k_release_gate() -> Result<(), String> {
        if cfg!(debug_assertions) {
            return Err("LH5 performance gate must run with --release".to_string());
        }
        let (_ten_temp, ten_store, ten_run) = seed_public_state_fixture(10_000)?;
        let (hundred_temp, hundred_store, hundred_run) = seed_public_state_fixture(100_000)?;

        let mut ten_samples = Vec::with_capacity(5);
        let mut hundred_samples = Vec::with_capacity(5);
        for _ in 0..5 {
            let started = std::time::Instant::now();
            assert!(
                ten_store
                    .get_run_state(&ten_run)
                    .map_err(|error| error.to_string())?
                    .is_some()
            );
            ten_samples.push(started.elapsed());

            let started = std::time::Instant::now();
            assert!(
                hundred_store
                    .get_run_state(&hundred_run)
                    .map_err(|error| error.to_string())?
                    .is_some()
            );
            hundred_samples.push(started.elapsed());
        }
        let ten_median = median_duration(&mut ten_samples)
            .ok_or_else(|| "10k sample set is empty".to_string())?;
        let hundred_median = median_duration(&mut hundred_samples)
            .ok_or_else(|| "100k sample set is empty".to_string())?;
        let ten_worst = ten_samples.iter().copied().max().unwrap_or_default();
        let hundred_worst = hundred_samples.iter().copied().max().unwrap_or_default();

        let append_started = std::time::Instant::now();
        hundred_store
            .note(&hundred_run, None, "one appended public state event")
            .map_err(|error| error.to_string())?;
        assert!(
            hundred_store
                .get_run_state(&hundred_run)
                .map_err(|error| error.to_string())?
                .is_some()
        );
        let append_read = append_started.elapsed();

        let checkpoint_path = hundred_temp
            .path()
            .join("tasks")
            .join(&hundred_run)
            .join("checkpoint.json");
        let events_path = hundred_temp
            .path()
            .join("tasks")
            .join(&hundred_run)
            .join("events.jsonl");
        drop(hundred_store);
        std::fs::write(&checkpoint_path, b"{corrupt checkpoint")
            .map_err(|error| error.to_string())?;
        let hundred_store =
            TaskRuntimeStore::new_in_memory_with_shadow_root(hundred_temp.path().join("tasks"))
                .map_err(|error| error.to_string())?;
        let rebuild_started = std::time::Instant::now();
        assert!(
            hundred_store
                .get_run_state(&hundred_run)
                .map_err(|error| error.to_string())?
                .is_some()
        );
        let corrupt_rebuild = rebuild_started.elapsed();
        let warm_started = std::time::Instant::now();
        assert!(
            hundred_store
                .get_run_state(&hundred_run)
                .map_err(|error| error.to_string())?
                .is_some()
        );
        let repaired_warm = warm_started.elapsed();
        let checkpoint_bytes = std::fs::metadata(&checkpoint_path)
            .map_err(|error| error.to_string())?
            .len();
        let event_bytes = std::fs::metadata(&events_path)
            .map_err(|error| error.to_string())?
            .len();

        println!(
            "{}",
            serde_json::json!({
                "ten_k_median_ms": ten_median.as_secs_f64() * 1_000.0,
                "ten_k_worst_ms": ten_worst.as_secs_f64() * 1_000.0,
                "hundred_k_median_ms": hundred_median.as_secs_f64() * 1_000.0,
                "hundred_k_worst_ms": hundred_worst.as_secs_f64() * 1_000.0,
                "append_read_ms": append_read.as_secs_f64() * 1_000.0,
                "corrupt_rebuild_ms": corrupt_rebuild.as_secs_f64() * 1_000.0,
                "repaired_warm_ms": repaired_warm.as_secs_f64() * 1_000.0,
                "checkpoint_bytes": checkpoint_bytes,
                "event_bytes": event_bytes,
            })
        );
        assert!(ten_median <= std::time::Duration::from_millis(2));
        assert!(hundred_median <= std::time::Duration::from_millis(2));
        assert!(hundred_median.as_nanos() <= ten_median.as_nanos().saturating_mul(2).max(1));
        assert!(append_read <= std::time::Duration::from_millis(50));
        assert!(corrupt_rebuild <= std::time::Duration::from_secs(5));
        assert!(repaired_warm <= std::time::Duration::from_millis(2));
        assert!(checkpoint_bytes <= 256 * 1024);
        assert!(checkpoint_bytes.saturating_mul(20) < event_bytes);
        Ok(())
    }

    #[test]
    #[ignore = "final 10k/100k scale gate; run explicitly after the refactor"]
    fn production_todo_artifact_review_summary_completion_queries_are_bounded_at_10k_and_100k()
    -> Result<(), String> {
        let (_ten_temp, ten_store, ten_run) = seed_public_query_fixture(10_000)?;
        let (_hundred_temp, hundred_store, hundred_run) = seed_public_query_fixture(100_000)?;
        let mut ten_samples = Vec::with_capacity(5);
        let mut hundred_samples = Vec::with_capacity(5);
        for _ in 0..5 {
            ten_samples.push(time_public_queries(&ten_store, &ten_run)?);
            hundred_samples.push(time_public_queries(&hundred_store, &hundred_run)?);
        }
        let ten_median = median_duration(&mut ten_samples)
            .ok_or_else(|| "10k query sample set is empty".to_string())?;
        let hundred_median = median_duration(&mut hundred_samples)
            .ok_or_else(|| "100k query sample set is empty".to_string())?;
        let ratio_budget = ten_median
            .saturating_mul(4)
            .max(std::time::Duration::from_millis(25));
        assert!(hundred_median <= std::time::Duration::from_millis(200));
        assert!(hundred_median <= ratio_budget);
        Ok(())
    }

    /// FINAL performance gate for artifact and per-task review history.
    ///
    /// Run explicitly with:
    /// `cargo test -p echo-agent-app-core artifact_and_per_task_review_history_scale_at_10k_and_100k --lib --release -- --ignored --nocapture`
    #[test]
    #[ignore = "final 10k/100k performance gate; run explicitly with --release --ignored --nocapture"]
    fn artifact_and_per_task_review_history_scale_at_10k_and_100k() -> Result<(), String> {
        let (_ten_temp, ten_store, ten_run, ten_first, ten_second) =
            seed_history_scale_fixture(10_000)?;
        let (hundred_temp, hundred_store, hundred_run, hundred_first, hundred_second) =
            seed_history_scale_fixture(100_000)?;
        let (
            _ten_artifact_temp,
            _ten_artifact_store,
            _ten_artifact_run,
            ten_artifact_first,
            ten_artifact_second,
            ten_artifact_scans,
            _,
        ) = seed_artifact_scale_fixture(10_000)?;
        let (
            hundred_artifact_temp,
            hundred_artifact_store,
            hundred_artifact_run,
            hundred_artifact_first,
            hundred_artifact_second,
            hundred_artifact_scans,
            hundred_artifact_appended_bytes,
        ) = seed_artifact_scale_fixture(100_000)?;

        let mut ten_queries = Vec::with_capacity(5);
        let mut hundred_queries = Vec::with_capacity(5);
        for _ in 0..5 {
            ten_queries.push(time_history_target_queries(&ten_store, &ten_run)?);
            hundred_queries.push(time_history_target_queries(&hundred_store, &hundred_run)?);
        }
        let ten_query = median_duration(&mut ten_queries)
            .ok_or_else(|| "10k history query samples are empty".to_string())?;
        let hundred_query = median_duration(&mut hundred_queries)
            .ok_or_else(|| "100k history query samples are empty".to_string())?;
        assert_eq!(
            hundred_store
                .list_reviews(&hundred_run, "other-task")
                .map_err(|error| error.to_string())?
                .len(),
            100_000
        );
        let artifact_query_started = std::time::Instant::now();
        assert_eq!(
            hundred_artifact_store
                .list_artifacts(&hundred_artifact_run)
                .map_err(|error| error.to_string())?
                .len(),
            100_000
        );
        let hundred_artifact_query = artifact_query_started.elapsed();

        let (artifact_path, target_review_path, _) = hundred_store
            .shadow
            .history_paths_for_test(&hundred_run, "target-task")
            .map_err(|error| error.to_string())?;
        let (_, other_review_path, _) = hundred_store
            .shadow
            .history_paths_for_test(&hundred_run, "other-task")
            .map_err(|error| error.to_string())?;
        let checkpoint_path = hundred_temp
            .path()
            .join("tasks")
            .join(&hundred_run)
            .join("checkpoint.json");
        let artifact_bytes = std::fs::metadata(&artifact_path)
            .map_err(|error| error.to_string())?
            .len();
        let target_review_bytes = std::fs::metadata(&target_review_path)
            .map_err(|error| error.to_string())?
            .len();
        let other_review_bytes = std::fs::metadata(&other_review_path)
            .map_err(|error| error.to_string())?
            .len();
        let checkpoint_bytes = std::fs::metadata(&checkpoint_path)
            .map_err(|error| error.to_string())?
            .len();
        let (hundred_artifact_path, _, _) = hundred_artifact_store
            .shadow
            .history_paths_for_test(&hundred_artifact_run, "target-task")
            .map_err(|error| error.to_string())?;
        let hundred_artifact_segment_bytes = std::fs::metadata(&hundred_artifact_path)
            .map_err(|error| error.to_string())?
            .len();
        println!(
            "{}",
            serde_json::json!({
                "ten_k_append_first_half_median_ms": ten_first.as_secs_f64() * 1_000.0,
                "ten_k_append_second_half_median_ms": ten_second.as_secs_f64() * 1_000.0,
                "hundred_k_append_first_half_median_ms": hundred_first.as_secs_f64() * 1_000.0,
                "hundred_k_append_second_half_median_ms": hundred_second.as_secs_f64() * 1_000.0,
                "ten_k_artifact_append_first_half_median_ms": ten_artifact_first.as_secs_f64() * 1_000.0,
                "ten_k_artifact_append_second_half_median_ms": ten_artifact_second.as_secs_f64() * 1_000.0,
                "hundred_k_artifact_append_first_half_median_ms": hundred_artifact_first.as_secs_f64() * 1_000.0,
                "hundred_k_artifact_append_second_half_median_ms": hundred_artifact_second.as_secs_f64() * 1_000.0,
                "ten_k_target_query_median_ms": ten_query.as_secs_f64() * 1_000.0,
                "hundred_k_target_query_median_ms": hundred_query.as_secs_f64() * 1_000.0,
                "hundred_k_artifact_query_ms": hundred_artifact_query.as_secs_f64() * 1_000.0,
                "ten_k_artifact_segment_scans": ten_artifact_scans,
                "hundred_k_artifact_segment_scans": hundred_artifact_scans,
                "artifact_segment_bytes": artifact_bytes,
                "hundred_k_artifact_segment_bytes": hundred_artifact_segment_bytes,
                "hundred_k_artifact_appended_bytes": hundred_artifact_appended_bytes,
                "target_review_segment_bytes": target_review_bytes,
                "other_review_segment_bytes": other_review_bytes,
                "hot_checkpoint_bytes": checkpoint_bytes,
            })
        );
        assert!(artifact_bytes <= 64 * 1024);
        assert!(target_review_bytes <= 64 * 1024);
        assert!(other_review_bytes > target_review_bytes);
        assert!(checkpoint_bytes <= 256 * 1024);
        assert!(ten_artifact_scans <= 1);
        assert!(hundred_artifact_scans <= 1);
        assert_eq!(
            hundred_artifact_segment_bytes,
            hundred_artifact_appended_bytes
        );
        assert!(
            ten_second
                <= ten_first
                    .saturating_mul(4)
                    .max(std::time::Duration::from_millis(250))
        );
        assert!(
            hundred_second
                <= hundred_first
                    .saturating_mul(4)
                    .max(std::time::Duration::from_millis(250))
        );
        assert!(hundred_second <= std::time::Duration::from_secs(2));
        assert!(
            hundred_second
                <= ten_second
                    .saturating_mul(4)
                    .max(std::time::Duration::from_millis(250))
        );
        assert!(
            hundred_artifact_second
                <= hundred_artifact_first
                    .saturating_mul(4)
                    .max(std::time::Duration::from_millis(250))
        );
        assert!(
            hundred_artifact_second
                <= ten_artifact_second
                    .saturating_mul(4)
                    .max(std::time::Duration::from_millis(250))
        );
        assert!(hundred_artifact_second <= std::time::Duration::from_secs(2));
        assert!(hundred_artifact_query <= std::time::Duration::from_secs(2));
        assert!(hundred_query <= std::time::Duration::from_millis(250));
        assert!(
            hundred_query
                <= ten_query
                    .saturating_mul(4)
                    .max(std::time::Duration::from_millis(50))
        );

        drop(hundred_store);
        let reopened =
            TaskRuntimeStore::new_in_memory_with_shadow_root(hundred_temp.path().join("tasks"))
                .map_err(|error| error.to_string())?;
        time_history_target_queries(&reopened, &hundred_run)?;
        assert_eq!(
            reopened
                .list_reviews(&hundred_run, "other-task")
                .map_err(|error| error.to_string())?
                .len(),
            100_000
        );
        drop(hundred_artifact_store);
        let reopened_artifacts = TaskRuntimeStore::new_in_memory_with_shadow_root(
            hundred_artifact_temp.path().join("tasks"),
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(
            reopened_artifacts
                .list_artifacts(&hundred_artifact_run)
                .map_err(|error| error.to_string())?
                .len(),
            100_000
        );
        Ok(())
    }

    #[test]
    #[ignore = "final 2k/10k scale characterization; run explicitly after the refactor"]
    fn artifact_and_per_task_review_history_characterization_at_2k_and_10k() -> Result<(), String> {
        let (small_temp, small_store, small_run, _, _) = seed_history_scale_fixture(2_000)?;
        let (large_temp, large_store, large_run, _, _) = seed_history_scale_fixture(10_000)?;

        for (store, run_id, expected_reviews) in [
            (&small_store, &small_run, 2_000_usize),
            (&large_store, &large_run, 10_000_usize),
        ] {
            let target_query = time_history_target_queries(store, run_id)?;
            assert!(
                target_query <= std::time::Duration::from_millis(250),
                "targeted history query exceeded daily characterization budget: {target_query:?}"
            );
            assert_eq!(
                store
                    .list_reviews(run_id, "other-task")
                    .map_err(|error| error.to_string())?
                    .len(),
                expected_reviews
            );
            assert_eq!(
                store
                    .list_artifacts(run_id)
                    .map_err(|error| error.to_string())?
                    .len(),
                1
            );
            let (artifact_path, target_review_path, _) = store
                .shadow
                .history_paths_for_test(run_id, "target-task")
                .map_err(|error| error.to_string())?;
            assert!(
                std::fs::metadata(&artifact_path)
                    .map_err(|error| error.to_string())?
                    .len()
                    <= 64 * 1024
            );
            assert!(
                std::fs::metadata(&target_review_path)
                    .map_err(|error| error.to_string())?
                    .len()
                    <= 64 * 1024
            );
        }

        drop(large_store);
        let restarted =
            TaskRuntimeStore::new_in_memory_with_shadow_root(large_temp.path().join("tasks"))
                .map_err(|error| error.to_string())?;
        assert_eq!(
            restarted
                .list_reviews(&large_run, "other-task")
                .map_err(|error| error.to_string())?
                .len(),
            10_000
        );
        assert_eq!(
            restarted
                .list_reviews(&large_run, "target-task")
                .map_err(|error| error.to_string())?
                .len(),
            1
        );
        assert_eq!(
            restarted
                .list_artifacts(&large_run)
                .map_err(|error| error.to_string())?
                .len(),
            1
        );
        time_history_target_queries(&restarted, &large_run)?;
        drop(small_store);
        drop(small_temp);
        Ok(())
    }

    #[test]
    fn query_checkpoint_corruption_self_heals_and_restart_preserves_results() -> Result<(), String>
    {
        let (temp, store, run_id) = seed_public_query_fixture(10_000)?;
        time_public_queries(&store, &run_id)?;
        let checkpoint_path = temp
            .path()
            .join("tasks")
            .join(&run_id)
            .join("checkpoint.json");
        drop(store);
        std::fs::write(&checkpoint_path, b"{corrupt checkpoint")
            .map_err(|error| error.to_string())?;
        let reopened = TaskRuntimeStore::new_in_memory_with_shadow_root(temp.path().join("tasks"))
            .map_err(|error| error.to_string())?;
        time_public_queries(&reopened, &run_id)?;
        let repaired: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&checkpoint_path).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(
            repaired
                .get("state")
                .and_then(|state| state.get("query_projection_schema"))
                .and_then(serde_json::Value::as_u64),
            Some(1)
        );
        drop(reopened);
        let restarted = TaskRuntimeStore::new_in_memory_with_shadow_root(temp.path().join("tasks"))
            .map_err(|error| error.to_string())?;
        time_public_queries(&restarted, &run_id)?;
        Ok(())
    }

    #[test]
    fn behind_checkpoint_is_repaired_once_on_query_only_restart() -> Result<(), String> {
        use echo_agent::state::journal::{CheckpointStore, FileCheckpointStore};

        let (temp, store, run_id) = seed_public_query_fixture(1_000)?;
        let checkpoint_path = temp
            .path()
            .join("tasks")
            .join(&run_id)
            .join("checkpoint.json");
        let receipt = store
            .shadow
            .append_event_batch(
                &run_id,
                vec![RuntimeJournalEvent::for_append(
                    &run_id,
                    None,
                    None,
                    RuntimeEventKind::Note,
                    serde_json::json!({"kind": "checkpoint_suffix"}),
                )],
            )
            .map_err(|error| error.to_string())?;
        let head = receipt.apply.last_sequence;
        let checkpoints = FileCheckpointStore::<super::super::event_rebuild::EventFoldState>::open(
            &checkpoint_path,
        );
        let before = checkpoints
            .load()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "checkpoint before restart is missing".to_string())?;
        assert!(before.sequence < head);
        drop(store);

        let reopened = TaskRuntimeStore::new_in_memory_with_shadow_root(temp.path().join("tasks"))
            .map_err(|error| error.to_string())?;
        time_public_queries(&reopened, &run_id)?;
        drop(reopened);
        let repaired = checkpoints
            .load()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "repaired checkpoint is missing".to_string())?;
        assert_eq!(repaired.sequence, head);

        let restarted = TaskRuntimeStore::new_in_memory_with_shadow_root(temp.path().join("tasks"))
            .map_err(|error| error.to_string())?;
        let stats = restarted
            .shadow
            .rewrite_plan_with_stats(&run_id)
            .map_err(|error| error.to_string())?;
        assert!(stats.used_checkpoint);
        assert_eq!(stats.folded_events, 0);
        Ok(())
    }

    #[test]
    fn full_history_rebuild_removes_stale_review_segments() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("tasks");
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(&root)
            .map_err(|error| error.to_string())?;
        seed_history_plan(&store, "stale-history")?;
        store
            .add_review(&history_review("stale-history", "target-task", "review-1"))
            .map_err(|error| error.to_string())?;
        let (_, review_path, cursor_path) = store
            .shadow
            .history_paths_for_test("stale-history", "target-task")
            .map_err(|error| error.to_string())?;
        let review_directory = review_path
            .parent()
            .ok_or_else(|| "review history has no directory".to_string())?
            .to_path_buf();
        drop(store);
        let stale_path = review_directory.join("stale-valid-segment.jsonl");
        std::fs::write(&stale_path, b"{}\n").map_err(|error| error.to_string())?;
        std::fs::remove_file(cursor_path).map_err(|error| error.to_string())?;

        let reopened = TaskRuntimeStore::new_in_memory_with_shadow_root(&root)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            reopened
                .list_reviews("stale-history", "target-task")
                .map_err(|error| error.to_string())?
                .len(),
            1
        );
        assert!(!stale_path.exists());
        Ok(())
    }

    #[test]
    fn legacy_query_checkpoint_schema_rebuilds_before_production_read() -> Result<(), String> {
        use echo_agent::state::journal::{CheckpointStore, FileCheckpointStore};

        let (temp, store, run_id) = seed_public_query_fixture(1_000)?;
        let checkpoint_path = temp
            .path()
            .join("tasks")
            .join(&run_id)
            .join("checkpoint.json");
        drop(store);
        let checkpoints = FileCheckpointStore::<super::super::event_rebuild::EventFoldState>::open(
            &checkpoint_path,
        );
        let mut legacy = checkpoints
            .load()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "query checkpoint is missing".to_string())?;
        legacy.state.clear_query_projection_schema_for_test();
        checkpoints
            .save(&legacy.state, legacy.sequence)
            .map_err(|error| error.to_string())?;
        let reopened = TaskRuntimeStore::new_in_memory_with_shadow_root(temp.path().join("tasks"))
            .map_err(|error| error.to_string())?;
        time_public_queries(&reopened, &run_id)?;
        drop(reopened);
        let repaired = checkpoints
            .load()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "repaired query checkpoint is missing".to_string())?;
        assert!(repaired.state.has_current_query_projection_schema());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn readonly_legacy_checkpoint_does_not_block_journal_derived_queries() -> Result<(), String> {
        use echo_agent::state::journal::{CheckpointStore, FileCheckpointStore};
        use std::os::unix::fs::PermissionsExt;

        let (temp, store, run_id) = seed_public_query_fixture(1_000)?;
        let run_directory = temp.path().join("tasks").join(&run_id);
        let checkpoint_path = run_directory.join("checkpoint.json");
        let (artifact_history, review_history, _) = store
            .shadow
            .history_paths_for_test(&run_id, "task-a")
            .map_err(|error| error.to_string())?;
        drop(store);
        let checkpoints = FileCheckpointStore::<super::super::event_rebuild::EventFoldState>::open(
            &checkpoint_path,
        );
        let mut legacy = checkpoints
            .load()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "query checkpoint is missing".to_string())?;
        legacy.state.clear_query_projection_schema_for_test();
        checkpoints
            .save(&legacy.state, legacy.sequence)
            .map_err(|error| error.to_string())?;
        std::fs::remove_file(artifact_history).map_err(|error| error.to_string())?;
        std::fs::remove_file(review_history).map_err(|error| error.to_string())?;
        let original_permissions = std::fs::metadata(&run_directory)
            .map_err(|error| error.to_string())?
            .permissions();
        let mut readonly = original_permissions.clone();
        readonly.set_mode(0o555);
        std::fs::set_permissions(&run_directory, readonly).map_err(|error| error.to_string())?;
        let query = (|| {
            let reopened =
                TaskRuntimeStore::new_in_memory_with_shadow_root(temp.path().join("tasks"))
                    .map_err(|error| error.to_string())?;
            time_public_queries(&reopened, &run_id)?;
            let first_replays = reopened
                .shadow
                .history_fallback_replay_count_for_test(&run_id)
                .map_err(|error| error.to_string())?;
            time_public_queries(&reopened, &run_id)?;
            let second_replays = reopened
                .shadow
                .history_fallback_replay_count_for_test(&run_id)
                .map_err(|error| error.to_string())?;
            if first_replays != second_replays {
                return Err("readonly history fallback was replayed on every query".to_string());
            }
            Ok(())
        })();
        std::fs::set_permissions(&run_directory, original_permissions)
            .map_err(|error| error.to_string())?;
        query?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn readonly_review_fallback_lru_avoids_aba_full_replay() -> Result<(), String> {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("tasks");
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(&root)
            .map_err(|error| error.to_string())?;
        seed_history_plan(&store, "readonly-lru")?;
        store
            .add_review(&history_review(
                "readonly-lru",
                "target-task",
                "target-review",
            ))
            .map_err(|error| error.to_string())?;
        store
            .add_review(&history_review(
                "readonly-lru",
                "other-task",
                "other-review",
            ))
            .map_err(|error| error.to_string())?;
        let (_, target_path, _) = store
            .shadow
            .history_paths_for_test("readonly-lru", "target-task")
            .map_err(|error| error.to_string())?;
        let (_, other_path, _) = store
            .shadow
            .history_paths_for_test("readonly-lru", "other-task")
            .map_err(|error| error.to_string())?;
        let review_directory = target_path
            .parent()
            .ok_or_else(|| "review segment has no directory".to_string())?
            .to_path_buf();
        drop(store);
        std::fs::remove_file(target_path).map_err(|error| error.to_string())?;
        std::fs::remove_file(other_path).map_err(|error| error.to_string())?;
        let original_permissions = std::fs::metadata(&review_directory)
            .map_err(|error| error.to_string())?
            .permissions();
        let mut readonly = original_permissions.clone();
        readonly.set_mode(0o555);
        std::fs::set_permissions(&review_directory, readonly).map_err(|error| error.to_string())?;
        let query = (|| {
            let reopened = TaskRuntimeStore::new_in_memory_with_shadow_root(&root)
                .map_err(|error| error.to_string())?;
            for task_id in ["target-task", "other-task", "target-task"] {
                if reopened
                    .list_reviews("readonly-lru", task_id)
                    .map_err(|error| error.to_string())?
                    .len()
                    != 1
                {
                    return Err(format!("readonly review fallback lost task {task_id}"));
                }
            }
            let replays = reopened
                .shadow
                .history_fallback_replay_count_for_test("readonly-lru")
                .map_err(|error| error.to_string())?;
            if replays != 2 {
                return Err(format!(
                    "readonly A/B/A review fallback replayed {replays} times"
                ));
            }
            Ok(())
        })();
        std::fs::set_permissions(&review_directory, original_permissions)
            .map_err(|error| error.to_string())?;
        query
    }

    #[test]
    fn todo_metadata_clears_across_revision_reset_running_and_restart() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("tasks");
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(&root)
            .map_err(|error| error.to_string())?;
        store
            .create_run(
                "reset-run",
                "test",
                "conversation",
                "root",
                DomainProfile::General,
                "reset metadata",
                "task",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        let mut plan = TaskPlan {
            plan_id: "reset-plan".to_string(),
            run_id: "reset-run".to_string(),
            revision: 1,
            domain_profile: DomainProfile::General,
            goal_revision: 1,
            goal_sha256: task_goal_sha256("reset metadata"),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Sequential,
            tasks: vec![PlanTask {
                id: "reset-task".to_string(),
                title: "Reset task".to_string(),
                description: "Verify metadata reset".to_string(),
                kind: PlanTaskKind::Investigation,
                agent_role: "old-owner".to_string(),
                domain_profile: DomainProfile::General,
                ..Default::default()
            }],
        };
        store
            .attach_plan_for_test(&plan)
            .map_err(|error| error.to_string())?;
        store
            .set_task_status(
                "reset-run",
                "reset-task",
                echo_agent::tasks::TaskStatus::Completed,
                Some("old-owner"),
                Some("old-summary"),
            )
            .map_err(|error| error.to_string())?;
        plan.revision = 2;
        store
            .commit_runtime_event(RuntimeJournalEvent::for_append(
                "reset-run",
                None,
                None,
                RuntimeEventKind::PlanRevisionCommitted,
                serde_json::json!({
                    "base_revision": 1,
                    "reason": "reset completed task",
                    "reset_task_ids": ["reset-task"],
                    "plan": plan.specification(),
                }),
            ))
            .map_err(|error| error.to_string())?;
        let pending = store
            .list_todos("reset-run")
            .map_err(|error| error.to_string())?
            .into_iter()
            .next()
            .ok_or_else(|| "reset Todo is missing".to_string())?;
        assert_eq!(pending.status, TodoStatus::Pending);
        assert!(pending.owner_agent.is_none());
        assert!(pending.started_at.is_none());
        assert!(pending.completed_at.is_none());
        assert!(pending.summary.is_none());
        store
            .set_task_status(
                "reset-run",
                "reset-task",
                echo_agent::tasks::TaskStatus::Running,
                Some("new-owner"),
                None,
            )
            .map_err(|error| error.to_string())?;
        let running = store
            .list_todos("reset-run")
            .map_err(|error| error.to_string())?
            .into_iter()
            .next()
            .ok_or_else(|| "running Todo is missing".to_string())?;
        assert_eq!(running.status, TodoStatus::Running);
        assert_eq!(running.owner_agent.as_deref(), Some("new-owner"));
        assert!(running.started_at.is_some());
        assert!(running.completed_at.is_none());
        assert!(running.summary.is_none());
        drop(store);
        let reopened = TaskRuntimeStore::new_in_memory_with_shadow_root(&root)
            .map_err(|error| error.to_string())?;
        let restarted = reopened
            .list_todos("reset-run")
            .map_err(|error| error.to_string())?
            .into_iter()
            .next()
            .ok_or_else(|| "restarted Todo is missing".to_string())?;
        assert_eq!(restarted.status, TodoStatus::Running);
        assert_eq!(restarted.owner_agent.as_deref(), Some("new-owner"));
        assert!(restarted.completed_at.is_none());
        assert!(restarted.summary.is_none());
        Ok(())
    }

    #[test]
    fn concurrent_public_queries_remain_coherent_with_incremental_appends() -> Result<(), String> {
        let (_temp, store, run_id) = seed_public_query_fixture(10_000)?;
        let store = std::sync::Arc::new(store);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(5));
        let mut readers = Vec::new();
        for _ in 0..4 {
            let reader_store = std::sync::Arc::clone(&store);
            let reader_run = run_id.clone();
            let reader_barrier = std::sync::Arc::clone(&barrier);
            readers.push(std::thread::spawn(move || -> Result<(), String> {
                reader_barrier.wait();
                for _ in 0..25 {
                    time_public_queries(&reader_store, &reader_run)?;
                }
                Ok(())
            }));
        }
        barrier.wait();
        for ordinal in 0..25 {
            store
                .note(&run_id, None, &format!("concurrent append {ordinal}"))
                .map_err(|error| error.to_string())?;
        }
        for reader in readers {
            reader
                .join()
                .map_err(|_| "public query reader panicked".to_string())??;
        }
        time_public_queries(&store, &run_id)?;
        Ok(())
    }

    #[test]
    fn projection_receipt_distinguishes_uncommitted_and_committed_degradation() -> Result<(), String>
    {
        let store = fresh().map_err(|error| error.to_string())?;
        store
            .create_run(
                "receipt-run",
                "test",
                "conversation",
                "root",
                DomainProfile::General,
                "receipt",
                "task",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        let before = store
            .list_events("receipt-run", 0)
            .map_err(|error| error.to_string())?
            .len();
        store.fail_next_cell_started_for_test();
        let uncommitted = store.record_background_cell_started(
            "receipt-run",
            "cell-uncommitted",
            "false",
            "hash-a",
            None,
            None,
            None,
        );
        assert!(uncommitted.is_err());
        assert_eq!(
            store
                .list_events("receipt-run", 0)
                .map_err(|error| error.to_string())?
                .len(),
            before
        );
        store.fail_next_cell_started_projection_for_test();
        let committed = store
            .record_background_cell_started(
                "receipt-run",
                "cell-committed",
                "true",
                "hash-b",
                None,
                None,
                None,
            )
            .map_err(|error| error.to_string())?;
        let ProjectionCommitReceipt::CommittedProjectionDegraded { seq, .. } = committed else {
            return Err("committed projection degradation lost its typed receipt".to_string());
        };
        assert!(seq > 0);
        assert_eq!(
            store
                .list_events("receipt-run", 0)
                .map_err(|error| error.to_string())?
                .iter()
                .filter(|event| event.event_type == RuntimeEventKind::BackgroundCellStarted)
                .count(),
            1
        );
        Ok(())
    }

    #[test]
    fn durability_receipt_preserves_batch_cell_and_reconciled_degradation() -> Result<(), String> {
        let store = fresh().map_err(|error| error.to_string())?;
        store
            .create_run(
                "durability-run",
                "test",
                "conversation",
                "root",
                DomainProfile::General,
                "durability",
                "task",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;

        store
            .shadow
            .fail_next_append_durability_for_test("durability-run")
            .map_err(|error| error.to_string())?;
        let degraded = store
            .commit_runtime_events_with_receipt(
                "durability-run",
                vec![RuntimeJournalEvent::for_append(
                    "durability-run",
                    None,
                    None,
                    RuntimeEventKind::Note,
                    serde_json::json!({"kind": "durability-batch"}),
                )],
            )
            .map_err(|error| error.to_string())?;
        let ProjectionCommitReceipt::CommittedProjectionDegraded { seq, detail } = degraded else {
            return Err("degraded journal batch was reported durable".to_string());
        };
        assert!(seq > 0);
        assert!(detail.contains("journal durability degraded"));

        store
            .shadow
            .reconcile_next_append_unconfirmed_for_test("durability-run")
            .map_err(|error| error.to_string())?;
        let reconciled = store
            .commit_runtime_events_with_receipt(
                "durability-run",
                vec![RuntimeJournalEvent::for_append(
                    "durability-run",
                    None,
                    None,
                    RuntimeEventKind::Note,
                    serde_json::json!({"kind": "reconciled-batch"}),
                )],
            )
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            reconciled,
            ProjectionCommitReceipt::CommittedProjectionDegraded { detail, .. }
                if detail.contains("journal durability unconfirmed")
        ));

        store
            .shadow
            .fail_next_append_durability_for_test("durability-run")
            .map_err(|error| error.to_string())?;
        let first = store
            .record_background_cell_started(
                "durability-run",
                "cell-durable",
                "command",
                "hash",
                None,
                None,
                None,
            )
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            first,
            ProjectionCommitReceipt::CommittedProjectionDegraded { detail, .. }
                if detail.contains("journal durability degraded")
        ));
        store
            .shadow
            .fail_next_durability_probe_for_test("durability-run")
            .map_err(|error| error.to_string())?;
        let still_degraded = store
            .record_background_cell_started(
                "durability-run",
                "cell-durable",
                "command",
                "hash",
                None,
                None,
                None,
            )
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            still_degraded,
            ProjectionCommitReceipt::CommittedProjectionDegraded { detail, .. }
                if detail.contains("journal durability degraded")
        ));
        let settled = store
            .record_background_cell_started(
                "durability-run",
                "cell-durable",
                "command",
                "hash",
                None,
                None,
                None,
            )
            .map_err(|error| error.to_string())?;
        assert!(matches!(settled, ProjectionCommitReceipt::Durable { .. }));

        store
            .shadow
            .fail_next_append_durability_for_test("durability-run")
            .map_err(|error| error.to_string())?;
        store
            .note("durability-run", None, "committed diagnostic boundary")
            .map_err(|error| error.to_string())?;
        assert_eq!(
            store
                .list_events("durability-run", 0)
                .map_err(|error| error.to_string())?
                .iter()
                .filter(|event| {
                    event.event_type == RuntimeEventKind::Note
                        && event
                            .payload
                            .get("message")
                            .and_then(serde_json::Value::as_str)
                            == Some("committed diagnostic boundary")
                })
                .count(),
            1
        );

        store
            .shadow
            .fail_history_cursor_writes_for_test("durability-run", 3)
            .map_err(|error| error.to_string())?;
        let history_degraded = store
            .record_background_cell_started(
                "durability-run",
                "cell-history",
                "command",
                "history-hash",
                None,
                None,
                None,
            )
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            history_degraded,
            ProjectionCommitReceipt::CommittedProjectionDegraded { detail, .. }
                if detail.contains("history projection degraded")
        ));
        let duplicate_degraded = store
            .record_background_cell_started(
                "durability-run",
                "cell-history",
                "command",
                "history-hash",
                None,
                None,
                None,
            )
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            duplicate_degraded,
            ProjectionCommitReceipt::CommittedProjectionDegraded { detail, .. }
                if detail.contains("history projection degraded")
        ));
        let duplicate_repaired = store
            .record_background_cell_started(
                "durability-run",
                "cell-history",
                "command",
                "history-hash",
                None,
                None,
                None,
            )
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            duplicate_repaired,
            ProjectionCommitReceipt::Durable { .. }
        ));

        let checkpoint = TaskRuntimeStore::classify_committed_projection(
            42,
            JournalDurabilityStatus::Confirmed,
            CheckpointApplyStatus::Degraded {
                error: "checkpoint-write".to_string(),
            },
            HistoryProjectionApplyStatus::Current,
            ProjectionCommitReceipt::Durable { seq: 42 },
        );
        assert!(matches!(
            checkpoint,
            ProjectionCommitReceipt::CommittedProjectionDegraded { seq: 42, detail }
                if detail.contains("checkpoint durability degraded")
        ));
        Ok(())
    }

    #[test]
    fn partial_history_failure_replays_without_duplicates_before_advancing_cursor()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("tasks");
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(&root)
            .map_err(|error| error.to_string())?;
        seed_history_plan(&store, "partial-history")?;
        let (artifact_path, review_path, cursor_path) = store
            .shadow
            .history_paths_for_test("partial-history", "target-task")
            .map_err(|error| error.to_string())?;
        let cursor_before = std::fs::read(&cursor_path).map_err(|error| error.to_string())?;
        store
            .shadow
            .fail_next_review_history_append_for_test("partial-history")
            .map_err(|error| error.to_string())?;
        let receipt = store
            .commit_runtime_events_with_receipt(
                "partial-history",
                vec![
                    artifact_history_event("partial-history", "artifact-1"),
                    review_history_event("partial-history", "target-task", "review-1"),
                ],
            )
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            receipt,
            ProjectionCommitReceipt::CommittedProjectionDegraded { detail, .. }
                if detail.contains("history projection degraded")
        ));
        assert_eq!(
            std::fs::read(&cursor_path).map_err(|error| error.to_string())?,
            cursor_before
        );
        assert_eq!(line_count(&artifact_path)?, 1);
        assert!(!review_path.exists());
        drop(store);

        let reopened = TaskRuntimeStore::new_in_memory_with_shadow_root(&root)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            reopened
                .list_artifacts("partial-history")
                .map_err(|error| error.to_string())?
                .len(),
            1
        );
        assert_eq!(
            reopened
                .list_reviews("partial-history", "target-task")
                .map_err(|error| error.to_string())?
                .len(),
            1
        );
        assert_eq!(line_count(&artifact_path)?, 1);
        assert_eq!(line_count(&review_path)?, 1);
        Ok(())
    }

    #[test]
    fn missing_history_segments_recover_old_and_new_facts_from_journal() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("tasks");
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(&root)
            .map_err(|error| error.to_string())?;
        seed_history_plan(&store, "missing-history")?;
        store
            .add_artifact(&history_artifact("missing-history", "artifact-old"))
            .map_err(|error| error.to_string())?;
        store
            .add_review(&history_review(
                "missing-history",
                "target-task",
                "review-old",
            ))
            .map_err(|error| error.to_string())?;
        let (artifact_path, review_path, cursor_path) = store
            .shadow
            .history_paths_for_test("missing-history", "target-task")
            .map_err(|error| error.to_string())?;
        std::fs::remove_file(&artifact_path).map_err(|error| error.to_string())?;
        assert_eq!(
            store
                .list_artifacts("missing-history")
                .map_err(|error| error.to_string())?
                .len(),
            1
        );
        let repaired_artifact_cursor = history_cursor_sequence(&cursor_path)?;
        store
            .add_artifact(&history_artifact("missing-history", "artifact-new"))
            .map_err(|error| error.to_string())?;
        let artifacts = store
            .list_artifacts("missing-history")
            .map_err(|error| error.to_string())?;
        assert_eq!(artifacts.len(), 2);
        assert_eq!(line_count(&artifact_path)?, 2);
        assert!(history_cursor_sequence(&cursor_path)? > repaired_artifact_cursor);
        std::fs::remove_file(&artifact_path).map_err(|error| error.to_string())?;
        let append_repair = store
            .commit_runtime_events_with_receipt(
                "missing-history",
                vec![artifact_history_event(
                    "missing-history",
                    "artifact-after-unlink",
                )],
            )
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            append_repair,
            ProjectionCommitReceipt::Durable { .. }
        ));
        assert_eq!(line_count(&artifact_path)?, 3);

        std::fs::remove_file(&review_path).map_err(|error| error.to_string())?;
        assert_eq!(
            store
                .list_reviews("missing-history", "target-task")
                .map_err(|error| error.to_string())?
                .len(),
            1
        );
        let repaired_review_cursor = history_cursor_sequence(&cursor_path)?;
        store
            .add_review(&history_review(
                "missing-history",
                "target-task",
                "review-new",
            ))
            .map_err(|error| error.to_string())?;
        let reviews = store
            .list_reviews("missing-history", "target-task")
            .map_err(|error| error.to_string())?;
        assert_eq!(reviews.len(), 2);
        assert_eq!(line_count(&review_path)?, 2);
        assert!(history_cursor_sequence(&cursor_path)? > repaired_review_cursor);
        std::fs::remove_file(&review_path).map_err(|error| error.to_string())?;
        let append_repair = store
            .commit_runtime_events_with_receipt(
                "missing-history",
                vec![review_history_event(
                    "missing-history",
                    "target-task",
                    "review-after-unlink",
                )],
            )
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            append_repair,
            ProjectionCommitReceipt::Durable { .. }
        ));
        assert_eq!(line_count(&review_path)?, 3);

        std::fs::write(&artifact_path, b"{corrupt artifact segment\n")
            .map_err(|error| error.to_string())?;
        assert_eq!(
            store
                .list_artifacts("missing-history")
                .map_err(|error| error.to_string())?
                .len(),
            3
        );
        assert_eq!(line_count(&artifact_path)?, 3);
        std::fs::write(&review_path, b"{corrupt review segment\n")
            .map_err(|error| error.to_string())?;
        assert_eq!(
            store
                .list_reviews("missing-history", "target-task")
                .map_err(|error| error.to_string())?
                .len(),
            3
        );
        assert_eq!(line_count(&review_path)?, 3);
        Ok(())
    }

    #[test]
    fn valid_jsonl_prefix_and_empty_segment_self_heal_after_restart() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("tasks");
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(&root)
            .map_err(|error| error.to_string())?;
        seed_history_plan(&store, "truncated-history")?;
        for id in ["artifact-1", "artifact-2"] {
            store
                .add_artifact(&history_artifact("truncated-history", id))
                .map_err(|error| error.to_string())?;
        }
        for id in ["review-1", "review-2"] {
            store
                .add_review(&history_review("truncated-history", "target-task", id))
                .map_err(|error| error.to_string())?;
        }
        let (artifact_path, review_path, _) = store
            .shadow
            .history_paths_for_test("truncated-history", "target-task")
            .map_err(|error| error.to_string())?;
        drop(store);

        retain_first_jsonl_record(&artifact_path)?;
        let reopened = TaskRuntimeStore::new_in_memory_with_shadow_root(&root)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            reopened
                .list_artifacts("truncated-history")
                .map_err(|error| error.to_string())?
                .len(),
            2
        );
        assert_eq!(line_count(&artifact_path)?, 2);
        drop(reopened);

        std::fs::write(&review_path, b"").map_err(|error| error.to_string())?;
        let restarted = TaskRuntimeStore::new_in_memory_with_shadow_root(&root)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            restarted
                .list_reviews("truncated-history", "target-task")
                .map_err(|error| error.to_string())?
                .len(),
            2
        );
        assert_eq!(line_count(&review_path)?, 2);
        Ok(())
    }

    #[test]
    fn committed_projection_degradation_preserves_all_mutation_outcomes() -> Result<(), String> {
        let store = fresh().map_err(|error| error.to_string())?;
        store
            .create_run(
                "transition-run",
                "test",
                "conversation",
                "root",
                DomainProfile::General,
                "transition",
                "task",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store.fail_next_runtime_mutation_projection_for_test();
        assert_eq!(
            store
                .transition_run("transition-run", TaskRunStatus::Running)
                .map_err(|error| error.to_string())?
                .status,
            TaskRunStatus::Running
        );
        store.fail_next_runtime_mutation_projection_for_test();
        assert_eq!(
            store
                .finalize_run("transition-run", TaskRunStatus::Failed, Some("terminal"))
                .map_err(|error| error.to_string())?
                .status,
            TaskRunStatus::Failed
        );

        store
            .create_run(
                "pause-goal-run",
                "test",
                "conversation",
                "root-2",
                DomainProfile::General,
                "old goal",
                "task",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run("pause-goal-run", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store.fail_next_runtime_mutation_projection_for_test();
        assert!(
            store
                .request_pause("pause-goal-run")
                .map_err(|error| error.to_string())?
        );
        store.fail_next_runtime_mutation_projection_for_test();
        let updated = store
            .update_run_goal(
                "pause-goal-run",
                1,
                "new goal",
                "user revised goal",
                RunGoalActorSource::Gui,
            )
            .map_err(|error| error.to_string())?;
        assert_eq!(updated.goal, "new goal");
        assert_eq!(updated.goal_revision, 2);

        store
            .create_run(
                "plan-run",
                "test",
                "conversation",
                "root-3",
                DomainProfile::General,
                "plan",
                "task",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store.fail_next_runtime_mutation_projection_for_test();
        store
            .attach_plan_for_test(&TaskPlan {
                plan_id: "plan-id".to_string(),
                run_id: "plan-run".to_string(),
                revision: 1,
                domain_profile: DomainProfile::General,
                goal_revision: 1,
                goal_sha256: task_goal_sha256("plan"),
                assumptions: Vec::new(),
                risks: Vec::new(),
                execution_mode: ExecutionMode::Sequential,
                tasks: vec![PlanTask {
                    id: "task-a".to_string(),
                    title: "Task A".to_string(),
                    description: "Do A".to_string(),
                    kind: PlanTaskKind::Investigation,
                    agent_role: "subagent".to_string(),
                    domain_profile: DomainProfile::General,
                    ..Default::default()
                }],
            })
            .map_err(|error| error.to_string())?;
        assert!(
            store
                .get_plan("plan-run")
                .map_err(|error| error.to_string())?
                .is_some()
        );

        store
            .create_run(
                "turn-run",
                "test",
                "conversation",
                "root-4",
                DomainProfile::General,
                "turn",
                "task",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run("turn-run", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store
            .configure_run_continuation("turn-run", true, false, None, None)
            .map_err(|error| error.to_string())?;
        store.fail_next_runtime_mutation_projection_for_test();
        assert!(matches!(
            store
                .claim_run_turn(
                    "turn-run",
                    "turn-1",
                    RunTurnOrigin::User,
                    TurnVisibility::Visible,
                )
                .map_err(|error| error.to_string())?,
            RunTurnClaimOutcome::Started(_)
        ));
        assert_eq!(
            store
                .list_events("turn-run", 0)
                .map_err(|error| error.to_string())?
                .iter()
                .filter(|event| event.event_type == RuntimeEventKind::RunTurnStarted)
                .count(),
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn direct_completion_survives_committed_projection_degradation() -> Result<(), String> {
        let store = std::sync::Arc::new(fresh().map_err(|error| error.to_string())?);
        let run_id = "direct-degraded";
        store
            .create_run(
                run_id,
                "test",
                "conversation",
                "root",
                DomainProfile::General,
                "direct answer",
                "agent_autonomous",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run(run_id, TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store
            .configure_run_continuation(run_id, true, false, None, None)
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            store
                .claim_run_turn(
                    run_id,
                    "turn-direct",
                    RunTurnOrigin::User,
                    TurnVisibility::Visible,
                )
                .map_err(|error| error.to_string())?,
            RunTurnClaimOutcome::Started(_)
        ));
        let task_id = "direct-answer";
        let plan = TaskPlan {
            plan_id: format!("plan:{run_id}"),
            run_id: run_id.to_string(),
            revision: 0,
            domain_profile: DomainProfile::General,
            goal_revision: 1,
            goal_sha256: task_goal_sha256("direct answer"),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Sequential,
            tasks: vec![PlanTask {
                id: task_id.to_string(),
                title: "Direct answer".to_string(),
                description: "direct answer".to_string(),
                kind: PlanTaskKind::Summary,
                agent_role: "primary-agent".to_string(),
                domain_profile: DomainProfile::General,
                ..Default::default()
            }],
        };
        let summary = TaskExecutionSummary {
            run_id: run_id.to_string(),
            task_id: task_id.to_string(),
            subagent_name: "primary-agent".to_string(),
            result: SubagentTaskResult::terminal(
                SubagentRunStatus::Completed,
                "complete answer",
                Vec::new(),
            ),
            decisions: Vec::new(),
            next_implications: Vec::new(),
            suggested_tasks: Vec::new(),
            created_at: Utc::now(),
        };
        store.fail_next_runtime_mutation_projection_for_test();
        super::super::revisioned_adapter::commit_eko_direct_completion(
            store.clone(),
            plan,
            summary,
            "complete answer".to_string(),
        )
        .await
        .map_err(|error| error.to_string())?;
        let todos = store
            .list_todos(run_id)
            .map_err(|error| error.to_string())?;
        assert_eq!(todos.len(), 1);
        assert_eq!(
            todos.first().map(|todo| todo.status),
            Some(TodoStatus::Completed)
        );
        assert_eq!(
            store
                .list_events(run_id, 0)
                .map_err(|error| error.to_string())?
                .iter()
                .filter(|event| event.event_type == RuntimeEventKind::PlanRevisionCommitted)
                .count(),
            1
        );
        Ok(())
    }

    /// Helper: a minimal `PlanTask` body with the given id and sane defaults,
    /// for the cycle test above (avoids repeating the full struct literal).
    fn sample_task_body(id: &str) -> PlanTask {
        PlanTask {
            id: id.to_string(),
            title: format!("task {id}"),
            description: format!("do {id}"),
            kind: PlanTaskKind::Investigation,
            agent_role: "explorer".to_string(),
            domain_profile: DomainProfile::General,
            depends_on: Vec::new(),
            parallel_group: None,
            execution_target: None,
            files: Vec::new(),
            allowed_tools: vec!["read_file".to_string()],
            required_artifacts: Vec::new(),
            execution_checks: Vec::new(),
            acceptance_criteria: Vec::new(),
            retry_count: 0,
            max_retries: 3,
            failure_fingerprint: None,
            status: echo_agent::tasks::TaskStatus::Pending,
            claim: None,
            sort_order: 0,
        }
    }
}
