// File-backed canonical store for the TaskRuntime.
//
// The file system (`events.jsonl` plus deterministic `plan.json` and
// `run-state.json` projections) is the source of truth for task/plan data and
// runtime usage. Conversation-replay events remain in memory. No SQLite
// dependency.
//
// Every state mutation appends a [`RuntimeTaskEvent`] to `events.jsonl` and
// refreshes only the affected projection through the shared checkpoint-aware
// event fold.

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
            let mut extension: EkoTaskExtension = task
                .spec
                .extension_as()
                .map_err(StoreError::InvalidPlan)?;
            extension.sort_order = i64::try_from(position).unwrap_or(i64::MAX);
            task.spec = task.spec.clone().with_extension(extension).map_err(|error| {
                StoreError::InvalidPlan(format!("task extension update failed: {error}"))
            })?;
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
            .push(
                EkoTaskSpec::try_from(task.spec.clone()).map_err(StoreError::InvalidPlan)?,
            );
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
