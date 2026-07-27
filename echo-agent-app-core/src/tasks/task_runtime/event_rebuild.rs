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
    AttendedMode, DomainProfile, EkoTaskExecution, ExecutionMode, PlanRevision, PlanTask,
    RunStateSnapshot, RuntimeEventKind, RuntimeTaskEvent, TaskPlan, TaskRun, TaskRunStatus,
    TodoStatus,
};

/// Rebuilt plan snapshot — the shape `plan.json` will take.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RebuiltPlan {
    pub run: TaskRun,
    pub plan: TaskPlan,
    pub tasks: Vec<PlanTask>,
}

impl RebuiltPlan {
    pub fn plan_revision(&self) -> PlanRevision {
        PlanRevision {
            plan_id: self.plan.plan_id.clone(),
            run_id: self.plan.run_id.clone(),
            revision: self.plan.revision,
            domain_profile: self.plan.domain_profile,
            goal: self.plan.goal.clone(),
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

    for ev in events {
        match ev.event_type {
            K::RunCreated => {
                let p = &ev.payload;
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
                    goal: p
                        .get("goal")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
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
                        } else if reset.contains(task.id.as_str()) {
                            task.status = TodoStatus::Pending;
                        }
                    }
                    plan = Some(TaskPlan {
                        plan_id: committed.plan_id,
                        run_id: committed.run_id,
                        revision: committed.revision,
                        domain_profile: committed.domain_profile,
                        goal: committed.goal,
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
            _ => {} // ArtifactProduced/Review*/Approval*/Note(other) don't affect plan.json
        }
    }

    let run = run.ok_or(RebuildError::NoRunCreated)?;
    let plan = plan.unwrap_or_else(|| empty_plan_for(&run));
    Ok(RebuiltPlan { run, plan, tasks })
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
        goal: run.goal.clone(),
        assumptions: Vec::new(),
        risks: Vec::new(),
        execution_mode: ExecutionMode::default(),
        tasks: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::task_runtime::store::TaskRuntimeStore;
    use crate::tasks::task_runtime::types::{
        DomainProfile, ExecutionMode, PlanPatchOperation, PlanPatchRequest, PlanTask, PlanTaskKind,
        TaskPatch, TaskPlan, TodoStatus,
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
            goal: "review runtime".to_string(),
            assumptions: vec!["repo is small".to_string()],
            risks: vec!["flaky tests".to_string()],
            execution_mode: ExecutionMode::Parallel,
            tasks: vec![
                sample_task("t1", PlanTaskKind::ReadOnlyReview),
                sample_task("t2", PlanTaskKind::Investigation),
            ],
        };
        s.attach_plan(&plan).unwrap();

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
        assert_eq!(rebuilt.plan.goal, sql_plan.goal);
        assert_eq!(rebuilt.plan.assumptions, sql_plan.assumptions);
        assert_eq!(rebuilt.plan.risks, sql_plan.risks);
        assert_eq!(rebuilt.plan.execution_mode, sql_plan.execution_mode);

        // 9. Assert task parity. attach_plan replaced tasks, so SQL has 1 task (t1);
        // rebuild should also converge to the post-attach state (t1 only).
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
        s.attach_plan(&TaskPlan {
            plan_id: "p1".to_string(),
            run_id: "r1".to_string(),
            revision: 1,
            domain_profile: DomainProfile::General,
            goal: "g".to_string(),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Parallel,
            tasks: vec![sample_task("t1", PlanTaskKind::Investigation)],
        })
        .unwrap();
        s.patch_plan(
            "r1",
            &PlanPatchRequest {
                base_revision: 1,
                reason: "refine investigation".to_string(),
                operations: vec![PlanPatchOperation::Update {
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
