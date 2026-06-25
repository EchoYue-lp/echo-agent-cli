//! File-based task store read API (U1c phase-0/0b step 1).
//!
//! Read-side equivalent of `TaskRuntimeStore` backed by `plan.json` +
//! `events.jsonl` (the file mirror produced by `FileTaskShadow`). In 0b this
//! replaces SQL as the read authority; SQL stays as a write mirror until 0c
//! retires it.
//!
//! Runtime-only fields not on `PlanTask` (`owner_agent`/`started_at`/
//! `completed_at`/`summary` — the former `tr_todos` columns) are derived from
//! the `Task*` event payloads rather than stored on `PlanTask`, to avoid
//! touching the ts-rs contract.

use chrono::{DateTime, Utc};

use super::event_rebuild::RebuiltPlan;
use super::file_shadow::FileTaskShadow;
use super::types::{
    Artifact, ReviewResult, RuntimeEventKind, RuntimeTaskEvent, TaskExecutionSummary, TaskPlan,
    TaskRun, TodoItem, TodoStatus,
};

/// File-backed read store. Cheap to clone (wraps a `FileTaskShadow`).
#[derive(Clone)]
pub struct FileTaskStore {
    shadow: FileTaskShadow,
}

impl FileTaskStore {
    pub fn new(shadow: FileTaskShadow) -> Self {
        Self { shadow }
    }

    pub fn from_root(root: impl Into<std::path::PathBuf>) -> Self {
        Self::new(FileTaskShadow::new(root))
    }

    fn load(&self, run_id: &str) -> Result<Option<Loaded>, FileReadError> {
        let plan = self
            .shadow
            .read_plan(run_id)
            .map_err(FileReadError::Shadow)?;
        let events = self
            .shadow
            .read_events(run_id)
            .map_err(FileReadError::Shadow)?;
        Ok(plan.map(|plan| Loaded { plan, events }))
    }

    pub fn get_run(&self, run_id: &str) -> Result<Option<TaskRun>, FileReadError> {
        Ok(self.load(run_id)?.map(|l| l.plan.run))
    }

    pub fn get_plan(&self, run_id: &str) -> Result<Option<TaskPlan>, FileReadError> {
        Ok(self.load(run_id)?.map(|l| {
            // RebuiltPlan.plan has tasks=empty; attach the rebuilt tasks so the
            // caller gets the full plan with its tasks (matching SQL get_plan).
            let mut p = l.plan.plan;
            p.tasks = l.plan.tasks.clone();
            p
        }))
    }

    /// Derive the todo projection from the rebuilt plan + the runtime fields
    /// carried on `Task*` events (owner_agent/started_at/completed_at/summary).
    pub fn list_todos(&self, run_id: &str) -> Result<Vec<TodoItem>, FileReadError> {
        let Some(loaded) = self.load(run_id)? else {
            return Ok(Vec::new());
        };
        let mut todos = Vec::with_capacity(loaded.plan.tasks.len());
        for t in &loaded.plan.tasks {
            // Fold this task's Task* events to recover the 4 runtime fields.
            let (owner, started, completed, summary, status) =
                fold_task_runtime(&loaded.events, &t.id, t.status);
            todos.push(TodoItem {
                id: t.id.clone(),
                run_id: loaded.plan.run.run_id.clone(),
                task_id: t.id.clone(),
                title: t.title.clone(),
                status,
                owner_agent: owner,
                started_at: started,
                completed_at: completed,
                summary,
            });
        }
        // Sort by sort_order to match SQL's display ordering.
        todos.sort_by_key(|t| {
            loaded
                .plan
                .tasks
                .iter()
                .position(|p| p.id == t.task_id)
                .map(|i| i as i64)
                .unwrap_or(i64::MAX)
        });
        Ok(todos)
    }

    pub fn list_events(
        &self,
        run_id: &str,
        since_seq: i64,
    ) -> Result<Vec<RuntimeTaskEvent>, FileReadError> {
        let events = self
            .shadow
            .read_events(run_id)
            .map_err(FileReadError::Shadow)?;
        Ok(events.into_iter().filter(|e| e.seq > since_seq).collect())
    }

    /// Artifacts: derive from `ArtifactProduced` events. (0c audit may revise.)
    pub fn list_artifacts(&self, run_id: &str) -> Result<Vec<Artifact>, FileReadError> {
        let events = self
            .shadow
            .read_events(run_id)
            .map_err(FileReadError::Shadow)?;
        Ok(events
            .iter()
            .filter(|e| e.event_type == RuntimeEventKind::ArtifactProduced)
            .filter_map(|e| {
                let p = &e.payload;
                Some(Artifact {
                    id: p.get("artifact_id")?.as_str()?.to_string(),
                    run_id: e.run_id.clone(),
                    task_id: e.task_id.clone(),
                    kind: super::types::ArtifactKind::from_str(
                        p.get("kind").and_then(|v| v.as_str())?,
                    )
                    .unwrap_or(super::types::ArtifactKind::File),
                    title: p.get("title")?.as_str()?.to_string(),
                    path: None, // not carried on the event today
                    metadata: serde_json::Value::Null,
                })
            })
            .collect())
    }

    /// Reviews: derive from ReviewPassed/NeedsFix/Blocked events. (0c audit may revise.)
    pub fn list_reviews(&self, run_id: &str) -> Result<Vec<ReviewResult>, FileReadError> {
        let events = self
            .shadow
            .read_events(run_id)
            .map_err(FileReadError::Shadow)?;
        Ok(events
            .iter()
            .filter(|e| {
                matches!(
                    e.event_type,
                    RuntimeEventKind::ReviewPassed
                        | RuntimeEventKind::ReviewNeedsFix
                        | RuntimeEventKind::ReviewBlocked
                )
            })
            .filter_map(|e| {
                let p = &e.payload;
                Some(ReviewResult {
                    id: p.get("review_id")?.as_str()?.to_string(),
                    run_id: e.run_id.clone(),
                    reviewer_agent: p.get("reviewer")?.as_str()?.to_string(),
                    outcome: match e.event_type {
                        RuntimeEventKind::ReviewPassed => super::types::ReviewOutcome::Pass,
                        RuntimeEventKind::ReviewNeedsFix => super::types::ReviewOutcome::NeedsFix,
                        _ => super::types::ReviewOutcome::Blocked,
                    },
                    issues: Vec::new(),
                    failure_fingerprint: None,
                    created_fix_task_id: None,
                    created_at: e.timestamp,
                    task_id: e.task_id.clone().unwrap_or_default(),
                })
            })
            .collect())
    }

    /// Summary: derive from Note{summary_persisted} events.
    pub fn get_summary(
        &self,
        run_id: &str,
        task_id: &str,
    ) -> Result<Option<TaskExecutionSummary>, FileReadError> {
        let events = self
            .shadow
            .read_events(run_id)
            .map_err(FileReadError::Shadow)?;
        Ok(events
            .iter()
            .rfind(|e| {
                e.event_type == RuntimeEventKind::Note
                    && e.task_id.as_deref() == Some(task_id)
                    && e.payload.get("kind").and_then(|v| v.as_str()) == Some("summary_persisted")
            })
            .and_then(|e| e.payload.get("summary"))
            .and_then(|v| serde_json::from_value::<TaskExecutionSummary>(v.clone()).ok()))
    }
}

struct Loaded {
    plan: RebuiltPlan,
    events: Vec<RuntimeTaskEvent>,
}

/// Fold a task's `Task*`/`TodoUpdated` events to recover the 4 tr_todos runtime
/// fields. Returns (owner_agent, started_at, completed_at, summary, status),
/// carrying forward the last non-None value of each (matching tr_todos semantics).
#[allow(clippy::type_complexity)] // 5-tuple of runtime fields; factoring a struct adds noise here
fn fold_task_runtime(
    events: &[RuntimeTaskEvent],
    task_id: &str,
    default_status: TodoStatus,
) -> (
    Option<String>,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
    Option<String>,
    TodoStatus,
) {
    let mut owner = None;
    let mut started = None;
    let mut completed = None;
    let mut summary = None;
    let mut status = default_status;
    for e in events {
        if e.task_id.as_deref() != Some(task_id) {
            continue;
        }
        if !matches!(
            e.event_type,
            RuntimeEventKind::TaskStarted
                | RuntimeEventKind::TaskCompleted
                | RuntimeEventKind::TaskFailed
                | RuntimeEventKind::TaskSkipped
                | RuntimeEventKind::TaskBlocked
                | RuntimeEventKind::TodoUpdated
        ) {
            continue;
        }
        if let Some(o) = e.payload.get("owner_agent").and_then(|v| v.as_str())
            && !o.is_empty()
        {
            owner = Some(o.to_string());
        }
        if let Some(s) = e
            .payload
            .get("started_at")
            .and_then(|v| v.as_str())
            .and_then(parse_rfc3339)
        {
            started = Some(s);
        }
        if let Some(c) = e
            .payload
            .get("completed_at")
            .and_then(|v| v.as_str())
            .and_then(parse_rfc3339)
        {
            completed = Some(c);
        }
        if let Some(s) = e.payload.get("summary").and_then(|v| v.as_str())
            && !s.is_empty()
        {
            summary = Some(s.to_string());
        }
        if let Some(s) = e
            .payload
            .get("status")
            .and_then(|v| v.as_str())
            .and_then(TodoStatus::from_str)
        {
            status = s;
        }
    }
    (owner, started, completed, summary, status)
}

fn parse_rfc3339(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

#[derive(Debug, thiserror::Error)]
pub enum FileReadError {
    #[error(transparent)]
    Shadow(#[from] super::file_shadow::ShadowError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::task_runtime::store::TaskRuntimeStore;
    use crate::tasks::task_runtime::types::{
        DomainProfile, ExecutionMode, PlanTask, PlanTaskKind, TaskPlan,
    };
    use std::sync::Arc;

    fn task(id: &str, kind: PlanTaskKind) -> PlanTask {
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

    /// FileTaskStore read API must match SQL read after a full lifecycle,
    /// including the TodoItem runtime fields derived from events.
    #[test]
    fn file_store_reads_match_sql() {
        let tmp = tempfile::tempdir().unwrap();
        let shadow = Arc::new(FileTaskShadow::new(tmp.path()));
        let mut store = TaskRuntimeStore::new_in_memory().unwrap();
        store.attach_shadow(shadow.clone());

        store
            .create_run(
                "r1",
                "ws",
                "c1",
                "m1",
                DomainProfile::AiCoding,
                "review",
                "complex",
            )
            .unwrap();
        let plan = TaskPlan {
            plan_id: "p1".to_string(),
            run_id: "r1".to_string(),
            domain_profile: DomainProfile::AiCoding,
            goal: "review".to_string(),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Parallel,
            tasks: vec![task("t1", PlanTaskKind::ReadOnlyReview)],
        };
        store.attach_plan(&plan).unwrap();
        store
            .set_task_status(
                "r1",
                "t1",
                TodoStatus::Running,
                Some("explorer"),
                Some("starting"),
            )
            .unwrap();
        store
            .set_task_status(
                "r1",
                "t1",
                TodoStatus::Completed,
                Some("explorer"),
                Some("done"),
            )
            .unwrap();

        let file = FileTaskStore::new((*shadow).clone());

        // get_run
        let sql_run = store.get_run("r1").unwrap().unwrap();
        let file_run = file.get_run("r1").unwrap().unwrap();
        assert_eq!(file_run.run_id, sql_run.run_id);
        assert_eq!(file_run.goal, sql_run.goal);
        assert_eq!(file_run.route, sql_run.route);

        // get_plan
        let sql_plan = store.get_plan("r1").unwrap().unwrap();
        let file_plan = file.get_plan("r1").unwrap().unwrap();
        assert_eq!(file_plan.plan_id, sql_plan.plan_id);
        assert_eq!(file_plan.tasks.len(), sql_plan.tasks.len());
        assert_eq!(file_plan.tasks[0].id, sql_plan.tasks[0].id);

        // list_todos — the 4 runtime fields must be derived correctly.
        let sql_todos = store.list_todos("r1").unwrap();
        let file_todos = file.list_todos("r1").unwrap();
        assert_eq!(sql_todos.len(), file_todos.len());
        let st = &sql_todos[0];
        let ft = &file_todos[0];
        assert_eq!(ft.title, st.title);
        assert_eq!(ft.status, st.status);
        assert_eq!(ft.status, TodoStatus::Completed);
        assert_eq!(ft.owner_agent, st.owner_agent);
        assert_eq!(ft.owner_agent.as_deref(), Some("explorer"));
        assert_eq!(ft.summary, st.summary);
        assert_eq!(ft.summary.as_deref(), Some("done"));
        assert!(ft.started_at.is_some());
        assert!(ft.completed_at.is_some());

        // list_events
        let sql_ev = store.list_events("r1", 0).unwrap();
        let file_ev = file.list_events("r1", 0).unwrap();
        assert_eq!(sql_ev.len(), file_ev.len());
    }
}
