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

/// Manual override of how a user message should be handled.
/// `Auto` lets the agent choose an execution path; the other two enforce the
/// available tool surface and formal-run contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "InteractionMode")]
pub enum InteractionMode {
    /// Prefer direct chat while retaining the explicit TaskRuntime graph tools.
    Chat,
    /// Create a formal TaskRuntime run and require a reviewable plan lifecycle.
    Task,
    /// Agent-selected direct or formal TaskRuntime execution (default).
    #[default]
    Auto,
}

impl InteractionMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            InteractionMode::Chat => "chat",
            InteractionMode::Task => "task",
            InteractionMode::Auto => "auto",
        }
    }

    pub fn as_u8(&self) -> u8 {
        match self {
            InteractionMode::Chat => 1,
            InteractionMode::Task => 2,
            InteractionMode::Auto => 0,
        }
    }

    pub fn from_u8(value: u8) -> Self {
        match value {
            1 => InteractionMode::Chat,
            2 => InteractionMode::Task,
            _ => InteractionMode::Auto,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            InteractionMode::Chat => "Chat",
            InteractionMode::Task => "Task",
            InteractionMode::Auto => "Auto",
        }
    }

    /// Per-turn behavior contract injected into the user message. Keeping this
    /// here ensures GUI, TUI, and channel entry points stay behaviorally equal.
    pub fn prompt_hint(&self) -> &'static str {
        match self {
            InteractionMode::Chat => {
                "Chat mode. Resolve simple requests directly. When a visible task list or delegated execution is useful, use the same task_create/task_update/task_list/task_execute API as every other mode; a single task does not require an artificial wrapper or DAG."
            }
            InteractionMode::Task => {
                "Task mode. Materialize a formal, reviewable task graph. The TaskRun already represents the overall goal, so never create a wrapper or placeholder task for it. Submit the complete initial graph in one task_create call using its tasks array, including when the graph has only one task. Inspect the returned revision with task_list, and pass it as revision to task_execute. Use task_update with the current base_revision for later changes. Keep task status and verification current. Do not claim dispatch before task_execute starts."
            }
            InteractionMode::Auto => {
                "Auto mode. Choose between direct work and formal TaskRuntime execution. Answer or act directly for simple work. When a visible task list, Subagent delegation, multi-step work, dependencies, or parallelism is useful, submit the complete graph in one task_create call using its tasks array, including for a single task. Inspect the revision with task_list, and pass it as revision to task_execute. Use task_update for later changes. Do not dispatch ad-hoc Subagents in Auto mode."
            }
        }
    }
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

    pub fn to_task_kind(self) -> echo_agent::tasks::TaskKind {
        match self {
            PlanTaskKind::ReadOnlyReview => echo_agent::tasks::TaskKind::ReadOnlyReview,
            PlanTaskKind::Investigation => echo_agent::tasks::TaskKind::Investigation,
            PlanTaskKind::TestPlan => echo_agent::tasks::TaskKind::TestPlan,
            PlanTaskKind::Implementation => echo_agent::tasks::TaskKind::Implementation,
            PlanTaskKind::Debugging => echo_agent::tasks::TaskKind::Debugging,
            PlanTaskKind::Review => echo_agent::tasks::TaskKind::Review,
            PlanTaskKind::Summary => echo_agent::tasks::TaskKind::Summary,
            PlanTaskKind::Verification => echo_agent::tasks::TaskKind::Verification,
        }
    }

    pub fn from_task_kind(kind: echo_agent::tasks::TaskKind) -> Self {
        match kind {
            echo_agent::tasks::TaskKind::ReadOnlyReview => Self::ReadOnlyReview,
            echo_agent::tasks::TaskKind::Investigation => Self::Investigation,
            echo_agent::tasks::TaskKind::TestPlan => Self::TestPlan,
            echo_agent::tasks::TaskKind::Implementation => Self::Implementation,
            echo_agent::tasks::TaskKind::Debugging => Self::Debugging,
            echo_agent::tasks::TaskKind::Review => Self::Review,
            echo_agent::tasks::TaskKind::Summary => Self::Summary,
            echo_agent::tasks::TaskKind::Verification => Self::Verification,
        }
    }
}

// ── Todo status ─────────────────────────────────────────────────────────

/// Status of an individual todo / plan task.
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

    #[allow(clippy::should_implement_trait)] // inherent helper returning Option; not the FromStr trait
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "pending" => TodoStatus::Pending,
            "running" => TodoStatus::Running,
            "blocked" => TodoStatus::Blocked,
            "completed" => TodoStatus::Completed,
            "failed" => TodoStatus::Failed,
            "cancelled" => TodoStatus::Cancelled,
            "timed_out" => TodoStatus::TimedOut,
            "skipped" => TodoStatus::Skipped,
            _ => return None,
        })
    }

    /// Project the persisted UI status into the framework's authoritative state.
    pub fn to_task_status(self, detail: Option<&str>) -> echo_agent::tasks::TaskStatus {
        let detail = detail.unwrap_or_else(|| self.as_str()).to_string();
        match self {
            TodoStatus::Pending => echo_agent::tasks::TaskStatus::Pending,
            TodoStatus::Running => echo_agent::tasks::TaskStatus::Running,
            TodoStatus::Blocked => echo_agent::tasks::TaskStatus::Blocked(detail),
            TodoStatus::Completed => echo_agent::tasks::TaskStatus::Completed,
            TodoStatus::Failed => echo_agent::tasks::TaskStatus::Failed(detail),
            TodoStatus::Cancelled => echo_agent::tasks::TaskStatus::Cancelled,
            TodoStatus::TimedOut => echo_agent::tasks::TaskStatus::TimedOut { error: detail },
            TodoStatus::Skipped => echo_agent::tasks::TaskStatus::Skipped,
        }
    }

    /// Convert framework state only when the EKO projection can represent it exactly.
    pub fn try_from_task_status(status: &echo_agent::tasks::TaskStatus) -> Result<Self, String> {
        match status {
            echo_agent::tasks::TaskStatus::Pending => Ok(TodoStatus::Pending),
            echo_agent::tasks::TaskStatus::Running => Ok(TodoStatus::Running),
            echo_agent::tasks::TaskStatus::Blocked(_) => Ok(TodoStatus::Blocked),
            echo_agent::tasks::TaskStatus::Completed => Ok(TodoStatus::Completed),
            echo_agent::tasks::TaskStatus::Failed(_) => Ok(TodoStatus::Failed),
            echo_agent::tasks::TaskStatus::Cancelled => Ok(TodoStatus::Cancelled),
            echo_agent::tasks::TaskStatus::TimedOut { .. } => Ok(TodoStatus::TimedOut),
            echo_agent::tasks::TaskStatus::Skipped => Ok(TodoStatus::Skipped),
            echo_agent::tasks::TaskStatus::Retrying { .. }
            | echo_agent::tasks::TaskStatus::Paused(_) => Err(format!(
                "framework task status {status:?} has no lossless EKO todo projection"
            )),
        }
    }

    /// UI-only projection for framework states that carry richer lifecycle
    /// semantics than the current todo badge model.
    pub fn project_task_status(status: &echo_agent::tasks::TaskStatus) -> Self {
        match status {
            echo_agent::tasks::TaskStatus::Pending => TodoStatus::Pending,
            echo_agent::tasks::TaskStatus::Running
            | echo_agent::tasks::TaskStatus::Retrying { .. } => TodoStatus::Running,
            echo_agent::tasks::TaskStatus::Blocked(_)
            | echo_agent::tasks::TaskStatus::Paused(_) => TodoStatus::Blocked,
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
/// persistence transaction as the state update (see `store.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "RuntimeEventKind")]
pub enum RuntimeEventKind {
    RunCreated,
    /// The user explicitly replaced the sole authoritative TaskRun Goal.
    RunGoalUpdated,
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
    TodoUpdated,
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
    /// The framework accepted the instruction for live or next-attempt delivery.
    SubagentGuidanceDelivered,
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
            TodoUpdated => "todo_updated",
            Started => "started",
            Running => "running",
            Completed => "completed",
            Failed => "failed",
            Cancelled => "cancelled",
            TimedOut => "timed_out",
            SubagentAssigned => "subagent_assigned",
            SubagentReleased => "subagent_released",
            SubagentGuidanceQueued => "subagent_guidance_queued",
            SubagentGuidanceDelivered => "subagent_guidance_delivered",
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
            "todo_updated" => TodoUpdated,
            "started" => Started,
            "running" => Running,
            "completed" => Completed,
            "failed" => Failed,
            "cancelled" => Cancelled,
            "timed_out" => TimedOut,
            "subagent_assigned" => SubagentAssigned,
            "subagent_released" => SubagentReleased,
            "subagent_guidance_queued" => SubagentGuidanceQueued,
            "subagent_guidance_delivered" => SubagentGuidanceDelivered,
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

/// A structured plan attached to a run. Generated by the planning runtime
/// (PR 2); never free-form markdown from the model.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, rename = "TaskPlan")]
pub struct TaskPlan {
    pub plan_id: String,
    pub run_id: String,
    /// Monotonic committed plan revision. Revision 1 is the initial complete
    /// DAG; every accepted dynamic patch increments it exactly once.
    #[ts(type = "number")]
    pub revision: u64,
    pub domain_profile: DomainProfile,
    #[ts(type = "number")]
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

/// EKO file/UI projection of the immutable framework task specification.
///
/// This DTO preserves EKO metadata for `plan.json` and generated TypeScript;
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
    pub files: Vec<String>,
    pub allowed_tools: Vec<String>,
    pub required_artifacts: Vec<String>,
    pub execution_checks: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub max_retries: u32,
    #[ts(type = "number")]
    pub sort_order: i64,
}

impl EkoTaskSpec {
    pub(crate) fn to_task_spec(&self) -> Result<echo_agent::tasks::TaskSpec, String> {
        let metadata = serde_json::to_value(EkoTaskMetadata {
            domain_profile: self.domain_profile,
            parallel_group: self.parallel_group.clone(),
            sort_order: self.sort_order,
        })
        .map_err(|error| format!("task '{}' has invalid EKO metadata: {error}", self.id))?;
        Ok(echo_agent::tasks::TaskSpec {
            id: self.id.clone(),
            title: self.title.clone(),
            description: self.description.clone(),
            kind: self.kind.to_task_kind(),
            agent_role: self.agent_role.clone(),
            depends_on: self.depends_on.clone(),
            files: self.files.clone(),
            allowed_tools: self.allowed_tools.clone(),
            required_artifacts: self.required_artifacts.clone(),
            execution_checks: self.execution_checks.clone(),
            acceptance_criteria: self.acceptance_criteria.clone(),
            max_retries: self.max_retries,
            metadata,
        })
    }
}

/// EKO file projection of framework task execution state.
///
/// The shared `TaskStatus` remains authoritative and lossless. `TodoStatus` is
/// derived only when building UI-facing plan/todo projections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EkoTaskExecution {
    pub task_id: String,
    pub status: echo_agent::tasks::TaskStatus,
    pub retry_count: u32,
    pub failure_fingerprint: Option<String>,
    #[serde(default)]
    pub claim: Option<echo_agent::tasks::TaskClaim>,
}

/// EKO-only metadata carried through the framework runtime extension point.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EkoTaskMetadata {
    pub domain_profile: DomainProfile,
    pub parallel_group: Option<String>,
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

impl EkoTaskExecution {
    pub fn pending(task_id: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            status: echo_agent::tasks::TaskStatus::Pending,
            retry_count: 0,
            failure_fingerprint: None,
            claim: None,
        }
    }
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunStateSnapshot {
    pub run: TaskRun,
    pub tasks: Vec<EkoTaskExecution>,
    /// Event-folded long-horizon control state. Absent for ordinary one-shot runs.
    #[serde(default)]
    pub continuation: Option<RunContinuationState>,
    /// Event-folded background command cells owned by this run.
    #[serde(default)]
    pub background_cells: Vec<BackgroundCellState>,
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
}

impl RunTurnBinding {
    pub fn resume(
        run_id: impl Into<String>,
        turn_id: impl Into<String>,
        root_message_id: impl Into<String>,
    ) -> Self {
        Self {
            run_id: Some(run_id.into()),
            turn_id: turn_id.into(),
            root_message_id: root_message_id.into(),
            origin: RunTurnOrigin::Resume,
            transcript_visibility: TurnVisibility::Visible,
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
            deferred: false,
            deferred_reason: None,
        }
    }
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
    pub phase: String,
    pub exit_code: Option<i32>,
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
        self.finished_at.is_none()
    }
}

/// Materialized EKO plan node used by tools, persistence, review, and UI.
/// Framework `Task` remains the authority for validation and DAG traversal.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, rename = "PlanTask")]
pub struct PlanTask {
    pub id: String,
    pub title: String,
    pub description: String,
    pub kind: PlanTaskKind,
    pub agent_role: String,
    pub domain_profile: DomainProfile,
    pub depends_on: Vec<String>,
    pub parallel_group: Option<String>,
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
    pub status: TodoStatus,
    /// Error/block reason carried by the shared status. This is deliberately
    /// independent from `failure_fingerprint`.
    #[serde(default)]
    pub status_detail: Option<String>,
    /// Durable dispatch claim. Internal runtime state; UI joins on task and
    /// Subagent execution ids instead.
    #[serde(default)]
    #[ts(skip)]
    pub claim: Option<echo_agent::tasks::TaskClaim>,
    /// Stable sort key for display ordering. Set by plan generation (sequential
    /// index) and updated by `reorder_tasks`. Separated from `parallel_group`
    /// (which encodes parallel-fanout grouping, not display order) to avoid
    /// semantic pollution.
    #[ts(type = "number")]
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
            files: Vec::new(),
            allowed_tools: Vec::new(),
            required_artifacts: Vec::new(),
            execution_checks: Vec::new(),
            acceptance_criteria: Vec::new(),
            retry_count: 0,
            max_retries: 3,
            failure_fingerprint: None,
            status: TodoStatus::Pending,
            status_detail: None,
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
            files: self.files.clone(),
            allowed_tools: self.allowed_tools.clone(),
            required_artifacts: self.required_artifacts.clone(),
            execution_checks: self.execution_checks.clone(),
            acceptance_criteria: self.acceptance_criteria.clone(),
            max_retries: self.max_retries,
            sort_order: self.sort_order,
        }
    }

    pub fn execution(&self) -> EkoTaskExecution {
        EkoTaskExecution {
            task_id: self.id.clone(),
            status: self.status.to_task_status(self.status_detail.as_deref()),
            retry_count: self.retry_count,
            failure_fingerprint: self.failure_fingerprint.clone(),
            claim: self.claim.clone(),
        }
    }

    pub fn from_parts(spec: EkoTaskSpec, execution: EkoTaskExecution) -> Self {
        Self {
            id: spec.id,
            title: spec.title,
            description: spec.description,
            kind: spec.kind,
            agent_role: spec.agent_role,
            domain_profile: spec.domain_profile,
            depends_on: spec.depends_on,
            parallel_group: spec.parallel_group,
            files: spec.files,
            allowed_tools: spec.allowed_tools,
            required_artifacts: spec.required_artifacts,
            execution_checks: spec.execution_checks,
            acceptance_criteria: spec.acceptance_criteria,
            retry_count: execution.retry_count,
            max_retries: spec.max_retries,
            failure_fingerprint: execution.failure_fingerprint,
            status: TodoStatus::project_task_status(&execution.status),
            status_detail: task_status_detail(&execution.status),
            claim: execution.claim,
            sort_order: spec.sort_order,
        }
    }

    /// Convert losslessly to the framework's canonical runtime task model.
    pub fn to_task(&self) -> echo_agent::tasks::Task {
        let metadata = serde_json::Value::Object(serde_json::Map::from_iter([
            (
                "domain_profile".to_string(),
                serde_json::Value::String(self.domain_profile.as_str().to_string()),
            ),
            (
                "parallel_group".to_string(),
                self.parallel_group
                    .clone()
                    .map(serde_json::Value::String)
                    .unwrap_or(serde_json::Value::Null),
            ),
            (
                "sort_order".to_string(),
                serde_json::Value::Number(self.sort_order.into()),
            ),
        ]));
        echo_agent::tasks::Task {
            spec: echo_agent::tasks::TaskSpec {
                id: self.id.clone(),
                title: self.title.clone(),
                description: self.description.clone(),
                kind: self.kind.to_task_kind(),
                agent_role: self.agent_role.clone(),
                depends_on: self.depends_on.clone(),
                files: self.files.clone(),
                allowed_tools: self.allowed_tools.clone(),
                required_artifacts: self.required_artifacts.clone(),
                execution_checks: self.execution_checks.clone(),
                acceptance_criteria: self.acceptance_criteria.clone(),
                max_retries: self.max_retries,
                metadata,
            },
            execution: echo_agent::tasks::TaskExecution {
                task_id: self.id.clone(),
                status: self.status.to_task_status(self.status_detail.as_deref()),
                retry_count: self.retry_count,
                failure_fingerprint: self.failure_fingerprint.clone(),
                claim: self.claim.clone(),
            },
        }
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
        let metadata: EkoTaskMetadata = serde_json::from_value(spec.metadata)
            .map_err(|error| format!("task '{}' has invalid EKO metadata: {error}", spec.id))?;
        let status = TodoStatus::try_from_task_status(&execution.status)?;
        let status_detail = task_status_detail(&execution.status);

        Ok(Self {
            id: spec.id,
            title: spec.title,
            description: spec.description,
            kind: PlanTaskKind::from_task_kind(spec.kind),
            agent_role: spec.agent_role,
            domain_profile: metadata.domain_profile,
            depends_on: spec.depends_on,
            parallel_group: metadata.parallel_group,
            files: spec.files,
            allowed_tools: spec.allowed_tools,
            required_artifacts: spec.required_artifacts,
            execution_checks: spec.execution_checks,
            acceptance_criteria: spec.acceptance_criteria,
            retry_count: execution.retry_count,
            max_retries: spec.max_retries,
            failure_fingerprint: execution.failure_fingerprint,
            status,
            status_detail,
            claim: execution.claim,
            sort_order: metadata.sort_order,
        })
    }
}

fn task_status_detail(status: &echo_agent::tasks::TaskStatus) -> Option<String> {
    match status {
        echo_agent::tasks::TaskStatus::Blocked(detail)
        | echo_agent::tasks::TaskStatus::Failed(detail)
        | echo_agent::tasks::TaskStatus::Paused(detail) => Some(detail.clone()),
        echo_agent::tasks::TaskStatus::TimedOut { error } => Some(error.clone()),
        echo_agent::tasks::TaskStatus::Retrying { last_error, .. } => Some(last_error.clone()),
        echo_agent::tasks::TaskStatus::Pending
        | echo_agent::tasks::TaskStatus::Running
        | echo_agent::tasks::TaskStatus::Completed
        | echo_agent::tasks::TaskStatus::Skipped
        | echo_agent::tasks::TaskStatus::Cancelled => None,
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

impl TaskPatch {
    pub(crate) fn to_task_spec_patch(&self) -> echo_agent::tasks::TaskSpecPatch {
        echo_agent::tasks::TaskSpecPatch {
            title: self.title.clone(),
            description: self.description.clone(),
            kind: self.kind.map(PlanTaskKind::to_task_kind),
            agent_role: self.agent_role.clone(),
            depends_on: self.depends_on.clone(),
            files: self.files.clone(),
            allowed_tools: self.allowed_tools.clone(),
            required_artifacts: self.required_artifacts.clone(),
            execution_checks: self.execution_checks.clone(),
            acceptance_criteria: self.acceptance_criteria.clone(),
            max_retries: self.max_retries,
        }
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

impl TaskUpdateRequest {
    pub(crate) fn to_task_plan_patch(&self) -> Result<echo_agent::tasks::TaskPlanPatch, String> {
        let mut operations = Vec::with_capacity(self.operations.len());
        for operation in &self.operations {
            operations.push(match operation {
                TaskUpdateOperation::Insert {
                    after_task_id,
                    task,
                } => echo_agent::tasks::TaskPlanPatchOp::Insert {
                    after_task_id: after_task_id.clone(),
                    task: task.to_task_spec()?,
                },
                TaskUpdateOperation::Update { task_id, patch } => {
                    echo_agent::tasks::TaskPlanPatchOp::Update {
                        task_id: task_id.clone(),
                        patch: patch.to_task_spec_patch(),
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
            base_revision: self.base_revision,
            reason: self.reason.clone(),
            operations,
        })
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
    use serde::Deserialize;
    // Accept either a string ("123") or a number (123) for robustness.
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Num {
        Str(String),
        Num(i64),
    }
    match Num::deserialize(d)? {
        Num::Str(s) => s.parse().map_err(serde::de::Error::custom),
        Num::Num(n) => Ok(n),
    }
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
// Subagent 统一重构(spec: docs/subagent-unification-plan.md):一次 subagent
// 派发的运行实例。Task → SubagentRun 关联通过 task_id 查询投影得到,PlanTask
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

/// Durable identity for one user control command and one exact task attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, rename = "SubagentControlIdentity")]
pub struct SubagentControlIdentity {
    pub run_id: String,
    pub task_id: String,
    pub execution_id: String,
    #[ts(type = "number")]
    pub plan_revision: u64,
    pub attempt: u32,
    pub command_id: String,
}

/// Whether guidance targets an already-active mailbox or one future attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "SubagentGuidanceKind")]
pub enum SubagentGuidanceKind {
    LiveMessage,
    NextAttempt,
}

impl SubagentGuidanceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LiveMessage => "live_message",
            Self::NextAttempt => "next_attempt",
        }
    }
}

/// Stable command status returned identically by GUI/TUI/CLI/channel adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "SubagentControlStatus")]
pub enum SubagentControlStatus {
    Pending,
    Delivered,
    Rejected,
    Settled,
}

impl SubagentControlStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Delivered => "delivered",
            Self::Rejected => "rejected",
            Self::Settled => "settled",
        }
    }
}

/// Idempotent projection returned for a durable Subagent command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, rename = "SubagentControlReceipt")]
pub struct SubagentControlReceipt {
    pub identity: SubagentControlIdentity,
    pub status: SubagentControlStatus,
    pub detail: Option<String>,
    pub framework_turn_id: Option<String>,
}

/// Lifecycle status of a [`SubagentRun`]. Mirrors the coarse states the
/// frontend already renders for the unified subagent concept, minus the
/// pending state (a SubagentRun only exists once dispatch has started).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "SubagentRunStatus")]
pub enum SubagentRunStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
    TimedOut,
}

impl SubagentRunStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            SubagentRunStatus::Running => "running",
            SubagentRunStatus::Completed => "completed",
            SubagentRunStatus::Failed => "failed",
            SubagentRunStatus::Cancelled => "cancelled",
            SubagentRunStatus::TimedOut => "timed_out",
        }
    }

    #[allow(clippy::should_implement_trait)] // inherent helper returning Option; not the FromStr trait
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "running" => SubagentRunStatus::Running,
            "completed" => SubagentRunStatus::Completed,
            "failed" => SubagentRunStatus::Failed,
            "cancelled" => SubagentRunStatus::Cancelled,
            "timed_out" => SubagentRunStatus::TimedOut,
            _ => return None,
        })
    }
}

impl From<echo_agent::agent::subagent::SubagentStatus> for SubagentRunStatus {
    fn from(status: echo_agent::agent::subagent::SubagentStatus) -> Self {
        match status {
            echo_agent::agent::subagent::SubagentStatus::Completed => Self::Completed,
            echo_agent::agent::subagent::SubagentStatus::Failed => Self::Failed,
            echo_agent::agent::subagent::SubagentStatus::Cancelled => Self::Cancelled,
            echo_agent::agent::subagent::SubagentStatus::TimedOut => Self::TimedOut,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, rename = "SubagentArtifactResult")]
pub struct SubagentArtifactResult {
    pub path: String,
    pub kind: String,
    pub bytes: Option<u64>,
    pub sha256: Option<String>,
    pub producer_execution_id: Option<String>,
    pub available: bool,
}

impl From<&echo_agent::agent::subagent::SubagentArtifact> for SubagentArtifactResult {
    fn from(artifact: &echo_agent::agent::subagent::SubagentArtifact) -> Self {
        Self {
            path: artifact.path.clone(),
            kind: artifact.kind.clone(),
            bytes: artifact.bytes,
            sha256: artifact.sha256.clone(),
            producer_execution_id: artifact.producer_execution_id.clone(),
            available: artifact.available,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "SubagentVerificationStatus")]
pub enum SubagentVerificationStatus {
    Passed,
    Failed,
    NotRun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "SubagentVerificationSource")]
pub enum SubagentVerificationSource {
    Observed,
    Reported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, rename = "SubagentVerificationResult")]
pub struct SubagentVerificationResult {
    pub check: String,
    pub status: SubagentVerificationStatus,
    pub details: String,
    pub source: SubagentVerificationSource,
}

impl From<&echo_agent::agent::subagent::SubagentVerification> for SubagentVerificationResult {
    fn from(verification: &echo_agent::agent::subagent::SubagentVerification) -> Self {
        Self {
            check: verification.check.clone(),
            status: match verification.status {
                echo_agent::agent::subagent::SubagentVerificationStatus::Passed => {
                    SubagentVerificationStatus::Passed
                }
                echo_agent::agent::subagent::SubagentVerificationStatus::Failed => {
                    SubagentVerificationStatus::Failed
                }
                echo_agent::agent::subagent::SubagentVerificationStatus::NotRun => {
                    SubagentVerificationStatus::NotRun
                }
            },
            details: verification.details.clone(),
            source: match verification.source {
                echo_agent::agent::subagent::SubagentVerificationSource::Observed => {
                    SubagentVerificationSource::Observed
                }
                echo_agent::agent::subagent::SubagentVerificationSource::Reported => {
                    SubagentVerificationSource::Reported
                }
            },
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, rename = "SubagentTouchedFiles")]
pub struct SubagentTouchedFiles {
    pub read: Vec<String>,
    pub written: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, rename = "SubagentTaskResult")]
pub struct SubagentTaskResult {
    pub contract_version: u32,
    pub status: SubagentRunStatus,
    pub summary: String,
    pub artifacts: Vec<SubagentArtifactResult>,
    pub verification: Vec<SubagentVerificationResult>,
    pub remaining_work: Vec<String>,
    pub touched_files: SubagentTouchedFiles,
}

impl SubagentTaskResult {
    pub fn from_framework(result: &echo_agent::agent::subagent::SubagentResult) -> Self {
        Self::from_framework_outcome(&result.outcome)
    }

    pub fn from_framework_outcome(outcome: &echo_agent::agent::subagent::SubagentOutcome) -> Self {
        Self {
            contract_version: outcome.contract_version,
            status: outcome.status.into(),
            summary: outcome.summary.clone(),
            artifacts: outcome.artifacts.iter().map(Into::into).collect(),
            verification: outcome.verification.iter().map(Into::into).collect(),
            remaining_work: outcome.remaining_work.clone(),
            touched_files: SubagentTouchedFiles {
                read: outcome.touched_files.read.clone(),
                written: outcome.touched_files.written.clone(),
            },
        }
    }

    pub fn terminal(
        status: SubagentRunStatus,
        summary: impl Into<String>,
        remaining_work: Vec<String>,
    ) -> Self {
        let summary: String = summary.into().chars().take(1_200).collect();
        let remaining_work = remaining_work
            .into_iter()
            .map(|item| item.chars().take(500).collect())
            .filter(|item: &String| !item.trim().is_empty())
            .take(64)
            .collect();
        Self {
            contract_version: 1,
            status,
            summary,
            artifacts: Vec::new(),
            verification: Vec::new(),
            remaining_work,
            touched_files: SubagentTouchedFiles::default(),
        }
    }
}

/// Aggregate cost/usage for a single [`SubagentRun`].
///
/// All fields are `Option` because they are populated progressively: a run
/// that just started has no usage yet. `duration_ms` is finalized on
/// completion.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, rename = "SubagentRunUsage")]
pub struct SubagentRunUsage {
    /// Total wall-clock duration in milliseconds (None while running).
    pub duration_ms: Option<u64>,
    /// Total tokens consumed (input + output), if reported by the framework.
    pub tokens_used: Option<u64>,
    /// Number of ReAct iterations executed, if reported.
    pub iterations: Option<u64>,
}

/// One subagent dispatch execution instance.
///
/// Created when the TaskRuntime dispatches a subagent role to execute a
/// [`PlanTask`]. `subagent_run_id` is the stable identity shared with the
/// framework `SubagentEvent::execution_id`, so the tauri bridge / frontend
/// can route thinking/tool/token streams without temporary id allocation.
///
/// Thinking/tool/token streams are NOT persisted here (they remain an
/// in-memory + realtime stream, matching the legacy execution behavior). Only
/// lifecycle + usage + result are durable.
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
    pub status: SubagentRunStatus,
    /// Aggregate cost. Populated progressively; finalized on completion.
    pub usage: SubagentRunUsage,
    /// Structured output returned to the Task (None while running).
    pub result: Option<SubagentTaskResult>,
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
            status: SubagentRunStatus::Running,
            usage: SubagentRunUsage::default(),
            result: None,
        }
    }
}

/// Result of a review gate over a task. When `outcome == NeedsFix`, the
/// runtime (PR 4) creates a new fix task and links it via
/// `created_fix_task_id`.
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
    pub result: SubagentTaskResult,
    pub decisions: Vec<String>,
    pub next_implications: Vec<String>,
    #[serde(default)]
    pub suggested_tasks: Vec<SuggestedTask>,
    #[serde(with = "echo_agent::utils::time::local_rfc3339")]
    #[ts(as = "String")]
    pub created_at: DateTime<Utc>,
}

impl TaskExecutionSummary {
    /// Convert to the framework's product-neutral task summary.
    pub fn to_runtime_summary(&self) -> echo_agent::tasks::TaskExecutionSummary {
        echo_agent::tasks::TaskExecutionSummary {
            run_id: self.run_id.clone(),
            task_id: self.task_id.clone(),
            subagent_name: self.subagent_name.clone(),
            completed_work: if self.result.summary.trim().is_empty() {
                Vec::new()
            } else {
                vec![self.result.summary.clone()]
            },
            files_read: self.result.touched_files.read.clone(),
            files_changed: self.result.touched_files.written.clone(),
            decisions: self.decisions.clone(),
            failures: if self.result.status == SubagentRunStatus::Completed {
                self.result.remaining_work.clone()
            } else {
                let mut failures =
                    vec![format!("subagent status: {}", self.result.status.as_str())];
                failures.extend(self.result.remaining_work.clone());
                failures
            },
            verification: self
                .result
                .verification
                .iter()
                .map(|item| format!("{}: {:?}", item.check, item.status))
                .collect(),
            next_implications: self.next_implications.clone(),
            suggested_tasks: self
                .suggested_tasks
                .iter()
                .map(SuggestedTask::to_runtime_suggested_task)
                .collect(),
            created_at: self.created_at,
        }
    }
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

impl SuggestedTask {
    /// Convert to the framework's product-neutral suggested task.
    pub fn to_runtime_suggested_task(&self) -> echo_agent::tasks::SuggestedTask {
        echo_agent::tasks::SuggestedTask {
            title: self.title.clone(),
            description: self.description.clone(),
            kind: self.kind.to_task_kind(),
            agent_role: self.agent_role.clone(),
            dependencies: self.dependencies.clone(),
            why_needed: self.why_needed.clone(),
            risk: self.risk.clone(),
        }
    }

    /// Convert a framework suggestion into the EKO app type.
    pub fn from_runtime_suggested_task(task: echo_agent::tasks::SuggestedTask) -> Self {
        Self {
            title: task.title,
            description: task.description,
            kind: PlanTaskKind::from_task_kind(task.kind),
            agent_role: task.agent_role,
            dependencies: task.dependencies,
            why_needed: task.why_needed,
            risk: task.risk,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interaction_mode_prompt_contracts_are_distinct_and_actionable() {
        let chat = InteractionMode::Chat.prompt_hint();
        let task = InteractionMode::Task.prompt_hint();
        let auto = InteractionMode::Auto.prompt_hint();

        assert!(chat.contains("same task_create/task_update/task_list/task_execute API"));
        assert!(task.contains("task_create"));
        assert!(task.contains("pass it as revision"));
        assert!(task.contains("task_update"));
        assert!(task.contains("never create a wrapper"));
        assert!(auto.contains("Choose between direct work"));
        assert!(auto.contains("formal TaskRuntime execution"));
        assert!(auto.contains("Do not dispatch ad-hoc Subagents"));
        assert!(auto.contains("task_list"));
        assert_ne!(chat, task);
        assert_ne!(task, auto);
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
            files: vec!["src/lib.rs".to_string()],
            allowed_tools: vec!["read_file".to_string()],
            required_artifacts: vec!["report.md".to_string()],
            execution_checks: vec!["cargo check".to_string()],
            acceptance_criteria: vec!["root cause is explained".to_string()],
            retry_count: 1,
            max_retries: 2,
            failure_fingerprint: Some("failure-1".to_string()),
            status: TodoStatus::Running,
            status_detail: None,
            claim: None,
            sort_order: 10,
        };

        let runtime = task.to_task();

        assert_eq!(runtime.spec.id, "t1");
        assert_eq!(
            runtime.spec.kind,
            echo_agent::tasks::TaskKind::Investigation
        );
        assert_eq!(
            runtime.execution.status,
            echo_agent::tasks::TaskStatus::Running
        );
        assert_eq!(runtime.spec.depends_on, vec!["t0".to_string()]);
        assert_eq!(runtime.spec.required_artifacts, vec!["report.md"]);
        assert_eq!(runtime.spec.execution_checks, vec!["cargo check"]);
        assert_eq!(
            runtime.spec.acceptance_criteria,
            vec!["root cause is explained"]
        );
        assert_eq!(runtime.spec.max_retries, 2);
        assert_eq!(runtime.execution.retry_count, 1);
        assert_eq!(
            runtime.execution.failure_fingerprint.as_deref(),
            Some("failure-1")
        );
        let metadata: EkoTaskMetadata = serde_json::from_value(runtime.spec.metadata.clone())
            .map_err(|error| error.to_string())?;
        assert_eq!(metadata.domain_profile, DomainProfile::AiCoding);
        assert_eq!(metadata.parallel_group.as_deref(), Some("g1"));
        assert_eq!(metadata.sort_order, 10);

        let round_trip = PlanTask::try_from(runtime)?;
        assert_eq!(round_trip.id, task.id);
        assert_eq!(round_trip.kind, task.kind);
        assert_eq!(round_trip.domain_profile, task.domain_profile);
        assert_eq!(round_trip.depends_on, task.depends_on);
        assert_eq!(round_trip.required_artifacts, task.required_artifacts);
        assert_eq!(round_trip.execution_checks, task.execution_checks);
        assert_eq!(round_trip.acceptance_criteria, task.acceptance_criteria);
        assert_eq!(round_trip.failure_fingerprint, task.failure_fingerprint);
        assert_eq!(round_trip.status, task.status);
        Ok(())
    }

    #[test]
    fn plan_task_status_projection_preserves_failure_detail() {
        let task = PlanTask {
            id: "failed-task".to_string(),
            title: "Failed task".to_string(),
            description: "Exercise status projection".to_string(),
            kind: PlanTaskKind::Investigation,
            agent_role: "explorer".to_string(),
            domain_profile: DomainProfile::AiCoding,
            depends_on: Vec::new(),
            parallel_group: None,
            files: Vec::new(),
            allowed_tools: Vec::new(),
            required_artifacts: Vec::new(),
            execution_checks: Vec::new(),
            acceptance_criteria: vec!["failure is persisted".to_string()],
            retry_count: 2,
            max_retries: 2,
            failure_fingerprint: Some("compile-error".to_string()),
            status: TodoStatus::Failed,
            status_detail: Some("cargo check failed".to_string()),
            claim: None,
            sort_order: 0,
        };

        let framework_task = task.to_task();

        assert_eq!(
            framework_task.execution.status,
            echo_agent::tasks::TaskStatus::Failed("cargo check failed".to_string())
        );
        assert_eq!(
            framework_task.execution.failure_fingerprint.as_deref(),
            Some("compile-error")
        );
        assert_eq!(
            TodoStatus::try_from_task_status(&framework_task.execution.status),
            Ok(TodoStatus::Failed)
        );
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
            assert_eq!(TodoStatus::try_from_task_status(&framework), Ok(expected));
            assert_eq!(TodoStatus::project_task_status(&framework), expected);
        }
    }

    #[test]
    fn task_execution_summary_converts_suggestions_to_framework() {
        let summary = TaskExecutionSummary {
            run_id: "r1".to_string(),
            task_id: "t1".to_string(),
            subagent_name: "explorer".to_string(),
            result: SubagentTaskResult {
                contract_version: 1,
                status: SubagentRunStatus::Completed,
                summary: "Read runtime".to_string(),
                artifacts: Vec::new(),
                verification: Vec::new(),
                remaining_work: Vec::new(),
                touched_files: SubagentTouchedFiles {
                    read: vec!["runtime.rs".to_string()],
                    written: Vec::new(),
                },
            },
            decisions: Vec::new(),
            next_implications: Vec::new(),
            suggested_tasks: vec![SuggestedTask {
                title: "Extract DAG kernel".to_string(),
                description: "Move pure scheduling next".to_string(),
                kind: PlanTaskKind::Implementation,
                agent_role: "implementer".to_string(),
                dependencies: vec!["t1".to_string()],
                why_needed: "Scheduling is reusable".to_string(),
                risk: "Adapter mismatch".to_string(),
            }],
            created_at: Utc::now(),
        };

        let runtime = summary.to_runtime_summary();

        assert_eq!(runtime.task_id, "t1");
        assert_eq!(runtime.suggested_tasks.len(), 1);
        assert_eq!(
            runtime.suggested_tasks.first().map(|task| task.kind),
            Some(echo_agent::tasks::TaskKind::Implementation)
        );
    }

    #[test]
    fn terminal_subagent_result_bounds_utf8_failure_text() {
        let long = "中".repeat(2_000);
        let result =
            SubagentTaskResult::terminal(SubagentRunStatus::TimedOut, long.clone(), vec![long; 70]);

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
}
