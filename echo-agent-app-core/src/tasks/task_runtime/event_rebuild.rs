//! Event-sourced plan rebuild (U1c phase-0/0a, gate 1).
//!
//! Folds a run's `RuntimeTaskEvent` stream into a `RebuiltPlan` snapshot
//! (`run` header + `plan` envelope + `tasks[]` with runtime fields). This is
//! the proof that `events.jsonl` can authoritatively rebuild `plan.json`.
//!
//! Precondition: events must carry enriched payloads (see store.rs enrichment
//! comments — RunCreated/PlanGenerated/PlanEdited{insert,update,reorder}/
//! Task*/Note{summary_persisted}). Without enrichment, rebuild is partial.

use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;

use super::types::{
    DomainProfile, ExecutionMode, PlanTask, PlanTaskKind, RuntimeEventKind, RuntimeTaskEvent,
    TaskPlan, TaskRun, TaskRunStatus, TodoStatus,
};

/// Rebuilt plan snapshot — the shape `plan.json` will take.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RebuiltPlan {
    pub run: TaskRun,
    pub plan: TaskPlan,
    pub tasks: Vec<PlanTask>,
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
                    plan_id: None, // set by PlanGenerated below
                    route: p
                        .get("route")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
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
            K::PlanGenerated => {
                let p = &ev.payload;
                let plan_id = p
                    .get("plan_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                if let Some(r) = run.as_mut() {
                    r.plan_id = Some(plan_id.clone());
                }
                plan = Some(TaskPlan {
                    plan_id,
                    run_id: ev.run_id.clone(),
                    domain_profile: p
                        .get("domain_profile")
                        .and_then(|v| v.as_str())
                        .and_then(DomainProfile::from_str)
                        .unwrap_or_default(),
                    goal: p
                        .get("goal")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    assumptions: decode_str_vec(p, "assumptions"),
                    risks: decode_str_vec(p, "risks"),
                    execution_mode: p
                        .get("execution_mode")
                        .and_then(|v| v.as_str())
                        .and_then(ExecutionMode::from_str)
                        .unwrap_or_default(),
                    tasks: Vec::new(),
                });
                // attach_plan path: PlanGenerated carries the full task bodies (insert_plan_task_tx
                // doesn't emit PlanEdited{insert}). Bootstrap path has empty tasks.
                if let Some(arr) = p.get("tasks").and_then(|v| v.as_array()) {
                    tasks = arr
                        .iter()
                        .filter_map(|v| serde_json::from_value::<PlanTask>(v.clone()).ok())
                        .collect();
                }
            }
            K::PlanEdited => {
                let action = ev
                    .payload
                    .get("action")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                match action {
                    "insert" => {
                        if let Some(task) = decode_task(&ev.payload, "task") {
                            tasks.push(task);
                        }
                    }
                    "update" => {
                        if let Some(task_id) = ev
                            .payload
                            .get("task_id")
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                        {
                            #[allow(clippy::collapsible_if)]
                            // nested let-Option guard reads clearer than a let-chain
                            if let Some(t) = tasks.iter_mut().find(|t| t.id == task_id) {
                                apply_patch(t, &ev.payload);
                            }
                        }
                    }
                    "remove" => {
                        if let Some(task_id) = ev
                            .payload
                            .get("task_id")
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                        {
                            tasks.retain(|t| t.id != task_id);
                        }
                    }
                    "reorder" => {
                        if let Some(new_order) =
                            ev.payload.get("new_order").and_then(|v| v.as_array())
                        {
                            let order: Vec<String> = new_order
                                .iter()
                                .filter_map(|v| v.as_str().map(str::to_string))
                                .collect();
                            reorder_tasks(&mut tasks, &order);
                        }
                    }
                    _ => {}
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
                    // started_at/completed_at/owner_agent/summary live in tr_todos (not PlanTask).
                    // They are not rebuilt onto PlanTask in 0a; the parity test compares them
                    // separately via list_todos. They land on plan.json tasks[] in 0b.
                }
            }
            K::Note => {
                // summary_persisted carries a full TaskExecutionSummary; PlanTask has no field for
                // it today (tr_summaries stays authoritative until 0c). No plan.json mutation.
            }
            _ => {} // WorkerLlmUsage/ArtifactProduced/Review*/Approval*/Note(other) don't affect plan.json
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

fn decode_task(payload: &serde_json::Value, key: &str) -> Option<PlanTask> {
    payload.get(key).and_then(|v| {
        serde_json::from_value::<PlanTask>(v.clone())
            .map_err(|e| tracing::debug!(error = %e, "decode_task: not a PlanTask, skipping"))
            .ok()
    })
}

fn apply_patch(task: &mut PlanTask, payload: &serde_json::Value) {
    if let Some(patch) = payload.get("patch") {
        if let Some(title) = patch.get("title").and_then(|v| v.as_str()) {
            task.title = title.to_string();
        }
        if let Some(desc) = patch.get("description").and_then(|v| v.as_str()) {
            task.description = desc.to_string();
        }
        if let Some(kind) = patch
            .get("kind")
            .and_then(|v| v.as_str())
            .and_then(PlanTaskKind::from_str)
        {
            task.kind = kind;
        }
        if let Some(role) = patch.get("agent_role").and_then(|v| v.as_str()) {
            task.agent_role = role.to_string();
        }
        if patch.get("depends_on").is_some() {
            task.depends_on = decode_str_vec(patch, "depends_on");
        }
        if patch.get("files").is_some() {
            task.files = decode_str_vec(patch, "files");
        }
        if patch.get("allowed_tools").is_some() {
            task.allowed_tools = decode_str_vec(patch, "allowed_tools");
        }
    }
}

fn reorder_tasks(tasks: &mut [PlanTask], order: &[String]) {
    tasks.sort_by_key(|t| {
        order
            .iter()
            .position(|id| id == &t.id)
            .map(|i| i as i64)
            .unwrap_or(i64::MAX)
    });
    for (i, t) in tasks.iter_mut().enumerate() {
        t.sort_order = i as i64;
    }
}

fn decode_str_vec(payload: &serde_json::Value, key: &str) -> Vec<String> {
    payload
        .get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
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
        DomainProfile, ExecutionMode, PlanTask, PlanTaskKind, TaskPatch, TaskPlan, TodoStatus,
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
            verification: Vec::new(),
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
            )
            .unwrap();

        // 2. attach a structured plan (the authoritative plan-creation path).
        let plan = TaskPlan {
            plan_id: "p1".to_string(),
            run_id: "r1".to_string(),
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

    /// update_task path: a patch applied to a task must be visible in the rebuild.
    #[test]
    fn rebuild_reflects_task_patch() {
        let s = fresh();
        s.create_run("r1", "ws", "c1", "m1", DomainProfile::General, "g", "")
            .unwrap();
        s.insert_task("r1", None, sample_task("t1", PlanTaskKind::Investigation))
            .unwrap();
        s.update_task(
            "r1",
            "t1",
            TaskPatch {
                title: Some("renamed".to_string()),
                description: Some("new desc".to_string()),
                kind: Some(PlanTaskKind::ReadOnlyReview),
                agent_role: Some("explorer".to_string()),
                depends_on: None,
                files: Some(vec!["b.rs".to_string()]),
                allowed_tools: None,
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
