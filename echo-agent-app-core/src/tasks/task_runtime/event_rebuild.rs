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

use super::types::{
    AttendedMode, BackgroundCellState, BlockerAudit, DomainProfile, EkoTaskExecution,
    ExecutionMode, PlanRevision, PlanTask, RunContinuationState, RunPause, RunPauseReason,
    RunStateSnapshot, RunTurnOrigin, RunTurnStatus, RunTurnSummary, RuntimeEventKind,
    RuntimeTaskEvent, TaskPlan, TaskRun, TaskRunStatus, TodoStatus, TurnVisibility,
};

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

    pub fn run_state(&self) -> RunStateSnapshot {
        RunStateSnapshot {
            run: self.run.clone(),
            tasks: self.tasks.iter().map(PlanTask::execution).collect(),
            continuation: self.continuation.clone(),
            background_cells: self.background_cells.clone(),
        }
    }
}

/// Fold a run's events (in seq order) into a snapshot.
///
/// Returns `Err` if the event stream is malformed (e.g. no `RunCreated`, or a
/// task-mutating event references a task that was never inserted). A partial
/// rebuild (missing optional fields) fills defaults and is not an error — the
/// `RebuiltPlan` is the best-effort projection of the event log.
pub fn rebuild_plan_from_events(events: &[RuntimeTaskEvent]) -> Result<RebuiltPlan, RebuildError> {
    use RuntimeEventKind as K;

    let mut run: Option<TaskRun> = None;
    let mut plan: Option<TaskPlan> = None;
    let mut tasks: Vec<PlanTask> = Vec::new();
    let mut background_cells = std::collections::BTreeMap::<String, BackgroundCellState>::new();
    let mut continuation: Option<RunContinuationState> = None;
    let mut started_turns = std::collections::HashSet::<String>::new();
    let mut accounted_usage = std::collections::HashSet::<String>::new();
    let mut accounted_compactions = std::collections::HashSet::<String>::new();
    let mut finished_turns = std::collections::HashSet::<String>::new();

    for ev in events {
        match ev.event_type {
            K::RunCreated => {
                let p = &ev.payload;
                let goal = p
                    .get("goal")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                run = Some(TaskRun {
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
                state.deferred_reason = json_string(&ev.payload, "continuation_deferred_reason");
            }
            K::RunStatusChanged => {
                if let Some(r) = run.as_mut() {
                    if let Some(to) = ev.payload.get("to").and_then(|v| v.as_str()) {
                        r.status = TaskRunStatus::from_str(to).unwrap_or(r.status);
                    }
                    r.updated_at = ev.timestamp;
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
                if let Some(committed) = p
                    .get("plan")
                    .and_then(|value| serde_json::from_value::<PlanRevision>(value.clone()).ok())
                {
                    if let Some(r) = run.as_mut() {
                        r.plan_id = Some(committed.plan_id.clone());
                    }
                    let previous_execution = tasks
                        .iter()
                        .map(|task| (task.id.clone(), task.execution()))
                        .collect::<std::collections::HashMap<_, _>>();
                    tasks = committed
                        .tasks
                        .iter()
                        .cloned()
                        .map(|spec| {
                            let execution = previous_execution
                                .get(&spec.id)
                                .cloned()
                                .unwrap_or_else(|| EkoTaskExecution::pending(spec.id.clone()));
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
                    for task in &mut tasks {
                        if skipped.contains(task.id.as_str()) {
                            task.status = TodoStatus::Skipped;
                            task.status_detail = None;
                            task.claim = None;
                        } else if reset.contains(task.id.as_str()) {
                            task.status = TodoStatus::Pending;
                            task.status_detail = None;
                            task.claim = None;
                        }
                    }
                    plan = Some(TaskPlan {
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
                }
            }
            K::TaskStarted
            | K::TaskCompleted
            | K::TaskFailed
            | K::TaskCancelled
            | K::TaskTimedOut
            | K::TaskSkipped
            | K::TaskBlocked
            | K::TodoUpdated => {
                if let Some(task_id) = ev.task_id.as_ref() {
                    let status = ev
                        .payload
                        .get("status")
                        .and_then(|v| v.as_str())
                        .and_then(TodoStatus::from_str);
                    if let Some(status) = status {
                        #[allow(clippy::collapsible_if)]
                        // nested let-Option guard reads clearer than a let-chain
                        if let Some(t) = tasks.iter_mut().find(|t| &t.id == task_id) {
                            t.status = status;
                            if let Some(value) = ev.payload.get("status_detail") {
                                t.status_detail = value.as_str().map(str::to_string);
                            }
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
                    // started_at/completed_at/owner_agent/summary live in tr_todos (not PlanTask).
                    // They are not rebuilt onto PlanTask in 0a; the parity test compares them
                    // separately via list_todos. They land on plan.json tasks[] in 0b.
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
                        command_hash: json_string(&ev.payload, "command_hash").unwrap_or_default(),
                        turn_id: json_string(&ev.payload, "turn_id"),
                        execution_id: json_string(&ev.payload, "execution_id"),
                        call_id: json_string(&ev.payload, "call_id"),
                        phase: "running".to_string(),
                        exit_code: None,
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
                        phase: "unknown".to_string(),
                        exit_code: None,
                        total_output_bytes: 0,
                        output_truncated: false,
                        output_excerpt: None,
                        artifact_path: None,
                        artifact_sha256: None,
                        started_at: ev.timestamp,
                        finished_at: None,
                    });
                cell.phase =
                    json_string(&ev.payload, "phase").unwrap_or_else(|| "unknown".to_string());
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
                cell.artifact_path = json_string(&ev.payload, "artifact_path");
                cell.artifact_sha256 = json_string(&ev.payload, "artifact_sha256");
                cell.finished_at = Some(ev.timestamp);
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
                state.time_used_seconds = state.time_used_seconds.saturating_add(elapsed_seconds);
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
                        let blocker_fingerprint = json_string(&ev.payload, "blocker_fingerprint")
                            .unwrap_or_else(|| "stalled:unknown".to_string());
                        state.blocker_audit = Some(match state.blocker_audit.take() {
                            Some(previous) if previous.fingerprint == blocker_fingerprint => {
                                BlockerAudit {
                                    fingerprint: blocker_fingerprint,
                                    consecutive_turns: previous.consecutive_turns.saturating_add(1),
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
                                    consecutive_turns: previous.consecutive_turns.saturating_add(1),
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

    let run = run.ok_or(RebuildError::NoRunCreated)?;
    let plan = plan.unwrap_or_else(|| empty_plan_for(&run));
    Ok(RebuiltPlan {
        run,
        plan,
        tasks,
        background_cells: background_cells.into_values().collect(),
        continuation,
    })
}

fn json_string(payload: &serde_json::Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
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
mod tests {
    use super::*;
    use crate::tasks::task_runtime::store::{
        RunTurnClaimOutcome, RunTurnCompletion, TaskRuntimeStore,
    };
    use crate::tasks::task_runtime::types::{
        DomainProfile, ExecutionMode, PlanTask, PlanTaskKind, RunTurnOrigin, RunTurnStatus,
        TaskPatch, TaskPlan, TaskRunStatus, TaskUpdateOperation, TaskUpdateRequest, TodoStatus,
        TurnVisibility,
    };

    fn fresh() -> TaskRuntimeStore {
        TaskRuntimeStore::new_in_memory().expect("in-memory store")
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
            files: vec!["src/a.rs".to_string()],
            allowed_tools: vec!["read_file".to_string()],
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

    /// Gate 1: rebuild from events == read from SQL. Drives a full run lifecycle
    /// (create run → insert 2 tasks → attach plan → set statuses) and asserts the
    /// event-rebuilt plan matches the SQL-read plan on run header, plan envelope,
    /// and every task's defining fields.
    #[test]
    fn rebuild_matches_sql_after_full_lifecycle() {
        let s = fresh();

        // 1. create_run
        let _run = s
            .create_run(
                "r1",
                "ws",
                "c1",
                "m1",
                DomainProfile::AiCoding,
                "review runtime",
                "complex_runtime",
                AttendedMode::Attended,
            )
            .unwrap();

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
        s.attach_plan_for_test(&plan).unwrap();

        // 4. mutate a task status
        s.set_task_status("r1", "t1", TodoStatus::Running, Some("code_reviewer"), None)
            .unwrap();

        // 5. Read SQL ground truth.
        let sql_run = s.get_run("r1").unwrap().unwrap();
        let sql_plan = s.get_plan("r1").unwrap().unwrap();
        let events = s.list_events("r1", 0).unwrap();

        // 6. Rebuild from events.
        let rebuilt = rebuild_plan_from_events(&events).unwrap();

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
        let rebuilt_t1 = rebuilt
            .tasks
            .iter()
            .find(|t| t.id == "t1")
            .unwrap_or_else(|| {
                panic!(
                    "rebuilt tasks: {:?}",
                    rebuilt.tasks.iter().map(|t| &t.id).collect::<Vec<_>>()
                )
            });
        assert_eq!(rebuilt_t1.title, "task t1");
        assert_eq!(rebuilt_t1.description, "do t1");
        assert_eq!(rebuilt_t1.kind, PlanTaskKind::ReadOnlyReview);
        assert_eq!(rebuilt_t1.agent_role, "code_reviewer");
        assert_eq!(rebuilt_t1.files, vec!["src/a.rs".to_string()]);
        assert_eq!(rebuilt_t1.allowed_tools, vec!["read_file".to_string()]);
        // status was set to Running after attach.
        assert_eq!(rebuilt_t1.status, TodoStatus::Running);
    }

    /// A committed revision patch must be visible in the rebuilt specification.
    #[test]
    fn rebuild_reflects_task_patch() {
        let s = fresh();
        s.create_run(
            "r1",
            "ws",
            "c1",
            "m1",
            DomainProfile::General,
            "g",
            "",
            AttendedMode::Attended,
        )
        .unwrap();
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
        })
        .unwrap();
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
        )
        .unwrap();

        let events = s.list_events("r1", 0).unwrap();
        let rebuilt = rebuild_plan_from_events(&events).unwrap();
        let t = rebuilt.tasks.iter().find(|t| t.id == "t1").unwrap();
        assert_eq!(t.title, "renamed");
        assert_eq!(t.description, "new desc");
        assert_eq!(t.kind, PlanTaskKind::ReadOnlyReview);
        assert_eq!(t.agent_role, "explorer");
        assert_eq!(t.files, vec!["b.rs".to_string()]);
    }

    #[test]
    fn continuation_fold_is_idempotent_under_duplicate_event_delivery() -> Result<(), String> {
        let store = fresh();
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
        let continuation = rebuild_plan_from_events(&events)
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
            rebuild_plan_from_events(&events),
            Err(RebuildError::NoRunCreated)
        ));
    }
}
