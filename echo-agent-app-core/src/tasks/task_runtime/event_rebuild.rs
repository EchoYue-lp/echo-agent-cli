//! Event-sourced plan rebuild (U1c phase-0/0a, gate 1).
//!
//! Folds a run's `RuntimeTaskEvent` stream into a `RebuiltPlan` snapshot
//! (`run` header + `plan` envelope + `tasks[]` with runtime fields). This is
//! the proof that `events.jsonl` can authoritatively rebuild `plan.json`.
//!
//! Precondition: events must carry enriched payloads. Plan specifications are
//! replaced only by atomic `PlanRevisionCommitted` events; task events update
//! the separate execution projection.

use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;

use echo_agent::state::journal::{EventReducer, JournalRecord};

use super::run_authority::RuntimeJournalEvent;

use super::types::{
    ActiveSubagentBoundary, ActiveToolBoundary, Artifact, ArtifactKind, AttendedMode,
    BackgroundCellArtifactStatus, BackgroundCellPhase, BackgroundCellState,
    BackgroundCellTerminalCause, BlockerAudit, DomainProfile, ExecutionMode, PlanRevision,
    PlanTask, ProviderRetryState, RecordedUserSteer, RecoveryBlocker, ReviewOutcome, ReviewResult,
    RunContinuationState, RunPause, RunPauseReason, RunStateEventIndex, RunStateSnapshot,
    RunTurnOrigin, RunTurnStatus, RunTurnSummary, RuntimeEventKind, RuntimeTaskEvent,
    TaskExecutionSummary, TaskPlan, TaskRun, TaskRunExecutionProfile, TaskRunStatus,
    TurnVisibility,
};

/// How many recent user steers the fold keeps for the recovery capsule.
const MAX_RECORDED_STEERS: usize = 8;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct TodoRuntimeProjection {
    pub(crate) owner_agent: Option<String>,
    pub(crate) started_at: Option<chrono::DateTime<Utc>>,
    pub(crate) completed_at: Option<chrono::DateTime<Utc>>,
    pub(crate) summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct BoundCompletionEvent {
    event: RuntimeTaskEvent,
    source_goal_revision: u64,
    source_plan_revision: u64,
}

impl BoundCompletionEvent {
    pub(crate) fn event(&self) -> &RuntimeTaskEvent {
        &self.event
    }

    pub(crate) fn source_binding(&self) -> (u64, u64) {
        (self.source_goal_revision, self.source_plan_revision)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct CompletionQueryProjection {
    summaries: std::collections::BTreeMap<String, BoundCompletionEvent>,
    reviews: std::collections::BTreeMap<String, BoundCompletionEvent>,
    requirement_skips: std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<String, std::collections::BTreeMap<u64, RuntimeTaskEvent>>,
    >,
    revalidations: std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<String, std::collections::BTreeMap<u64, RuntimeTaskEvent>>,
    >,
}

impl CompletionQueryProjection {
    fn retain_tasks(&mut self, task_ids: &std::collections::HashSet<&str>) {
        self.summaries
            .retain(|task_id, _| task_ids.contains(task_id.as_str()));
        self.reviews
            .retain(|task_id, _| task_ids.contains(task_id.as_str()));
    }

    fn retain_requirements(&mut self, requirements: &[super::types::GoalRequirement]) {
        let identities = requirements
            .iter()
            .map(|requirement| {
                (
                    requirement.requirement_id.as_str(),
                    requirement.requirement_sha256.as_str(),
                    requirement.goal_revision,
                )
            })
            .collect::<std::collections::HashSet<_>>();
        retain_requirement_index(&mut self.requirement_skips, &identities);
        retain_requirement_index(&mut self.revalidations, &identities);
    }

    fn for_tasks(&self, task_ids: &std::collections::HashSet<&str>) -> Self {
        Self {
            summaries: self
                .summaries
                .iter()
                .filter(|(task_id, _)| task_ids.contains(task_id.as_str()))
                .map(|(task_id, event)| (task_id.clone(), event.clone()))
                .collect(),
            reviews: self
                .reviews
                .iter()
                .filter(|(task_id, _)| task_ids.contains(task_id.as_str()))
                .map(|(task_id, event)| (task_id.clone(), event.clone()))
                .collect(),
            requirement_skips: self.requirement_skips.clone(),
            revalidations: self.revalidations.clone(),
        }
    }

    pub(crate) fn summary(&self, task_id: &str) -> Option<&BoundCompletionEvent> {
        self.summaries.get(task_id)
    }

    pub(crate) fn review_after(
        &self,
        task_id: &str,
        sequence: i64,
    ) -> Option<&BoundCompletionEvent> {
        self.reviews
            .get(task_id)
            .filter(|review| review.event.seq > sequence)
    }

    pub(crate) fn requirement_skip(
        &self,
        requirement_id: &str,
        requirement_sha256: &str,
        goal_revision: u64,
    ) -> Option<&RuntimeTaskEvent> {
        self.requirement_skips
            .get(requirement_id)?
            .get(requirement_sha256)?
            .get(&goal_revision)
    }

    pub(crate) fn has_revalidation(
        &self,
        source_sequence: i64,
        requirement_id: &str,
        requirement_sha256: &str,
        goal_revision: u64,
    ) -> bool {
        self.revalidations
            .get(requirement_id)
            .and_then(|by_sha| by_sha.get(requirement_sha256))
            .and_then(|by_goal| by_goal.get(&goal_revision))
            .is_some_and(|event| event.seq > source_sequence)
    }
}

/// One coherent EKO read model cloned from the framework-backed reducer state.
///
/// It is not another authority: every field is rebuilt from `events.jsonl` and
/// stored in the same discardable `checkpoint.json` as the operational fold.
#[derive(Debug, Clone)]
pub(crate) struct TodoQueryProjection {
    pub(crate) run: TaskRun,
    pub(crate) plan: Option<TaskPlan>,
    pub(crate) todo_runtime: std::collections::BTreeMap<String, TodoRuntimeProjection>,
}

#[derive(Debug, Clone)]
pub(crate) struct CompletionGateProjection {
    pub(crate) run: TaskRun,
    pub(crate) plan: Option<TaskPlan>,
    pub(crate) completion: CompletionQueryProjection,
    pub(crate) active_subagents: Vec<ActiveSubagentBoundary>,
    pub(crate) background_cells: Vec<BackgroundCellState>,
    pub(crate) recovery_blockers: Vec<RecoveryBlocker>,
}

/// Rebuilt plan snapshot — the shape `plan.json` will take.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RebuiltPlan {
    pub run: TaskRun,
    pub plan: TaskPlan,
    pub tasks: Vec<PlanTask>,
    #[serde(default)]
    pub background_cells: Vec<BackgroundCellState>,
    #[serde(default)]
    pub continuation: Option<RunContinuationState>,
    #[serde(default)]
    pub recent_constraints: Vec<RecordedUserSteer>,
    #[serde(default)]
    pub execution_profile: TaskRunExecutionProfile,
    #[serde(default)]
    pub(crate) event_index: RunStateEventIndex,
}

impl RebuiltPlan {
    pub fn plan_revision(&self) -> PlanRevision {
        PlanRevision {
            plan_id: self.plan.plan_id.clone(),
            run_id: self.plan.run_id.clone(),
            revision: self.plan.revision,
            domain_profile: self.plan.domain_profile,
            goal_revision: self.plan.goal_revision,
            goal_sha256: self.plan.goal_sha256.clone(),
            assumptions: self.plan.assumptions.clone(),
            risks: self.plan.risks.clone(),
            execution_mode: self.plan.execution_mode,
            tasks: self.tasks.iter().map(PlanTask::spec).collect(),
        }
    }

    pub fn run_state_with_sequence(&self, journal_sequence: u64) -> RunStateSnapshot {
        RunStateSnapshot {
            run: self.run.clone(),
            tasks: self.tasks.iter().map(PlanTask::execution).collect(),
            continuation: self.continuation.clone(),
            background_cells: self.background_cells.clone(),
            recent_constraints: self.recent_constraints.clone(),
            execution_profile: self.execution_profile,
            journal_sequence,
            event_index: self.event_index.clone(),
        }
    }
}

/// Serializable state for the one authoritative runtime-event fold.
///
/// The deduplication sets are part of the state rather than rebuild-local
/// scratch data: a checkpoint must retain them so a duplicate source event
/// arriving after the checkpoint cannot double-account usage or compaction.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct EventFoldState {
    /// Missing in checkpoints written before the bounded query projection was
    /// introduced. Such checkpoints must be discarded before suffix recovery;
    /// otherwise their empty default indexes would look current at journal head.
    #[serde(default)]
    query_projection_schema: Option<u8>,
    #[serde(default)]
    run: Option<TaskRun>,
    #[serde(default)]
    plan: Option<TaskPlan>,
    #[serde(default)]
    tasks: Vec<PlanTask>,
    #[serde(default)]
    background_cells: std::collections::BTreeMap<String, BackgroundCellState>,
    #[serde(default)]
    continuation: Option<RunContinuationState>,
    #[serde(default)]
    recent_constraints: Vec<RecordedUserSteer>,
    #[serde(default)]
    execution_profile: TaskRunExecutionProfile,
    #[serde(default)]
    started_turns: std::collections::BTreeSet<String>,
    #[serde(default)]
    accounted_usage: std::collections::BTreeSet<String>,
    #[serde(default)]
    accounted_compactions: std::collections::BTreeSet<String>,
    #[serde(default)]
    finished_turns: std::collections::BTreeSet<String>,
    #[serde(default)]
    assigned_subagents: std::collections::BTreeSet<String>,
    #[serde(default)]
    active_subagents: std::collections::BTreeMap<String, ActiveSubagentBoundary>,
    #[serde(default)]
    active_tools: std::collections::BTreeMap<String, ActiveToolBoundary>,
    #[serde(default)]
    recovery_blockers: std::collections::BTreeMap<String, RecoveryBlocker>,
    #[serde(default)]
    todo_runtime: std::collections::BTreeMap<String, TodoRuntimeProjection>,
    #[serde(default)]
    summaries: std::collections::BTreeMap<String, TaskExecutionSummary>,
    #[serde(default)]
    completion: CompletionQueryProjection,
    #[serde(default)]
    seen_run_ids: std::collections::BTreeSet<String>,
    #[serde(skip)]
    sequence_overflow: Option<u64>,
    #[serde(skip)]
    missing_record_sequence: bool,
}

impl EventFoldState {
    pub(crate) fn has_committed_plan(&self) -> bool {
        self.plan.is_some()
    }

    pub(crate) fn seen_run_ids(&self) -> &std::collections::BTreeSet<String> {
        &self.seen_run_ids
    }

    pub(crate) fn sequence_overflow(&self) -> Option<u64> {
        self.sequence_overflow
    }

    pub(crate) fn missing_record_sequence(&self) -> bool {
        self.missing_record_sequence
    }

    pub(crate) fn has_current_query_projection_schema(&self) -> bool {
        self.query_projection_schema == Some(1)
    }

    #[cfg(test)]
    pub(crate) fn clear_query_projection_schema_for_test(&mut self) {
        self.query_projection_schema = None;
    }

    fn apply_projected_event(&mut self, event: RuntimeTaskEvent) {
        self.apply_runtime_event(&event);
        self.sequence_overflow = None;
        self.missing_record_sequence = false;
    }

    fn apply_runtime_event(&mut self, event: &RuntimeTaskEvent) {
        let Self {
            query_projection_schema,
            run,
            plan,
            tasks,
            background_cells,
            continuation,
            recent_constraints,
            execution_profile,
            started_turns,
            accounted_usage,
            accounted_compactions,
            finished_turns,
            assigned_subagents,
            active_subagents,
            active_tools,
            recovery_blockers,
            todo_runtime,
            summaries,
            completion,
            seen_run_ids,
            sequence_overflow: _,
            missing_record_sequence: _,
        } = self;
        *query_projection_schema = Some(1);
        use RuntimeEventKind as K;
        for ev in std::slice::from_ref(event) {
            seen_run_ids.insert(ev.run_id.clone());
            match ev.event_type {
                K::RunCreated => {
                    let p = &ev.payload;
                    let goal = p
                        .get("goal")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    *run = Some(TaskRun {
                        run_id: ev.run_id.clone(),
                        workspace_id: p
                            .get("workspace_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        conversation_id: p
                            .get("conversation_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        root_message_id: p
                            .get("root_message_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        domain_profile: p
                            .get("domain_profile")
                            .and_then(|v| v.as_str())
                            .and_then(DomainProfile::from_str)
                            .unwrap_or_default(),
                        status: TaskRunStatus::Pending,
                        goal_revision: p
                            .get("goal_revision")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(1),
                        goal_sha256: p
                            .get("goal_sha256")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)
                            .unwrap_or_else(|| super::types::task_goal_sha256(&goal)),
                        goal,
                        plan_id: None, // set by PlanRevisionCommitted below
                        route: p
                            .get("route")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        attended_mode: p
                            .get("attended_mode")
                            .and_then(|v| v.as_str())
                            .and_then(AttendedMode::from_str)
                            .unwrap_or_default(),
                        attachments: p
                            .get("attachments")
                            .and_then(|v| serde_json::from_value(v.clone()).ok())
                            .unwrap_or_default(),
                        created_at: parse_event_dt(p, "created_at", ev.timestamp),
                        updated_at: ev.timestamp,
                    });
                    *execution_profile = p
                        .get("execution_profile")
                        .cloned()
                        .and_then(|value| serde_json::from_value(value).ok())
                        .unwrap_or_default();
                }
                K::RunGoalUpdated => {
                    if let Some(r) = run.as_mut()
                        && let (Some(new_goal), Some(new_revision), Some(new_sha256)) = (
                            json_string(&ev.payload, "new_goal"),
                            ev.payload
                                .get("new_goal_revision")
                                .and_then(serde_json::Value::as_u64),
                            json_string(&ev.payload, "new_goal_sha256"),
                        )
                    {
                        r.goal = new_goal;
                        r.goal_revision = new_revision;
                        r.goal_sha256 = new_sha256;
                        r.updated_at = parse_event_dt(&ev.payload, "updated_at", ev.timestamp);
                    }
                    let state = continuation.get_or_insert_with(RunContinuationState::default);
                    state.deferred = true;
                    state.deferred_reason =
                        json_string(&ev.payload, "continuation_deferred_reason");
                    completion.requirement_skips.clear();
                    completion.revalidations.clear();
                }
                K::RunSteerRecorded => {
                    if let Some(text) = json_string(&ev.payload, "text") {
                        recent_constraints.push(RecordedUserSteer {
                            turn_id: json_string(&ev.payload, "turn_id").unwrap_or_default(),
                            text,
                            recorded_at: ev.timestamp,
                        });
                        // Bounded fold: keep the most recent constraints only.
                        let excess = recent_constraints.len().saturating_sub(MAX_RECORDED_STEERS);
                        recent_constraints.drain(0..excess);
                    }
                }
                K::RunStatusChanged => {
                    if let Some(r) = run.as_mut() {
                        if let Some(to) = ev.payload.get("to").and_then(|v| v.as_str()) {
                            r.status = TaskRunStatus::from_str(to).unwrap_or(r.status);
                        }
                        r.updated_at = ev.timestamp;
                    }
                    if ev
                        .payload
                        .get("recovery")
                        .and_then(|value| value.get("kind"))
                        .and_then(serde_json::Value::as_str)
                        == Some("boot_recovery")
                    {
                        apply_boot_recovery(
                            ev,
                            tasks,
                            background_cells,
                            continuation,
                            finished_turns,
                        );
                        apply_boot_recovery_todo_runtime(ev, todo_runtime);
                        if let Some(recovery) = ev.payload.get("recovery") {
                            if let Some(subagents) = recovery
                                .get("subagents")
                                .and_then(serde_json::Value::as_array)
                            {
                                for recovered in subagents {
                                    if let Some(execution_id) =
                                        json_string(recovered, "execution_id")
                                    {
                                        active_subagents.remove(&execution_id);
                                    }
                                }
                            }
                            if let Some(tools) =
                                recovery.get("tools").and_then(serde_json::Value::as_array)
                            {
                                for recovered in tools {
                                    if let (Some(task_id), Some(call_id)) = (
                                        json_string(recovered, "task_id"),
                                        json_string(recovered, "call_id"),
                                    ) {
                                        active_tools.remove(&tool_boundary_key(&task_id, &call_id));
                                    }
                                }
                            }
                            if let Some(recovered_tasks) =
                                recovery.get("tasks").and_then(serde_json::Value::as_array)
                            {
                                for recovered in recovered_tasks {
                                    let Some(task_id) = json_string(recovered, "task_id") else {
                                        continue;
                                    };
                                    let Some(blocker) = recovered.get("blocker") else {
                                        continue;
                                    };
                                    if blocker.is_null() {
                                        continue;
                                    }
                                    recovery_blockers.insert(
                                        task_id.clone(),
                                        RecoveryBlocker {
                                            run_id: ev.run_id.clone(),
                                            task_id,
                                            execution_id: json_string(blocker, "execution_id"),
                                            call_id: json_string(blocker, "call_id"),
                                            tool_name: json_string(blocker, "tool_name"),
                                            reason: json_string(blocker, "reason").unwrap_or_else(
                                                || {
                                                    "mutating side effect is indeterminate"
                                                        .to_string()
                                                },
                                            ),
                                        },
                                    );
                                }
                            }
                        }
                    }
                }
                K::RunAttachmentsUpdated => {
                    // Attachments bound to the run (so plan-level subagents see the
                    // same user uploads as the main agent). Decoded the same way as
                    // the RunCreated `attachments` field above.
                    if let Some(r) = run.as_mut() {
                        r.attachments = ev
                            .payload
                            .get("attachments")
                            .and_then(|v| serde_json::from_value(v.clone()).ok())
                            .unwrap_or_default();
                        r.updated_at = ev.timestamp;
                    }
                }
                K::PlanRevisionCommitted => {
                    let p = &ev.payload;
                    if let Some(committed) = p.get("plan").and_then(|value| {
                        serde_json::from_value::<PlanRevision>(value.clone()).ok()
                    }) {
                        let requirements =
                            super::completion_gate::requirements_for_revision(&committed);
                        if let Some(r) = run.as_mut() {
                            r.plan_id = Some(committed.plan_id.clone());
                        }
                        let previous_execution = tasks
                            .iter()
                            .map(|task| (task.id.clone(), task.execution()))
                            .collect::<std::collections::HashMap<_, _>>();
                        *tasks = committed
                            .tasks
                            .iter()
                            .cloned()
                            .map(|spec| {
                                let execution = previous_execution
                                    .get(&spec.id)
                                    .cloned()
                                    .unwrap_or_else(|| {
                                        echo_agent::tasks::TaskExecution::pending(spec.id.clone())
                                    });
                                PlanTask::from_parts(spec, execution)
                            })
                            .collect();
                        let skipped = p
                            .get("skipped_task_ids")
                            .and_then(|value| value.as_array())
                            .map(|values| {
                                values
                                    .iter()
                                    .filter_map(|value| value.as_str())
                                    .collect::<std::collections::HashSet<_>>()
                            })
                            .unwrap_or_default();
                        let reset = p
                            .get("reset_task_ids")
                            .and_then(|value| value.as_array())
                            .map(|values| {
                                values
                                    .iter()
                                    .filter_map(|value| value.as_str())
                                    .collect::<std::collections::HashSet<_>>()
                            })
                            .unwrap_or_default();
                        for task in tasks.iter_mut() {
                            if skipped.contains(task.id.as_str()) {
                                task.status = echo_agent::tasks::TaskStatus::Skipped;
                                task.claim = None;
                            } else if reset.contains(task.id.as_str()) {
                                task.status = echo_agent::tasks::TaskStatus::Pending;
                                task.claim = None;
                                todo_runtime.remove(&task.id);
                            }
                        }
                        *plan = Some(TaskPlan {
                            plan_id: committed.plan_id,
                            run_id: committed.run_id,
                            revision: committed.revision,
                            domain_profile: committed.domain_profile,
                            goal_revision: committed.goal_revision,
                            goal_sha256: committed.goal_sha256,
                            assumptions: committed.assumptions,
                            risks: committed.risks,
                            execution_mode: committed.execution_mode,
                            tasks: Vec::new(),
                        });
                        let task_ids = tasks
                            .iter()
                            .map(|task| task.id.as_str())
                            .collect::<std::collections::HashSet<_>>();
                        todo_runtime.retain(|task_id, _| task_ids.contains(task_id.as_str()));
                        summaries.retain(|task_id, _| task_ids.contains(task_id.as_str()));
                        completion.retain_tasks(&task_ids);
                        completion.retain_requirements(&requirements);
                    }
                }
                K::TaskStarted
                | K::TaskCompleted
                | K::TaskFailed
                | K::TaskCancelled
                | K::TaskTimedOut
                | K::TaskSkipped
                | K::TaskBlocked
                | K::TaskStatusChanged => {
                    if let Some(task_id) = ev.task_id.as_ref() {
                        let status = ev
                            .payload
                            .get("status")
                            .and_then(|value| task_status_from_event(value, &ev.payload));
                        if let Some(status) = status {
                            #[allow(clippy::collapsible_if)]
                            // nested let-Option guard reads clearer than a let-chain
                            if let Some(t) = tasks.iter_mut().find(|t| &t.id == task_id) {
                                t.status = status;
                                if let Some(value) = ev.payload.get("claim") {
                                    t.claim = serde_json::from_value(value.clone()).ok();
                                }
                            }
                        }
                        if let Some(retry_count) = ev
                            .payload
                            .get("retry_count")
                            .and_then(|value| value.as_u64())
                            .and_then(|value| u32::try_from(value).ok())
                            && let Some(task) = tasks.iter_mut().find(|task| &task.id == task_id)
                        {
                            task.retry_count = retry_count;
                        }
                        if let Some(value) = ev.payload.get("failure_fingerprint")
                            && let Some(task) = tasks.iter_mut().find(|task| &task.id == task_id)
                        {
                            task.failure_fingerprint = value.as_str().map(str::to_string);
                        }
                        apply_todo_runtime_event(
                            todo_runtime.entry(task_id.clone()).or_default(),
                            ev,
                        );
                    }
                }
                K::ArtifactProduced => {}
                K::ReviewPassed | K::ReviewNeedsFix | K::ReviewBlocked => {
                    if let Some(task_id) = ev.task_id.as_ref() {
                        completion
                            .reviews
                            .insert(task_id.clone(), bind_completion_event(ev, plan.as_ref()));
                    }
                }
                K::RequirementSkipped => {
                    if let (Some(requirement_id), Some(requirement_sha256), Some(goal_revision)) = (
                        json_string(&ev.payload, "requirement_id"),
                        json_string(&ev.payload, "requirement_sha256"),
                        ev.payload
                            .get("goal_revision")
                            .and_then(serde_json::Value::as_u64),
                    ) {
                        completion
                            .requirement_skips
                            .entry(requirement_id)
                            .or_default()
                            .entry(requirement_sha256)
                            .or_default()
                            .insert(goal_revision, ev.clone());
                    }
                }
                K::RequirementEvidenceRevalidated => {
                    if let (Some(requirement_id), Some(requirement_sha256), Some(goal_revision)) = (
                        json_string(&ev.payload, "requirement_id"),
                        json_string(&ev.payload, "requirement_sha256"),
                        ev.payload
                            .get("new_goal_revision")
                            .and_then(serde_json::Value::as_u64),
                    ) {
                        completion
                            .revalidations
                            .entry(requirement_id)
                            .or_default()
                            .entry(requirement_sha256)
                            .or_default()
                            .insert(goal_revision, ev.clone());
                    }
                }
                K::Note
                    if ev.payload.get("kind").and_then(serde_json::Value::as_str)
                        == Some("summary_persisted") =>
                {
                    if let Some(task_id) = ev.task_id.as_ref() {
                        match ev.payload.get("summary").cloned().and_then(|value| {
                            serde_json::from_value::<TaskExecutionSummary>(value).ok()
                        }) {
                            Some(summary) => {
                                summaries.insert(task_id.clone(), summary);
                            }
                            None => {
                                summaries.remove(task_id);
                            }
                        }
                        completion
                            .summaries
                            .insert(task_id.clone(), bind_completion_event(ev, plan.as_ref()));
                    }
                }
                K::BackgroundCellStarted => {
                    let Some(cell_id) = ev.payload.get("cell_id").and_then(|value| value.as_str())
                    else {
                        continue;
                    };
                    background_cells
                        .entry(cell_id.to_string())
                        .or_insert_with(|| BackgroundCellState {
                            cell_id: cell_id.to_string(),
                            name: json_string(&ev.payload, "name").unwrap_or_default(),
                            command_hash: json_string(&ev.payload, "command_hash")
                                .unwrap_or_default(),
                            turn_id: json_string(&ev.payload, "turn_id"),
                            execution_id: json_string(&ev.payload, "execution_id"),
                            call_id: json_string(&ev.payload, "call_id"),
                            phase: json_enum(&ev.payload, "phase")
                                .unwrap_or(BackgroundCellPhase::Running),
                            terminal_cause: None,
                            terminal_message: None,
                            exit_code: None,
                            artifact_status: json_enum(&ev.payload, "artifact_status")
                                .unwrap_or(BackgroundCellArtifactStatus::NotRequested),
                            artifact_message: None,
                            total_output_bytes: 0,
                            output_truncated: false,
                            output_excerpt: None,
                            artifact_path: None,
                            artifact_sha256: None,
                            started_at: ev.timestamp,
                            finished_at: None,
                        });
                }
                K::BackgroundCellFinished => {
                    let Some(cell_id) = ev.payload.get("cell_id").and_then(|value| value.as_str())
                    else {
                        continue;
                    };
                    let cell = background_cells
                        .entry(cell_id.to_string())
                        .or_insert_with(|| BackgroundCellState {
                            cell_id: cell_id.to_string(),
                            name: json_string(&ev.payload, "name").unwrap_or_default(),
                            command_hash: String::new(),
                            turn_id: None,
                            execution_id: None,
                            call_id: None,
                            phase: BackgroundCellPhase::Unknown,
                            terminal_cause: None,
                            terminal_message: None,
                            exit_code: None,
                            artifact_status: BackgroundCellArtifactStatus::NotRequested,
                            artifact_message: None,
                            total_output_bytes: 0,
                            output_truncated: false,
                            output_excerpt: None,
                            artifact_path: None,
                            artifact_sha256: None,
                            started_at: ev.timestamp,
                            finished_at: None,
                        });
                    cell.name = json_string(&ev.payload, "name").unwrap_or_default();
                    cell.call_id = json_string(&ev.payload, "call_id");
                    cell.phase =
                        json_enum(&ev.payload, "phase").unwrap_or(BackgroundCellPhase::Unknown);
                    cell.terminal_cause = json_enum(&ev.payload, "terminal_cause");
                    cell.terminal_message = json_string(&ev.payload, "terminal_message");
                    cell.exit_code = ev
                        .payload
                        .get("exit_code")
                        .and_then(serde_json::Value::as_i64)
                        .and_then(|value| i32::try_from(value).ok());
                    cell.total_output_bytes = ev
                        .payload
                        .get("total_output_bytes")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0);
                    cell.output_truncated = ev
                        .payload
                        .get("output_truncated")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false);
                    cell.output_excerpt = json_string(&ev.payload, "output_excerpt");
                    cell.artifact_status = json_enum(&ev.payload, "artifact_status")
                        .unwrap_or(BackgroundCellArtifactStatus::NotRequested);
                    cell.artifact_message = json_string(&ev.payload, "artifact_message");
                    cell.artifact_path = json_string(&ev.payload, "artifact_path");
                    cell.artifact_sha256 = json_string(&ev.payload, "artifact_sha256");
                    cell.finished_at = Some(ev.timestamp);
                }
                K::SubagentAssigned => {
                    let (Some(task_id), Some(execution_id)) =
                        (ev.task_id.clone(), ev.step_id.clone())
                    else {
                        continue;
                    };
                    assigned_subagents.insert(execution_id.clone());
                    active_subagents.insert(
                        execution_id.clone(),
                        ActiveSubagentBoundary {
                            task_id,
                            execution_id,
                            replay_safe: ev
                                .payload
                                .get("replay_safe")
                                .and_then(serde_json::Value::as_bool)
                                .unwrap_or(false),
                        },
                    );
                }
                K::SubagentReleased => {
                    if let Some(execution_id) = ev.step_id.as_ref() {
                        active_subagents.remove(execution_id);
                    }
                }
                K::ToolStarted => {
                    let Some(task_id) = ev.task_id.clone() else {
                        continue;
                    };
                    let call_id = json_string(&ev.payload, "call_id")
                        .or_else(|| ev.step_id.clone())
                        .unwrap_or_default();
                    if call_id.is_empty() {
                        continue;
                    }
                    active_tools.insert(
                        tool_boundary_key(&task_id, &call_id),
                        ActiveToolBoundary {
                            task_id,
                            execution_id: json_string(&ev.payload, "execution_id"),
                            call_id,
                            tool_name: json_string(&ev.payload, "tool_name")
                                .unwrap_or_else(|| "unknown".to_string()),
                            replay_safe: ev
                                .payload
                                .get("replay_safe")
                                .and_then(serde_json::Value::as_bool)
                                .unwrap_or(false),
                        },
                    );
                }
                K::ToolCompleted | K::ToolFailed => {
                    let Some(task_id) = ev.task_id.as_ref() else {
                        continue;
                    };
                    let call_id = json_string(&ev.payload, "call_id")
                        .or_else(|| ev.step_id.clone())
                        .unwrap_or_default();
                    active_tools.remove(&tool_boundary_key(task_id, &call_id));
                }
                K::RecoveryBlocked => {
                    let Some(task_id) = ev.task_id.clone() else {
                        continue;
                    };
                    recovery_blockers.insert(
                        task_id.clone(),
                        RecoveryBlocker {
                            run_id: ev.run_id.clone(),
                            task_id,
                            execution_id: json_string(&ev.payload, "execution_id"),
                            call_id: json_string(&ev.payload, "call_id"),
                            tool_name: json_string(&ev.payload, "tool_name"),
                            reason: json_string(&ev.payload, "reason").unwrap_or_else(|| {
                                "mutating side effect is indeterminate".to_string()
                            }),
                        },
                    );
                }
                K::RecoveryResolved => {
                    if let Some(task_id) = ev.task_id.as_ref() {
                        recovery_blockers.remove(task_id);
                    }
                }
                K::RunContinuationConfigured => {
                    let state = continuation.get_or_insert_with(RunContinuationState::default);
                    state.enabled = ev
                        .payload
                        .get("enabled")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(state.enabled);
                    state.auto_resume_after_restart = ev
                        .payload
                        .get("auto_resume_after_restart")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(state.auto_resume_after_restart);
                    if ev.payload.get("token_budget").is_some() {
                        state.token_budget = ev
                            .payload
                            .get("token_budget")
                            .and_then(serde_json::Value::as_u64);
                    }
                    if ev.payload.get("time_budget_seconds").is_some() {
                        state.time_budget_seconds = ev
                            .payload
                            .get("time_budget_seconds")
                            .and_then(serde_json::Value::as_u64);
                    }
                    if let Some(reason) = json_string(&ev.payload, "pause_reason")
                        .as_deref()
                        .and_then(RunPauseReason::from_wire)
                    {
                        state.pause = Some(RunPause {
                            reason,
                            detail: json_string(&ev.payload, "pause_detail"),
                            changed_at: ev.timestamp,
                        });
                        if let Some(run) = run.as_mut() {
                            run.status = TaskRunStatus::Paused;
                            run.updated_at = ev.timestamp;
                        }
                    }
                }
                K::RunTurnStarted => {
                    let state = continuation.get_or_insert_with(RunContinuationState::default);
                    let ordinal = ev
                        .payload
                        .get("ordinal")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(state.next_turn_ordinal);
                    let turn_id = json_string(&ev.payload, "turn_id")
                        .unwrap_or_else(|| format!("{}:turn:{ordinal}", ev.run_id));
                    if !started_turns.insert(turn_id.clone()) {
                        continue;
                    }
                    let origin = json_string(&ev.payload, "origin")
                        .as_deref()
                        .and_then(RunTurnOrigin::from_wire)
                        .unwrap_or(RunTurnOrigin::Continuation);
                    let transcript_visibility =
                        match json_string(&ev.payload, "transcript_visibility").as_deref() {
                            Some("visible") => TurnVisibility::Visible,
                            _ => TurnVisibility::Internal,
                        };
                    state.next_turn_ordinal = ordinal.saturating_add(1);
                    state.active_turn = Some(RunTurnSummary {
                        turn_id,
                        ordinal,
                        origin,
                        status: RunTurnStatus::Running,
                        transcript_visibility,
                        started_at: ev.timestamp,
                        ended_at: None,
                        input_tokens: 0,
                        output_tokens: 0,
                        elapsed_seconds: 0,
                        compaction_count: 0,
                        final_message_id: None,
                        error_fingerprint: None,
                    });
                    state.pause = None;
                }
                K::RunTurnUsageAccounted => {
                    let Some(event_id) = json_string(&ev.payload, "event_id") else {
                        continue;
                    };
                    if !accounted_usage.insert(event_id) {
                        continue;
                    }
                    let state = continuation.get_or_insert_with(RunContinuationState::default);
                    let input_tokens = ev
                        .payload
                        .get("input_tokens")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0);
                    let output_tokens = ev
                        .payload
                        .get("output_tokens")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0);
                    state.tokens_used = state
                        .tokens_used
                        .saturating_add(input_tokens.saturating_add(output_tokens));
                    let elapsed_seconds = ev
                        .payload
                        .get("elapsed_seconds")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0);
                    state.time_used_seconds =
                        state.time_used_seconds.saturating_add(elapsed_seconds);
                    let turn_id = json_string(&ev.payload, "turn_id");
                    if let Some(active) = state.active_turn.as_mut()
                        && turn_id.as_deref() == Some(active.turn_id.as_str())
                    {
                        active.input_tokens = active.input_tokens.saturating_add(input_tokens);
                        active.output_tokens = active.output_tokens.saturating_add(output_tokens);
                    }
                    if let Some(reason) = json_string(&ev.payload, "pause_reason")
                        .as_deref()
                        .and_then(RunPauseReason::from_wire)
                    {
                        state.pause = Some(RunPause {
                            reason,
                            detail: json_string(&ev.payload, "pause_detail"),
                            changed_at: ev.timestamp,
                        });
                        if let Some(run) = run.as_mut() {
                            run.status = TaskRunStatus::Paused;
                            run.updated_at = ev.timestamp;
                        }
                    }
                }
                K::RunTurnCompacted => {
                    let Some(event_id) = json_string(&ev.payload, "event_id") else {
                        continue;
                    };
                    if !accounted_compactions.insert(event_id) {
                        continue;
                    }
                    let state = continuation.get_or_insert_with(RunContinuationState::default);
                    state.compaction_count = state.compaction_count.saturating_add(1);
                    let turn_id = json_string(&ev.payload, "turn_id");
                    if let Some(active) = state.active_turn.as_mut()
                        && turn_id.as_deref() == Some(active.turn_id.as_str())
                    {
                        active.compaction_count = active.compaction_count.saturating_add(1);
                    }
                }
                K::RunTurnFinished => {
                    let Some(turn_id) = json_string(&ev.payload, "turn_id") else {
                        continue;
                    };
                    if finished_turns.contains(&turn_id) {
                        continue;
                    }
                    let state = continuation.get_or_insert_with(RunContinuationState::default);
                    let mut finished = match state.active_turn.as_ref() {
                        Some(active) if active.turn_id == turn_id => {
                            finished_turns.insert(turn_id);
                            state.active_turn.take()
                        }
                        _ => continue,
                    };
                    if let Some(summary) = finished.as_mut() {
                        summary.status = json_string(&ev.payload, "status")
                            .as_deref()
                            .and_then(RunTurnStatus::from_wire)
                            .unwrap_or(RunTurnStatus::Failed);
                        if summary.status == RunTurnStatus::Ended {
                            state.provider_retry = None;
                        }
                        summary.ended_at = Some(ev.timestamp);
                        summary.elapsed_seconds = ev
                            .payload
                            .get("elapsed_seconds")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(0);
                        summary.final_message_id = json_string(&ev.payload, "final_message_id");
                        summary.error_fingerprint = json_string(&ev.payload, "error_fingerprint");
                        state.time_used_seconds = state
                            .time_used_seconds
                            .saturating_add(summary.elapsed_seconds);
                        state.last_turn = Some(summary.clone());
                    }
                    match ev
                        .payload
                        .get("made_progress")
                        .and_then(serde_json::Value::as_bool)
                    {
                        Some(true) => state.blocker_audit = None,
                        Some(false) => {
                            let blocker_fingerprint =
                                json_string(&ev.payload, "blocker_fingerprint")
                                    .unwrap_or_else(|| "stalled:unknown".to_string());
                            state.blocker_audit = Some(match state.blocker_audit.take() {
                                Some(previous) if previous.fingerprint == blocker_fingerprint => {
                                    BlockerAudit {
                                        fingerprint: blocker_fingerprint,
                                        consecutive_turns: previous
                                            .consecutive_turns
                                            .saturating_add(1),
                                    }
                                }
                                _ => BlockerAudit {
                                    fingerprint: blocker_fingerprint,
                                    consecutive_turns: 1,
                                },
                            });
                        }
                        None => {
                            let Some(progress_fingerprint) =
                                json_string(&ev.payload, "progress_fingerprint")
                            else {
                                continue;
                            };
                            state.blocker_audit = Some(match state.blocker_audit.take() {
                                Some(previous) if previous.fingerprint == progress_fingerprint => {
                                    BlockerAudit {
                                        fingerprint: progress_fingerprint,
                                        consecutive_turns: previous
                                            .consecutive_turns
                                            .saturating_add(1),
                                    }
                                }
                                _ => BlockerAudit {
                                    fingerprint: progress_fingerprint,
                                    consecutive_turns: 1,
                                },
                            });
                        }
                    }
                }
                K::RunProviderRetryScheduled => {
                    let state = continuation.get_or_insert_with(RunContinuationState::default);
                    let attempt_count = ev
                        .payload
                        .get("attempt_count")
                        .and_then(serde_json::Value::as_u64)
                        .and_then(|value| u32::try_from(value).ok())
                        .unwrap_or(1);
                    state.provider_retry = Some(ProviderRetryState {
                        attempt_count,
                        next_retry_at: parse_event_dt(&ev.payload, "next_retry_at", ev.timestamp),
                        error_fingerprint: json_string(&ev.payload, "error_fingerprint")
                            .unwrap_or_else(|| "provider:unknown".to_string()),
                        first_failure_at: parse_event_dt(
                            &ev.payload,
                            "first_failure_at",
                            ev.timestamp,
                        ),
                        exhausted: ev
                            .payload
                            .get("exhausted")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false),
                    });
                    if let Some(reason) = json_string(&ev.payload, "pause_reason")
                        .as_deref()
                        .and_then(RunPauseReason::from_wire)
                    {
                        state.pause = Some(RunPause {
                            reason,
                            detail: json_string(&ev.payload, "pause_detail"),
                            changed_at: ev.timestamp,
                        });
                        if let Some(run) = run.as_mut() {
                            run.status = TaskRunStatus::Paused;
                            run.updated_at = ev.timestamp;
                        }
                    }
                }
                K::RunContinuationDeferred => {
                    let state = continuation.get_or_insert_with(RunContinuationState::default);
                    state.deferred = true;
                    state.deferred_reason = json_string(&ev.payload, "reason");
                }
                K::RunContinuationResumed => {
                    let state = continuation.get_or_insert_with(RunContinuationState::default);
                    state.deferred = false;
                    state.deferred_reason = None;
                    state.blocker_audit = None;
                    if ev
                        .payload
                        .get("reset_provider_retry")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false)
                    {
                        state.provider_retry = None;
                    }
                }
                K::RunPauseReasonChanged => {
                    let state = continuation.get_or_insert_with(RunContinuationState::default);
                    state.pause = json_string(&ev.payload, "reason")
                        .as_deref()
                        .and_then(RunPauseReason::from_wire)
                        .map(|reason| RunPause {
                            reason,
                            detail: json_string(&ev.payload, "detail"),
                            changed_at: ev.timestamp,
                        });
                }
                _ => {} // ArtifactProduced/Review*/Approval*/Note(other) don't affect plan.json
            }
        }
    }

    pub(crate) fn rebuilt_plan(&self) -> Result<RebuiltPlan, RebuildError> {
        let run = self.run.clone().ok_or(RebuildError::NoRunCreated)?;
        let plan = self.plan.clone().unwrap_or_else(|| empty_plan_for(&run));
        Ok(RebuiltPlan {
            run,
            plan,
            tasks: self.tasks.clone(),
            background_cells: self.background_cells.values().cloned().collect(),
            continuation: self.continuation.clone(),
            recent_constraints: self.recent_constraints.clone(),
            execution_profile: self.execution_profile,
            event_index: RunStateEventIndex {
                started_turns: self.started_turns.clone(),
                accounted_usage: self.accounted_usage.clone(),
                accounted_compactions: self.accounted_compactions.clone(),
                finished_turns: self.finished_turns.clone(),
                assigned_subagents: self.assigned_subagents.clone(),
                active_subagents: self.active_subagents.values().cloned().collect(),
                active_tools: self.active_tools.values().cloned().collect(),
                recovery_blockers: self.recovery_blockers.values().cloned().collect(),
            },
        })
    }

    fn current_plan(&self, rebuilt: &RebuiltPlan) -> Option<TaskPlan> {
        self.plan.as_ref().map(|_| TaskPlan {
            plan_id: rebuilt.plan.plan_id.clone(),
            run_id: rebuilt.plan.run_id.clone(),
            revision: rebuilt.plan.revision,
            domain_profile: rebuilt.plan.domain_profile,
            goal_revision: rebuilt.plan.goal_revision,
            goal_sha256: rebuilt.plan.goal_sha256.clone(),
            assumptions: rebuilt.plan.assumptions.clone(),
            risks: rebuilt.plan.risks.clone(),
            execution_mode: rebuilt.plan.execution_mode,
            tasks: rebuilt.tasks.clone(),
        })
    }

    pub(crate) fn todo_query_projection(&self) -> Result<TodoQueryProjection, RebuildError> {
        let rebuilt = self.rebuilt_plan()?;
        let plan = self.current_plan(&rebuilt);
        let task_ids = rebuilt
            .tasks
            .iter()
            .map(|task| task.id.as_str())
            .collect::<std::collections::HashSet<_>>();
        Ok(TodoQueryProjection {
            run: rebuilt.run,
            plan,
            todo_runtime: self
                .todo_runtime
                .iter()
                .filter(|(task_id, _)| task_ids.contains(task_id.as_str()))
                .map(|(task_id, runtime)| (task_id.clone(), runtime.clone()))
                .collect(),
        })
    }

    pub(crate) fn completion_gate_projection(
        &self,
    ) -> Result<CompletionGateProjection, RebuildError> {
        let rebuilt = self.rebuilt_plan()?;
        let plan = self.current_plan(&rebuilt);
        let task_ids = rebuilt
            .tasks
            .iter()
            .map(|task| task.id.as_str())
            .collect::<std::collections::HashSet<_>>();
        Ok(CompletionGateProjection {
            run: rebuilt.run,
            plan,
            completion: self.completion.for_tasks(&task_ids),
            active_subagents: self.active_subagents.values().cloned().collect(),
            background_cells: self.background_cells.values().cloned().collect(),
            recovery_blockers: self.recovery_blockers.values().cloned().collect(),
        })
    }

    pub(crate) fn summary_projection(&self, task_id: &str) -> Option<TaskExecutionSummary> {
        self.summaries.get(task_id).cloned()
    }
}

fn retain_requirement_index(
    index: &mut std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<String, std::collections::BTreeMap<u64, RuntimeTaskEvent>>,
    >,
    identities: &std::collections::HashSet<(&str, &str, u64)>,
) {
    index.retain(|requirement_id, by_sha| {
        by_sha.retain(|requirement_sha256, by_goal| {
            by_goal.retain(|goal_revision, _| {
                identities.contains(&(
                    requirement_id.as_str(),
                    requirement_sha256.as_str(),
                    *goal_revision,
                ))
            });
            !by_goal.is_empty()
        });
        !by_sha.is_empty()
    });
}

impl EventReducer for EventFoldState {
    type Event = RuntimeJournalEvent;

    fn apply(&mut self, _event: &Self::Event) {
        // RuntimeJournalEvent has no caller-assigned sequence by design. The
        // framework CheckpointedReducer always invokes apply_record; treating
        // payload-only apply as degraded avoids reintroducing a second EKO
        // sequence allocator.
        self.missing_record_sequence = true;
    }

    fn apply_record(&mut self, record: &JournalRecord<Self::Event>) {
        match record.event.project(record.sequence) {
            Ok(event) => self.apply_projected_event(event),
            Err(sequence) => self.sequence_overflow = Some(sequence),
        }
    }
}

fn tool_boundary_key(task_id: &str, call_id: &str) -> String {
    format!("{task_id}\0{call_id}")
}

fn json_string(payload: &serde_json::Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

fn json_enum<T: serde::de::DeserializeOwned>(payload: &serde_json::Value, key: &str) -> Option<T> {
    payload
        .get(key)
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

fn bind_completion_event(
    event: &RuntimeTaskEvent,
    plan: Option<&TaskPlan>,
) -> BoundCompletionEvent {
    BoundCompletionEvent {
        event: event.clone(),
        source_goal_revision: plan.map_or(0, |plan| plan.goal_revision),
        source_plan_revision: plan.map_or(0, |plan| plan.revision),
    }
}

fn apply_todo_runtime_event(runtime: &mut TodoRuntimeProjection, event: &RuntimeTaskEvent) {
    let status = event
        .payload
        .get("status")
        .and_then(serde_json::Value::as_str);
    if status == Some("pending")
        || event.event_type == RuntimeEventKind::TaskStarted
        || matches!(status, Some("running" | "retrying"))
    {
        *runtime = TodoRuntimeProjection::default();
    }
    if let Some(owner) =
        json_string(&event.payload, "owner_agent").filter(|value| !value.is_empty())
    {
        runtime.owner_agent = Some(owner);
    }
    if let Some(started_at) = event
        .payload
        .get("started_at")
        .and_then(serde_json::Value::as_str)
        .and_then(parse_rfc3339)
    {
        runtime.started_at = Some(started_at);
    }
    if let Some(completed_at) = event
        .payload
        .get("completed_at")
        .and_then(serde_json::Value::as_str)
        .and_then(parse_rfc3339)
    {
        runtime.completed_at = Some(completed_at);
    }
    if let Some(summary) = json_string(&event.payload, "summary").filter(|value| !value.is_empty())
    {
        runtime.summary = Some(summary);
    }
}

fn apply_boot_recovery_todo_runtime(
    event: &RuntimeTaskEvent,
    todo_runtime: &mut std::collections::BTreeMap<String, TodoRuntimeProjection>,
) {
    let Some(tasks) = event
        .payload
        .get("recovery")
        .and_then(|recovery| recovery.get("tasks"))
        .and_then(serde_json::Value::as_array)
    else {
        return;
    };
    for task in tasks {
        let (Some(task_id), Some(summary)) = (
            task.get("task_id").and_then(serde_json::Value::as_str),
            task.get("summary").and_then(serde_json::Value::as_str),
        ) else {
            continue;
        };
        todo_runtime.entry(task_id.to_string()).or_default().summary = Some(summary.to_string());
    }
}

pub(crate) fn artifact_from_event(event: &RuntimeTaskEvent) -> Option<Artifact> {
    let payload = &event.payload;
    Some(Artifact {
        id: json_string(payload, "artifact_id")?,
        run_id: event.run_id.clone(),
        task_id: event.task_id.clone(),
        kind: json_string(payload, "kind")
            .as_deref()
            .and_then(ArtifactKind::from_str)
            .unwrap_or(ArtifactKind::File),
        title: json_string(payload, "title")?,
        path: json_string(payload, "path"),
        metadata: payload
            .get("metadata")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
    })
}

pub(crate) fn review_from_event(event: &RuntimeTaskEvent) -> Option<ReviewResult> {
    let payload = &event.payload;
    Some(ReviewResult {
        id: json_string(payload, "review_id")?,
        run_id: event.run_id.clone(),
        reviewer_agent: json_string(payload, "reviewer")?,
        outcome: match event.event_type {
            RuntimeEventKind::ReviewPassed => ReviewOutcome::Pass,
            RuntimeEventKind::ReviewNeedsFix => ReviewOutcome::NeedsFix,
            RuntimeEventKind::ReviewBlocked => ReviewOutcome::Blocked,
            _ => return None,
        },
        issues: payload
            .get("issues")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default(),
        failure_fingerprint: json_string(payload, "failure_fingerprint"),
        created_fix_task_id: json_string(payload, "created_fix_task_id"),
        created_at: payload
            .get("created_at")
            .and_then(serde_json::Value::as_str)
            .and_then(parse_rfc3339)
            .unwrap_or(event.timestamp),
        task_id: event.task_id.clone().unwrap_or_default(),
    })
}

fn apply_boot_recovery(
    event: &RuntimeTaskEvent,
    tasks: &mut [PlanTask],
    background_cells: &mut std::collections::BTreeMap<String, BackgroundCellState>,
    continuation: &mut Option<RunContinuationState>,
    finished_turns: &mut std::collections::BTreeSet<String>,
) {
    let Some(recovery) = event.payload.get("recovery") else {
        return;
    };
    let state = continuation.get_or_insert_with(RunContinuationState::default);
    let target = event
        .payload
        .get("to")
        .and_then(serde_json::Value::as_str)
        .and_then(TaskRunStatus::from_str)
        .unwrap_or(TaskRunStatus::Paused);
    if let Some(turn_id) = recovery
        .get("active_turn")
        .and_then(|value| value.get("turn_id"))
        .and_then(serde_json::Value::as_str)
        && finished_turns.insert(turn_id.to_string())
        && state.active_turn.as_ref().map(|turn| turn.turn_id.as_str()) == Some(turn_id)
        && let Some(mut turn) = state.active_turn.take()
    {
        turn.status = if target == TaskRunStatus::Cancelled {
            RunTurnStatus::Cancelled
        } else {
            RunTurnStatus::Failed
        };
        turn.ended_at = Some(event.timestamp);
        turn.elapsed_seconds = 0;
        turn.final_message_id = None;
        turn.error_fingerprint = Some("process_interrupted".to_string());
        state.last_turn = Some(turn);
    }
    state.pause = (target == TaskRunStatus::Paused).then(|| RunPause {
        reason: RunPauseReason::BootRecovery,
        detail: recovery
            .get("pause")
            .and_then(|value| value.get("detail"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        changed_at: event.timestamp,
    });

    if let Some(cells) = recovery.get("cells").and_then(serde_json::Value::as_array) {
        for recovered in cells {
            let Some(cell_id) = recovered.get("cell_id").and_then(serde_json::Value::as_str) else {
                continue;
            };
            let cell = background_cells
                .entry(cell_id.to_string())
                .or_insert_with(|| BackgroundCellState {
                    cell_id: cell_id.to_string(),
                    name: json_string(recovered, "name").unwrap_or_default(),
                    command_hash: String::new(),
                    turn_id: None,
                    execution_id: None,
                    call_id: json_string(recovered, "call_id"),
                    phase: BackgroundCellPhase::Unknown,
                    terminal_cause: None,
                    terminal_message: None,
                    exit_code: None,
                    artifact_status: BackgroundCellArtifactStatus::NotRequested,
                    artifact_message: None,
                    total_output_bytes: 0,
                    output_truncated: false,
                    output_excerpt: None,
                    artifact_path: None,
                    artifact_sha256: None,
                    started_at: event.timestamp,
                    finished_at: None,
                });
            cell.phase = BackgroundCellPhase::Failed;
            cell.terminal_cause = Some(BackgroundCellTerminalCause::Interrupted);
            cell.terminal_message =
                Some("command cell was interrupted by process restart".to_string());
            cell.exit_code = None;
            cell.total_output_bytes = recovered
                .get("total_output_bytes")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(cell.total_output_bytes);
            cell.output_truncated = recovered
                .get("output_truncated")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(cell.output_truncated);
            cell.output_excerpt = json_string(recovered, "output_excerpt");
            cell.artifact_path = json_string(recovered, "artifact_path");
            cell.artifact_sha256 = json_string(recovered, "artifact_sha256");
            cell.finished_at = Some(event.timestamp);
        }
    }

    if let Some(recovered_tasks) = recovery.get("tasks").and_then(serde_json::Value::as_array) {
        for recovered in recovered_tasks {
            let Some(task_id) = recovered.get("task_id").and_then(serde_json::Value::as_str) else {
                continue;
            };
            let Some(status) = recovered
                .get("status")
                .and_then(|value| task_status_from_event(value, recovered))
            else {
                continue;
            };
            if let Some(task) = tasks.iter_mut().find(|task| task.id == task_id) {
                task.status = status;
                task.claim = None;
            }
        }
    }
}

fn task_status_from_event(
    value: &serde_json::Value,
    payload: &serde_json::Value,
) -> Option<echo_agent::tasks::TaskStatus> {
    let status = value.as_str()?;
    let detail = payload
        .get("status_detail")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(status)
        .to_string();
    Some(match status {
        "pending" => echo_agent::tasks::TaskStatus::Pending,
        "running" => echo_agent::tasks::TaskStatus::Running,
        "blocked" => echo_agent::tasks::TaskStatus::Blocked(detail),
        "completed" => echo_agent::tasks::TaskStatus::Completed,
        "failed" => echo_agent::tasks::TaskStatus::Failed(detail),
        "skipped" => echo_agent::tasks::TaskStatus::Skipped,
        "cancelled" => echo_agent::tasks::TaskStatus::Cancelled,
        "timed_out" => echo_agent::tasks::TaskStatus::TimedOut { error: detail },
        "retrying" => echo_agent::tasks::TaskStatus::Retrying {
            attempt: payload
                .get("retry_count")
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .unwrap_or_default(),
            last_error: detail,
        },
        "paused" => echo_agent::tasks::TaskStatus::Paused(detail),
        _ => return None,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum RebuildError {
    #[error("event stream has no RunCreated event")]
    NoRunCreated,
}

fn parse_event_dt(
    payload: &serde_json::Value,
    key: &str,
    fallback: DateTime<Utc>,
) -> DateTime<Utc> {
    payload
        .get(key)
        .and_then(|v| v.as_str())
        .and_then(parse_rfc3339)
        .unwrap_or(fallback)
}

fn parse_rfc3339(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

fn empty_plan_for(run: &TaskRun) -> TaskPlan {
    TaskPlan {
        plan_id: String::new(),
        run_id: run.run_id.clone(),
        revision: 0,
        domain_profile: run.domain_profile,
        goal_revision: run.goal_revision,
        goal_sha256: run.goal_sha256.clone(),
        assumptions: Vec::new(),
        risks: Vec::new(),
        execution_mode: ExecutionMode::default(),
        tasks: Vec::new(),
    }
}

#[cfg(test)]
pub(crate) fn fold_fixture_for_test(
    events: &[RuntimeTaskEvent],
) -> Result<RebuiltPlan, RebuildError> {
    let mut state = EventFoldState::default();
    for event in events {
        state.apply_runtime_event(event);
    }
    state.rebuilt_plan()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::task_runtime::store::{
        RunTurnClaimOutcome, RunTurnCompletion, StoreError, TaskRuntimeStore,
    };
    use crate::tasks::task_runtime::types::{
        DomainProfile, ExecutionMode, PlanTask, PlanTaskKind, RunTurnOrigin, RunTurnStatus,
        TaskPatch, TaskPlan, TaskRunStatus, TaskUpdateOperation, TaskUpdateRequest, TurnVisibility,
    };

    fn fresh() -> Result<TaskRuntimeStore, StoreError> {
        TaskRuntimeStore::new_in_memory()
            .map_err(|error| StoreError::InvalidPlan(error.to_string()))
    }

    fn sample_task(id: &str, kind: PlanTaskKind) -> PlanTask {
        PlanTask {
            id: id.to_string(),
            title: format!("task {id}"),
            description: format!("do {id}"),
            kind,
            agent_role: "code_reviewer".to_string(),
            domain_profile: DomainProfile::AiCoding,
            depends_on: Vec::new(),
            parallel_group: None,
            execution_target: None,
            files: vec!["src/a.rs".to_string()],
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

    /// Gate 1: rebuild from events == read from SQL. Drives a full run lifecycle
    /// (create run → insert 2 tasks → attach plan → set statuses) and asserts the
    /// event-rebuilt plan matches the SQL-read plan on run header, plan envelope,
    /// and every task's defining fields.
    #[test]
    fn rebuild_matches_sql_after_full_lifecycle() -> Result<(), Box<dyn std::error::Error>> {
        let s = fresh()?;

        // 1. create_run
        let _run = s.create_run(
            "r1",
            "ws",
            "c1",
            "m1",
            DomainProfile::AiCoding,
            "review runtime",
            "complex_runtime",
            AttendedMode::Attended,
        )?;

        // 2. attach a structured plan (the authoritative plan-creation path).
        let plan = TaskPlan {
            plan_id: "p1".to_string(),
            run_id: "r1".to_string(),
            revision: 1,
            domain_profile: DomainProfile::AiCoding,
            goal_revision: 1,
            goal_sha256: crate::tasks::task_runtime::task_goal_sha256("review runtime"),
            assumptions: vec!["repo is small".to_string()],
            risks: vec!["flaky tests".to_string()],
            execution_mode: ExecutionMode::Parallel,
            tasks: vec![
                sample_task("t1", PlanTaskKind::ReadOnlyReview),
                sample_task("t2", PlanTaskKind::Investigation),
            ],
        };
        s.attach_plan_for_test(&plan)?;

        // 4. mutate a task status
        s.set_task_status(
            "r1",
            "t1",
            echo_agent::tasks::TaskStatus::Running,
            Some("code_reviewer"),
            None,
        )?;

        // 5. Read SQL ground truth.
        let sql_run = s
            .get_run("r1")?
            .ok_or_else(|| std::io::Error::other("run r1 missing"))?;
        let sql_plan = s
            .get_plan("r1")?
            .ok_or_else(|| std::io::Error::other("plan r1 missing"))?;
        let events = s.list_events("r1", 0)?;

        // 6. Rebuild from events.
        let rebuilt = fold_fixture_for_test(&events)?;

        // 7. Assert run header parity.
        assert_eq!(rebuilt.run.run_id, sql_run.run_id);
        assert_eq!(rebuilt.run.workspace_id, sql_run.workspace_id);
        assert_eq!(rebuilt.run.conversation_id, sql_run.conversation_id);
        assert_eq!(rebuilt.run.root_message_id, sql_run.root_message_id);
        assert_eq!(rebuilt.run.domain_profile, sql_run.domain_profile);
        assert_eq!(rebuilt.run.goal, sql_run.goal);
        assert_eq!(rebuilt.run.route, sql_run.route);
        assert_eq!(rebuilt.run.status, sql_run.status);
        assert_eq!(rebuilt.run.plan_id, sql_run.plan_id);

        // 8. Assert plan envelope parity.
        assert_eq!(rebuilt.plan.plan_id, sql_plan.plan_id);
        assert_eq!(rebuilt.plan.domain_profile, sql_plan.domain_profile);
        assert_eq!(rebuilt.plan.goal_revision, sql_plan.goal_revision);
        assert_eq!(rebuilt.plan.goal_sha256, sql_plan.goal_sha256);
        assert_eq!(rebuilt.plan.assumptions, sql_plan.assumptions);
        assert_eq!(rebuilt.plan.risks, sql_plan.risks);
        assert_eq!(rebuilt.plan.execution_mode, sql_plan.execution_mode);

        // 9. Assert task parity. The revision commit replaced tasks, so the
        // projection has 1 task (t1) and rebuild must converge to that state.
        let rebuilt_t1 = rebuilt.tasks.iter().find(|t| t.id == "t1").ok_or_else(|| {
            std::io::Error::other(format!(
                "task t1 missing from rebuilt tasks: {:?}",
                rebuilt.tasks.iter().map(|t| &t.id).collect::<Vec<_>>()
            ))
        })?;
        assert_eq!(rebuilt_t1.title, "task t1");
        assert_eq!(rebuilt_t1.description, "do t1");
        assert_eq!(rebuilt_t1.kind, PlanTaskKind::ReadOnlyReview);
        assert_eq!(rebuilt_t1.agent_role, "code_reviewer");
        assert_eq!(rebuilt_t1.files, vec!["src/a.rs".to_string()]);
        assert_eq!(rebuilt_t1.allowed_tools, vec!["read_file".to_string()]);
        // status was set to Running after attach.
        assert_eq!(rebuilt_t1.status, echo_agent::tasks::TaskStatus::Running);
        Ok(())
    }

    /// A committed revision patch must be visible in the rebuilt specification.
    #[test]
    fn rebuild_reflects_task_patch() -> Result<(), Box<dyn std::error::Error>> {
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
            goal_sha256: crate::tasks::task_runtime::task_goal_sha256("g"),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Parallel,
            tasks: vec![sample_task("t1", PlanTaskKind::Investigation)],
        })?;
        s.apply_task_patch_for_test(
            "r1",
            &TaskUpdateRequest {
                base_revision: 1,
                reason: "refine investigation".to_string(),
                operations: vec![TaskUpdateOperation::Update {
                    task_id: "t1".to_string(),
                    patch: TaskPatch {
                        title: Some("renamed".to_string()),
                        description: Some("new desc".to_string()),
                        kind: Some(PlanTaskKind::ReadOnlyReview),
                        agent_role: Some("explorer".to_string()),
                        depends_on: None,
                        files: Some(vec!["b.rs".to_string()]),
                        allowed_tools: None,
                        required_artifacts: None,
                        execution_checks: None,
                        acceptance_criteria: None,
                        max_retries: None,
                    },
                }],
            },
        )?;

        let events = s.list_events("r1", 0)?;
        let rebuilt = fold_fixture_for_test(&events)?;
        let t = rebuilt
            .tasks
            .iter()
            .find(|t| t.id == "t1")
            .ok_or_else(|| std::io::Error::other("rebuilt task t1 missing"))?;
        assert_eq!(t.title, "renamed");
        assert_eq!(t.description, "new desc");
        assert_eq!(t.kind, PlanTaskKind::ReadOnlyReview);
        assert_eq!(t.agent_role, "explorer");
        assert_eq!(t.files, vec!["b.rs".to_string()]);
        Ok(())
    }

    #[test]
    fn continuation_fold_is_idempotent_under_duplicate_event_delivery() -> Result<(), String> {
        let store = fresh().map_err(|error| error.to_string())?;
        store
            .create_run(
                "continuation-run",
                "ws",
                "c1",
                "m1",
                DomainProfile::General,
                "finish the goal",
                "task",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run("continuation-run", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store
            .configure_run_continuation("continuation-run", true, false, Some(100), None)
            .map_err(|error| error.to_string())?;
        if !matches!(
            store
                .claim_run_turn(
                    "continuation-run",
                    "turn-1",
                    RunTurnOrigin::User,
                    TurnVisibility::Visible,
                )
                .map_err(|error| error.to_string())?,
            RunTurnClaimOutcome::Started(_)
        ) {
            return Err("first RunTurn was not started".to_string());
        }
        store
            .account_run_turn_usage("continuation-run", "turn-1", "usage-1", 11, 7)
            .map_err(|error| error.to_string())?;
        store
            .record_run_turn_compaction("continuation-run", "turn-1", "compact-1")
            .map_err(|error| error.to_string())?;
        store
            .finish_run_turn(
                "continuation-run",
                RunTurnCompletion {
                    turn_id: "turn-1",
                    status: RunTurnStatus::Ended,
                    elapsed_seconds: 5,
                    final_message_id: Some("message-1"),
                    error_fingerprint: None,
                },
            )
            .map_err(|error| error.to_string())?;

        let mut events = store
            .list_events("continuation-run", 0)
            .map_err(|error| error.to_string())?;
        let duplicates = events
            .iter()
            .filter(|event| {
                matches!(
                    event.event_type,
                    RuntimeEventKind::RunTurnStarted
                        | RuntimeEventKind::RunTurnUsageAccounted
                        | RuntimeEventKind::RunTurnCompacted
                        | RuntimeEventKind::RunTurnFinished
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        events.extend(duplicates);
        let continuation = fold_fixture_for_test(&events)
            .map_err(|error| error.to_string())?
            .continuation
            .ok_or_else(|| "continuation projection missing".to_string())?;
        assert!(continuation.active_turn.is_none());
        assert_eq!(continuation.tokens_used, 18);
        assert_eq!(continuation.time_used_seconds, 5);
        assert_eq!(continuation.compaction_count, 1);
        assert_eq!(continuation.next_turn_ordinal, 2);
        let last = continuation
            .last_turn
            .ok_or_else(|| "finished turn missing".to_string())?;
        assert_eq!(last.compaction_count, 1);
        assert_eq!(last.input_tokens, 11);
        assert_eq!(last.output_tokens, 7);
        Ok(())
    }

    /// Rebuild without RunCreated is an error (can't have a plan with no run).
    #[test]
    fn rebuild_without_run_created_is_error() {
        let events: Vec<RuntimeTaskEvent> = Vec::new();
        assert!(matches!(
            fold_fixture_for_test(&events),
            Err(RebuildError::NoRunCreated)
        ));
    }
}
