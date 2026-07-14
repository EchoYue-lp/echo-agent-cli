//! TaskRuntime data model — the canonical types for complex-task execution.
//!
//! These types live in the application layer because the framework
//! (`echo-agent`) intentionally holds no `AgentPool` / conversation-registry
//! / complex-task runtime: those are product-layer concerns. The framework
//! provides `Task` / `TaskExecutor` / `CheckpointStore` primitives, and this
//! module composes a higher-level *run → plan → plan-task → todo → event*
//! lifecycle on top of them.
//!
//! Naming note: the framework already re-exports a `TaskEvent`
//! (`crate::tasks::TaskEvent`). This module's event type is therefore named
//! [`RuntimeTaskEvent`] and is stored on its own table; we never shadow the
//! framework type.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
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
    /// Stable lowercase identifier used as the SQLite discriminator column.
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

/// How the user wants a plan to execute after approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "ExecutionMode")]
pub enum ExecutionMode {
    /// Execute sequentially, one plan task at a time.
    Sequential,
    /// Execute parallel groups concurrently within the configured limits.
    #[default]
    Parallel,
    /// Plan only — never execute until the user explicitly launches it.
    PlanOnly,
}

impl ExecutionMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ExecutionMode::Sequential => "sequential",
            ExecutionMode::Parallel => "parallel",
            ExecutionMode::PlanOnly => "plan_only",
        }
    }

    #[allow(clippy::should_implement_trait)] // inherent helper returning Option; not the FromStr trait
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "sequential" => ExecutionMode::Sequential,
            "parallel" => ExecutionMode::Parallel,
            "plan_only" => ExecutionMode::PlanOnly,
            _ => return None,
        })
    }
}

/// Manual override of how a user message should be handled.
/// `Auto` defers to the router; the other two force a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "InteractionMode")]
pub enum InteractionMode {
    /// Force normal chat — never enter TaskRuntime even for complex input.
    Chat,
    /// Force TaskRuntime for the message instead of leaving it to Auto.
    Task,
    /// Auto-route: classifier decides (default).
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
                "Chat mode. TaskRuntime tools are unavailable for this turn. Resolve the request directly with ordinary conversation and available non-task tools. Do not claim to create, execute, or update a formal plan."
            }
            InteractionMode::Task => {
                "Task mode. Use a formal, reviewable DAG: create concrete PlanTask items with plan_create, then call plan_execute() without a task argument. Keep task status and verification current. Do not use inline plan_execute({task}) dispatch in this mode."
            }
            InteractionMode::Auto => {
                "Auto mode. Choose the lightest reliable path: answer or act directly for simple work; use one isolated subagent for a bounded high-noise investigation; use plan_create plus plan_execute() for multi-step, multi-file, dependent, or long-running work."
            }
        }
    }
}

// ── Attended mode ───────────────────────────────────────────────────────

/// Whether a human is present during a run. Drives safety-gate behaviour:
/// unattended runs skip the approval gate (when the plan passes preflight),
/// reject writes at pre-scan, and never pause for human intervention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "AttendedMode")]
pub enum AttendedMode {
    /// Chat-triggered run — a human is present and can approve / resume.
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
/// * `Worktree` (default) — write tasks run inside an isolated git worktree
///   branched from the main workspace; the main checkout is never touched.
///   Created lazily: a read-only plan still runs in-place (zero overhead).
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

    pub fn to_runtime_kind(self) -> echo_agent::tasks::RuntimeTaskKind {
        match self {
            PlanTaskKind::ReadOnlyReview => echo_agent::tasks::RuntimeTaskKind::ReadOnlyReview,
            PlanTaskKind::Investigation => echo_agent::tasks::RuntimeTaskKind::Investigation,
            PlanTaskKind::TestPlan => echo_agent::tasks::RuntimeTaskKind::TestPlan,
            PlanTaskKind::Implementation => echo_agent::tasks::RuntimeTaskKind::Implementation,
            PlanTaskKind::Debugging => echo_agent::tasks::RuntimeTaskKind::Debugging,
            PlanTaskKind::Review => echo_agent::tasks::RuntimeTaskKind::Review,
            PlanTaskKind::Summary => echo_agent::tasks::RuntimeTaskKind::Summary,
            PlanTaskKind::Verification => echo_agent::tasks::RuntimeTaskKind::Verification,
        }
    }

    pub fn from_runtime_kind(kind: echo_agent::tasks::RuntimeTaskKind) -> Self {
        match kind {
            echo_agent::tasks::RuntimeTaskKind::ReadOnlyReview => PlanTaskKind::ReadOnlyReview,
            echo_agent::tasks::RuntimeTaskKind::Investigation => PlanTaskKind::Investigation,
            echo_agent::tasks::RuntimeTaskKind::TestPlan => PlanTaskKind::TestPlan,
            echo_agent::tasks::RuntimeTaskKind::Implementation => PlanTaskKind::Implementation,
            echo_agent::tasks::RuntimeTaskKind::Debugging => PlanTaskKind::Debugging,
            echo_agent::tasks::RuntimeTaskKind::Review => PlanTaskKind::Review,
            echo_agent::tasks::RuntimeTaskKind::Summary => PlanTaskKind::Summary,
            echo_agent::tasks::RuntimeTaskKind::Verification => PlanTaskKind::Verification,
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
            "skipped" => TodoStatus::Skipped,
            _ => return None,
        })
    }

    pub fn to_runtime_status(self) -> echo_agent::tasks::RuntimeTaskStatus {
        match self {
            TodoStatus::Pending => echo_agent::tasks::RuntimeTaskStatus::Pending,
            TodoStatus::Running => echo_agent::tasks::RuntimeTaskStatus::Running,
            TodoStatus::Blocked => echo_agent::tasks::RuntimeTaskStatus::Blocked,
            TodoStatus::Completed => echo_agent::tasks::RuntimeTaskStatus::Completed,
            TodoStatus::Failed => echo_agent::tasks::RuntimeTaskStatus::Failed,
            TodoStatus::Skipped => echo_agent::tasks::RuntimeTaskStatus::Skipped,
        }
    }

    pub fn from_runtime_status(status: echo_agent::tasks::RuntimeTaskStatus) -> Self {
        match status {
            echo_agent::tasks::RuntimeTaskStatus::Pending => TodoStatus::Pending,
            echo_agent::tasks::RuntimeTaskStatus::Running => TodoStatus::Running,
            echo_agent::tasks::RuntimeTaskStatus::Blocked => TodoStatus::Blocked,
            echo_agent::tasks::RuntimeTaskStatus::Completed => TodoStatus::Completed,
            echo_agent::tasks::RuntimeTaskStatus::Failed => TodoStatus::Failed,
            echo_agent::tasks::RuntimeTaskStatus::Skipped
            | echo_agent::tasks::RuntimeTaskStatus::Cancelled => TodoStatus::Skipped,
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
/// 极简 6 态(对齐 Claude Code/Codex:plan 审批不进状态机,用 Paused 表达)。
/// 删去了 Planning/AwaitingPlanApproval/Ready/WaitingApproval/WaitingInput/Suspended/Cancelling
/// —— 这些"plan 是否被批准"的语义改由编排层(L1) + Paused 承载,不进 run 状态机。
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
    RunStatusChanged,
    /// User-uploaded attachments were bound to this run (so plan-level
    /// subagents can see the same images/files as the main agent).
    RunAttachmentsUpdated,
    PlanGenerated,
    PlanEdited,
    TaskStarted,
    TaskCompleted,
    TaskFailed,
    TaskSkipped,
    TaskBlocked,
    TodoUpdated,
    WorkerAssigned,
    WorkerReleased,
    WorkerLlmUsage,
    ArtifactProduced,
    ReviewPassed,
    ReviewNeedsFix,
    ReviewBlocked,
    ApprovalRequested,
    ApprovalResolved,
    ApprovalRejected,
    CircuitBreakerTripped,
    RunCancelled,
    Note,
}

impl RuntimeEventKind {
    pub fn as_str(&self) -> &'static str {
        use RuntimeEventKind::*;
        match self {
            RunCreated => "run_created",
            RunStatusChanged => "run_status_changed",
            RunAttachmentsUpdated => "run_attachments_updated",
            PlanGenerated => "plan_generated",
            PlanEdited => "plan_edited",
            TaskStarted => "task_started",
            TaskCompleted => "task_completed",
            TaskFailed => "task_failed",
            TaskSkipped => "task_skipped",
            TaskBlocked => "task_blocked",
            TodoUpdated => "todo_updated",
            WorkerAssigned => "worker_assigned",
            WorkerReleased => "worker_released",
            WorkerLlmUsage => "worker_llm_usage",
            ArtifactProduced => "artifact_produced",
            ReviewPassed => "review_passed",
            ReviewNeedsFix => "review_needs_fix",
            ReviewBlocked => "review_blocked",
            ApprovalRequested => "approval_requested",
            ApprovalResolved => "approval_resolved",
            ApprovalRejected => "approval_rejected",
            CircuitBreakerTripped => "circuit_breaker_tripped",
            RunCancelled => "run_cancelled",
            Note => "note",
        }
    }
    #[allow(clippy::should_implement_trait)] // inherent helper returning Option; not the FromStr trait
    pub fn from_str(s: &str) -> Option<Self> {
        use RuntimeEventKind::*;
        Some(match s {
            "run_created" => RunCreated,
            "run_status_changed" => RunStatusChanged,
            "run_attachments_updated" => RunAttachmentsUpdated,
            "plan_generated" => PlanGenerated,
            "plan_edited" => PlanEdited,
            "task_started" => TaskStarted,
            "task_completed" => TaskCompleted,
            "task_failed" => TaskFailed,
            "task_skipped" => TaskSkipped,
            "task_blocked" => TaskBlocked,
            "todo_updated" => TodoUpdated,
            "worker_assigned" => WorkerAssigned,
            "worker_released" => WorkerReleased,
            "worker_llm_usage" => WorkerLlmUsage,
            "artifact_produced" => ArtifactProduced,
            "review_passed" => ReviewPassed,
            "review_needs_fix" => ReviewNeedsFix,
            "review_blocked" => ReviewBlocked,
            "approval_requested" => ApprovalRequested,
            "approval_resolved" => ApprovalResolved,
            "approval_rejected" => ApprovalRejected,
            "circuit_breaker_tripped" => CircuitBreakerTripped,
            "run_cancelled" => RunCancelled,
            "note" => Note,
            _ => return None,
        })
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
    pub plan_id: Option<String>,
    pub route: String,
    /// Whether a human is present (Attended) or this is a cron/IM trigger
    /// (Unattended). Drives safety-gate behaviour in plan_execute /
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

/// A structured plan attached to a run. Generated by the planning runtime
/// (PR 2); never free-form markdown from the model.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, rename = "TaskPlan")]
pub struct TaskPlan {
    pub plan_id: String,
    pub run_id: String,
    pub domain_profile: DomainProfile,
    pub goal: String,
    pub assumptions: Vec<String>,
    pub risks: Vec<String>,
    pub execution_mode: ExecutionMode,
    pub tasks: Vec<PlanTask>,
}

/// One node in the plan DAG. `depends_on` is the canonical edge list; the
/// scheduler (PR 3) builds adjacency indexes from it but `PlanTask` remains
/// the serialized node.
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
    pub files: Vec<String>,
    pub allowed_tools: Vec<String>,
    pub verification: Vec<String>,
    pub retry_count: u32,
    pub max_retries: u32,
    pub failure_fingerprint: Option<String>,
    pub status: TodoStatus,
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
            description: String::new(),
            kind: PlanTaskKind::ReadOnlyReview,
            agent_role: "general".to_string(),
            domain_profile: DomainProfile::General,
            depends_on: Vec::new(),
            parallel_group: None,
            files: Vec::new(),
            allowed_tools: Vec::new(),
            verification: Vec::new(),
            retry_count: 0,
            max_retries: 3,
            failure_fingerprint: None,
            status: TodoStatus::Pending,
            sort_order: 0,
        }
    }
}

impl PlanTask {
    /// Convert to the framework's product-neutral runtime task view.
    pub fn to_runtime_task(&self) -> echo_agent::tasks::RuntimeTask {
        echo_agent::tasks::RuntimeTask {
            id: self.id.clone(),
            title: self.title.clone(),
            description: self.description.clone(),
            kind: self.kind.to_runtime_kind(),
            agent_role: self.agent_role.clone(),
            depends_on: self.depends_on.clone(),
            files: self.files.clone(),
            allowed_tools: self.allowed_tools.clone(),
            verification: self.verification.clone(),
            retry_count: self.retry_count,
            max_retries: self.max_retries,
            status: self.status.to_runtime_status(),
        }
    }
}

/// Partial update patch for a [`PlanTask`]. Only non-`None` fields are applied.
/// Used by [`TaskRuntimeStore::update_task`] for in-flight plan edits.
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
// `subagent_run_id` 与框架 SubagentEvent.execution_id 对齐(格式
// "{task_id}:{attempt}"),由 TaskRuntime 派发时生成并经 ExternalRunContext
// 透传,不再由 tauri bridge 临时分配(消除双账本)。

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
}

impl SubagentRunStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            SubagentRunStatus::Running => "running",
            SubagentRunStatus::Completed => "completed",
            SubagentRunStatus::Failed => "failed",
            SubagentRunStatus::Cancelled => "cancelled",
        }
    }

    #[allow(clippy::should_implement_trait)] // inherent helper returning Option; not the FromStr trait
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "running" => SubagentRunStatus::Running,
            "completed" => SubagentRunStatus::Completed,
            "failed" => SubagentRunStatus::Failed,
            "cancelled" => SubagentRunStatus::Cancelled,
            _ => return None,
        })
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
    /// Stable execution id, format "{task_id}:{attempt}". Aligns with
    /// `SubagentEvent::execution_id`. Was the legacy "worker_id".
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
    /// Output returned to the Task on success (None while running).
    pub result: Option<String>,
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
    pub worker_agent: String,
    pub completed_work: Vec<String>,
    pub files_read: Vec<String>,
    pub files_changed: Vec<String>,
    pub decisions: Vec<String>,
    pub failures: Vec<String>,
    pub verification: Vec<String>,
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
            worker_agent: self.worker_agent.clone(),
            completed_work: self.completed_work.clone(),
            files_read: self.files_read.clone(),
            files_changed: self.files_changed.clone(),
            decisions: self.decisions.clone(),
            failures: self.failures.clone(),
            verification: self.verification.clone(),
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
            kind: self.kind.to_runtime_kind(),
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
            kind: PlanTaskKind::from_runtime_kind(task.kind),
            agent_role: task.agent_role,
            dependencies: task.dependencies,
            why_needed: task.why_needed,
            risk: task.risk,
        }
    }
}

// ── Usage trend persistence ────────────────────────────────────────────

/// A single LLM usage record persisted to SQLite for trend analysis.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, rename = "UsageRecord")]
pub struct UsageRecord {
    pub id: String,
    pub session_id: String,
    pub run_id: Option<String>,
    pub worker_id: Option<String>,
    pub model: String,
    pub provider: Option<String>,
    pub route_kind: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub usage_reported: bool,
    pub system_prompt_hash: Option<String>,
    pub tools_schema_hash: Option<String>,
    pub cwd_hash: Option<String>,
    pub worker_prompt_hash: Option<String>,
    #[serde(with = "echo_agent::utils::time::local_rfc3339")]
    #[ts(as = "String")]
    pub created_at: DateTime<Utc>,
}

/// Query filter for listing usage records.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export, rename = "UsageQueryFilter")]
pub struct UsageQueryFilter {
    pub session_id: Option<String>,
    pub run_id: Option<String>,
    pub worker_id: Option<String>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub route_kind: Option<String>,
    #[serde(with = "echo_agent::utils::time::option_local_rfc3339")]
    #[ts(as = "Option<String>")]
    pub created_after: Option<DateTime<Utc>>,
    #[serde(with = "echo_agent::utils::time::option_local_rfc3339")]
    #[ts(as = "Option<String>")]
    pub created_before: Option<DateTime<Utc>>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

/// Grouping dimension for aggregation queries.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, rename = "UsageGroupBy")]
pub enum UsageGroupBy {
    Model,
    Provider,
    RouteKind,
    Session,
    Worker,
    TimeWindow,
}

/// Filter for aggregated usage queries.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, rename = "UsageAggregationFilter")]
pub struct UsageAggregationFilter {
    pub group_by: Vec<UsageGroupBy>,
    #[serde(with = "echo_agent::utils::time::option_local_rfc3339")]
    #[ts(as = "Option<String>")]
    pub created_after: Option<DateTime<Utc>>,
    #[serde(with = "echo_agent::utils::time::option_local_rfc3339")]
    #[ts(as = "Option<String>")]
    pub created_before: Option<DateTime<Utc>>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub route_kind: Option<String>,
    pub session_id: Option<String>,
    pub window_seconds: Option<u64>,
}

/// Aggregated usage values for one group.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, rename = "UsageAggregation")]
pub struct UsageAggregation {
    pub group_key: Option<String>,
    pub group_value: Option<String>,
    pub window_start: Option<String>,
    pub window_end: Option<String>,
    pub llm_calls: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_rate: f64,
    pub calls_missing_usage: u64,
}

/// Per-model breakdown in a run usage summary.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, rename = "ModelUsageSummary")]
pub struct ModelUsageSummary {
    pub model: String,
    pub llm_calls: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
}

/// End-of-run usage summary displayed after TaskRuntime/chat completion.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, rename = "RunUsageSummary")]
pub struct RunUsageSummary {
    pub run_id: Option<String>,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cached_input_tokens: u64,
    pub total_cache_creation_input_tokens: u64,
    pub cache_read_rate: f64,
    pub llm_calls: u64,
    pub model_breakdown: Vec<ModelUsageSummary>,
    pub top_low_hit_reasons: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interaction_mode_prompt_contracts_are_distinct_and_actionable() {
        let chat = InteractionMode::Chat.prompt_hint();
        let task = InteractionMode::Task.prompt_hint();
        let auto = InteractionMode::Auto.prompt_hint();

        assert!(chat.contains("TaskRuntime tools are unavailable"));
        assert!(task.contains("plan_create"));
        assert!(task.contains("plan_execute()"));
        assert!(auto.contains("lightest reliable path"));
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
    fn plan_task_converts_to_framework_runtime_task() {
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
            verification: vec!["cargo check".to_string()],
            retry_count: 1,
            max_retries: 2,
            failure_fingerprint: None,
            status: TodoStatus::Running,
            sort_order: 10,
        };

        let runtime = task.to_runtime_task();

        assert_eq!(runtime.id, "t1");
        assert_eq!(
            runtime.kind,
            echo_agent::tasks::RuntimeTaskKind::Investigation
        );
        assert_eq!(
            runtime.status,
            echo_agent::tasks::RuntimeTaskStatus::Running
        );
        assert_eq!(runtime.depends_on, vec!["t0".to_string()]);
        assert_eq!(runtime.max_retries, 2);
    }

    #[test]
    fn task_execution_summary_converts_suggestions_to_framework() {
        let summary = TaskExecutionSummary {
            run_id: "r1".to_string(),
            task_id: "t1".to_string(),
            worker_agent: "explorer".to_string(),
            completed_work: vec!["Read runtime".to_string()],
            files_read: vec!["runtime.rs".to_string()],
            files_changed: Vec::new(),
            decisions: Vec::new(),
            failures: Vec::new(),
            verification: Vec::new(),
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
            Some(echo_agent::tasks::RuntimeTaskKind::Implementation)
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
