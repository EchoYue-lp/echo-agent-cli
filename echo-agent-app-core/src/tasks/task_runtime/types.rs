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
use uuid::Uuid;

// ── Domain profile ──────────────────────────────────────────────────────

/// Cross-domain profile that customizes plan templates, worker roles,
/// allowed tools, review checklists, and verification standards.
///
/// Selection order (resolved by the planning runtime, PR 2):
/// 1. User-selected profile in GUI
/// 2. Workspace default profile
/// 3. Intent router inference
/// 4. `General` fallback
///
/// `General` is always first-class because many tasks declare no domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "DomainProfile")]
pub enum DomainProfile {
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

impl Default for DomainProfile {
    fn default() -> Self {
        DomainProfile::General
    }
}

// ── Execution mode ──────────────────────────────────────────────────────

/// How the user wants a plan to execute after approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "ExecutionMode")]
pub enum ExecutionMode {
    /// Execute sequentially, one plan task at a time.
    Sequential,
    /// Execute parallel groups concurrently within the configured limits.
    Parallel,
    /// Plan only — never execute until the user explicitly launches it.
    PlanOnly,
}

/// Manual override of how a user message should be handled.
/// `Auto` defers to the router; the other two force a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "InteractionMode")]
pub enum InteractionMode {
    /// Force normal chat — never enter TaskRuntime even for complex input.
    Chat,
    /// Force TaskRuntime for the message instead of leaving it to Auto.
    Task,
    /// Auto-route: classifier decides (default).
    Auto,
}

impl Default for InteractionMode {
    fn default() -> Self {
        InteractionMode::Auto
    }
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
}

impl Default for ExecutionMode {
    fn default() -> Self {
        ExecutionMode::Parallel
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

/// Status of an individual todo / plan task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "TodoStatus")]
pub enum TodoStatus {
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
}

impl Default for TodoStatus {
    fn default() -> Self {
        TodoStatus::Pending
    }
}

// ── Run status (state machine) ──────────────────────────────────────────

/// Lifecycle status of a [`TaskRun`]. The GUI must render from these states
/// and `RuntimeTaskEvent`s, never from local guesses.
///
/// Allowed transitions (see [`TaskRunStatus::can_transition_to`]):
/// ```text
/// Pending -> Planning
/// Planning -> AwaitingPlanApproval
/// AwaitingPlanApproval -> Ready | Cancelled
/// Ready -> Running
/// Running -> WaitingApproval | WaitingInput | Suspended | Paused | Cancelling | Failed | Completed
/// WaitingApproval -> Running | Suspended | Cancelled
/// WaitingInput -> Running | Suspended | Cancelled
/// Suspended -> Ready | Cancelled
/// Paused -> Running | AwaitingPlanApproval | Cancelled
/// Cancelling -> Cancelled | Failed
/// Failed -> Ready | Cancelled  (Ready: reserved for future retry-from-failed)
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "TaskRunStatus")]
pub enum TaskRunStatus {
    Pending,
    Planning,
    AwaitingPlanApproval,
    Ready,
    Running,
    WaitingApproval,
    WaitingInput,
    Suspended,
    Paused,
    Cancelling,
    Cancelled,
    Failed,
    Completed,
}

impl TaskRunStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskRunStatus::Pending => "pending",
            TaskRunStatus::Planning => "planning",
            TaskRunStatus::AwaitingPlanApproval => "awaiting_plan_approval",
            TaskRunStatus::Ready => "ready",
            TaskRunStatus::Running => "running",
            TaskRunStatus::WaitingApproval => "waiting_approval",
            TaskRunStatus::WaitingInput => "waiting_input",
            TaskRunStatus::Suspended => "suspended",
            TaskRunStatus::Paused => "paused",
            TaskRunStatus::Cancelling => "cancelling",
            TaskRunStatus::Cancelled => "cancelled",
            TaskRunStatus::Failed => "failed",
            TaskRunStatus::Completed => "completed",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "pending" => TaskRunStatus::Pending,
            "planning" => TaskRunStatus::Planning,
            "awaiting_plan_approval" => TaskRunStatus::AwaitingPlanApproval,
            "ready" => TaskRunStatus::Ready,
            "running" => TaskRunStatus::Running,
            "waiting_approval" => TaskRunStatus::WaitingApproval,
            "waiting_input" => TaskRunStatus::WaitingInput,
            "suspended" => TaskRunStatus::Suspended,
            "paused" => TaskRunStatus::Paused,
            "cancelling" => TaskRunStatus::Cancelling,
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
            Pending => matches!(next, Planning | Cancelled),
            Planning => matches!(next, AwaitingPlanApproval | Failed | Cancelled),
            AwaitingPlanApproval => matches!(next, Ready | Cancelled),
            Ready => matches!(next, Running | Cancelled),
            Running => matches!(
                next,
                WaitingApproval
                    | WaitingInput
                    | Suspended
                    | Paused
                    | Cancelling
                    | Failed
                    | Completed
            ),
            WaitingApproval => matches!(next, Running | Suspended | Cancelled),
            WaitingInput => matches!(next, Running | Suspended | Cancelled),
            Suspended => matches!(next, Ready | Cancelled),
            Paused => matches!(next, Running | AwaitingPlanApproval | Cancelled),
            Cancelling => matches!(next, Cancelled | Failed),
            Failed => matches!(next, Ready | Cancelled),
            // Terminal states.
            Cancelled | Completed => false,
        }
    }
}

impl Default for TaskRunStatus {
    fn default() -> Self {
        TaskRunStatus::Pending
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
    PlanGenerated,
    PlanApproved,
    PlanRejected,
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

/// Realtime trace events for GUI-visible run/worker execution.
///
/// `RuntimeTaskEvent` is the persisted TaskRuntime ledger. This type is the
/// lightweight realtime protocol used by Chat, TaskRuntime, Auto routing, and
/// subagents so the frontend can render one coherent "run with workers" view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename = "WorkerTraceEventKind")]
pub enum WorkerTraceEventKind {
    RunStarted,
    RunStatusChanged,
    RunCompleted,
    RunFailed,
    RunCancelled,
    WorkerPlanned,
    WorkerStarted,
    WorkerThinkingStart,
    WorkerThinkingDelta,
    WorkerThinkingEnd,
    WorkerLlmUsage,
    WorkerToolStart,
    WorkerToolResult,
    WorkerTokenDelta,
    WorkerArtifact,
    WorkerCompleted,
    WorkerFailed,
    WorkerCancelled,
    ApprovalRequested,
    ApprovalResolved,
    Note,
}

/// A realtime event scoped to a top-level run and, optionally, a child worker.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, rename = "WorkerTraceEvent")]
pub struct WorkerTraceEvent {
    pub event_id: String,
    pub run_id: String,
    pub worker_id: Option<String>,
    pub parent_worker_id: Option<String>,
    pub agent_name: Option<String>,
    pub title: Option<String>,
    pub task: Option<String>,
    /// 关联触发该 worker 的 assistant message id(用于前端按 message 过滤 worker)。
    /// 可空:兼容旧事件或不适用场景。
    pub message_id: Option<String>,
    pub event_type: WorkerTraceEventKind,
    pub payload: serde_json::Value,
    pub timestamp: DateTime<Utc>,
}

impl WorkerTraceEvent {
    pub fn new(
        run_id: impl Into<String>,
        event_type: WorkerTraceEventKind,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            event_id: Uuid::new_v4().to_string(),
            run_id: run_id.into(),
            worker_id: None,
            parent_worker_id: None,
            agent_name: None,
            title: None,
            task: None,
            message_id: None,
            event_type,
            payload,
            timestamp: Utc::now(),
        }
    }

    pub fn for_worker(
        run_id: impl Into<String>,
        worker_id: impl Into<String>,
        event_type: WorkerTraceEventKind,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            worker_id: Some(worker_id.into()),
            ..Self::new(run_id, event_type, payload)
        }
    }

    pub fn with_parent_worker(mut self, parent_worker_id: impl Into<String>) -> Self {
        self.parent_worker_id = Some(parent_worker_id.into());
        self
    }

    pub fn with_agent(mut self, agent_name: impl Into<String>) -> Self {
        self.agent_name = Some(agent_name.into());
        self
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn with_task(mut self, task: impl Into<String>) -> Self {
        self.task = Some(task.into());
        self
    }

    pub fn with_message_id(mut self, message_id: impl Into<String>) -> Self {
        self.message_id = Some(message_id.into());
        self
    }
}

impl RuntimeEventKind {
    pub fn as_str(&self) -> &'static str {
        use RuntimeEventKind::*;
        match self {
            RunCreated => "run_created",
            RunStatusChanged => "run_status_changed",
            PlanGenerated => "plan_generated",
            PlanApproved => "plan_approved",
            PlanRejected => "plan_rejected",
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
    pub fn from_str(s: &str) -> Option<Self> {
        use RuntimeEventKind::*;
        Some(match s {
            "run_created" => RunCreated,
            "run_status_changed" => RunStatusChanged,
            "plan_generated" => PlanGenerated,
            "plan_approved" => PlanApproved,
            "plan_rejected" => PlanRejected,
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
    pub created_at: DateTime<Utc>,
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
    pub started_at: Option<DateTime<Utc>>,
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
/// workers consume this instead of the full raw conversation — see the
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
    pub created_at: DateTime<Utc>,
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
    pub created_after: Option<DateTime<Utc>>,
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
    pub created_after: Option<DateTime<Utc>>,
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
    fn status_machine_allows_documented_transitions() {
        use TaskRunStatus::*;
        // Every transition explicitly named in the plan doc.
        assert!(Pending.can_transition_to(Planning));
        assert!(Planning.can_transition_to(AwaitingPlanApproval));
        assert!(AwaitingPlanApproval.can_transition_to(Ready));
        assert!(AwaitingPlanApproval.can_transition_to(Cancelled));
        assert!(Ready.can_transition_to(Running));
        assert!(Running.can_transition_to(WaitingApproval));
        assert!(Running.can_transition_to(Completed));
        assert!(WaitingApproval.can_transition_to(Running));
        assert!(Suspended.can_transition_to(Ready));
        assert!(Cancelling.can_transition_to(Cancelled));
        assert!(Cancelling.can_transition_to(Failed));
        assert!(Failed.can_transition_to(Ready));
        // Paused transitions: user interrupt / resume / edit plan / abandon.
        assert!(Running.can_transition_to(Paused));
        assert!(Paused.can_transition_to(Running));
        assert!(Paused.can_transition_to(AwaitingPlanApproval));
        assert!(Paused.can_transition_to(Cancelled));
    }

    #[test]
    fn status_machine_rejects_invalid_transitions() {
        use TaskRunStatus::*;
        assert!(!Pending.can_transition_to(Running));
        assert!(!Running.can_transition_to(Pending));
        assert!(!Completed.can_transition_to(Running));
        assert!(!Cancelled.can_transition_to(Running));
        assert!(!Ready.can_transition_to(Completed));
        // Paused cannot go to Suspended (different semantics).
        assert!(!Paused.can_transition_to(Suspended));
    }

    #[test]
    fn status_roundtrips_through_string() {
        // Enumerate every variant — guards against future additions
        // forgetting to wire up `from_str`.
        let all = [
            TaskRunStatus::Pending,
            TaskRunStatus::Planning,
            TaskRunStatus::AwaitingPlanApproval,
            TaskRunStatus::Ready,
            TaskRunStatus::Running,
            TaskRunStatus::WaitingApproval,
            TaskRunStatus::WaitingInput,
            TaskRunStatus::Suspended,
            TaskRunStatus::Paused,
            TaskRunStatus::Cancelling,
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
