//! File-based task store read API (U1c phase-0/0b step 1).
//!
//! Read-side equivalent of `TaskRuntimeStore` backed by `events.jsonl` plus
//! rebuildable `plan.json` and `run-state.json` projections. A discardable
//! checkpoint accelerates projection refresh without replacing the event log.
//!
//! Runtime-only fields not on `PlanTask` (`owner_agent`/`started_at`/
//! `completed_at`/`summary` — the former `tr_todos` columns) are derived from
//! the `Task*` event payloads rather than stored on `PlanTask`, to avoid
//! touching the ts-rs contract.

use super::file_shadow::FileTaskShadow;
use super::types::{
    Artifact, EkoTaskExecution, PlanRevision, PlanTask, ReviewResult, RunStateSnapshot,
    RuntimeTaskEvent, TaskExecutionSummary, TaskPlan, TaskRun, TodoItem, TodoStatus,
};

/// File-backed read store. Cheap to clone (wraps a `FileTaskShadow`).
#[derive(Clone)]
pub(crate) struct FileTaskStore {
    shadow: FileTaskShadow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TaskRunLoadIssue {
    pub run_id: String,
    pub error: String,
}

#[derive(Debug, Default)]
pub(crate) struct TaskRunScan {
    pub runs: Vec<TaskRun>,
    pub issues: Vec<TaskRunLoadIssue>,
}

impl FileTaskStore {
    pub(crate) fn new(shadow: FileTaskShadow) -> Self {
        Self { shadow }
    }

    #[cfg(test)]
    pub(crate) fn from_root(root: impl Into<std::path::PathBuf>) -> Result<Self, FileReadError> {
        Ok(Self::new(FileTaskShadow::try_new(root)?))
    }

    fn read_run_state_resilient(
        &self,
        run_id: &str,
    ) -> Result<Option<RunStateSnapshot>, FileReadError> {
        self.shadow.ensure_projections_current(run_id)?;
        match self.shadow.read_run_state(run_id) {
            Ok(Some(state)) => Ok(Some(state)),
            Ok(None) | Err(super::file_shadow::ShadowError::Decode(_)) => {
                self.shadow.rewrite_plan(run_id)?;
                self.shadow.read_run_state(run_id).map_err(Into::into)
            }
            Err(error) => Err(error.into()),
        }
    }

    fn read_plan_resilient(&self, run_id: &str) -> Result<Option<PlanRevision>, FileReadError> {
        self.shadow.ensure_projections_current(run_id)?;
        match self.shadow.read_plan(run_id) {
            Ok(Some(plan)) => Ok(Some(plan)),
            Ok(None) | Err(super::file_shadow::ShadowError::Decode(_)) => {
                self.shadow.rewrite_plan(run_id)?;
                self.shadow.read_plan(run_id).map_err(Into::into)
            }
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) fn todo_query_projection(
        &self,
        run_id: &str,
    ) -> Result<Option<super::event_rebuild::TodoQueryProjection>, FileReadError> {
        self.shadow
            .read_todo_query_projection(run_id)
            .map_err(FileReadError::Shadow)
    }

    pub(crate) fn get_run(&self, run_id: &str) -> Result<Option<TaskRun>, FileReadError> {
        Ok(self
            .read_run_state_resilient(run_id)?
            .map(|state| state.run))
    }

    /// Read the checkpoint-backed runtime projection after validating and
    /// repairing its event suffix through the one shadow authority.
    pub(crate) fn get_run_state(
        &self,
        run_id: &str,
    ) -> Result<Option<RunStateSnapshot>, FileReadError> {
        self.read_run_state_resilient(run_id)
    }

    /// Enumerate every run under root, returning the run headers ordered by
    /// `created_at` descending (matching SQL `list_runs_in` ordering). Replaces
    /// `SELECT ... FROM tr_runs ORDER BY created_at DESC`.
    ///
    /// Each candidate must own a valid event authority; its checkpoint is
    /// recovered and only a missing journal tail is replayed. Cost is
    /// O(runs + missing tails), while the shadow LRU bounds retained journal
    /// handles and fold state after the scan.
    pub(crate) fn scan_runs(&self) -> Result<TaskRunScan, FileReadError> {
        let mut runs = Vec::new();
        let mut issues = Vec::new();
        for run_id in self.shadow.list_run_ids()? {
            match self.read_run_state_resilient(&run_id) {
                Ok(Some(state)) => runs.push(state.run),
                Ok(None) => {}
                Err(error) => issues.push(TaskRunLoadIssue {
                    run_id,
                    error: error.to_string(),
                }),
            }
        }
        // Descending by created_at (stable on ties, matching SQL behavior closely
        // enough for the run-list UI; exact tie-break is not load-bearing here).
        runs.sort_by_key(|a| std::cmp::Reverse(a.created_at));
        Ok(TaskRunScan { runs, issues })
    }

    pub(crate) fn list_runs(&self) -> Result<Vec<TaskRun>, FileReadError> {
        let scan = self.scan_runs()?;
        for issue in scan.issues {
            tracing::warn!(
                run_id = %issue.run_id,
                error = %issue.error,
                "isolating unreadable TaskRun from collection query"
            );
        }
        Ok(scan.runs)
    }

    /// Most recent run for a conversation (replaces
    /// `SELECT ... WHERE conversation_id = ? ORDER BY created_at DESC LIMIT 1`).
    pub(crate) fn latest_run_for_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<Option<TaskRun>, FileReadError> {
        Ok(self
            .list_runs()?
            .into_iter()
            .find(|r| r.conversation_id == conversation_id))
    }

    /// Find an in-progress (Running or Paused) run for a conversation, if any.
    /// Replaces `WHERE conversation_id = ? AND status IN ('running','paused')`.
    pub(crate) fn find_in_progress_run_by_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<Option<TaskRun>, FileReadError> {
        Ok(self.list_runs()?.into_iter().find(|r| {
            r.conversation_id == conversation_id
                && matches!(
                    r.status,
                    super::types::TaskRunStatus::Running | super::types::TaskRunStatus::Paused
                )
        }))
    }

    /// Runs whose status is in `statuses` (replaces
    /// `WHERE status IN (...)`). Empty `statuses` → empty result.
    pub(crate) fn list_runs_in(
        &self,
        statuses: &[super::types::TaskRunStatus],
    ) -> Result<Vec<TaskRun>, FileReadError> {
        if statuses.is_empty() {
            return Ok(Vec::new());
        }
        Ok(self
            .list_runs()?
            .into_iter()
            .filter(|r| statuses.contains(&r.status))
            .collect())
    }

    pub(crate) fn get_plan(&self, run_id: &str) -> Result<Option<TaskPlan>, FileReadError> {
        let Some(state) = self.read_run_state_resilient(run_id)? else {
            return Ok(None);
        };
        if state.run.plan_id.is_none() {
            return Ok(None);
        }
        let Some(plan) = self.read_plan_resilient(run_id)? else {
            return Ok(None);
        };
        let execution = state
            .tasks
            .into_iter()
            .map(|task| (task.task_id.clone(), task))
            .collect::<std::collections::HashMap<_, _>>();
        let tasks = plan
            .tasks
            .into_iter()
            .map(|spec| {
                let state = execution
                    .get(&spec.id)
                    .cloned()
                    .unwrap_or_else(|| EkoTaskExecution::pending(spec.id.clone()));
                PlanTask::from_parts(spec, state)
            })
            .collect();
        Ok(Some(TaskPlan {
            plan_id: plan.plan_id,
            run_id: plan.run_id,
            revision: plan.revision,
            domain_profile: plan.domain_profile,
            goal_revision: plan.goal_revision,
            goal_sha256: plan.goal_sha256,
            assumptions: plan.assumptions,
            risks: plan.risks,
            execution_mode: plan.execution_mode,
            tasks,
        }))
    }

    /// Derive the todo projection from the rebuilt plan + the runtime fields
    /// carried on `Task*` events (owner_agent/started_at/completed_at/summary).
    pub(crate) fn list_todos(&self, run_id: &str) -> Result<Vec<TodoItem>, FileReadError> {
        let Some(projected) = self.todo_query_projection(run_id)? else {
            return Ok(Vec::new());
        };
        let Some(plan) = projected.plan else {
            return Ok(Vec::new());
        };
        let execution = plan
            .tasks
            .iter()
            .map(|task| (task.id.as_str(), task))
            .collect::<std::collections::HashMap<_, _>>();
        let runtime_tasks = plan
            .tasks
            .iter()
            .map(|task| task.to_task().map_err(FileReadError::InvalidPlan))
            .collect::<Result<Vec<_>, _>>()?;
        let dependency_states = echo_agent::tasks::DagExecutionState::from_tasks(&runtime_tasks)
            .dependency_states(&runtime_tasks);
        let mut todos = Vec::with_capacity(plan.tasks.len());
        for task in &plan.tasks {
            let default_status = execution
                .get(task.id.as_str())
                .map(|task| task.status)
                .unwrap_or(TodoStatus::Pending);
            let dependency_block = dependency_states
                .get(&task.id)
                .and_then(|state| match state {
                    echo_agent::tasks::DagDependencyState::BlockedByFailure {
                        failed_ancestor_ids,
                    } => Some(failed_ancestor_ids),
                    _ => None,
                });
            let status = if default_status == TodoStatus::Pending && dependency_block.is_some() {
                TodoStatus::Blocked
            } else {
                default_status
            };
            let runtime = projected
                .todo_runtime
                .get(&task.id)
                .cloned()
                .unwrap_or_default();
            let summary = if default_status == TodoStatus::Pending {
                dependency_block.map_or(runtime.summary, |ancestor_ids| {
                    Some(format!(
                        "blocked by failed ancestor task(s): {}",
                        ancestor_ids.join(", ")
                    ))
                })
            } else {
                runtime.summary
            };
            todos.push(TodoItem {
                id: task.id.clone(),
                run_id: projected.run.run_id.clone(),
                task_id: task.id.clone(),
                title: task.title.clone(),
                // run-state.json is the authoritative execution projection.
                // Historical Task* events only supply fields that are not
                // stored in EkoTaskExecution; otherwise an earlier Blocked event
                // can overwrite a later plan skip/reset.
                status,
                owner_agent: runtime.owner_agent,
                started_at: runtime.started_at,
                completed_at: runtime.completed_at,
                summary,
            });
        }
        let sort_order = plan
            .tasks
            .iter()
            .map(|task| (task.id.as_str(), task.sort_order))
            .collect::<std::collections::HashMap<_, _>>();
        todos.sort_by_key(|todo| {
            sort_order
                .get(todo.task_id.as_str())
                .copied()
                .unwrap_or(i64::MAX)
        });
        Ok(todos)
    }

    pub(crate) fn list_events(
        &self,
        run_id: &str,
        since_seq: i64,
    ) -> Result<Vec<RuntimeTaskEvent>, FileReadError> {
        let after_sequence = u64::try_from(since_seq).unwrap_or_default();
        self.shadow
            .read_events_after(run_id, after_sequence)
            .map_err(FileReadError::Shadow)
    }

    /// Artifacts come from the journal-derived incremental history segment.
    /// Query cost is proportional to returned artifacts plus an uncheckpointed
    /// journal suffix, not the full journal length.
    pub(crate) fn list_artifacts(&self, run_id: &str) -> Result<Vec<Artifact>, FileReadError> {
        self.shadow
            .read_artifacts_projection(run_id)
            .map_err(FileReadError::Shadow)
    }

    /// Reviews come from the journal-derived segment for this stable task ID.
    pub(crate) fn list_reviews(
        &self,
        run_id: &str,
        task_id: &str,
    ) -> Result<Vec<ReviewResult>, FileReadError> {
        self.shadow
            .read_reviews_projection(run_id, task_id)
            .map_err(FileReadError::Shadow)
    }

    /// Summary: derive from Note{summary_persisted} events.
    pub(crate) fn get_summary(
        &self,
        run_id: &str,
        task_id: &str,
    ) -> Result<Option<TaskExecutionSummary>, FileReadError> {
        self.shadow
            .read_summary_projection(run_id, task_id)
            .map_err(FileReadError::Shadow)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FileReadError {
    #[error(transparent)]
    Shadow(#[from] super::file_shadow::ShadowError),
    #[error("invalid task graph: {0}")]
    InvalidPlan(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::task_runtime::store::TaskRuntimeStore;
    use crate::tasks::task_runtime::types::{
        AttendedMode, DomainProfile, ExecutionMode, PlanTask, PlanTaskKind, RuntimeEventKind,
        TaskPlan, TaskRunStatus, TaskUpdateOperation, TaskUpdateRequest,
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
            execution_target: None,
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

    #[test]
    fn missing_or_corrupt_snapshots_rebuild_from_event_authority() -> Result<(), String> {
        let tmp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(tmp.path())
            .map_err(|error| error.to_string())?;
        store
            .create_run(
                "rebuild-run",
                "ws",
                "conversation",
                "message",
                DomainProfile::General,
                "rebuild projections",
                "complex_runtime",
                AttendedMode::Unattended,
            )
            .map_err(|error| error.to_string())?;
        store
            .attach_plan_for_test(&TaskPlan {
                plan_id: "rebuild-plan".to_string(),
                run_id: "rebuild-run".to_string(),
                revision: 1,
                domain_profile: DomainProfile::General,
                goal_revision: 1,
                goal_sha256: crate::tasks::task_runtime::task_goal_sha256("rebuild projections"),
                assumptions: Vec::new(),
                risks: Vec::new(),
                execution_mode: ExecutionMode::Sequential,
                tasks: vec![task("rebuild-task", PlanTaskKind::Summary)],
            })
            .map_err(|error| error.to_string())?;
        let file = FileTaskStore::from_root(tmp.path()).map_err(|error| error.to_string())?;

        std::fs::remove_file(tmp.path().join("rebuild-run/run-state.json"))
            .map_err(|error| error.to_string())?;
        let run = file
            .get_run("rebuild-run")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "run did not rebuild".to_string())?;
        assert_eq!(run.goal, "rebuild projections");

        std::fs::write(tmp.path().join("rebuild-run/plan.json"), b"{partial")
            .map_err(|error| error.to_string())?;
        let plan = file
            .get_plan("rebuild-run")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "plan did not rebuild".to_string())?;
        assert_eq!(plan.plan_id, "rebuild-plan");
        assert_eq!(plan.tasks.len(), 1);

        let shadow = FileTaskShadow::new(tmp.path()).map_err(|error| error.to_string())?;
        shadow
            .append_event_line(
                "rebuild-run",
                None,
                None,
                RuntimeEventKind::RunStatusChanged,
                serde_json::json!({"from": "pending", "to": "running"}),
            )
            .map_err(|error| error.to_string())?;
        let recovered = file
            .get_run("rebuild-run")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "run did not recover the durable event tail".to_string())?;
        assert_eq!(recovered.status, TaskRunStatus::Running);
        Ok(())
    }

    /// FileTaskStore read API must match SQL read after a full lifecycle,
    /// including the TodoItem runtime fields derived from events.
    #[test]
    fn file_store_reads_match_sql() -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir().unwrap();
        let shadow = Arc::new(FileTaskShadow::new(tmp.path())?);
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(tmp.path()).unwrap();

        store
            .create_run(
                "r1",
                "ws",
                "c1",
                "m1",
                DomainProfile::AiCoding,
                "review",
                "complex",
                AttendedMode::Attended,
            )
            .unwrap();
        let plan = TaskPlan {
            plan_id: "p1".to_string(),
            run_id: "r1".to_string(),
            revision: 1,
            domain_profile: DomainProfile::AiCoding,
            goal_revision: 1,
            goal_sha256: crate::tasks::task_runtime::task_goal_sha256("review"),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Parallel,
            tasks: vec![task("t1", PlanTaskKind::ReadOnlyReview)],
        };
        store.attach_plan_for_test(&plan).unwrap();
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
        Ok(())
    }

    #[test]
    fn task_update_status_overrides_earlier_task_events() -> Result<(), Box<dyn std::error::Error>>
    {
        let tmp = tempfile::tempdir()?;
        let shadow = Arc::new(FileTaskShadow::new(tmp.path())?);
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(tmp.path())?;
        store.create_run(
            "r1",
            "ws",
            "c1",
            "m1",
            DomainProfile::General,
            "review",
            "complex",
            AttendedMode::Attended,
        )?;
        store.attach_plan_for_test(&TaskPlan {
            plan_id: "p1".to_string(),
            run_id: "r1".to_string(),
            revision: 1,
            domain_profile: DomainProfile::General,
            goal_revision: 1,
            goal_sha256: crate::tasks::task_runtime::task_goal_sha256("review"),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Sequential,
            tasks: vec![task("t1", PlanTaskKind::Investigation)],
        })?;
        store.set_task_status(
            "r1",
            "t1",
            TodoStatus::Blocked,
            Some("reviewer"),
            Some("review needs fix"),
        )?;
        store.apply_task_patch_for_test(
            "r1",
            &TaskUpdateRequest {
                base_revision: 1,
                reason: "result already incorporated".to_string(),
                operations: vec![TaskUpdateOperation::Skip {
                    task_id: "t1".to_string(),
                }],
            },
        )?;

        let todos = FileTaskStore::new((*shadow).clone()).list_todos("r1")?;
        let todo = todos
            .first()
            .ok_or_else(|| std::io::Error::other("todo t1 missing"))?;
        assert_eq!(todo.status, TodoStatus::Skipped);
        assert_eq!(todo.summary.as_deref(), Some("review needs fix"));
        Ok(())
    }

    // ── 0bc step-2: collection-query read API (replaces SQL WHERE/ORDER BY) ──

    /// `list_runs` enumerates every run directory under root and returns the run
    /// headers, ordered by created_at descending (matching SQL `list_runs_in`
    /// ordering). Drives a store with two runs and asserts both surface.
    #[test]
    fn list_runs_enumerates_all_runs_desc_by_created() -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir().unwrap();
        let shadow = Arc::new(FileTaskShadow::new(tmp.path())?);
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(tmp.path()).unwrap();
        store
            .create_run(
                "r1",
                "ws",
                "c1",
                "m1",
                DomainProfile::General,
                "g1",
                "",
                AttendedMode::Attended,
            )
            .unwrap();
        store
            .create_run(
                "r2",
                "ws",
                "c2",
                "m2",
                DomainProfile::General,
                "g2",
                "",
                AttendedMode::Attended,
            )
            .unwrap();

        let file = FileTaskStore::new((*shadow).clone());
        let runs = file.list_runs().unwrap();
        assert_eq!(runs.len(), 2);
        let ids: Vec<_> = runs.iter().map(|r| r.run_id.as_str()).collect();
        assert!(ids.contains(&"r1"));
        assert!(ids.contains(&"r2"));
        Ok(())
    }

    /// `latest_run_for_conversation` returns the most recent run for a
    /// conversation; `find_in_progress_run_by_conversation` returns one only
    /// if it is Running/Paused.
    #[test]
    fn conversation_queries_filter_and_order() -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir().unwrap();
        let shadow = Arc::new(FileTaskShadow::new(tmp.path())?);
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(tmp.path()).unwrap();
        store
            .create_run(
                "r1",
                "ws",
                "cX",
                "m1",
                DomainProfile::General,
                "g1",
                "",
                AttendedMode::Attended,
            )
            .unwrap();
        store
            .create_run(
                "r2",
                "ws",
                "cX",
                "m2",
                DomainProfile::General,
                "g2",
                "",
                AttendedMode::Attended,
            )
            .unwrap();
        // r1 Running, r2 Pending — latest is r2 (newer), in-progress is r1.
        store.transition_run("r1", TaskRunStatus::Running).unwrap();

        let file = FileTaskStore::new((*shadow).clone());
        let latest = file.latest_run_for_conversation("cX").unwrap();
        assert_eq!(latest.as_ref().map(|r| r.run_id.as_str()), Some("r2"));
        let in_prog = file.find_in_progress_run_by_conversation("cX").unwrap();
        assert_eq!(in_prog.as_ref().map(|r| r.run_id.as_str()), Some("r1"));
        // Different conversation → none.
        assert!(file.latest_run_for_conversation("other").unwrap().is_none());
        Ok(())
    }

    /// `list_runs_in` filters by status set.
    #[test]
    fn list_runs_in_filters_by_status() -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir().unwrap();
        let shadow = Arc::new(FileTaskShadow::new(tmp.path())?);
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(tmp.path()).unwrap();
        store
            .create_run(
                "r1",
                "ws",
                "c1",
                "m1",
                DomainProfile::General,
                "g1",
                "",
                AttendedMode::Attended,
            )
            .unwrap();
        store
            .create_run(
                "r2",
                "ws",
                "c2",
                "m2",
                DomainProfile::General,
                "g2",
                "",
                AttendedMode::Attended,
            )
            .unwrap();
        store.transition_run("r1", TaskRunStatus::Running).unwrap();
        store
            .transition_run("r1", TaskRunStatus::Completed)
            .unwrap();
        // r1 Completed, r2 Pending.

        let file = FileTaskStore::new((*shadow).clone());
        let completed = file.list_runs_in(&[TaskRunStatus::Completed]).unwrap();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].run_id, "r1");
        let pending = file.list_runs_in(&[TaskRunStatus::Pending]).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].run_id, "r2");
        Ok(())
    }

    #[test]
    fn scan_runs_isolates_unreadable_run_from_healthy_results()
    -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(tmp.path())?;
        store.create_run(
            "healthy",
            "ws",
            "conversation",
            "message",
            DomainProfile::General,
            "goal",
            "",
            AttendedMode::Attended,
        )?;
        let broken = tmp.path().join("broken");
        std::fs::create_dir_all(&broken)?;
        std::fs::write(broken.join("events.jsonl"), b"{not-json}\n")?;

        let scan = FileTaskStore::from_root(tmp.path())?.scan_runs()?;
        assert!(scan.runs.iter().any(|run| run.run_id == "healthy"));
        assert!(scan.issues.iter().any(|issue| issue.run_id == "broken"));
        Ok(())
    }

    #[test]
    fn dependency_block_is_derived_and_disappears_after_ancestor_retry()
    -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let store = TaskRuntimeStore::new_in_memory_with_shadow_root(tmp.path())?;
        store.create_run(
            "derived-block",
            "ws",
            "conversation",
            "message",
            DomainProfile::AiCoding,
            "derive dependency state",
            "task",
            AttendedMode::Attended,
        )?;
        let upstream = task("upstream", PlanTaskKind::ReadOnlyReview);
        let mut downstream = task("downstream", PlanTaskKind::Summary);
        downstream.depends_on = vec![upstream.id.clone()];
        store.attach_plan_for_test(&TaskPlan {
            plan_id: "derived-block-plan".to_string(),
            run_id: "derived-block".to_string(),
            revision: 1,
            domain_profile: DomainProfile::AiCoding,
            goal_revision: 1,
            goal_sha256: crate::tasks::task_runtime::task_goal_sha256("derive dependency state"),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Sequential,
            tasks: vec![upstream, downstream],
        })?;
        store.transition_run("derived-block", TaskRunStatus::Running)?;
        store.set_task_status(
            "derived-block",
            "upstream",
            TodoStatus::Failed,
            None,
            Some("upstream failed"),
        )?;
        store.transition_run("derived-block", TaskRunStatus::Failed)?;

        let file = FileTaskStore::from_root(tmp.path())?;
        let blocked = file.list_todos("derived-block")?;
        assert_eq!(
            blocked
                .iter()
                .find(|todo| todo.task_id == "downstream")
                .map(|todo| todo.status),
            Some(TodoStatus::Blocked)
        );
        store.retry_blocked_task("derived-block", "upstream")?;
        let unblocked = file.list_todos("derived-block")?;
        assert_eq!(
            unblocked
                .iter()
                .find(|todo| todo.task_id == "downstream")
                .map(|todo| todo.status),
            Some(TodoStatus::Pending)
        );
        Ok(())
    }
}
