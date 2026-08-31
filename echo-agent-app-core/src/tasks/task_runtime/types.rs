//! TaskRuntime data model — the canonical types for complex-task execution.
//!
//! These types live in the application layer because `DomainProfile`, UI todo
//! projections, conversations, artifacts, reviews, and file persistence are
//! EKO product concerns. The framework owns the product-neutral runtime task
//! view and the only dynamic DAG execution loop; this module adapts EKO plan
//! and execution records to that kernel.
//!
//! Naming note: the framework already re-exports a `TaskEvent`
//! (`crate::tasks::TaskEvent`). This module's event type is therefore named
//! [`RuntimeTaskEvent`] and is stored in EKO's append-only file event stream;
//! we never shadow the framework type.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ts_rs::TS;

// ── Domain profile ──────────────────────────────────────────────────────

/// Cross-domain profile that customizes plan templates, subagent roles,
/// allowed tools, review checklists, and verification standards.
///
/// Selection order (resolved by the planning runtime, PR 2):
/// 1. User-selected profile in GUI
/// 2. Workspace default profile
/// 3. Intent router inference
/// 4. `General` fallback
///
/// `General` is always first-class because many tasks declare no domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "DomainProfile")]
pub enum DomainProfile {
    #[default]
    General,
    AiCoding,
    DataAnalysis,
    AcademicResearch,
    MedicalResearch,
}

impl DomainProfile {
    /// Stable lowercase identifier persisted in TaskRun files.
    pub fn as_str(&self) -> &'static str {
        match self {
            DomainProfile::General => "general",
            DomainProfile::AiCoding => "ai_coding",
            DomainProfile::DataAnalysis => "data_analysis",
            DomainProfile::AcademicResearch => "academic_research",
            DomainProfile::MedicalResearch => "medical_research",
        }
    }

    #[allow(clippy::should_implement_trait)] // inherent helper returning Option; not the FromStr trait
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "general" => DomainProfile::General,
            "ai_coding" => DomainProfile::AiCoding,
            "data_analysis" => DomainProfile::DataAnalysis,
            "academic_research" => DomainProfile::AcademicResearch,
            "medical_research" => DomainProfile::MedicalResearch,
            _ => return None,
        })
    }
}

// ── Execution mode ──────────────────────────────────────────────────────

/// How a plan executes after it is created.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "ExecutionMode")]
pub enum ExecutionMode {
    /// Execute sequentially, one plan task at a time.
    Sequential,
    /// Execute parallel groups concurrently within the configured limits.
    #[default]
    Parallel,
}

impl ExecutionMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ExecutionMode::Sequential => "sequential",
            ExecutionMode::Parallel => "parallel",
        }
    }

    #[allow(clippy::should_implement_trait)] // inherent helper returning Option; not the FromStr trait
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "sequential" => ExecutionMode::Sequential,
            "parallel" => ExecutionMode::Parallel,
            _ => return None,
        })
    }
}

/// Result of a run-level pause or cancellation request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, rename = "TaskRunControlReceipt")]
pub struct TaskRunControlReceipt {
    /// `true` when this request changed or signalled the run. `false` means
    /// the addressed run was already terminal or otherwise required no action.
    pub success: bool,
    pub run_id: String,
}

/// Which canonical resume path accepted a TaskRun.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "TaskRunResumeKind")]
pub enum TaskRunResumeKind {
    Resumed,
    ContinuationResumed,
}

/// Typed result of resuming a TaskRun.
///
/// `turn_id` is present when the resume launches a foreground continuation
/// turn. Planned DAG resumes have no foreground turn identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, rename = "TaskRunResumeReceipt")]
pub struct TaskRunResumeReceipt {
    pub kind: TaskRunResumeKind,
    pub run_id: String,
    pub turn_id: Option<String>,
}

/// Which canonical retry path accepted a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "TaskRetryKind")]
pub enum TaskRetryKind {
    RetryScheduled,
    RecoveryRetryRecorded,
}

/// Typed result of retrying one exact task attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, rename = "TaskRetryReceipt")]
pub struct TaskRetryReceipt {
    pub kind: TaskRetryKind,
    pub run_id: String,
    pub task_id: String,
    pub next_attempt: Option<u32>,
}

// ── Attended mode ───────────────────────────────────────────────────────

/// Whether a human is present during a run. Unattended runs apply the
/// configured write preflight and fail task errors without waiting for input;
/// both modes can still be explicitly paused through the shared control path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "AttendedMode")]
pub enum AttendedMode {
    /// Chat-triggered run — a human is present and can answer tool HITL.
    #[default]
    Attended,
    /// Cron / IM triggered run — no human, must be self-contained.
    Unattended,
}

impl AttendedMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            AttendedMode::Attended => "attended",
            AttendedMode::Unattended => "unattended",
        }
    }

    #[allow(clippy::should_implement_trait)] // inherent helper returning Option; not the FromStr trait
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "attended" => AttendedMode::Attended,
            "unattended" => AttendedMode::Unattended,
            _ => return None,
        })
    }
}

// ── Unattended write mode (D7 stage 2) ───────────────────────────────────

/// Write policy for an unattended (cron / IM) plan run. D7 stage 2 lifts the
/// stage-1 `ReadOnlyPlanNoShell` blanket write ban: writes become possible,
/// with safety coming from isolation rather than prohibition.
///
/// * `Worktree` (default) — direct workspace mutation tools are hidden from
///   the unattended planning Agent. Write PlanTasks create an isolated
///   Subagent worktree only when the writer is dispatched, then pass through
///   the shared review/integration stage. Read-only work creates no worktree.
/// * `Disabled` — stage-1 behaviour: write tasks are rejected by preflight.
/// * `InPlace` — user explicitly accepts the risk; writes go directly to the
///   main workspace with no isolation. Logged as a warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "UnattendedWriteMode")]
pub enum UnattendedWriteMode {
    #[default]
    Worktree,
    Disabled,
    InPlace,
}

impl UnattendedWriteMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            UnattendedWriteMode::Worktree => "worktree",
            UnattendedWriteMode::Disabled => "disabled",
            UnattendedWriteMode::InPlace => "in_place",
        }
    }

    #[allow(clippy::should_implement_trait)] // inherent helper returning Option; not the FromStr trait
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "worktree" => UnattendedWriteMode::Worktree,
            "disabled" => UnattendedWriteMode::Disabled,
            "in_place" => UnattendedWriteMode::InPlace,
            _ => return None,
        })
    }

    /// `true` when write task kinds are permitted under this mode. The
    /// preflight gate loosens accordingly — safety comes from isolation
    /// (`Worktree`) or explicit user consent (`InPlace`), not from banning.
    pub fn writes_allowed(&self) -> bool {
        matches!(
            self,
            UnattendedWriteMode::Worktree | UnattendedWriteMode::InPlace
        )
    }
}

// ── Plan-task kind ──────────────────────────────────────────────────────

/// Operation class for a single plan task. The scheduler (PR 3) uses this
/// to decide parallelism and locking: read-only kinds parallelize freely,
/// mutating kinds serialize behind file/workspace locks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "PlanTaskKind")]
pub enum PlanTaskKind {
    /// Read-only repository / data exploration or review.
    ReadOnlyReview,
    /// Read-only search, grep, file reads, hypothesis investigation.
    Investigation,
    /// Read-only verification plan (test layout, repro plan).
    TestPlan,
    /// Scoped code or data change (default serialized).
    Implementation,
    /// Focused root-cause investigation that may read widely.
    Debugging,
    /// Spec / quality review of another task's output.
    Review,
    /// Final synthesis / report.
    Summary,
    /// Shell / build / test execution (limited concurrency, may mutate).
    Verification,
}

impl PlanTaskKind {
    /// `true` when the task does not mutate workspace state and is therefore
    /// safe to run in parallel with other read-only tasks.
    pub fn is_read_only(&self) -> bool {
        matches!(
            self,
            PlanTaskKind::ReadOnlyReview
                | PlanTaskKind::Investigation
                | PlanTaskKind::TestPlan
                | PlanTaskKind::Review
                | PlanTaskKind::Summary
        )
    }

    /// `true` when the task kind is allowed in an unattended (cron/IM) run
    /// under the `ReadOnlyPlanNoShell` profile. This is stricter than
    /// `is_read_only()`: `TestPlan` and `Review` are excluded because they
    /// may involve test execution or output modification in practice.
    /// This whitelist is the **unattended authorisation** boundary, not the
    /// concurrency/parallelism signal (which `is_read_only()` provides).
    pub fn is_unattended_readonly_allowed(&self) -> bool {
        matches!(
            self,
            PlanTaskKind::ReadOnlyReview | PlanTaskKind::Investigation | PlanTaskKind::Summary
        )
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            PlanTaskKind::ReadOnlyReview => "read_only_review",
            PlanTaskKind::Investigation => "investigation",
            PlanTaskKind::TestPlan => "test_plan",
            PlanTaskKind::Implementation => "implementation",
            PlanTaskKind::Debugging => "debugging",
            PlanTaskKind::Review => "review",
            PlanTaskKind::Summary => "summary",
            PlanTaskKind::Verification => "verification",
        }
    }

    #[allow(clippy::should_implement_trait)] // inherent helper returning Option; not the FromStr trait
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "read_only_review" => PlanTaskKind::ReadOnlyReview,
            "investigation" => PlanTaskKind::Investigation,
            "test_plan" => PlanTaskKind::TestPlan,
            "implementation" => PlanTaskKind::Implementation,
            "debugging" => PlanTaskKind::Debugging,
            "review" => PlanTaskKind::Review,
            "summary" => PlanTaskKind::Summary,
            "verification" => PlanTaskKind::Verification,
            _ => return None,
        })
    }
}

// ── Todo status ─────────────────────────────────────────────────────────

/// Read-only UI projection of a canonical framework task status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "TodoStatus")]
pub enum TodoStatus {
    #[default]
    Pending,
    Running,
    Blocked,
    Completed,
    Failed,
    Cancelled,
    TimedOut,
    Skipped,
}

impl TodoStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TodoStatus::Pending => "pending",
            TodoStatus::Running => "running",
            TodoStatus::Blocked => "blocked",
            TodoStatus::Completed => "completed",
            TodoStatus::Failed => "failed",
            TodoStatus::Cancelled => "cancelled",
            TodoStatus::TimedOut => "timed_out",
            TodoStatus::Skipped => "skipped",
        }
    }

    /// Project richer framework lifecycle states into the deliberately smaller
    /// Todo badge vocabulary. This is the only TaskStatus -> TodoStatus path.
    pub fn project_task_status(status: &echo_agent::tasks::TaskStatus) -> Self {
        match status {
            echo_agent::tasks::TaskStatus::Pending => TodoStatus::Pending,
            echo_agent::tasks::TaskStatus::Running
            | echo_agent::tasks::TaskStatus::Retrying { .. } => TodoStatus::Running,
            echo_agent::tasks::TaskStatus::Blocked(_) => TodoStatus::Blocked,
            echo_agent::tasks::TaskStatus::Paused(_) => TodoStatus::Pending,
            echo_agent::tasks::TaskStatus::Completed => TodoStatus::Completed,
            echo_agent::tasks::TaskStatus::Failed(_) => TodoStatus::Failed,
            echo_agent::tasks::TaskStatus::Cancelled => TodoStatus::Cancelled,
            echo_agent::tasks::TaskStatus::TimedOut { .. } => TodoStatus::TimedOut,
            echo_agent::tasks::TaskStatus::Skipped => TodoStatus::Skipped,
        }
    }
}

// ── Run status (state machine) ──────────────────────────────────────────

/// Lifecycle status of a [`TaskRun`]. The GUI must render from these states
/// and `RuntimeTaskEvent`s, never from local guesses.
///
/// Allowed transitions (see [`TaskRunStatus::can_transition_to`]):
///
/// ```text
/// Pending → Running → Completed
///              │  ↘
///           Paused  Failed → Running (retry)
///              │
///           Cancelled
/// ```
///
/// 极简 6 态:plan 审批不进状态机;Paused 只表达用户中断、进程恢复或可恢复失败。
/// 删去了 Planning/AwaitingPlanApproval/Ready/WaitingApproval/WaitingInput/Suspended/Cancelling
/// —— 这些交互语义由工具/HITL 和事件流承载,不进入 run 状态机。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "TaskRunStatus")]
pub enum TaskRunStatus {
    #[default]
    Pending,
    Running,
    Paused,
    Cancelled,
    Failed,
    Completed,
}

impl TaskRunStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskRunStatus::Pending => "pending",
            TaskRunStatus::Running => "running",
            TaskRunStatus::Paused => "paused",
            TaskRunStatus::Cancelled => "cancelled",
            TaskRunStatus::Failed => "failed",
            TaskRunStatus::Completed => "completed",
        }
    }

    #[allow(clippy::should_implement_trait)] // inherent helper returning Option; not the FromStr trait
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "pending" => TaskRunStatus::Pending,
            "running" => TaskRunStatus::Running,
            "paused" => TaskRunStatus::Paused,
            "cancelled" => TaskRunStatus::Cancelled,
            "failed" => TaskRunStatus::Failed,
            "completed" => TaskRunStatus::Completed,
            _ => return None,
        })
    }

    /// Whether transitioning from `self` to `next` is allowed by the
    /// state machine defined above.
    pub fn can_transition_to(&self, next: TaskRunStatus) -> bool {
        use TaskRunStatus::*;
        match self {
            Pending => matches!(next, Running | Cancelled),
            Running => matches!(next, Paused | Failed | Completed | Cancelled),
            Paused => matches!(next, Running | Cancelled),
            Failed => matches!(next, Running | Cancelled),
            // 终态
            Cancelled | Completed => false,
        }
    }
}

// ── Review outcome / artifact kind / event type ─────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "ReviewOutcome")]
pub enum ReviewOutcome {
    Pass,
    NeedsFix,
    Blocked,
}

impl ReviewOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            ReviewOutcome::Pass => "pass",
            ReviewOutcome::NeedsFix => "needs_fix",
            ReviewOutcome::Blocked => "blocked",
        }
    }
    #[allow(clippy::should_implement_trait)] // inherent helper returning Option; not the FromStr trait
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "pass" => ReviewOutcome::Pass,
            "needs_fix" => ReviewOutcome::NeedsFix,
            "blocked" => ReviewOutcome::Blocked,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "ArtifactKind")]
pub enum ArtifactKind {
    File,
    Report,
    Chart,
    Notebook,
    EvidenceTable,
    Trace,
    Other,
}

impl ArtifactKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ArtifactKind::File => "file",
            ArtifactKind::Report => "report",
            ArtifactKind::Chart => "chart",
            ArtifactKind::Notebook => "notebook",
            ArtifactKind::EvidenceTable => "evidence_table",
            ArtifactKind::Trace => "trace",
            ArtifactKind::Other => "other",
        }
    }
    #[allow(clippy::should_implement_trait)] // inherent helper returning Option; not the FromStr trait
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "file" => ArtifactKind::File,
            "report" => ArtifactKind::Report,
            "chart" => ArtifactKind::Chart,
            "notebook" => ArtifactKind::Notebook,
            "evidence_table" => ArtifactKind::EvidenceTable,
            "trace" => ArtifactKind::Trace,
            "other" => ArtifactKind::Other,
            _ => return None,
        })
    }
}

/// Structured event emitted at every run / task / step boundary.
///
/// Every state transition must write one of these inside the same
/// persistence transaction as the state update (see `store/runtime.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "RuntimeEventKind")]
pub enum RuntimeEventKind {
    RunCreated,
    /// The user explicitly replaced the sole authoritative TaskRun Goal.
    RunGoalUpdated,
    /// Existing evidence was detached from an older Goal revision.
    RequirementEvidenceInvalidated,
    /// Unchanged evidence was explicitly rebound after a Goal-aware plan update.
    RequirementEvidenceRevalidated,
    /// A local user explicitly accepted skipping one exact Goal requirement.
    RequirementSkipped,
    RunStarted,
    RunCompleted,
    RunFailed,
    RunStatusChanged,
    /// User-uploaded attachments were bound to this run (so plan-level
    /// subagents can see the same images/files as the main agent).
    RunAttachmentsUpdated,
    PlanRevisionCommitted,
    TaskStarted,
    TaskCompleted,
    TaskFailed,
    TaskCancelled,
    TaskTimedOut,
    TaskSkipped,
    TaskBlocked,
    /// A canonical task execution changed to Pending, Retrying, or Paused.
    TaskStatusChanged,
    #[serde(rename = "started")]
    Started,
    #[serde(rename = "running")]
    Running,
    #[serde(rename = "completed")]
    Completed,
    #[serde(rename = "failed")]
    Failed,
    #[serde(rename = "cancelled")]
    Cancelled,
    #[serde(rename = "timed_out")]
    TimedOut,
    SubagentAssigned,
    SubagentReleased,
    /// A user instruction was durably accepted for one exact Subagent attempt.
    SubagentGuidanceQueued,
    /// The framework tracked mailbox confirmed that a live instruction was
    /// inserted into the active turn's input queue.
    SubagentGuidanceMailboxAccepted,
    /// The tracked instruction reached the target model context.
    SubagentGuidanceDrained,
    /// The owning target turn reached its typed terminal outcome.
    SubagentGuidanceSettled,
    /// The exact target rejected an instruction; it was not rerouted.
    SubagentGuidanceRejected,
    /// A user requested cancellation of one exact Subagent attempt.
    SubagentInterruptRequested,
    /// The exact-attempt interrupt reached a typed framework outcome.
    SubagentInterruptSettled,
    IsolationObserved,
    ThinkingStarted,
    ThinkingDelta,
    ThinkingEnded,
    TokenDelta,
    Usage,
    ToolStarted,
    ToolOutput,
    ToolCompleted,
    ToolFailed,
    ArtifactProduced,
    MergeStarted,
    MergeCompleted,
    MergeFailed,
    ReviewPassed,
    ReviewNeedsFix,
    ReviewBlocked,
    CircuitBreakerTripped,
    RecoveryBlocked,
    RecoveryResolved,
    /// A process-scoped background command cell was launched for this run.
    BackgroundCellStarted,
    /// The command cell reached a terminal state and its result was captured.
    BackgroundCellFinished,
    /// Long-horizon continuation policy was created or updated for this run.
    RunContinuationConfigured,
    /// One finite primary-Agent turn claimed the run.
    RunTurnStarted,
    /// One provider usage event was durably accounted exactly once.
    RunTurnUsageAccounted,
    /// The active primary-Agent turn crossed a context-compaction boundary.
    RunTurnCompacted,
    /// One finite primary-Agent turn reached a terminal outcome.
    RunTurnFinished,
    /// A retryable provider failure scheduled the next finite RunTurn.
    RunProviderRetryScheduled,
    /// Automatic continuation is temporarily yielding to user/control input.
    RunContinuationDeferred,
    /// A previous continuation deferral was cleared.
    RunContinuationResumed,
    /// Structured explanation for a recoverable Paused run.
    RunPauseReasonChanged,
    RunCancelled,
    Note,
}

impl RuntimeEventKind {
    pub fn as_str(&self) -> &'static str {
        use RuntimeEventKind::*;
        match self {
            RunCreated => "run_created",
            RunGoalUpdated => "run_goal_updated",
            RequirementEvidenceInvalidated => "requirement_evidence_invalidated",
            RequirementEvidenceRevalidated => "requirement_evidence_revalidated",
            RequirementSkipped => "requirement_skipped",
            RunStarted => "run_started",
            RunCompleted => "run_completed",
            RunFailed => "run_failed",
            RunStatusChanged => "run_status_changed",
            RunAttachmentsUpdated => "run_attachments_updated",
            PlanRevisionCommitted => "plan_revision_committed",
            TaskStarted => "task_started",
            TaskCompleted => "task_completed",
            TaskFailed => "task_failed",
            TaskCancelled => "task_cancelled",
            TaskTimedOut => "task_timed_out",
            TaskSkipped => "task_skipped",
            TaskBlocked => "task_blocked",
            TaskStatusChanged => "task_status_changed",
            Started => "started",
            Running => "running",
            Completed => "completed",
            Failed => "failed",
            Cancelled => "cancelled",
            TimedOut => "timed_out",
            SubagentAssigned => "subagent_assigned",
            SubagentReleased => "subagent_released",
            SubagentGuidanceQueued => "subagent_guidance_queued",
            SubagentGuidanceMailboxAccepted => "subagent_guidance_mailbox_accepted",
            SubagentGuidanceDrained => "subagent_guidance_drained",
            SubagentGuidanceSettled => "subagent_guidance_settled",
            SubagentGuidanceRejected => "subagent_guidance_rejected",
            SubagentInterruptRequested => "subagent_interrupt_requested",
            SubagentInterruptSettled => "subagent_interrupt_settled",
            IsolationObserved => "isolation_observed",
            ThinkingStarted => "thinking_started",
            ThinkingDelta => "thinking_delta",
            ThinkingEnded => "thinking_ended",
            TokenDelta => "token_delta",
            Usage => "usage",
            ToolStarted => "tool_started",
            ToolOutput => "tool_output",
            ToolCompleted => "tool_completed",
            ToolFailed => "tool_failed",
            ArtifactProduced => "artifact_produced",
            MergeStarted => "merge_started",
            MergeCompleted => "merge_completed",
            MergeFailed => "merge_failed",
            ReviewPassed => "review_passed",
            ReviewNeedsFix => "review_needs_fix",
            ReviewBlocked => "review_blocked",
            CircuitBreakerTripped => "circuit_breaker_tripped",
            RecoveryBlocked => "recovery_blocked",
            RecoveryResolved => "recovery_resolved",
            BackgroundCellStarted => "background_cell_started",
            BackgroundCellFinished => "background_cell_finished",
            RunContinuationConfigured => "run_continuation_configured",
            RunTurnStarted => "run_turn_started",
            RunTurnUsageAccounted => "run_turn_usage_accounted",
            RunTurnCompacted => "run_turn_compacted",
            RunTurnFinished => "run_turn_finished",
            RunProviderRetryScheduled => "run_provider_retry_scheduled",
            RunContinuationDeferred => "run_continuation_deferred",
            RunContinuationResumed => "run_continuation_resumed",
            RunPauseReasonChanged => "run_pause_reason_changed",
            RunCancelled => "run_cancelled",
            Note => "note",
        }
    }

    /// Events that interactive surfaces should render as explicit lifecycle
    /// notices instead of treating as high-volume progress.
    pub fn is_attention_event(self) -> bool {
        matches!(
            self,
            RuntimeEventKind::RunGoalUpdated
                | RuntimeEventKind::RequirementEvidenceInvalidated
                | RuntimeEventKind::RequirementSkipped
                | RuntimeEventKind::RunFailed
                | RuntimeEventKind::RunCancelled
                | RuntimeEventKind::TaskFailed
                | RuntimeEventKind::TaskCancelled
                | RuntimeEventKind::TaskTimedOut
                | RuntimeEventKind::SubagentGuidanceRejected
                | RuntimeEventKind::SubagentInterruptSettled
                | RuntimeEventKind::Failed
                | RuntimeEventKind::Cancelled
                | RuntimeEventKind::TimedOut
                | RuntimeEventKind::ArtifactProduced
                | RuntimeEventKind::BackgroundCellFinished
                | RuntimeEventKind::MergeStarted
                | RuntimeEventKind::MergeCompleted
                | RuntimeEventKind::MergeFailed
        )
    }

    #[allow(clippy::should_implement_trait)] // inherent helper returning Option; not the FromStr trait
    pub fn from_str(s: &str) -> Option<Self> {
        use RuntimeEventKind::*;
        Some(match s {
            "run_created" => RunCreated,
            "run_goal_updated" => RunGoalUpdated,
            "requirement_evidence_invalidated" => RequirementEvidenceInvalidated,
            "requirement_evidence_revalidated" => RequirementEvidenceRevalidated,
            "requirement_skipped" => RequirementSkipped,
            "run_started" => RunStarted,
            "run_completed" => RunCompleted,
            "run_failed" => RunFailed,
            "run_status_changed" => RunStatusChanged,
            "run_attachments_updated" => RunAttachmentsUpdated,
            "plan_revision_committed" => PlanRevisionCommitted,
            "task_started" => TaskStarted,
            "task_completed" => TaskCompleted,
            "task_failed" => TaskFailed,
            "task_cancelled" => TaskCancelled,
            "task_timed_out" => TaskTimedOut,
            "task_skipped" => TaskSkipped,
            "task_blocked" => TaskBlocked,
            "task_status_changed" => TaskStatusChanged,
            "started" => Started,
            "running" => Running,
            "completed" => Completed,
            "failed" => Failed,
            "cancelled" => Cancelled,
            "timed_out" => TimedOut,
            "subagent_assigned" => SubagentAssigned,
            "subagent_released" => SubagentReleased,
            "subagent_guidance_queued" => SubagentGuidanceQueued,
            "subagent_guidance_mailbox_accepted" => SubagentGuidanceMailboxAccepted,
            "subagent_guidance_drained" => SubagentGuidanceDrained,
            "subagent_guidance_settled" => SubagentGuidanceSettled,
            "subagent_guidance_rejected" => SubagentGuidanceRejected,
            "subagent_interrupt_requested" => SubagentInterruptRequested,
            "subagent_interrupt_settled" => SubagentInterruptSettled,
            "isolation_observed" => IsolationObserved,
            "thinking_started" => ThinkingStarted,
            "thinking_delta" => ThinkingDelta,
            "thinking_ended" => ThinkingEnded,
            "token_delta" => TokenDelta,
            "usage" => Usage,
            "tool_started" => ToolStarted,
            "tool_output" => ToolOutput,
            "tool_completed" => ToolCompleted,
            "tool_failed" => ToolFailed,
            "artifact_produced" => ArtifactProduced,
            "merge_started" => MergeStarted,
            "merge_completed" => MergeCompleted,
            "merge_failed" => MergeFailed,
            "review_passed" => ReviewPassed,
            "review_needs_fix" => ReviewNeedsFix,
            "review_blocked" => ReviewBlocked,
            "circuit_breaker_tripped" => CircuitBreakerTripped,
            "recovery_blocked" => RecoveryBlocked,
            "recovery_resolved" => RecoveryResolved,
            "background_cell_started" => BackgroundCellStarted,
            "background_cell_finished" => BackgroundCellFinished,
            "run_continuation_configured" => RunContinuationConfigured,
            "run_turn_started" => RunTurnStarted,
            "run_turn_usage_accounted" => RunTurnUsageAccounted,
            "run_turn_compacted" => RunTurnCompacted,
            "run_turn_finished" => RunTurnFinished,
            "run_provider_retry_scheduled" => RunProviderRetryScheduled,
            "run_continuation_deferred" => RunContinuationDeferred,
            "run_continuation_resumed" => RunContinuationResumed,
            "run_pause_reason_changed" => RunPauseReasonChanged,
            "run_cancelled" => RunCancelled,
            "note" => Note,
            _ => return None,
        })
    }
}

impl std::fmt::Display for RuntimeEventKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

// ── Core persisted structs ──────────────────────────────────────────────

/// A single complex-task run. One run = one user goal that goes through the
/// plan → approve → execute → review → synthesize lifecycle.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, rename = "TaskRun")]
pub struct TaskRun {
    pub run_id: String,
    pub workspace_id: String,
    pub conversation_id: String,
    pub root_message_id: String,
    pub domain_profile: DomainProfile,
    pub status: TaskRunStatus,
    pub goal: String,
    #[ts(type = "number")]
    pub goal_revision: u64,
    pub goal_sha256: String,
    pub plan_id: Option<String>,
    pub route: String,
    /// Whether a human is present (Attended) or this is a cron/IM trigger
    /// (Unattended). Drives safety-gate behaviour in task_execute /
    /// executor.  Default: Attended (chat behaviours unchanged).
    pub attended_mode: AttendedMode,
    /// User-uploaded attachments shared across all subagents in this run (so
    /// plan-level subagents see the same images/files as the main agent).
    /// Empty for text-only runs. `#[serde(default)]` keeps old run files
    /// readable. TS-skipped — this is backend-only state (paths on disk),
    /// not consumed by the frontend.
    #[serde(default)]
    #[ts(skip)]
    pub attachments: Vec<crate::attachments::AttachmentRef>,
    #[serde(with = "echo_agent::utils::time::local_rfc3339")]
    #[ts(as = "String")]
    pub created_at: DateTime<Utc>,
    #[serde(with = "echo_agent::utils::time::local_rfc3339")]
    #[ts(as = "String")]
    pub updated_at: DateTime<Utc>,
}

/// Immutable TaskRun identity captured when a surface queues a resume turn.
///
/// A later lookup must match every field and remain paused. This prevents a
/// delayed command from driving a deleted-and-recreated run that reused the
/// same external run id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRunResumeIdentity {
    pub run_id: String,
    pub workspace_id: String,
    pub conversation_id: String,
    pub root_message_id: String,
    pub created_at: DateTime<Utc>,
    pub goal_revision: u64,
    pub journal_sequence: u64,
    pub continuation_enabled: bool,
}

impl TaskRunResumeIdentity {
    pub fn capture(snapshot: &RunStateSnapshot) -> Self {
        let run = &snapshot.run;
        Self {
            run_id: run.run_id.clone(),
            workspace_id: run.workspace_id.clone(),
            conversation_id: run.conversation_id.clone(),
            root_message_id: run.root_message_id.clone(),
            created_at: run.created_at,
            goal_revision: run.goal_revision,
            journal_sequence: snapshot.journal_sequence,
            continuation_enabled: snapshot
                .continuation
                .as_ref()
                .is_some_and(|continuation| continuation.enabled),
        }
    }

    pub fn validate_resumable(&self, snapshot: &RunStateSnapshot) -> Result<(), String> {
        let run = &snapshot.run;
        let mut changed = Vec::new();
        if run.run_id != self.run_id {
            changed.push("run_id");
        }
        if run.workspace_id != self.workspace_id {
            changed.push("workspace_id");
        }
        if run.conversation_id != self.conversation_id {
            changed.push("conversation_id");
        }
        if run.root_message_id != self.root_message_id {
            changed.push("root_message_id");
        }
        if run.created_at != self.created_at {
            changed.push("created_at");
        }
        if run.goal_revision != self.goal_revision {
            changed.push("goal_revision");
        }
        if snapshot.journal_sequence != self.journal_sequence {
            changed.push("journal_sequence");
        }
        if snapshot
            .continuation
            .as_ref()
            .is_some_and(|continuation| continuation.enabled)
            != self.continuation_enabled
        {
            changed.push("continuation_enabled");
        }
        if !changed.is_empty() {
            return Err(format!(
                "TaskRun '{}' identity changed after resume was queued (fields: {}; journal sequence expected {}, current {})",
                self.run_id,
                changed.join(","),
                self.journal_sequence,
                snapshot.journal_sequence,
            ));
        }
        if run.status != TaskRunStatus::Paused {
            return Err(format!(
                "TaskRun '{}' is {}; resume requires paused",
                self.run_id,
                run.status.as_str()
            ));
        }
        Ok(())
    }
}

/// Stable digest used to bind Plans and evidence to the authoritative Goal.
pub fn task_goal_sha256(goal: &str) -> String {
    format!("{:x}", Sha256::digest(goal.as_bytes()))
}

/// Product surface that initiated an explicit user Goal update.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "RunGoalActorSource")]
pub enum RunGoalActorSource {
    Gui,
    Tui,
    Cli,
    Channel,
}

impl RunGoalActorSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gui => "gui",
            Self::Tui => "tui",
            Self::Cli => "cli",
            Self::Channel => "channel",
        }
    }
}

/// Internal materialized join of one committed plan revision and its canonical
/// framework execution state. It is never persisted or exposed over IPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskPlan {
    pub plan_id: String,
    pub run_id: String,
    /// Monotonic committed plan revision. Revision 1 is the initial complete
    /// DAG; every accepted dynamic patch increments it exactly once.
    pub revision: u64,
    pub domain_profile: DomainProfile,
    pub goal_revision: u64,
    pub goal_sha256: String,
    pub assumptions: Vec<String>,
    pub risks: Vec<String>,
    pub execution_mode: ExecutionMode,
    pub tasks: Vec<PlanTask>,
}

impl TaskPlan {
    pub fn specification(&self) -> PlanRevision {
        PlanRevision {
            plan_id: self.plan_id.clone(),
            run_id: self.run_id.clone(),
            revision: self.revision,
            domain_profile: self.domain_profile,
            goal_revision: self.goal_revision,
            goal_sha256: self.goal_sha256.clone(),
            assumptions: self.assumptions.clone(),
            risks: self.risks.clone(),
            execution_mode: self.execution_mode,
            tasks: self.tasks.iter().map(PlanTask::spec).collect(),
        }
    }
}

/// One stable, versioned completion obligation derived from a PlanTask.
///
/// The authoritative Goal remains on TaskRun. This projection only binds a
/// task's declared work and acceptance evidence to that Goal revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, rename = "GoalRequirement")]
pub struct GoalRequirement {
    pub requirement_id: String,
    #[ts(type = "number")]
    pub goal_revision: u64,
    #[ts(type = "number")]
    pub plan_revision: u64,
    pub task_id: String,
    pub title: String,
    pub description: String,
    pub requirement_sha256: String,
    pub required_artifacts: Vec<String>,
    pub execution_checks: Vec<String>,
    pub acceptance_criteria: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "RequirementEvidenceKind")]
pub enum RequirementEvidenceKind {
    TaskExecution,
    Artifact,
    Test,
    Review,
    Revalidation,
    UserSkip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "RequirementEvidenceStatus")]
pub enum RequirementEvidenceStatus {
    Passed,
    Failed,
    Stale,
}

/// One evidence fact linked back to its source event sequence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, rename = "RequirementEvidence")]
pub struct RequirementEvidence {
    pub evidence_id: String,
    pub requirement_id: String,
    #[ts(type = "number")]
    pub goal_revision: u64,
    #[ts(type = "number")]
    pub plan_revision: u64,
    pub task_id: String,
    pub kind: RequirementEvidenceKind,
    pub source_event_seq: String,
    pub status: RequirementEvidenceStatus,
    pub producer_identity: Option<String>,
    pub subject: String,
    pub sha256: Option<String>,
    pub details: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "RequirementStatus")]
pub enum RequirementStatus {
    Pending,
    Accepted,
    Skipped,
    Stale,
    Failed,
}

impl RequirementStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Accepted => "accepted",
            Self::Skipped => "skipped",
            Self::Stale => "stale",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, rename = "RequirementAssessment")]
pub struct RequirementAssessment {
    pub requirement: GoalRequirement,
    pub status: RequirementStatus,
    pub evidence: Vec<RequirementEvidence>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "CompletionBlockerCode")]
pub enum CompletionBlockerCode {
    NoPlan,
    EmptyPlan,
    PlanGoalMismatch,
    TaskNotComplete,
    RequirementUncovered,
    RequirementEvidenceMissing,
    ArtifactMissing,
    ArtifactHashMismatch,
    TestFailed,
    ReviewMissing,
    ReviewFailed,
    StaleEvidence,
    ActiveSubagent,
    ActiveCommandCell,
    RecoveryBlocker,
    StoreReadFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, rename = "RunCompletionBlocker")]
pub struct RunCompletionBlocker {
    pub code: CompletionBlockerCode,
    pub requirement_id: Option<String>,
    pub task_id: Option<String>,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, rename = "CompletionGateReport")]
pub struct CompletionGateReport {
    pub run_id: String,
    #[ts(type = "number")]
    pub goal_revision: u64,
    #[ts(type = "number")]
    pub plan_revision: u64,
    pub ready: bool,
    pub requirements: Vec<RequirementAssessment>,
    pub blockers: Vec<RunCompletionBlocker>,
}

/// Frozen product-layer address for one cross-workspace PlanTask dispatch.
/// `group_id` and `subagent_role` retain the user's group intent while the
/// concrete address prevents later group edits from retargeting this revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, rename = "TaskExecutionTarget")]
pub struct TaskExecutionTarget {
    pub group_id: String,
    pub subagent_role: String,
    pub address: crate::agent_router::AgentAddress,
}

impl TaskExecutionTarget {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.group_id.trim().is_empty() || self.group_id.chars().count() > 128 {
            return Err("execution_target.group_id must contain 1-128 characters".to_string());
        }
        if self.subagent_role.trim().is_empty() || self.subagent_role.chars().count() > 128 {
            return Err("execution_target.subagent_role must contain 1-128 characters".to_string());
        }
        self.address.validate().map_err(|error| error.to_string())
    }
}

/// EKO file/UI projection of the immutable framework task specification.
///
/// This DTO preserves EKO product fields for `plan.json` and generated TypeScript;
/// framework validation and DAG scheduling never consume it directly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, rename = "TaskSpec")]
pub struct EkoTaskSpec {
    pub id: String,
    pub title: String,
    pub description: String,
    pub kind: PlanTaskKind,
    pub agent_role: String,
    pub domain_profile: DomainProfile,
    pub depends_on: Vec<String>,
    pub parallel_group: Option<String>,
    #[serde(default)]
    pub execution_target: Option<TaskExecutionTarget>,
    pub files: Vec<String>,
    pub allowed_tools: Vec<String>,
    pub required_artifacts: Vec<String>,
    pub execution_checks: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub max_retries: u32,
    #[ts(type = "number")]
    pub sort_order: i64,
}

impl TryFrom<&EkoTaskSpec> for echo_agent::tasks::TaskSpec {
    type Error = String;

    fn try_from(value: &EkoTaskSpec) -> Result<Self, Self::Error> {
        echo_agent::tasks::TaskSpec {
            id: value.id.clone(),
            title: value.title.clone(),
            description: value.description.clone(),
            depends_on: value.depends_on.clone(),
            max_retries: value.max_retries,
            extension: serde_json::Value::Null,
        }
        .with_extension(EkoTaskExtension {
            kind: value.kind,
            agent_role: value.agent_role.clone(),
            domain_profile: value.domain_profile,
            parallel_group: value.parallel_group.clone(),
            execution_target: value.execution_target.clone(),
            files: value.files.clone(),
            allowed_tools: value.allowed_tools.clone(),
            required_artifacts: value.required_artifacts.clone(),
            execution_checks: value.execution_checks.clone(),
            acceptance_criteria: value.acceptance_criteria.clone(),
            sort_order: value.sort_order,
        })
        .map_err(|error| format!("task '{}' has invalid EKO extension: {error}", value.id))
    }
}

impl TryFrom<EkoTaskSpec> for echo_agent::tasks::TaskSpec {
    type Error = String;

    fn try_from(value: EkoTaskSpec) -> Result<Self, Self::Error> {
        (&value).try_into()
    }
}

impl TryFrom<echo_agent::tasks::TaskSpec> for EkoTaskSpec {
    type Error = String;

    fn try_from(spec: echo_agent::tasks::TaskSpec) -> Result<Self, Self::Error> {
        let extension: EkoTaskExtension = spec
            .extension_as()
            .map_err(|error| format!("task '{}' has invalid EKO extension: {error}", spec.id))?;
        Ok(Self {
            id: spec.id,
            title: spec.title,
            description: spec.description,
            kind: extension.kind,
            agent_role: extension.agent_role,
            domain_profile: extension.domain_profile,
            depends_on: spec.depends_on,
            parallel_group: extension.parallel_group,
            execution_target: extension.execution_target,
            files: extension.files,
            allowed_tools: extension.allowed_tools,
            required_artifacts: extension.required_artifacts,
            execution_checks: extension.execution_checks,
            acceptance_criteria: extension.acceptance_criteria,
            max_retries: spec.max_retries,
            sort_order: extension.sort_order,
        })
    }
}

/// Lossless EKO payload carried through the framework task extension point.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EkoTaskExtension {
    pub kind: PlanTaskKind,
    pub agent_role: String,
    pub domain_profile: DomainProfile,
    pub parallel_group: Option<String>,
    #[serde(default)]
    pub execution_target: Option<TaskExecutionTarget>,
    pub files: Vec<String>,
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub required_artifacts: Vec<String>,
    #[serde(default)]
    pub execution_checks: Vec<String>,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    pub sort_order: i64,
}

/// EKO-only plan metadata carried through the framework graph context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EkoPlanMetadata {
    pub plan_id: String,
    pub domain_profile: DomainProfile,
    pub goal_revision: u64,
    pub goal_sha256: String,
}

/// EKO's file-backed plan projection persisted in `plan.json`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, rename = "PlanRevision")]
pub struct PlanRevision {
    pub plan_id: String,
    pub run_id: String,
    #[ts(type = "number")]
    pub revision: u64,
    pub domain_profile: DomainProfile,
    #[ts(type = "number")]
    pub goal_revision: u64,
    pub goal_sha256: String,
    pub assumptions: Vec<String>,
    pub risks: Vec<String>,
    pub execution_mode: ExecutionMode,
    pub tasks: Vec<EkoTaskSpec>,
}

/// EKO's file-backed execution projection persisted in `run-state.json`.
/// Task execution entries are the framework `TaskExecution` values directly;
/// EKO owns only the surrounding run and product projections.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunStateSnapshot {
    pub run: TaskRun,
    pub tasks: Vec<echo_agent::tasks::TaskExecution>,
    /// Event-folded long-horizon control state. Absent for ordinary one-shot runs.
    #[serde(default)]
    pub continuation: Option<RunContinuationState>,
    /// Event-folded background command cells owned by this run.
    #[serde(default)]
    pub background_cells: Vec<BackgroundCellState>,
    /// Last authoritative journal sequence folded into this snapshot. This is
    /// the optimistic-concurrency epoch for queued resume actions.
    #[serde(default)]
    pub(crate) journal_sequence: u64,
    /// Internal operational/idempotency index carried by the same checkpoint
    /// fold. It is not a second authority and is intentionally not a UI wire.
    #[serde(default)]
    pub(crate) event_index: RunStateEventIndex,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct RunStateEventIndex {
    #[serde(default)]
    pub(crate) started_turns: std::collections::BTreeSet<String>,
    #[serde(default)]
    pub(crate) accounted_usage: std::collections::BTreeSet<String>,
    #[serde(default)]
    pub(crate) accounted_compactions: std::collections::BTreeSet<String>,
    #[serde(default)]
    pub(crate) finished_turns: std::collections::BTreeSet<String>,
    #[serde(default)]
    pub(crate) assigned_subagents: std::collections::BTreeSet<String>,
    #[serde(default)]
    pub(crate) active_subagents: Vec<ActiveSubagentBoundary>,
    #[serde(default)]
    pub(crate) active_tools: Vec<ActiveToolBoundary>,
    #[serde(default)]
    pub(crate) recovery_blockers: Vec<RecoveryBlocker>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ActiveSubagentBoundary {
    pub(crate) task_id: String,
    pub(crate) execution_id: String,
    pub(crate) replay_safe: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ActiveToolBoundary {
    pub(crate) task_id: String,
    pub(crate) execution_id: Option<String>,
    pub(crate) call_id: String,
    pub(crate) tool_name: String,
    pub(crate) replay_safe: bool,
}

/// Source of one finite primary-Agent turn within a long-horizon TaskRun.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "RunTurnOrigin")]
pub enum RunTurnOrigin {
    User,
    Continuation,
    Resume,
    Recovery,
}

impl RunTurnOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Continuation => "continuation",
            Self::Resume => "resume",
            Self::Recovery => "recovery",
        }
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "user" => Some(Self::User),
            "continuation" => Some(Self::Continuation),
            "resume" => Some(Self::Resume),
            "recovery" => Some(Self::Recovery),
            _ => None,
        }
    }
}

/// Whether a turn instruction belongs in the user-visible transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "TurnVisibility")]
pub enum TurnVisibility {
    Visible,
    Internal,
}

impl TurnVisibility {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Visible => "visible",
            Self::Internal => "internal",
        }
    }
}

/// Thin identity binding that prevents continuation turns from deriving a new run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunTurnBinding {
    pub run_id: Option<String>,
    pub turn_id: String,
    pub root_message_id: String,
    pub origin: RunTurnOrigin,
    pub transcript_visibility: TurnVisibility,
    #[serde(default)]
    pub expected_resume: Option<TaskRunResumeIdentity>,
}

impl RunTurnBinding {
    pub fn resume_expected(identity: TaskRunResumeIdentity, turn_id: impl Into<String>) -> Self {
        Self::resume_expected_with_visibility(identity, turn_id, TurnVisibility::Visible)
    }

    pub fn resume_expected_with_visibility(
        identity: TaskRunResumeIdentity,
        turn_id: impl Into<String>,
        transcript_visibility: TurnVisibility,
    ) -> Self {
        let root_message_id = identity.root_message_id.clone();
        Self {
            run_id: Some(identity.run_id.clone()),
            turn_id: turn_id.into(),
            root_message_id,
            origin: RunTurnOrigin::Resume,
            transcript_visibility,
            expected_resume: Some(identity),
        }
    }
}

/// Terminal execution state of one finite RunTurn. `Ended` is not Goal completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "RunTurnStatus")]
pub enum RunTurnStatus {
    Running,
    Ended,
    Cancelled,
    Failed,
}

impl RunTurnStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Ended => "ended",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "running" => Some(Self::Running),
            "ended" => Some(Self::Ended),
            "cancelled" => Some(Self::Cancelled),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// Why a run is recoverably Paused instead of irreversibly Failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "RunPauseReason")]
pub enum RunPauseReason {
    User,
    NeedsInput,
    Approval,
    BootRecovery,
    UsageLimit,
    TokenBudget,
    TimeBudget,
    RepeatedBlocker,
    IndeterminateSideEffect,
    ProviderUnavailable,
}

impl RunPauseReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::NeedsInput => "needs_input",
            Self::Approval => "approval",
            Self::BootRecovery => "boot_recovery",
            Self::UsageLimit => "usage_limit",
            Self::TokenBudget => "token_budget",
            Self::TimeBudget => "time_budget",
            Self::RepeatedBlocker => "repeated_blocker",
            Self::IndeterminateSideEffect => "indeterminate_side_effect",
            Self::ProviderUnavailable => "provider_unavailable",
        }
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "user" => Some(Self::User),
            "needs_input" => Some(Self::NeedsInput),
            "approval" => Some(Self::Approval),
            "boot_recovery" => Some(Self::BootRecovery),
            "usage_limit" => Some(Self::UsageLimit),
            "token_budget" => Some(Self::TokenBudget),
            "time_budget" => Some(Self::TimeBudget),
            "repeated_blocker" => Some(Self::RepeatedBlocker),
            "indeterminate_side_effect" => Some(Self::IndeterminateSideEffect),
            "provider_unavailable" => Some(Self::ProviderUnavailable),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, rename = "RunPause")]
pub struct RunPause {
    pub reason: RunPauseReason,
    pub detail: Option<String>,
    #[serde(with = "echo_agent::utils::time::local_rfc3339")]
    #[ts(as = "String")]
    pub changed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, rename = "BlockerAudit")]
pub struct BlockerAudit {
    pub fingerprint: String,
    pub consecutive_turns: u32,
}

/// Durable retry schedule for transient provider failures between finite
/// primary-Agent turns. The concrete deadline is persisted so event replay
/// never draws a new jitter value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, rename = "ProviderRetryState")]
pub struct ProviderRetryState {
    pub attempt_count: u32,
    #[serde(with = "echo_agent::utils::time::local_rfc3339")]
    #[ts(as = "String")]
    pub next_retry_at: DateTime<Utc>,
    pub error_fingerprint: String,
    #[serde(with = "echo_agent::utils::time::local_rfc3339")]
    #[ts(as = "String")]
    pub first_failure_at: DateTime<Utc>,
    pub exhausted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, rename = "RunTurnSummary")]
pub struct RunTurnSummary {
    pub turn_id: String,
    #[ts(type = "number")]
    pub ordinal: u64,
    pub origin: RunTurnOrigin,
    pub status: RunTurnStatus,
    pub transcript_visibility: TurnVisibility,
    #[serde(with = "echo_agent::utils::time::local_rfc3339")]
    #[ts(as = "String")]
    pub started_at: DateTime<Utc>,
    #[serde(with = "echo_agent::utils::time::option_local_rfc3339")]
    #[ts(as = "Option<String>")]
    pub ended_at: Option<DateTime<Utc>>,
    #[ts(type = "number")]
    pub input_tokens: u64,
    #[ts(type = "number")]
    pub output_tokens: u64,
    #[ts(type = "number")]
    pub elapsed_seconds: u64,
    pub compaction_count: u32,
    pub final_message_id: Option<String>,
    pub error_fingerprint: Option<String>,
}

/// Rebuildable control projection for one long-horizon TaskRun.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, rename = "RunContinuationState")]
pub struct RunContinuationState {
    pub enabled: bool,
    pub auto_resume_after_restart: bool,
    #[ts(type = "number | null")]
    pub token_budget: Option<u64>,
    #[ts(type = "number | null")]
    pub time_budget_seconds: Option<u64>,
    #[ts(type = "number")]
    pub tokens_used: u64,
    #[ts(type = "number")]
    pub time_used_seconds: u64,
    pub compaction_count: u32,
    #[ts(type = "number")]
    pub next_turn_ordinal: u64,
    pub active_turn: Option<RunTurnSummary>,
    pub last_turn: Option<RunTurnSummary>,
    pub pause: Option<RunPause>,
    pub blocker_audit: Option<BlockerAudit>,
    #[serde(default)]
    pub provider_retry: Option<ProviderRetryState>,
    pub deferred: bool,
    pub deferred_reason: Option<String>,
}

impl Default for RunContinuationState {
    fn default() -> Self {
        Self {
            enabled: false,
            auto_resume_after_restart: false,
            token_budget: None,
            time_budget_seconds: None,
            tokens_used: 0,
            time_used_seconds: 0,
            compaction_count: 0,
            next_turn_ordinal: 1,
            active_turn: None,
            last_turn: None,
            pause: None,
            blocker_audit: None,
            provider_retry: None,
            deferred: false,
            deferred_reason: None,
        }
    }
}

/// Stable application projection of the framework command-cell phase.
/// Stable typed reason for a terminal application cell projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "BackgroundCellPhase")]
pub enum BackgroundCellPhase {
    Prepared,
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    LaunchFailed,
    Unknown,
}

impl BackgroundCellPhase {
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::Prepared | Self::Queued | Self::Running)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::LaunchFailed => "launch_failed",
            Self::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for BackgroundCellPhase {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Stable application projection of complete-output artifact settlement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "BackgroundCellTerminalCause")]
pub enum BackgroundCellTerminalCause {
    Exited,
    TimedOut,
    Cancelled,
    LaunchFailed,
    WaitFailed,
    OutputDrainFailed,
    ObserverFailed,
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "BackgroundCellArtifactStatus")]
pub enum BackgroundCellArtifactStatus {
    NotRequested,
    Writing,
    BelowThreshold,
    Available,
    Failed,
}

/// Durable TaskRuntime projection of one process-scoped command cell.
///
/// The OS handle is never persisted. A started record without `finished_at`
/// means the current process still owns the cell, or that boot recovery must
/// close it as interrupted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, rename = "BackgroundCellState")]
pub struct BackgroundCellState {
    pub cell_id: String,
    pub name: String,
    pub command_hash: String,
    pub turn_id: Option<String>,
    pub execution_id: Option<String>,
    pub call_id: Option<String>,
    pub phase: BackgroundCellPhase,
    pub terminal_cause: Option<BackgroundCellTerminalCause>,
    pub terminal_message: Option<String>,
    pub exit_code: Option<i32>,
    pub artifact_status: BackgroundCellArtifactStatus,
    pub artifact_message: Option<String>,
    #[ts(type = "number")]
    pub total_output_bytes: u64,
    pub output_truncated: bool,
    pub output_excerpt: Option<String>,
    pub artifact_path: Option<String>,
    pub artifact_sha256: Option<String>,
    #[serde(with = "echo_agent::utils::time::local_rfc3339")]
    #[ts(as = "String")]
    pub started_at: DateTime<Utc>,
    #[serde(with = "echo_agent::utils::time::option_local_rfc3339")]
    #[ts(as = "Option<String>")]
    pub finished_at: Option<DateTime<Utc>>,
}

impl BackgroundCellState {
    pub fn is_active(&self) -> bool {
        !self.phase.is_terminal()
    }
}

/// Internal materialized join of one EKO specification and the framework's
/// canonical execution state. Persistence remains split between `PlanRevision`
/// and `RunStateSnapshot`; UI callers receive `TaskSpec` plus `TodoItem`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanTask {
    pub id: String,
    pub title: String,
    pub description: String,
    pub kind: PlanTaskKind,
    pub agent_role: String,
    pub domain_profile: DomainProfile,
    pub depends_on: Vec<String>,
    pub parallel_group: Option<String>,
    /// Optional cross-workspace member selected from a persistent Agent group.
    /// The full address is frozen into the plan revision so later group edits
    /// cannot silently retarget an accepted task.
    #[serde(default)]
    pub execution_target: Option<TaskExecutionTarget>,
    /// Read targets for read-only tasks. For mutating tasks, exact
    /// workspace-relative files are exclusive ownership; empty/broad/invalid
    /// declarations are unknown ownership and serialize with every writer.
    pub files: Vec<String>,
    pub allowed_tools: Vec<String>,
    /// Artifact paths or suffixes that must be present and integrity-checked
    /// before this task may enter Completed.
    #[serde(default)]
    pub required_artifacts: Vec<String>,
    /// 可执行验证项(命令类)。Subagent 运行这些命令时,Runtime 从工具事件
    /// 记录 `observed` 证据;每个 execution_check 必须有 `observed + passed`
    /// 才能通过 execution 门禁。示例:`cargo test --lib`、`npm run build`。
    #[serde(default)]
    pub execution_checks: Vec<String>,
    /// 语义验收标准(描述类)。由 Reviewer 基于 Subagent 输出判定,不要求
    /// 工具事件证据。示例:`模块边界清晰`、`前端组件层级合理`。
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    pub retry_count: u32,
    pub max_retries: u32,
    pub failure_fingerprint: Option<String>,
    pub status: echo_agent::tasks::TaskStatus,
    /// Durable dispatch claim. Internal runtime state; UI joins on task and
    /// Subagent execution ids instead.
    #[serde(default)]
    pub claim: Option<echo_agent::tasks::TaskClaim>,
    /// Stable sort key for display ordering. Set by plan generation (sequential
    /// index) and updated by `reorder_tasks`. Separated from `parallel_group`
    /// (which encodes parallel-fanout grouping, not display order) to avoid
    /// semantic pollution.
    pub sort_order: i64,
}

impl Default for PlanTask {
    fn default() -> Self {
        Self {
            id: String::new(),
            title: String::new(),
            description: "Complete the assigned task".to_string(),
            kind: PlanTaskKind::ReadOnlyReview,
            agent_role: "general".to_string(),
            domain_profile: DomainProfile::General,
            depends_on: Vec::new(),
            parallel_group: None,
            execution_target: None,
            files: Vec::new(),
            allowed_tools: Vec::new(),
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

impl PlanTask {
    pub fn spec(&self) -> EkoTaskSpec {
        EkoTaskSpec {
            id: self.id.clone(),
            title: self.title.clone(),
            description: self.description.clone(),
            kind: self.kind,
            agent_role: self.agent_role.clone(),
            domain_profile: self.domain_profile,
            depends_on: self.depends_on.clone(),
            parallel_group: self.parallel_group.clone(),
            execution_target: self.execution_target.clone(),
            files: self.files.clone(),
            allowed_tools: self.allowed_tools.clone(),
            required_artifacts: self.required_artifacts.clone(),
            execution_checks: self.execution_checks.clone(),
            acceptance_criteria: self.acceptance_criteria.clone(),
            max_retries: self.max_retries,
            sort_order: self.sort_order,
        }
    }

    pub fn execution(&self) -> echo_agent::tasks::TaskExecution {
        echo_agent::tasks::TaskExecution {
            task_id: self.id.clone(),
            status: self.status.clone(),
            retry_count: self.retry_count,
            failure_fingerprint: self.failure_fingerprint.clone(),
            claim: self.claim.clone(),
        }
    }

    pub fn from_parts(spec: EkoTaskSpec, execution: echo_agent::tasks::TaskExecution) -> Self {
        Self {
            id: spec.id,
            title: spec.title,
            description: spec.description,
            kind: spec.kind,
            agent_role: spec.agent_role,
            domain_profile: spec.domain_profile,
            depends_on: spec.depends_on,
            parallel_group: spec.parallel_group,
            execution_target: spec.execution_target,
            files: spec.files,
            allowed_tools: spec.allowed_tools,
            required_artifacts: spec.required_artifacts,
            execution_checks: spec.execution_checks,
            acceptance_criteria: spec.acceptance_criteria,
            retry_count: execution.retry_count,
            max_retries: spec.max_retries,
            failure_fingerprint: execution.failure_fingerprint,
            status: execution.status,
            claim: execution.claim,
            sort_order: spec.sort_order,
        }
    }
}

impl TryFrom<&PlanTask> for echo_agent::tasks::Task {
    type Error = String;

    fn try_from(value: &PlanTask) -> Result<Self, Self::Error> {
        Ok(echo_agent::tasks::Task {
            spec: (&value.spec()).try_into()?,
            execution: echo_agent::tasks::TaskExecution {
                task_id: value.id.clone(),
                status: value.status.clone(),
                retry_count: value.retry_count,
                failure_fingerprint: value.failure_fingerprint.clone(),
                claim: value.claim.clone(),
            },
        })
    }
}

impl TryFrom<echo_agent::tasks::Task> for PlanTask {
    type Error = String;

    fn try_from(task: echo_agent::tasks::Task) -> Result<Self, Self::Error> {
        let echo_agent::tasks::Task { spec, execution } = task;
        if spec.id != execution.task_id {
            return Err(format!(
                "framework task spec id '{}' does not match execution id '{}'",
                spec.id, execution.task_id
            ));
        }
        let spec = EkoTaskSpec::try_from(spec)?;
        Ok(Self {
            id: spec.id,
            title: spec.title,
            description: spec.description,
            kind: spec.kind,
            agent_role: spec.agent_role,
            domain_profile: spec.domain_profile,
            depends_on: spec.depends_on,
            parallel_group: spec.parallel_group,
            execution_target: spec.execution_target,
            files: spec.files,
            allowed_tools: spec.allowed_tools,
            required_artifacts: spec.required_artifacts,
            execution_checks: spec.execution_checks,
            acceptance_criteria: spec.acceptance_criteria,
            retry_count: execution.retry_count,
            max_retries: spec.max_retries,
            failure_fingerprint: execution.failure_fingerprint,
            status: execution.status,
            claim: execution.claim,
            sort_order: spec.sort_order,
        })
    }
}

/// Partial specification update used by a revisioned [`TaskUpdateOperation`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export, rename = "TaskPatch")]
pub struct TaskPatch {
    pub title: Option<String>,
    pub description: Option<String>,
    pub kind: Option<PlanTaskKind>,
    pub agent_role: Option<String>,
    pub depends_on: Option<Vec<String>>,
    pub files: Option<Vec<String>>,
    pub allowed_tools: Option<Vec<String>>,
    pub required_artifacts: Option<Vec<String>>,
    pub execution_checks: Option<Vec<String>>,
    pub acceptance_criteria: Option<Vec<String>>,
    pub max_retries: Option<u32>,
}

impl TryFrom<&TaskPatch> for echo_agent::tasks::TaskSpecPatch {
    type Error = String;

    fn try_from(value: &TaskPatch) -> Result<Self, Self::Error> {
        let mut extension = serde_json::Map::new();
        if let Some(kind) = value.kind {
            extension.insert(
                "kind".to_string(),
                serde_json::Value::String(kind.as_str().to_string()),
            );
        }
        if let Some(agent_role) = &value.agent_role {
            extension.insert(
                "agent_role".to_string(),
                serde_json::Value::String(agent_role.clone()),
            );
        }
        for (key, values) in [
            ("files", value.files.as_ref()),
            ("allowed_tools", value.allowed_tools.as_ref()),
            ("required_artifacts", value.required_artifacts.as_ref()),
            ("execution_checks", value.execution_checks.as_ref()),
            ("acceptance_criteria", value.acceptance_criteria.as_ref()),
        ] {
            if let Some(values) = values {
                extension.insert(
                    key.to_string(),
                    serde_json::Value::Array(
                        values
                            .iter()
                            .cloned()
                            .map(serde_json::Value::String)
                            .collect(),
                    ),
                );
            }
        }
        Ok(echo_agent::tasks::TaskSpecPatch {
            title: value.title.clone(),
            description: value.description.clone(),
            depends_on: value.depends_on.clone(),
            max_retries: value.max_retries,
            extension: (!extension.is_empty()).then_some(serde_json::Value::Object(extension)),
        })
    }
}

/// One atomic operation in a revisioned plan patch.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(tag = "op", rename_all = "snake_case")]
#[ts(export, rename = "TaskUpdateOperation")]
pub enum TaskUpdateOperation {
    Insert {
        after_task_id: Option<String>,
        task: EkoTaskSpec,
    },
    Update {
        task_id: String,
        patch: TaskPatch,
    },
    Skip {
        task_id: String,
    },
    Reorder {
        task_ids: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, rename = "TaskUpdateRequest")]
pub struct TaskUpdateRequest {
    #[ts(type = "number")]
    pub base_revision: u64,
    pub reason: String,
    pub operations: Vec<TaskUpdateOperation>,
}

impl TryFrom<&TaskUpdateRequest> for echo_agent::tasks::TaskPlanPatch {
    type Error = String;

    fn try_from(value: &TaskUpdateRequest) -> Result<Self, Self::Error> {
        let mut operations = Vec::with_capacity(value.operations.len());
        for operation in &value.operations {
            operations.push(match operation {
                TaskUpdateOperation::Insert {
                    after_task_id,
                    task,
                } => echo_agent::tasks::TaskPlanPatchOp::Insert {
                    after_task_id: after_task_id.clone(),
                    task: task.try_into()?,
                },
                TaskUpdateOperation::Update { task_id, patch } => {
                    echo_agent::tasks::TaskPlanPatchOp::Update {
                        task_id: task_id.clone(),
                        patch: patch.try_into()?,
                    }
                }
                TaskUpdateOperation::Skip { task_id } => echo_agent::tasks::TaskPlanPatchOp::Skip {
                    task_id: task_id.clone(),
                },
                TaskUpdateOperation::Reorder { task_ids } => {
                    echo_agent::tasks::TaskPlanPatchOp::Reorder {
                        task_ids: task_ids.clone(),
                    }
                }
            });
        }
        Ok(echo_agent::tasks::TaskPlanPatch {
            base_revision: value.base_revision,
            reason: value.reason.clone(),
            operations,
        })
    }
}

impl TryFrom<TaskUpdateRequest> for echo_agent::tasks::TaskPlanPatch {
    type Error = String;

    fn try_from(value: TaskUpdateRequest) -> Result<Self, Self::Error> {
        (&value).try_into()
    }
}

/// A todo row — the GUI-facing projection of a plan task's progress.
/// One `PlanTask` maps to exactly one `TodoItem`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, rename = "TodoItem")]
pub struct TodoItem {
    pub id: String,
    pub run_id: String,
    pub task_id: String,
    pub title: String,
    pub status: TodoStatus,
    pub retry_count: u32,
    pub max_retries: u32,
    pub owner_agent: Option<String>,
    #[serde(with = "echo_agent::utils::time::option_local_rfc3339")]
    #[ts(as = "Option<String>")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(with = "echo_agent::utils::time::option_local_rfc3339")]
    #[ts(as = "Option<String>")]
    pub completed_at: Option<DateTime<Utc>>,
    pub summary: Option<String>,
}

/// A structured runtime event. Appended on every state transition.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, rename = "RuntimeTaskEvent")]
pub struct RuntimeTaskEvent {
    /// Monotonic event sequence. Serialized as a JSON string (not a number)
    /// so it survives Tauri/HTTP transport without JS bigint precision loss
    /// and parses cleanly on the frontend; deserialize accepts a string too.
    #[serde(
        serialize_with = "serialize_seq_as_string",
        deserialize_with = "deserialize_seq_from_string"
    )]
    #[ts(type = "string")]
    pub seq: i64,
    pub run_id: String,
    pub task_id: Option<String>,
    pub step_id: Option<String>,
    pub event_type: RuntimeEventKind,
    pub payload: serde_json::Value,
    #[serde(with = "echo_agent::utils::time::local_rfc3339")]
    #[ts(as = "String")]
    pub timestamp: DateTime<Utc>,
}

/// User decision for a mutating task whose side effects are indeterminate
/// after a process interruption.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "RecoveryDecision")]
pub enum RecoveryDecision {
    /// The user inspected the workspace and accepts re-running the task.
    Retry,
    /// The user chooses not to execute the task again.
    Skip,
}

impl RecoveryDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Retry => "retry",
            Self::Skip => "skip",
        }
    }
}

/// Durable recovery barrier for a task whose mutating side effect may have
/// happened even though no terminal Subagent/tool event was persisted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, rename = "RecoveryBlocker")]
pub struct RecoveryBlocker {
    pub run_id: String,
    pub task_id: String,
    pub execution_id: Option<String>,
    pub call_id: Option<String>,
    pub tool_name: Option<String>,
    pub reason: String,
}

fn serialize_seq_as_string<S: serde::Serializer>(seq: &i64, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&seq.to_string())
}

fn deserialize_seq_from_string<'de, D: serde::Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    struct SequenceVisitor;

    impl serde::de::Visitor<'_> for SequenceVisitor {
        type Value = i64;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("an i64 event sequence as a decimal string or integer")
        }

        fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Self::Value, E> {
            Ok(value)
        }

        fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> {
            i64::try_from(value).map_err(E::custom)
        }

        fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
            value.parse::<i64>().map_err(E::custom)
        }

        fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Self::Value, E> {
            self.visit_str(&value)
        }
    }

    d.deserialize_any(SequenceVisitor)
}

/// A concrete output produced by a task (file, report, chart, trace, …).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, rename = "RuntimeArtifact")]
pub struct Artifact {
    pub id: String,
    pub run_id: String,
    pub task_id: Option<String>,
    pub kind: ArtifactKind,
    pub title: String,
    pub path: Option<String>,
    pub metadata: serde_json::Value,
}

// ── SubagentRun(执行实例,原执行体概念的归一化载体)──────────────────────
//
// 一次 subagent 派发的运行实例。Task → SubagentRun 关联通过 task_id 查询投影得到,PlanTask
// 不持有 executions 字段(避免污染 plan artifact)。
//
// `subagent_run_id` 与框架 SubagentEvent.execution_id 对齐(正式 PlanTask 格式
// "{run_id}:{task_id}:{plan_revision}:{attempt}:{claim_id}"),由 TaskRuntime 派发时生成并经
// ExternalRunContext 透传,不再由 tauri bridge 临时分配(消除双账本)。

/// Product surface that issued an explicit Subagent control command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "SubagentControlActorSource")]
pub enum SubagentControlActorSource {
    Gui,
    Tui,
    Cli,
    Channel,
}

impl SubagentControlActorSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gui => "gui",
            Self::Tui => "tui",
            Self::Cli => "cli",
            Self::Channel => "channel",
        }
    }
}

/// Framework-owned durable identity for one user command and one exact
/// Subagent attempt. EKO adds only its product-facing receipts and policy.
pub use echo_agent::subagent::SubagentCommandIdentity as SubagentControlIdentity;

/// Whether guidance targets an already-active mailbox or one future attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "SubagentGuidanceKind")]
pub enum SubagentGuidanceKind {
    LiveMessage,
    NextAttempt,
}

/// Framework durable command phase for one exact Subagent command.
pub use echo_agent::subagent::SubagentCommandPhase as SubagentControlPhase;

/// Framework terminal outcome of the turn owning a Subagent command.
pub use echo_agent::agent::AgentSteerTurnOutcome as SubagentControlOutcome;

impl SubagentGuidanceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LiveMessage => "live_message",
            Self::NextAttempt => "next_attempt",
        }
    }
}

/// Read-only convenience label derived from `SubagentControlReceipt.phase` and
/// rejection detail. It is never persisted or reduced as lifecycle authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "SubagentControlStatus")]
pub enum SubagentControlStatus {
    Pending,
    Accepted,
    Rejected,
    Settled,
}

impl SubagentControlStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Settled => "settled",
        }
    }
}

/// Idempotent projection returned for a durable Subagent command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, rename = "SubagentControlReceipt")]
pub struct SubagentControlReceipt {
    #[ts(type = "SubagentControlIdentity")]
    pub identity: SubagentControlIdentity,
    /// True when the durable command already existed and this call replayed
    /// its authoritative receipt rather than appending a second command.
    #[serde(default)]
    pub duplicate: bool,
    pub status: SubagentControlStatus,
    #[ts(type = "SubagentControlPhase")]
    pub phase: SubagentControlPhase,
    #[ts(type = "SubagentControlOutcome | null")]
    pub outcome: Option<SubagentControlOutcome>,
    pub drained: Option<bool>,
    pub detail: Option<String>,
    pub framework_turn_id: Option<String>,
}

// TaskRuntime uses the framework result contract directly. Product code may
// add task identity and policy around these values, but never duplicates or
// translates the framework outcome itself.
pub use echo_agent::runtime::ExecutionUsage;
pub use echo_agent::subagent::{
    SubagentArtifact, SubagentEvidence, SubagentEvidenceSource, SubagentOutcome, SubagentStatus,
    SubagentTouchedFiles, SubagentVerification, SubagentVerificationStatus,
};

/// One subagent dispatch execution instance.
///
/// Created when the TaskRuntime dispatches a subagent role to execute a
/// [`PlanTask`]. `subagent_run_id` is the stable identity shared with the
/// framework `SubagentEvent::execution_id`, so the tauri bridge / frontend
/// can route thinking/tool/token streams without temporary id allocation.
///
/// Thinking/tool/token streams are NOT persisted here (they remain an
/// in-memory + realtime stream, matching the legacy execution behavior). Only
/// lifecycle + usage + outcome are durable.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, rename = "SubagentRun")]
pub struct SubagentRun {
    /// Stable execution id. Formal PlanTasks use
    /// "{run_id}:{task_id}:{plan_revision}:{attempt}:{claim_id}". Aligns with
    /// `SubagentEvent::execution_id`.
    pub subagent_run_id: String,
    /// Parent [`TaskRun`] id.
    pub run_id: String,
    /// Parent [`PlanTask`] id. Task → SubagentRun association is projected
    /// via this field (PlanTask itself holds no executions list).
    pub task_id: String,
    /// Role name: explorer / reviewer / implementer / ...
    pub subagent_name: String,
    /// Retry ordinal (1-based; first attempt = 1).
    pub attempt: u32,
    /// Current lifecycle status.
    #[ts(type = "SubagentStatus")]
    pub status: SubagentStatus,
    /// Aggregate cost. Populated progressively; finalized on completion.
    /// Framework usage snapshot; the TS wire name remains `SubagentRunUsage`.
    #[ts(type = "SubagentRunUsage")]
    pub usage: ExecutionUsage,
    /// Structured outcome returned to the Task (None while running).
    #[ts(type = "SubagentOutcome | null")]
    pub outcome: Option<SubagentOutcome>,
}

impl SubagentRun {
    /// Construct a freshly-started SubagentRun (status = Running, no usage).
    pub fn new(
        subagent_run_id: impl Into<String>,
        run_id: impl Into<String>,
        task_id: impl Into<String>,
        subagent_name: impl Into<String>,
        attempt: u32,
    ) -> Self {
        Self {
            subagent_run_id: subagent_run_id.into(),
            run_id: run_id.into(),
            task_id: task_id.into(),
            subagent_name: subagent_name.into(),
            attempt,
            status: SubagentStatus::Running,
            usage: ExecutionUsage::default(),
            outcome: None,
        }
    }
}

/// Result of a review gate over one exact task claim. `NeedsFix` blocks the
/// current task until an explicit retry or task revision; it does not create a
/// parallel task graph.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, rename = "ReviewResult")]
pub struct ReviewResult {
    pub id: String,
    pub run_id: String,
    pub task_id: String,
    pub reviewer_agent: String,
    pub outcome: ReviewOutcome,
    pub issues: Vec<ReviewIssue>,
    pub failure_fingerprint: Option<String>,
    pub created_fix_task_id: Option<String>,
    #[serde(with = "echo_agent::utils::time::local_rfc3339")]
    #[ts(as = "String")]
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, rename = "ReviewIssue")]
pub struct ReviewIssue {
    pub severity: IssueSeverity,
    pub category: String,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "IssueSeverity")]
pub enum IssueSeverity {
    Info,
    Warning,
    Error,
    Blocker,
}

impl IssueSeverity {
    pub fn as_str(&self) -> &'static str {
        match self {
            IssueSeverity::Info => "info",
            IssueSeverity::Warning => "warning",
            IssueSeverity::Error => "error",
            IssueSeverity::Blocker => "blocker",
        }
    }
    #[allow(clippy::should_implement_trait)] // inherent helper returning Option; not the FromStr trait
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "info" => IssueSeverity::Info,
            "warning" => IssueSeverity::Warning,
            "error" => IssueSeverity::Error,
            "blocker" => IssueSeverity::Blocker,
            _ => return None,
        })
    }
}

/// Compact per-task summary produced at every task boundary. Downstream
/// subagents consume this instead of the full raw conversation — see the
/// "Summary Chain" section of the plan.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, rename = "TaskExecutionSummary")]
pub struct TaskExecutionSummary {
    pub run_id: String,
    pub task_id: String,
    pub subagent_name: String,
    #[ts(type = "SubagentOutcome")]
    pub outcome: SubagentOutcome,
    pub decisions: Vec<String>,
    pub next_implications: Vec<String>,
    #[serde(default)]
    pub suggested_tasks: Vec<SuggestedTask>,
    #[serde(with = "echo_agent::utils::time::local_rfc3339")]
    #[ts(as = "String")]
    pub created_at: DateTime<Utc>,
}

/// A bounded follow-up task proposed by a subagent. Subagents may suggest new work,
/// but only the TaskRuntime appends it to the canonical plan.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, rename = "SuggestedTask")]
pub struct SuggestedTask {
    pub title: String,
    pub description: String,
    pub kind: PlanTaskKind,
    pub agent_role: String,
    #[serde(default)]
    pub dependencies: Vec<String>,
    pub why_needed: String,
    pub risk: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct SequenceProbe {
        #[serde(deserialize_with = "deserialize_seq_from_string")]
        seq: i64,
    }

    #[test]
    fn runtime_event_wire_contract_preserves_cron_terminal_facts() -> Result<(), String> {
        let event = RuntimeTaskEvent {
            seq: 9,
            run_id: "cron-run-1".to_string(),
            task_id: Some("task-1".to_string()),
            step_id: Some("execution-1".to_string()),
            event_type: RuntimeEventKind::TaskFailed,
            payload: serde_json::json!({
                "conversation_id": "cron:daily-summary",
                "category": "provider",
                "error": "stream setup failed",
                "recovery": "retry",
                "artifact": {
                    "path": "/tmp/cron.log",
                    "sha256": "def456",
                    "available": true
                }
            }),
            timestamp: Utc::now(),
        };
        let value = serde_json::to_value(event).map_err(|error| error.to_string())?;
        for (pointer, expected) in [
            ("/seq", "9"),
            ("/run_id", "cron-run-1"),
            ("/task_id", "task-1"),
            ("/step_id", "execution-1"),
            ("/event_type", "task_failed"),
            ("/payload/error", "stream setup failed"),
            ("/payload/artifact/path", "/tmp/cron.log"),
            ("/payload/artifact/sha256", "def456"),
        ] {
            assert_eq!(
                value.pointer(pointer).and_then(serde_json::Value::as_str),
                Some(expected)
            );
        }
        Ok(())
    }

    #[test]
    fn event_sequence_accepts_string_and_lossless_integer_forms() -> Result<(), String> {
        for (encoded, expected) in [
            (r#"{"seq":42}"#, 42_i64),
            (r#"{"seq":9007199254740993}"#, 9_007_199_254_740_993_i64),
            (r#"{"seq":"9007199254740993"}"#, 9_007_199_254_740_993_i64),
        ] {
            let decoded: SequenceProbe =
                serde_json::from_str(encoded).map_err(|error| error.to_string())?;
            assert_eq!(decoded.seq, expected);
        }
        for invalid in [
            r#"{"seq":9223372036854775808}"#,
            r#"{"seq":1.5}"#,
            r#"{"seq":"not-a-sequence"}"#,
        ] {
            if serde_json::from_str::<SequenceProbe>(invalid).is_ok() {
                return Err(format!(
                    "invalid event sequence decoded successfully: {invalid}"
                ));
            }
        }
        Ok(())
    }

    #[test]
    fn task_runtime_ipc_receipts_preserve_typed_wire_fields() -> Result<(), String> {
        let planned = TaskRunResumeReceipt {
            kind: TaskRunResumeKind::Resumed,
            run_id: "run-planned".to_string(),
            turn_id: None,
        };
        let planned_json = serde_json::to_value(&planned).map_err(|error| error.to_string())?;
        assert_eq!(
            planned_json.get("kind").and_then(serde_json::Value::as_str),
            Some("resumed")
        );
        assert!(
            planned_json
                .get("turn_id")
                .is_some_and(serde_json::Value::is_null)
        );

        let continuation = TaskRunResumeReceipt {
            kind: TaskRunResumeKind::ContinuationResumed,
            run_id: "run-continuation".to_string(),
            turn_id: Some("turn-1".to_string()),
        };
        let encoded = serde_json::to_string(&continuation).map_err(|error| error.to_string())?;
        let decoded: TaskRunResumeReceipt =
            serde_json::from_str(&encoded).map_err(|error| error.to_string())?;
        assert_eq!(decoded, continuation);
        assert_eq!(decoded.turn_id.as_deref(), Some("turn-1"));

        let control = TaskRunControlReceipt {
            success: false,
            run_id: "already-terminal".to_string(),
        };
        assert!(!control.success);
        let retry = TaskRetryReceipt {
            kind: TaskRetryKind::RecoveryRetryRecorded,
            run_id: "retry-run".to_string(),
            task_id: "task-1".to_string(),
            next_attempt: None,
        };
        let retry_json = serde_json::to_value(retry).map_err(|error| error.to_string())?;
        assert_eq!(
            retry_json.get("kind").and_then(serde_json::Value::as_str),
            Some("recovery_retry_recorded")
        );
        assert!(
            retry_json
                .get("next_attempt")
                .is_some_and(serde_json::Value::is_null)
        );
        Ok(())
    }

    #[test]
    fn status_machine_allows_documented_transitions() {
        use TaskRunStatus::*;
        assert!(Pending.can_transition_to(Running));
        assert!(Pending.can_transition_to(Cancelled));
        assert!(Running.can_transition_to(Paused));
        assert!(Running.can_transition_to(Completed));
        assert!(Running.can_transition_to(Failed));
        assert!(Paused.can_transition_to(Running));
        assert!(Failed.can_transition_to(Running)); // 重试
        assert!(!Cancelled.can_transition_to(Running));
        assert!(!Completed.can_transition_to(Running));
    }

    #[test]
    fn status_machine_rejects_invalid_transitions() {
        use TaskRunStatus::*;
        assert!(Pending.can_transition_to(Running)); // 修复:主 agent ReAct 路径需要 Pending→Running 直达
        assert!(!Running.can_transition_to(Pending));
        assert!(!Completed.can_transition_to(Running));
        assert!(!Cancelled.can_transition_to(Running));
    }

    #[test]
    fn status_roundtrips_through_string() {
        // Enumerate every variant — guards against future additions
        // forgetting to wire up `from_str`.
        let all = [
            TaskRunStatus::Pending,
            TaskRunStatus::Running,
            TaskRunStatus::Paused,
            TaskRunStatus::Cancelled,
            TaskRunStatus::Failed,
            TaskRunStatus::Completed,
        ];
        for s in all {
            assert_eq!(TaskRunStatus::from_str(s.as_str()), Some(s), "{s:?}");
        }
        assert_eq!(TaskRunStatus::from_str("unknown"), None);
    }

    #[test]
    fn read_only_kinds_parallelize() {
        assert!(PlanTaskKind::ReadOnlyReview.is_read_only());
        assert!(PlanTaskKind::Investigation.is_read_only());
        assert!(PlanTaskKind::Review.is_read_only());
        assert!(!PlanTaskKind::Implementation.is_read_only());
        assert!(!PlanTaskKind::Verification.is_read_only());
    }

    #[test]
    fn plan_task_round_trips_through_framework_task() -> Result<(), String> {
        let task = PlanTask {
            id: "t1".to_string(),
            title: "Inspect runtime".to_string(),
            description: "Read task runtime code".to_string(),
            kind: PlanTaskKind::Investigation,
            agent_role: "explorer".to_string(),
            domain_profile: DomainProfile::AiCoding,
            depends_on: vec!["t0".to_string()],
            parallel_group: Some("g1".to_string()),
            execution_target: Some(TaskExecutionTarget {
                group_id: "group-1".to_string(),
                subagent_role: "explorer".to_string(),
                address: crate::agent_router::AgentAddress::new(
                    crate::workspace::WorkspaceId::from_name("remote"),
                    "remote-conversation",
                ),
            }),
            files: vec!["src/lib.rs".to_string()],
            allowed_tools: vec!["read_file".to_string()],
            required_artifacts: vec!["report.md".to_string()],
            execution_checks: vec!["cargo check".to_string()],
            acceptance_criteria: vec!["root cause is explained".to_string()],
            retry_count: 1,
            max_retries: 2,
            failure_fingerprint: Some("failure-1".to_string()),
            status: echo_agent::tasks::TaskStatus::Running,
            claim: Some(echo_agent::tasks::TaskClaim::new(
                7,
                2,
                "spec-hash".to_string(),
            )),
            sort_order: 10,
        };

        let runtime = echo_agent::tasks::Task::try_from(&task)?;

        assert_eq!(runtime.spec.id, "t1");
        assert_eq!(
            runtime.execution.status,
            echo_agent::tasks::TaskStatus::Running
        );
        assert_eq!(runtime.spec.depends_on, vec!["t0".to_string()]);
        assert_eq!(runtime.spec.max_retries, 2);
        assert_eq!(runtime.execution.retry_count, 1);
        assert_eq!(
            runtime.execution.failure_fingerprint.as_deref(),
            Some("failure-1")
        );
        let extension: EkoTaskExtension = runtime
            .spec
            .extension_as()
            .map_err(|error| error.to_string())?;
        assert_eq!(extension.kind, PlanTaskKind::Investigation);
        assert_eq!(extension.agent_role, "explorer");
        assert_eq!(extension.domain_profile, DomainProfile::AiCoding);
        assert_eq!(extension.parallel_group.as_deref(), Some("g1"));
        assert_eq!(extension.execution_target, task.execution_target);
        assert_eq!(extension.files, task.files);
        assert_eq!(extension.allowed_tools, task.allowed_tools);
        assert_eq!(extension.required_artifacts, task.required_artifacts);
        assert_eq!(extension.execution_checks, task.execution_checks);
        assert_eq!(extension.acceptance_criteria, task.acceptance_criteria);
        assert_eq!(extension.sort_order, 10);

        let round_trip = PlanTask::try_from(runtime)?;
        assert_eq!(round_trip.id, task.id);
        assert_eq!(round_trip.kind, task.kind);
        assert_eq!(round_trip.domain_profile, task.domain_profile);
        assert_eq!(round_trip.depends_on, task.depends_on);
        assert_eq!(round_trip.execution_target, task.execution_target);
        assert_eq!(round_trip.required_artifacts, task.required_artifacts);
        assert_eq!(round_trip.execution_checks, task.execution_checks);
        assert_eq!(round_trip.acceptance_criteria, task.acceptance_criteria);
        assert_eq!(round_trip.failure_fingerprint, task.failure_fingerprint);
        assert_eq!(round_trip.status, task.status);
        assert_eq!(round_trip.title, task.title);
        assert_eq!(round_trip.description, task.description);
        assert_eq!(round_trip.agent_role, task.agent_role);
        assert_eq!(round_trip.files, task.files);
        assert_eq!(round_trip.allowed_tools, task.allowed_tools);
        assert_eq!(round_trip.retry_count, task.retry_count);
        assert_eq!(round_trip.max_retries, task.max_retries);
        assert_eq!(round_trip.claim, task.claim);
        assert_eq!(round_trip.sort_order, task.sort_order);
        Ok(())
    }

    #[test]
    fn canonical_task_status_restart_round_trip_preserves_framework_and_eko_fields()
    -> Result<(), String> {
        let framework_statuses = vec![
            echo_agent::tasks::TaskStatus::Pending,
            echo_agent::tasks::TaskStatus::Running,
            echo_agent::tasks::TaskStatus::Blocked("dependency failed".to_string()),
            echo_agent::tasks::TaskStatus::Completed,
            echo_agent::tasks::TaskStatus::Failed("compile failed".to_string()),
            echo_agent::tasks::TaskStatus::Skipped,
            echo_agent::tasks::TaskStatus::Cancelled,
            echo_agent::tasks::TaskStatus::TimedOut {
                error: "deadline elapsed".to_string(),
            },
            echo_agent::tasks::TaskStatus::Retrying {
                attempt: 3,
                last_error: "provider unavailable".to_string(),
            },
            echo_agent::tasks::TaskStatus::Paused("user paused".to_string()),
        ];
        for status in framework_statuses {
            let encoded = serde_json::to_vec(&status).map_err(|error| error.to_string())?;
            let restored: echo_agent::tasks::TaskStatus =
                serde_json::from_slice(&encoded).map_err(|error| error.to_string())?;
            assert_eq!(restored, status);
        }

        let statuses = vec![
            echo_agent::tasks::TaskStatus::Pending,
            echo_agent::tasks::TaskStatus::Running,
            echo_agent::tasks::TaskStatus::Blocked("dependency failed".to_string()),
            echo_agent::tasks::TaskStatus::Completed,
            echo_agent::tasks::TaskStatus::Failed("compile failed".to_string()),
            echo_agent::tasks::TaskStatus::Skipped,
            echo_agent::tasks::TaskStatus::Cancelled,
            echo_agent::tasks::TaskStatus::TimedOut {
                error: "deadline elapsed".to_string(),
            },
        ];

        for (ordinal, status) in statuses.into_iter().enumerate() {
            let task = PlanTask {
                id: format!("restart-status-{ordinal}"),
                title: "Restart status fixture".to_string(),
                description: "Serialize and restore the canonical task".to_string(),
                kind: PlanTaskKind::Investigation,
                agent_role: "explorer".to_string(),
                domain_profile: DomainProfile::AiCoding,
                files: vec!["src/runtime.rs".to_string()],
                allowed_tools: vec!["read_file".to_string()],
                retry_count: 2,
                max_retries: 4,
                failure_fingerprint: Some("fixture-fingerprint".to_string()),
                status: status.clone(),
                claim: Some(echo_agent::tasks::TaskClaim::new(
                    3,
                    2,
                    format!("fixture-spec-hash-{ordinal}"),
                )),
                sort_order: 11,
                ..PlanTask::default()
            };
            let canonical = echo_agent::tasks::Task::try_from(&task)?;
            assert_eq!(canonical.execution.status, status);

            // Simulate the file-backed restart boundary: the framework Task
            // is serialized, dropped, and decoded before the EKO adapter sees
            // it again.
            let encoded = serde_json::to_vec(&canonical).map_err(|error| error.to_string())?;
            let restored: echo_agent::tasks::Task =
                serde_json::from_slice(&encoded).map_err(|error| error.to_string())?;
            assert_eq!(restored, canonical);

            let projected = PlanTask::try_from(restored)?;
            assert_eq!(projected.id, task.id);
            assert_eq!(projected.title, task.title);
            assert_eq!(projected.description, task.description);
            assert_eq!(projected.kind, task.kind);
            assert_eq!(projected.agent_role, task.agent_role);
            assert_eq!(projected.domain_profile, task.domain_profile);
            assert_eq!(projected.files, task.files);
            assert_eq!(projected.allowed_tools, task.allowed_tools);
            assert_eq!(projected.retry_count, task.retry_count);
            assert_eq!(projected.max_retries, task.max_retries);
            assert_eq!(projected.failure_fingerprint, task.failure_fingerprint);
            assert_eq!(projected.status, task.status);
            assert_eq!(projected.claim, task.claim);
            assert_eq!(projected.sort_order, task.sort_order);
        }
        Ok(())
    }

    #[test]
    fn retrying_and_paused_are_one_way_todo_projections() {
        let retrying = echo_agent::tasks::TaskStatus::Retrying {
            attempt: 3,
            last_error: "provider unavailable".to_string(),
        };
        assert_eq!(
            TodoStatus::project_task_status(&retrying),
            TodoStatus::Running
        );

        let paused = echo_agent::tasks::TaskStatus::Paused("user paused".to_string());
        assert_eq!(
            TodoStatus::project_task_status(&paused),
            TodoStatus::Pending
        );
    }

    #[test]
    fn plan_task_status_projection_preserves_failure_detail() -> Result<(), String> {
        let task = PlanTask {
            id: "failed-task".to_string(),
            title: "Failed task".to_string(),
            description: "Exercise status projection".to_string(),
            kind: PlanTaskKind::Investigation,
            agent_role: "explorer".to_string(),
            domain_profile: DomainProfile::AiCoding,
            depends_on: Vec::new(),
            parallel_group: None,
            execution_target: None,
            files: Vec::new(),
            allowed_tools: Vec::new(),
            required_artifacts: Vec::new(),
            execution_checks: Vec::new(),
            acceptance_criteria: vec!["failure is persisted".to_string()],
            retry_count: 2,
            max_retries: 2,
            failure_fingerprint: Some("compile-error".to_string()),
            status: echo_agent::tasks::TaskStatus::Failed("cargo check failed".to_string()),
            claim: None,
            sort_order: 0,
        };

        let framework_task = echo_agent::tasks::Task::try_from(&task)?;

        assert_eq!(
            framework_task.execution.status,
            echo_agent::tasks::TaskStatus::Failed("cargo check failed".to_string())
        );
        assert_eq!(
            framework_task.execution.failure_fingerprint.as_deref(),
            Some("compile-error")
        );
        assert_eq!(
            TodoStatus::project_task_status(&framework_task.execution.status),
            TodoStatus::Failed
        );
        Ok(())
    }

    #[test]
    fn task_status_projection_preserves_cancelled_and_timed_out() {
        for (framework, expected) in [
            (
                echo_agent::tasks::TaskStatus::Cancelled,
                TodoStatus::Cancelled,
            ),
            (
                echo_agent::tasks::TaskStatus::TimedOut {
                    error: "deadline".to_string(),
                },
                TodoStatus::TimedOut,
            ),
        ] {
            assert_eq!(TodoStatus::project_task_status(&framework), expected);
        }
    }

    #[test]
    fn plan_revision_artifact_excludes_execution_and_todo_state() -> Result<(), String> {
        let task = PlanTask {
            id: "artifact-task".to_string(),
            title: "Artifact task".to_string(),
            status: echo_agent::tasks::TaskStatus::Failed("execution failed".to_string()),
            retry_count: 2,
            failure_fingerprint: Some("failure-fingerprint".to_string()),
            claim: Some(echo_agent::tasks::TaskClaim::new(
                4,
                2,
                "spec-hash".to_string(),
            )),
            ..PlanTask::default()
        };
        let plan = TaskPlan {
            plan_id: "plan-artifact".to_string(),
            run_id: "run-artifact".to_string(),
            revision: 4,
            domain_profile: DomainProfile::General,
            goal_revision: 1,
            goal_sha256: "goal-hash".to_string(),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Sequential,
            tasks: vec![task],
        };

        let value =
            serde_json::to_value(plan.specification()).map_err(|error| error.to_string())?;
        let task = value
            .get("tasks")
            .and_then(serde_json::Value::as_array)
            .and_then(|tasks| tasks.first())
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| "plan revision omitted its task specification".to_string())?;
        for execution_field in [
            "status",
            "status_detail",
            "retry_count",
            "failure_fingerprint",
            "claim",
        ] {
            if task.contains_key(execution_field) {
                return Err(format!(
                    "plan revision leaked execution field '{execution_field}'"
                ));
            }
        }
        Ok(())
    }

    #[test]
    fn terminal_subagent_result_bounds_utf8_failure_text() {
        let long = "中".repeat(2_000);
        let result =
            SubagentOutcome::terminal(SubagentStatus::TimedOut, long.clone(), vec![long; 70]);

        assert_eq!(result.summary.chars().count(), 1_200);
        assert_eq!(result.remaining_work.len(), 64);
        assert!(
            result
                .remaining_work
                .iter()
                .all(|item| item.chars().count() == 500)
        );
    }

    #[test]
    fn framework_evidence_round_trips_and_projects_product_fields() {
        let mut outcome = echo_agent::subagent::SubagentOutcome {
            contract_version: 2,
            status: echo_agent::subagent::SubagentStatus::Completed,
            summary: "done".to_string(),
            artifacts: Vec::new(),
            evidence: vec![
                echo_agent::subagent::SubagentEvidence {
                    kind: "tool_result".to_string(),
                    subject: "shell".to_string(),
                    outcome: Some("succeeded".to_string()),
                    details: "ok".to_string(),
                    source: echo_agent::subagent::SubagentEvidenceSource::Observed,
                    attributes: serde_json::json!({ "args": { "command": "cargo test" } }),
                },
                echo_agent::subagent::SubagentEvidence {
                    kind: "tool_result".to_string(),
                    subject: "write_file".to_string(),
                    outcome: Some("succeeded".to_string()),
                    details: String::new(),
                    source: echo_agent::subagent::SubagentEvidenceSource::Observed,
                    attributes: serde_json::json!({ "args": { "path": "src/lib.rs" } }),
                },
                echo_agent::subagent::SubagentEvidence {
                    kind: "domain_fact".to_string(),
                    subject: "schema".to_string(),
                    outcome: Some("confirmed".to_string()),
                    details: "field-level evidence".to_string(),
                    source: echo_agent::subagent::SubagentEvidenceSource::Reported,
                    attributes: serde_json::json!({ "confidence": "high" }),
                },
            ],
            verification: Vec::new(),
            remaining_work: Vec::new(),
            touched_files: echo_agent::subagent::SubagentTouchedFiles::default(),
        };
        outcome.refresh_derived_views();

        let projected = outcome.clone();

        assert_eq!(projected.evidence.len(), outcome.evidence.len());
        assert_eq!(
            projected.evidence.get(2).map(|item| item.kind.as_str()),
            Some("domain_fact")
        );
        assert_eq!(
            projected
                .evidence
                .get(2)
                .and_then(|item| item.attributes.get("confidence"))
                .and_then(serde_json::Value::as_str),
            Some("high")
        );
        assert_eq!(projected.verification.len(), 1);
        assert_eq!(projected.touched_files.written, vec!["src/lib.rs"]);
    }

    #[test]
    fn unattended_readonly_whitelist() {
        // Stage 1 ReadOnlyPlanNoShell: only 3 kinds are allowed.
        assert!(PlanTaskKind::ReadOnlyReview.is_unattended_readonly_allowed());
        assert!(PlanTaskKind::Investigation.is_unattended_readonly_allowed());
        assert!(PlanTaskKind::Summary.is_unattended_readonly_allowed());
        // TestPlan and Review are read-only for parallelism but NOT for unattended.
        assert!(!PlanTaskKind::TestPlan.is_unattended_readonly_allowed());
        assert!(!PlanTaskKind::Review.is_unattended_readonly_allowed());
        // Mutating kinds are always rejected.
        assert!(!PlanTaskKind::Implementation.is_unattended_readonly_allowed());
        assert!(!PlanTaskKind::Debugging.is_unattended_readonly_allowed());
        assert!(!PlanTaskKind::Verification.is_unattended_readonly_allowed());
    }

    #[test]
    fn unattended_write_mode_default_and_roundtrip() {
        // D7 stage 2: default is Worktree (safe isolation).
        assert_eq!(
            UnattendedWriteMode::default(),
            UnattendedWriteMode::Worktree
        );
        // Round-trip every variant.
        for m in [
            UnattendedWriteMode::Worktree,
            UnattendedWriteMode::Disabled,
            UnattendedWriteMode::InPlace,
        ] {
            assert_eq!(UnattendedWriteMode::from_str(m.as_str()), Some(m));
        }
        assert_eq!(UnattendedWriteMode::from_str("bogus"), None);
        // writes_allowed: Worktree + InPlace permit writes; Disabled bans them.
        assert!(UnattendedWriteMode::Worktree.writes_allowed());
        assert!(UnattendedWriteMode::InPlace.writes_allowed());
        assert!(!UnattendedWriteMode::Disabled.writes_allowed());
    }

    #[test]
    fn profile_discriminator_is_stable() {
        for p in [
            DomainProfile::General,
            DomainProfile::AiCoding,
            DomainProfile::DataAnalysis,
            DomainProfile::AcademicResearch,
            DomainProfile::MedicalResearch,
        ] {
            assert_eq!(DomainProfile::from_str(p.as_str()), Some(p));
        }
        assert_eq!(DomainProfile::from_str("unknown"), None);
    }

    #[test]
    fn delayed_resume_rejects_deleted_and_recreated_run_identity() {
        let now = Utc::now();
        let run = TaskRun {
            run_id: "same-run".to_string(),
            workspace_id: "workspace-a".to_string(),
            conversation_id: "conversation-a".to_string(),
            root_message_id: "root-a".to_string(),
            domain_profile: DomainProfile::General,
            status: TaskRunStatus::Paused,
            goal: "goal".to_string(),
            goal_revision: 1,
            goal_sha256: task_goal_sha256("goal"),
            plan_id: None,
            route: "task".to_string(),
            attended_mode: AttendedMode::Attended,
            attachments: Vec::new(),
            created_at: now,
            updated_at: now,
        };
        let mut snapshot = RunStateSnapshot {
            run,
            tasks: Vec::new(),
            continuation: None,
            background_cells: Vec::new(),
            journal_sequence: 7,
            event_index: RunStateEventIndex::default(),
        };
        let identity = TaskRunResumeIdentity::capture(&snapshot);
        assert!(identity.validate_resumable(&snapshot).is_ok());

        snapshot.run.created_at = now + chrono::Duration::milliseconds(1);
        assert!(
            identity
                .validate_resumable(&snapshot)
                .is_err_and(|error| error.contains("identity changed"))
        );
    }
}
