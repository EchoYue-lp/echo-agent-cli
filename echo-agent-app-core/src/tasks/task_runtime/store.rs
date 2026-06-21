//! SQLite-backed canonical store for the TaskRuntime.
//!
//! Design constraints (from the plan):
//!
//! - SQLite is the **single source of truth**. JSON/Markdown exports under
//!   `.eko/runtime/{run_id}/` are derived from these tables, never the
//!   canonical state.
//! - Every state mutation must append a [`RuntimeTaskEvent`] **inside the
//!   same transaction** as the state update. This module enforces that by
//!   routing all writes through transaction-scoped helpers.
//!
//! This mirrors the `SessionSearchEngine` pattern (`Mutex<Connection>` +
//! `init_schema` + `new_in_memory`) so it composes with the rest of the
//! app's rusqlite usage.

use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::Context;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, Row, params, params_from_iter, types::Value as SqlValue};

use super::types::*;

/// Error returned by store operations. Kept separate from `anyhow::Error`
/// so callers can distinguish invariant violations (e.g. illegal status
/// transition) from infrastructure failures.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("run not found: {0}")]
    RunNotFound(String),
    #[error("plan not found for run: {0}")]
    PlanNotFound(String),
    #[error("task not found: {0}")]
    TaskNotFound(String),
    #[error("illegal transition {from} -> {to} for run {run_id}")]
    IllegalTransition {
        run_id: String,
        from: String,
        to: String,
    },
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("lock poisoned")]
    LockPoisoned,
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

/// Canonical TaskRuntime store. One instance per process; cheap to clone
/// behind `Arc`. The connection is single-writer (`Mutex`), which is fine
/// because TaskRuntime writes are low-frequency state transitions, not
/// hot-path tool calls.
pub struct TaskRuntimeStore {
    conn: Mutex<Connection>,
}

impl TaskRuntimeStore {
    /// Open (or create) the store at the default location:
    /// `~/.echo-agent/task_runtime.db`.
    pub fn new() -> anyhow::Result<Self> {
        Self::open(&default_db_path())
    }

    /// Open (or create) the store at an explicit path. Used for
    /// workspace-scoped databases and tests.
    pub fn open(path: &PathBuf) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating db parent dir {}", parent.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening task_runtime db at {}", path.display()))?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.init_schema()?;
        Ok(store)
    }

    /// In-memory store for tests / fallback. Schema is identical.
    pub fn new_in_memory() -> anyhow::Result<Self> {
        let conn = Connection::open_in_memory()?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.init_schema()?;
        Ok(store)
    }

    // ── Schema ──────────────────────────────────────────────────────────

    fn init_schema(&self) -> anyhow::Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        conn.execute_batch(
            // NOTE: every table uses TEXT PRIMARY KEY ids (UUIDs from the
            // caller). Status / kind columns store the `as_str()` discriminator
            // for human-readable SQL and stable cross-version decoding.
            "
            CREATE TABLE IF NOT EXISTS tr_runs (
                run_id            TEXT PRIMARY KEY,
                workspace_id      TEXT NOT NULL,
                conversation_id   TEXT NOT NULL,
                root_message_id   TEXT NOT NULL DEFAULT '',
                domain_profile    TEXT NOT NULL DEFAULT 'general',
                status            TEXT NOT NULL DEFAULT 'pending',
                goal              TEXT NOT NULL DEFAULT '',
                plan_id           TEXT,
                created_at        TEXT NOT NULL,
                updated_at        TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS ix_runs_conv ON tr_runs(conversation_id);
            CREATE INDEX IF NOT EXISTS ix_runs_workspace ON tr_runs(workspace_id);
            CREATE INDEX IF NOT EXISTS ix_runs_status ON tr_runs(status);

            CREATE TABLE IF NOT EXISTS tr_plans (
                plan_id           TEXT PRIMARY KEY,
                run_id            TEXT NOT NULL,
                domain_profile    TEXT NOT NULL DEFAULT 'general',
                goal              TEXT NOT NULL DEFAULT '',
                assumptions       TEXT NOT NULL DEFAULT '[]',
                risks             TEXT NOT NULL DEFAULT '[]',
                execution_mode    TEXT NOT NULL DEFAULT 'parallel',
                created_at        TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS ix_plans_run ON tr_plans(run_id);

            CREATE TABLE IF NOT EXISTS tr_plan_tasks (
                id                TEXT PRIMARY KEY,
                plan_id           TEXT NOT NULL,
                run_id            TEXT NOT NULL,
                title             TEXT NOT NULL DEFAULT '',
                description       TEXT NOT NULL DEFAULT '',
                kind              TEXT NOT NULL DEFAULT 'read_only_review',
                agent_role        TEXT NOT NULL DEFAULT 'general',
                domain_profile    TEXT NOT NULL DEFAULT 'general',
                depends_on        TEXT NOT NULL DEFAULT '[]',
                parallel_group    TEXT,
                files             TEXT NOT NULL DEFAULT '[]',
                allowed_tools     TEXT NOT NULL DEFAULT '[]',
                verification      TEXT NOT NULL DEFAULT '[]',
                retry_count       INTEGER NOT NULL DEFAULT 0,
                max_retries       INTEGER NOT NULL DEFAULT 3,
                failure_fingerprint TEXT,
                status            TEXT NOT NULL DEFAULT 'pending'
            );
            CREATE INDEX IF NOT EXISTS ix_tasks_plan ON tr_plan_tasks(plan_id);
            CREATE INDEX IF NOT EXISTS ix_tasks_run  ON tr_plan_tasks(run_id);
            CREATE INDEX IF NOT EXISTS ix_tasks_status ON tr_plan_tasks(status);

            CREATE TABLE IF NOT EXISTS tr_todos (
                id                TEXT PRIMARY KEY,
                run_id            TEXT NOT NULL,
                task_id           TEXT NOT NULL,
                title             TEXT NOT NULL DEFAULT '',
                status            TEXT NOT NULL DEFAULT 'pending',
                owner_agent       TEXT,
                started_at        TEXT,
                completed_at      TEXT,
                summary           TEXT
            );
            CREATE INDEX IF NOT EXISTS ix_todos_run ON tr_todos(run_id);

            CREATE TABLE IF NOT EXISTS tr_events (
                seq               INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id            TEXT NOT NULL,
                task_id           TEXT,
                step_id           TEXT,
                event_type        TEXT NOT NULL,
                payload           TEXT NOT NULL DEFAULT '{}',
                timestamp         TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS ix_events_run ON tr_events(run_id, seq);

            CREATE TABLE IF NOT EXISTS tr_artifacts (
                id                TEXT PRIMARY KEY,
                run_id            TEXT NOT NULL,
                task_id           TEXT,
                kind              TEXT NOT NULL DEFAULT 'other',
                title             TEXT NOT NULL DEFAULT '',
                path              TEXT,
                metadata          TEXT NOT NULL DEFAULT '{}'
            );
            CREATE INDEX IF NOT EXISTS ix_artifacts_run ON tr_artifacts(run_id);
            CREATE INDEX IF NOT EXISTS ix_artifacts_task ON tr_artifacts(task_id);

            CREATE TABLE IF NOT EXISTS tr_reviews (
                id                TEXT PRIMARY KEY,
                run_id            TEXT NOT NULL,
                task_id           TEXT NOT NULL,
                reviewer_agent    TEXT NOT NULL DEFAULT '',
                outcome           TEXT NOT NULL DEFAULT 'pass',
                issues            TEXT NOT NULL DEFAULT '[]',
                failure_fingerprint TEXT,
                created_fix_task_id TEXT,
                created_at        TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS ix_reviews_task ON tr_reviews(task_id);

            CREATE TABLE IF NOT EXISTS tr_summaries (
                run_id            TEXT NOT NULL,
                task_id           TEXT NOT NULL,
                worker_agent      TEXT NOT NULL DEFAULT '',
                completed_work    TEXT NOT NULL DEFAULT '[]',
                files_read        TEXT NOT NULL DEFAULT '[]',
                files_changed     TEXT NOT NULL DEFAULT '[]',
                decisions         TEXT NOT NULL DEFAULT '[]',
                failures          TEXT NOT NULL DEFAULT '[]',
                verification      TEXT NOT NULL DEFAULT '[]',
                next_implications TEXT NOT NULL DEFAULT '[]',
                created_at        TEXT NOT NULL,
                PRIMARY KEY (run_id, task_id)
            );

            CREATE TABLE IF NOT EXISTS tr_approvals (
                run_id            TEXT NOT NULL,
                tool_name         TEXT NOT NULL,
                scope_level       TEXT NOT NULL DEFAULT 'conversation',
                conversation_id   TEXT NOT NULL,
                created_at        TEXT NOT NULL,
                PRIMARY KEY (run_id, tool_name, conversation_id)
            );
            CREATE INDEX IF NOT EXISTS ix_approvals_lookup
                ON tr_approvals(run_id, conversation_id, tool_name);

            CREATE TABLE IF NOT EXISTS tr_usage_records (
                id                            TEXT PRIMARY KEY,
                session_id                    TEXT NOT NULL,
                run_id                        TEXT,
                worker_id                     TEXT,
                model                         TEXT NOT NULL,
                provider                      TEXT,
                route_kind                    TEXT,
                input_tokens                  INTEGER NOT NULL DEFAULT 0,
                output_tokens                 INTEGER NOT NULL DEFAULT 0,
                cached_input_tokens           INTEGER NOT NULL DEFAULT 0,
                cache_creation_input_tokens   INTEGER NOT NULL DEFAULT 0,
                usage_reported                INTEGER NOT NULL DEFAULT 1,
                system_prompt_hash            TEXT,
                tools_schema_hash             TEXT,
                cwd_hash                      TEXT,
                worker_prompt_hash            TEXT,
                created_at                    TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS ix_usage_model ON tr_usage_records(model);
            CREATE INDEX IF NOT EXISTS ix_usage_run ON tr_usage_records(run_id);
            CREATE INDEX IF NOT EXISTS ix_usage_created ON tr_usage_records(created_at);
            CREATE INDEX IF NOT EXISTS ix_usage_route ON tr_usage_records(route_kind);
            CREATE INDEX IF NOT EXISTS ix_usage_session ON tr_usage_records(session_id);

            CREATE TABLE IF NOT EXISTS tr_conversation_events (
                seq               INTEGER PRIMARY KEY AUTOINCREMENT,
                conversation_id   TEXT NOT NULL,
                event_type        TEXT NOT NULL,
                payload           TEXT NOT NULL DEFAULT '{}',
                timestamp         TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS ix_conv_events
                ON tr_conversation_events(conversation_id, seq);
            ",
        )?;
        Ok(())
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, StoreError> {
        self.conn.lock().map_err(|_| StoreError::LockPoisoned)
    }

    // ── Runs ────────────────────────────────────────────────────────────

    /// Create a new run in `Pending` and emit `RunCreated`. Returns the
    /// created run.
    pub fn create_run(
        &self,
        run_id: &str,
        workspace_id: &str,
        conversation_id: &str,
        root_message_id: &str,
        domain_profile: DomainProfile,
        goal: &str,
    ) -> Result<TaskRun, StoreError> {
        let now = Utc::now();
        let run = TaskRun {
            run_id: run_id.to_string(),
            workspace_id: workspace_id.to_string(),
            conversation_id: conversation_id.to_string(),
            root_message_id: root_message_id.to_string(),
            domain_profile,
            status: TaskRunStatus::Pending,
            goal: goal.to_string(),
            plan_id: None,
            created_at: now,
            updated_at: now,
        };

        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO tr_runs
                (run_id, workspace_id, conversation_id, root_message_id, domain_profile,
                 status, goal, plan_id, created_at, updated_at)
             VALUES (?,?,?,?,?, 'pending', ?, NULL, ?, ?)",
            params![
                run.run_id,
                run.workspace_id,
                run.conversation_id,
                run.root_message_id,
                domain_profile.as_str(),
                run.goal,
                run.created_at.to_rfc3339(),
                run.updated_at.to_rfc3339(),
            ],
        )?;
        append_event_tx(
            &tx,
            run.run_id.as_str(),
            None,
            None,
            RuntimeEventKind::RunCreated,
            serde_json::json!({ "goal": goal, "domain_profile": domain_profile.as_str() }),
        )?;
        tx.commit()?;
        Ok(run)
    }

    /// Atomically transition a run to `next` and append `RunStatusChanged`.
    /// Rejects illegal transitions (see [`TaskRunStatus::can_transition_to`]).
    pub fn transition_run(&self, run_id: &str, next: TaskRunStatus) -> Result<TaskRun, StoreError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;

        let (current_str, mut run) = load_run_for_update(&tx, run_id)?;
        let current = TaskRunStatus::from_str(&current_str).unwrap_or(TaskRunStatus::Pending);
        if !current.can_transition_to(next) {
            return Err(StoreError::IllegalTransition {
                run_id: run_id.to_string(),
                from: current.as_str().to_string(),
                to: next.as_str().to_string(),
            });
        }

        let now = Utc::now();
        let now_str = now.to_rfc3339();
        run.status = next;
        run.updated_at = now;
        tx.execute(
            "UPDATE tr_runs SET status = ?, updated_at = ? WHERE run_id = ?",
            params![next.as_str(), now_str, run_id],
        )?;
        append_event_tx(
            &tx,
            run_id,
            None,
            None,
            RuntimeEventKind::RunStatusChanged,
            serde_json::json!({ "from": current.as_str(), "to": next.as_str() }),
        )?;
        // A run moving to Cancelled is significant enough to deserve its own
        // event kind for consumers that filter on it.
        if next == TaskRunStatus::Cancelled {
            append_event_tx(
                &tx,
                run_id,
                None,
                None,
                RuntimeEventKind::RunCancelled,
                serde_json::json!({}),
            )?;
        }
        tx.commit()?;
        Ok(run)
    }

    /// Attach a generated plan to a run, replacing any prior plan, and
    /// transition the run to `AwaitingPlanApproval` (from `Planning`).
    /// All plan tasks and their todo rows are inserted in the same tx.
    pub fn attach_plan(&self, plan: &TaskPlan) -> Result<(), StoreError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;

        // Upsert the plan row.
        tx.execute(
            "INSERT INTO tr_plans
                (plan_id, run_id, domain_profile, goal, assumptions, risks,
                 execution_mode, created_at)
             VALUES (?,?,?,?,?,?,?,?)
             ON CONFLICT(plan_id) DO UPDATE SET
                goal=excluded.goal,
                assumptions=excluded.assumptions,
                risks=excluded.risks,
                execution_mode=excluded.execution_mode",
            params![
                plan.plan_id,
                plan.run_id,
                plan.domain_profile.as_str(),
                plan.goal,
                serde_json::to_string(&plan.assumptions)?,
                serde_json::to_string(&plan.risks)?,
                match plan.execution_mode {
                    ExecutionMode::Sequential => "sequential",
                    ExecutionMode::Parallel => "parallel",
                    ExecutionMode::PlanOnly => "plan_only",
                },
                Utc::now().to_rfc3339(),
            ],
        )?;

        // Replace all plan-task rows for this plan atomically.
        tx.execute(
            "DELETE FROM tr_plan_tasks WHERE plan_id = ?",
            params![plan.plan_id],
        )?;
        tx.execute(
            "DELETE FROM tr_todos WHERE run_id = ?",
            params![plan.run_id],
        )?;

        for t in &plan.tasks {
            insert_plan_task_tx(&tx, &plan.plan_id, &plan.run_id, t)?;
            // Mirror each task into the todo projection.
            tx.execute(
                "INSERT INTO tr_todos
                    (id, run_id, task_id, title, status, owner_agent,
                     started_at, completed_at, summary)
                 VALUES (?,?,?,?,'pending',NULL,NULL,NULL,NULL)",
                params![t.id, plan.run_id, t.id, t.title],
            )?;
        }

        // Link run -> plan and advance to AwaitingPlanApproval. This goes
        // through the state-machine path: read current status, validate the
        // transition is legal (Planning → AwaitingPlanApproval, or
        // AwaitingPlanApproval → AwaitingPlanApproval when re-editing a plan),
        // and emit RunStatusChanged so the event log stays consistent with the
        // "SQLite state machine is the single source of truth" invariant.
        let (current_status_str, _run) = load_run_for_update(&tx, &plan.run_id)?;
        let current =
            TaskRunStatus::from_str(&current_status_str).unwrap_or(TaskRunStatus::Pending);
        // Allowed entry states for attach_plan: Planning (first generation)
        // and AwaitingPlanApproval (re-edit of an existing plan before
        // approval). Anything else (e.g. Running) is a bug — refuse rather
        // than silently stamp a status.
        if !matches!(
            current,
            TaskRunStatus::Planning | TaskRunStatus::AwaitingPlanApproval
        ) {
            return Err(StoreError::IllegalTransition {
                run_id: plan.run_id.clone(),
                from: current.as_str().to_string(),
                to: TaskRunStatus::AwaitingPlanApproval.as_str().to_string(),
            });
        }
        let now = Utc::now().to_rfc3339();
        tx.execute(
            "UPDATE tr_runs SET plan_id = ?, updated_at = ?, status = 'awaiting_plan_approval'
             WHERE run_id = ?",
            params![plan.plan_id, now, plan.run_id],
        )?;
        // Only emit RunStatusChanged when the status actually changes —
        // re-editing a plan (Already AwaitingPlanApproval) shouldn't log a
        // no-op transition, but PlanEdited captures the edit.
        if current != TaskRunStatus::AwaitingPlanApproval {
            append_event_tx(
                &tx,
                plan.run_id.as_str(),
                None,
                None,
                RuntimeEventKind::RunStatusChanged,
                serde_json::json!({
                    "from": current.as_str(),
                    "to": TaskRunStatus::AwaitingPlanApproval.as_str(),
                }),
            )?;
        }
        append_event_tx(
            &tx,
            plan.run_id.as_str(),
            None,
            None,
            RuntimeEventKind::PlanGenerated,
            serde_json::json!({ "plan_id": plan.plan_id, "task_count": plan.tasks.len() }),
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Record that the user approved/rejected/edited the plan.
    pub fn resolve_plan(
        &self,
        run_id: &str,
        approved: bool,
        note: Option<&str>,
    ) -> Result<(), StoreError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        append_event_tx(
            &tx,
            run_id,
            None,
            None,
            if approved {
                RuntimeEventKind::PlanApproved
            } else {
                RuntimeEventKind::PlanRejected
            },
            serde_json::json!({ "note": note.unwrap_or("") }),
        )?;
        tx.commit()?;
        Ok(())
    }

    // ── Task / todo mutations ───────────────────────────────────────────

    /// Update a plan task's status and its mirrored todo row, emitting a
    /// kind-appropriate event. Used by the scheduler (PR 3) and review
    /// gates (PR 4).
    pub fn set_task_status(
        &self,
        run_id: &str,
        task_id: &str,
        status: TodoStatus,
        owner_agent: Option<&str>,
        summary: Option<&str>,
    ) -> Result<(), StoreError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let now = Utc::now().to_rfc3339();

        tx.execute(
            "UPDATE tr_plan_tasks SET status = ? WHERE id = ?",
            params![status.as_str(), task_id],
        )?;
        if tx.changes() == 0 {
            return Err(StoreError::TaskNotFound(task_id.to_string()));
        }

        // Keep the todo projection in lock-step.
        let started = matches!(status, TodoStatus::Running);
        let finished = matches!(
            status,
            TodoStatus::Completed | TodoStatus::Failed | TodoStatus::Skipped
        );
        tx.execute(
            "UPDATE tr_todos SET status = ?, owner_agent = ?,
                started_at = CASE WHEN ? THEN ? ELSE started_at END,
                completed_at = CASE WHEN ? THEN ? ELSE completed_at END,
                summary = ?
             WHERE run_id = ? AND task_id = ?",
            params![
                status.as_str(),
                owner_agent,
                started,
                if started { Some(&now) } else { None },
                finished,
                if finished { Some(&now) } else { None },
                summary,
                run_id,
                task_id
            ],
        )?;

        let kind = match status {
            TodoStatus::Running => RuntimeEventKind::TaskStarted,
            TodoStatus::Completed => RuntimeEventKind::TaskCompleted,
            TodoStatus::Failed => RuntimeEventKind::TaskFailed,
            TodoStatus::Skipped => RuntimeEventKind::TaskSkipped,
            TodoStatus::Blocked => RuntimeEventKind::TaskBlocked,
            TodoStatus::Pending => RuntimeEventKind::TodoUpdated,
        };
        append_event_tx(
            &tx,
            run_id,
            Some(task_id),
            None,
            kind,
            serde_json::json!({
                "status": status.as_str(),
                "owner_agent": owner_agent,
                "summary": summary,
            }),
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Update a plan task's mutable fields (title, description, retry_count,
    /// failure_fingerprint, status) in place. Used by the review gate when a
    /// NeedsFix outcome produces a fix variant of a task — the fix shape must
    /// be persisted so a process restart doesn't lose retry progress or the
    /// review-informed brief. The task id is unchanged so downstream
    /// depends_on keeps resolving. Emits a `Note` event for traceability.
    pub fn update_plan_task(&self, run_id: &str, task: &PlanTask) -> Result<(), StoreError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE tr_plan_tasks SET
                title = ?, description = ?, retry_count = ?,
                failure_fingerprint = ?, status = ?
             WHERE run_id = ? AND id = ?",
            params![
                task.title,
                task.description,
                task.retry_count,
                task.failure_fingerprint,
                task.status.as_str(),
                run_id,
                task.id,
            ],
        )?;
        if tx.changes() == 0 {
            return Err(StoreError::TaskNotFound(task.id.clone()));
        }
        // Keep the todo title in sync (it shows in the GUI).
        tx.execute(
            "UPDATE tr_todos SET title = ?, status = ? WHERE run_id = ? AND task_id = ?",
            params![task.title, task.status.as_str(), run_id, task.id],
        )?;
        append_event_tx(
            &tx,
            run_id,
            Some(task.id.as_str()),
            None,
            RuntimeEventKind::Note,
            serde_json::json!({
                "kind": "fix_task_persisted",
                "retry_count": task.retry_count,
                "failure_fingerprint": task.failure_fingerprint,
            }),
        )?;
        tx.commit()?;
        Ok(())
    }

    // ── Reviews, artifacts, summaries ───────────────────────────────────

    pub fn add_review(&self, r: &ReviewResult) -> Result<(), StoreError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO tr_reviews
                (id, run_id, task_id, reviewer_agent, outcome, issues,
                 failure_fingerprint, created_fix_task_id, created_at)
             VALUES (?,?,?,?,?,?,?,?,?)",
            params![
                r.id,
                r.run_id,
                r.task_id,
                r.reviewer_agent,
                r.outcome.as_str(),
                serde_json::to_string(&r.issues)?,
                r.failure_fingerprint,
                r.created_fix_task_id,
                r.created_at.to_rfc3339(),
            ],
        )?;
        let kind = match r.outcome {
            ReviewOutcome::Pass => RuntimeEventKind::ReviewPassed,
            ReviewOutcome::NeedsFix => RuntimeEventKind::ReviewNeedsFix,
            ReviewOutcome::Blocked => RuntimeEventKind::ReviewBlocked,
        };
        append_event_tx(
            &tx,
            r.run_id.as_str(),
            Some(r.task_id.as_str()),
            None,
            kind,
            serde_json::json!({ "review_id": r.id, "reviewer": r.reviewer_agent }),
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn add_artifact(&self, a: &Artifact) -> Result<(), StoreError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO tr_artifacts
                (id, run_id, task_id, kind, title, path, metadata)
             VALUES (?,?,?,?,?,?,?)",
            params![
                a.id,
                a.run_id,
                a.task_id,
                a.kind.as_str(),
                a.title,
                a.path,
                a.metadata.to_string(),
            ],
        )?;
        append_event_tx(
            &tx,
            a.run_id.as_str(),
            a.task_id.as_deref(),
            None,
            RuntimeEventKind::ArtifactProduced,
            serde_json::json!({ "artifact_id": a.id, "kind": a.kind.as_str(), "title": a.title }),
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Persist or overwrite the per-task execution summary. Primary key is
    /// `(run_id, task_id)` so a re-execution replaces the prior summary. The
    /// write is transactional and appends a `Note` event so the GUI and the
    /// recovery path can tell when a summary was updated (consistent with the
    /// "every state-relevant change writes a TaskEvent" invariant).
    pub fn put_summary(&self, s: &TaskExecutionSummary) -> Result<(), StoreError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO tr_summaries
                (run_id, task_id, worker_agent, completed_work, files_read,
                 files_changed, decisions, failures, verification,
                 next_implications, created_at)
             VALUES (?,?,?,?,?,?,?,?,?,?,?)
             ON CONFLICT(run_id, task_id) DO UPDATE SET
                worker_agent=excluded.worker_agent,
                completed_work=excluded.completed_work,
                files_read=excluded.files_read,
                files_changed=excluded.files_changed,
                decisions=excluded.decisions,
                failures=excluded.failures,
                verification=excluded.verification,
                next_implications=excluded.next_implications,
                created_at=excluded.created_at",
            params![
                s.run_id,
                s.task_id,
                s.worker_agent,
                serde_json::to_string(&s.completed_work)?,
                serde_json::to_string(&s.files_read)?,
                serde_json::to_string(&s.files_changed)?,
                serde_json::to_string(&s.decisions)?,
                serde_json::to_string(&s.failures)?,
                serde_json::to_string(&s.verification)?,
                serde_json::to_string(&s.next_implications)?,
                s.created_at.to_rfc3339(),
            ],
        )?;
        append_event_tx(
            &tx,
            s.run_id.as_str(),
            Some(s.task_id.as_str()),
            None,
            RuntimeEventKind::Note,
            serde_json::json!({
                "kind": "summary_persisted",
                "worker_agent": s.worker_agent,
                "files_changed": s.files_changed.len(),
            }),
        )?;
        tx.commit()?;
        Ok(())
    }

    // ── Read paths (used by Tauri query commands + recovery) ────────────

    pub fn get_run(&self, run_id: &str) -> Result<Option<TaskRun>, StoreError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT run_id, workspace_id, conversation_id, root_message_id,
                    domain_profile, status, goal, plan_id, created_at, updated_at
             FROM tr_runs WHERE run_id = ?",
        )?;
        let mut rows = stmt.query(params![run_id])?;
        match rows.next()? {
            Some(row) => Ok(Some(decode_run(&row)?)),
            None => Ok(None),
        }
    }

    /// Latest run for a conversation (used by GUI to bind a chat to its run).
    pub fn latest_run_for_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<Option<TaskRun>, StoreError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT run_id, workspace_id, conversation_id, root_message_id,
                    domain_profile, status, goal, plan_id, created_at, updated_at
             FROM tr_runs WHERE conversation_id = ?
             ORDER BY created_at DESC LIMIT 1",
        )?;
        let mut rows = stmt.query(params![conversation_id])?;
        match rows.next()? {
            Some(row) => Ok(Some(decode_run(&row)?)),
            None => Ok(None),
        }
    }

    pub fn list_runs_in(&self, statuses: &[TaskRunStatus]) -> Result<Vec<TaskRun>, StoreError> {
        let conn = self.lock()?;
        if statuses.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders: Vec<String> = (0..statuses.len()).map(|_| "?".to_string()).collect();
        let sql = format!(
            "SELECT run_id, workspace_id, conversation_id, root_message_id,
                    domain_profile, status, goal, plan_id, created_at, updated_at
             FROM tr_runs WHERE status IN ({})
             ORDER BY created_at DESC",
            placeholders.join(", ")
        );
        let vals: Vec<SqlValue> = statuses
            .iter()
            .map(|s| SqlValue::Text(s.as_str().to_string()))
            .collect();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(vals.iter()), decode_run)?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    /// Boot-time recovery of runs interrupted by a process restart (P1-8).
    ///
    /// A run left in an active state (Running / WaitingApproval / WaitingInput /
    /// Suspended / Cancelling) when the process died has no driver to finish it
    /// — it is a zombie that blocks the run list and can never complete. The
    /// per-run lazy recovery in `run_dag` only fires when *that specific run*
    /// is next executed, so a zombie that is never revisited stays forever.
    ///
    /// This scans all interrupted runs once at startup and transitions each to
    /// `Failed` (with a note), matching the lazy-recovery outcome but applied
    /// proactively. Pending / Planning / AwaitingPlanApproval / Ready are left
    /// untouched: they had not begun executing, so they are not zombies and may
    /// be resumed by the caller. Returns the number of runs recovered.
    ///
    /// Safe to call on an empty/fresh store (no-op).
    pub fn recover_incomplete(&self) -> usize {
        const INTERRUPTED: &[TaskRunStatus] = &[
            TaskRunStatus::Running,
            TaskRunStatus::WaitingApproval,
            TaskRunStatus::WaitingInput,
            TaskRunStatus::Suspended,
            TaskRunStatus::Cancelling,
        ];
        let zombies = match self.list_runs_in(INTERRUPTED) {
            Ok(z) => z,
            Err(e) => {
                tracing::warn!(error = %e, "recover_incomplete: failed to list interrupted runs");
                return 0;
            }
        };
        let count = zombies.len();
        for run in &zombies {
            let reason = format!(
                "recovered from {} (interrupted by process restart)",
                run.status.as_str()
            );
            if let Err(e) = self.note(&run.run_id, None, &reason) {
                tracing::warn!(
                    run_id = %run.run_id,
                    error = %e,
                    "recover_incomplete: failed to note recovery"
                );
            }
            match self.transition_run(&run.run_id, TaskRunStatus::Failed) {
                Ok(_) => tracing::info!(
                    run_id = %run.run_id,
                    from = %run.status.as_str(),
                    "recovered interrupted run → Failed at boot"
                ),
                Err(StoreError::IllegalTransition { from, .. }) => {
                    // State changed concurrently between list and transition —
                    // not an error, just skip this run.
                    tracing::debug!(
                        run_id = %run.run_id,
                        from,
                        "recover_incomplete: run no longer in interrupted state, skipped"
                    );
                }
                Err(e) => tracing::warn!(
                    run_id = %run.run_id,
                    error = %e,
                    "recover_incomplete: failed to transition run to Failed"
                ),
            }
        }
        count
    }

    pub fn get_plan(&self, run_id: &str) -> Result<Option<TaskPlan>, StoreError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT plan_id, run_id, domain_profile, goal, assumptions, risks, execution_mode
             FROM tr_plans WHERE run_id = ? ORDER BY rowid DESC LIMIT 1",
        )?;
        let mut rows = stmt.query(params![run_id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let plan_id: String = row.get(0)?;
        let domain_profile = DomainProfile::from_str(&row.get::<_, String>(2)?).unwrap_or_default();
        let goal: String = row.get(3)?;
        let assumptions: Vec<String> = serde_json::from_str(&row.get::<_, String>(4)?)?;
        let risks: Vec<String> = serde_json::from_str(&row.get::<_, String>(5)?)?;
        let execution_mode = match row.get::<_, String>(6)?.as_str() {
            "sequential" => ExecutionMode::Sequential,
            "plan_only" => ExecutionMode::PlanOnly,
            _ => ExecutionMode::Parallel,
        };

        let tasks = load_plan_tasks(&conn, &plan_id)?;
        Ok(Some(TaskPlan {
            plan_id,
            run_id: run_id.to_string(),
            domain_profile,
            goal,
            assumptions,
            risks,
            execution_mode,
            tasks,
        }))
    }

    pub fn list_todos(&self, run_id: &str) -> Result<Vec<TodoItem>, StoreError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, run_id, task_id, title, status, owner_agent,
                    started_at, completed_at, summary
             FROM tr_todos WHERE run_id = ? ORDER BY rowid ASC",
        )?;
        let rows = stmt.query_map(params![run_id], |row| {
            Ok(TodoItem {
                id: row.get(0)?,
                run_id: row.get(1)?,
                task_id: row.get(2)?,
                title: row.get(3)?,
                status: TodoStatus::from_str(&row.get::<_, String>(4)?).unwrap_or_default(),
                owner_agent: row.get(5)?,
                started_at: parse_opt_dt(row.get(6)?),
                completed_at: parse_opt_dt(row.get(7)?),
                summary: row.get(8)?,
            })
        })?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    pub fn list_events(
        &self,
        run_id: &str,
        since_seq: i64,
    ) -> Result<Vec<RuntimeTaskEvent>, StoreError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT seq, run_id, task_id, step_id, event_type, payload, timestamp
             FROM tr_events WHERE run_id = ? AND seq > ? ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map(params![run_id, since_seq], |row| {
            Ok(RuntimeTaskEvent {
                seq: row.get(0)?,
                run_id: row.get(1)?,
                task_id: row.get(2)?,
                step_id: row.get(3)?,
                event_type: RuntimeEventKind::from_str(&row.get::<_, String>(4)?)
                    .unwrap_or(RuntimeEventKind::Note),
                payload: serde_json::from_str(&row.get::<_, String>(5)?).unwrap_or_default(),
                timestamp: parse_dt(row.get(6)?),
            })
        })?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    pub fn list_artifacts(&self, run_id: &str) -> Result<Vec<Artifact>, StoreError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, run_id, task_id, kind, title, path, metadata
             FROM tr_artifacts WHERE run_id = ? ORDER BY rowid ASC",
        )?;
        let rows = stmt.query_map(params![run_id], |row| {
            Ok(Artifact {
                id: row.get(0)?,
                run_id: row.get(1)?,
                task_id: row.get(2)?,
                kind: ArtifactKind::from_str(&row.get::<_, String>(3)?)
                    .unwrap_or(ArtifactKind::Other),
                title: row.get(4)?,
                path: row.get(5)?,
                metadata: serde_json::from_str(&row.get::<_, String>(6)?).unwrap_or_default(),
            })
        })?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    pub fn list_reviews(
        &self,
        run_id: &str,
        task_id: &str,
    ) -> Result<Vec<ReviewResult>, StoreError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, run_id, task_id, reviewer_agent, outcome, issues,
                    failure_fingerprint, created_fix_task_id, created_at
             FROM tr_reviews WHERE run_id = ? AND task_id = ? ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map(params![run_id, task_id], |row| {
            Ok(ReviewResult {
                id: row.get(0)?,
                run_id: row.get(1)?,
                task_id: row.get(2)?,
                reviewer_agent: row.get(3)?,
                outcome: ReviewOutcome::from_str(&row.get::<_, String>(4)?)
                    .unwrap_or(ReviewOutcome::Blocked),
                issues: serde_json::from_str(&row.get::<_, String>(5)?).unwrap_or_default(),
                failure_fingerprint: row.get(6)?,
                created_fix_task_id: row.get(7)?,
                created_at: parse_dt(row.get(8)?),
            })
        })?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    pub fn get_summary(
        &self,
        run_id: &str,
        task_id: &str,
    ) -> Result<Option<TaskExecutionSummary>, StoreError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT run_id, task_id, worker_agent, completed_work, files_read,
                    files_changed, decisions, failures, verification,
                    next_implications, created_at
             FROM tr_summaries WHERE run_id = ? AND task_id = ?",
        )?;
        let mut rows = stmt.query(params![run_id, task_id])?;
        match rows.next()? {
            Some(row) => {
                let completed_work: String = row.get(3)?;
                let files_read: String = row.get(4)?;
                let files_changed: String = row.get(5)?;
                let decisions: String = row.get(6)?;
                let failures: String = row.get(7)?;
                let verification: String = row.get(8)?;
                let next_implications: String = row.get(9)?;
                let created_at: String = row.get(10)?;
                Ok(Some(TaskExecutionSummary {
                    run_id: row.get(0)?,
                    task_id: row.get(1)?,
                    worker_agent: row.get(2)?,
                    completed_work: serde_json::from_str(&completed_work).unwrap_or_default(),
                    files_read: serde_json::from_str(&files_read).unwrap_or_default(),
                    files_changed: serde_json::from_str(&files_changed).unwrap_or_default(),
                    decisions: serde_json::from_str(&decisions).unwrap_or_default(),
                    failures: serde_json::from_str(&failures).unwrap_or_default(),
                    verification: serde_json::from_str(&verification).unwrap_or_default(),
                    next_implications: serde_json::from_str(&next_implications).unwrap_or_default(),
                    created_at: parse_dt(created_at),
                }))
            }
            None => Ok(None),
        }
    }

    /// Append a free-form `Note` event for diagnostics / trace breadcrumbs.
    pub fn note(
        &self,
        run_id: &str,
        task_id: Option<&str>,
        message: &str,
    ) -> Result<(), StoreError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        append_event_tx(
            &tx,
            run_id,
            task_id,
            None,
            RuntimeEventKind::Note,
            serde_json::json!({ "message": message }),
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Persist a provider-reported LLM usage event for a worker.
    ///
    /// This is intentionally a low-frequency structured event rather than raw
    /// token streaming. It lets the GUI reconstruct token/cache metrics after
    /// refresh or restart without storing every text delta in SQLite.
    pub fn record_worker_llm_usage(
        &self,
        run_id: &str,
        task_id: &str,
        worker_id: &str,
        agent_name: &str,
        title: &str,
        payload: serde_json::Value,
    ) -> Result<(), StoreError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        append_event_tx(
            &tx,
            run_id,
            Some(task_id),
            Some(worker_id),
            RuntimeEventKind::WorkerLlmUsage,
            serde_json::json!({
                "worker_id": worker_id,
                "agent_name": agent_name,
                "title": title,
                "usage": payload.clone(),
            }),
        )?;
        tx.execute(
            "INSERT OR REPLACE INTO tr_usage_records
             (id, session_id, run_id, worker_id, model, provider, route_kind,
              input_tokens, output_tokens, cached_input_tokens, cache_creation_input_tokens,
              usage_reported, system_prompt_hash, tools_schema_hash, cwd_hash,
              worker_prompt_hash, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
            params![
                uuid::Uuid::new_v4().to_string(),
                json_string(&payload, "session_id").unwrap_or_else(|| run_id.to_string()),
                run_id,
                worker_id,
                json_string(&payload, "model").unwrap_or_else(|| "unknown".to_string()),
                json_string(&payload, "provider"),
                json_string(&payload, "route_kind").or_else(|| Some("task_runtime".to_string())),
                json_u64(&payload, "prompt_tokens") as i64,
                json_u64(&payload, "completion_tokens") as i64,
                json_u64(&payload, "cached_prompt_tokens") as i64,
                json_u64(&payload, "cache_creation_prompt_tokens") as i64,
                json_bool(&payload, "usage_reported", true) as i32,
                json_string(&payload, "system_prompt_hash"),
                json_string(&payload, "tools_schema_hash"),
                json_string(&payload, "cwd_hash"),
                json_string(&payload, "worker_prompt_hash"),
                Utc::now().to_rfc3339(),
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    // ── Approval scope tracking ─────────────────────────────────────────

    /// Grant a scoped approval for a tool call. Returns true if newly recorded.
    /// `scope_level` is one of: once | task | conversation | workspace | tool | all_tools.
    ///
    /// Reserved for future HITL approval-scope integration (executor does not
    /// call this yet — see hitrisk fail-closed path in executor.rs).
    #[allow(dead_code)]
    pub fn grant_approval(
        &self,
        run_id: &str,
        tool_name: &str,
        scope_level: &str,
        conversation_id: &str,
    ) -> Result<bool, StoreError> {
        let conn = self.lock()?;
        let created = conn.execute(
            "INSERT OR IGNORE INTO tr_approvals (run_id, tool_name, scope_level, conversation_id, created_at)
             VALUES (?,?,?,?,?)",
            params![run_id, tool_name, scope_level, conversation_id, chrono::Utc::now().to_rfc3339()],
        )?;
        Ok(created > 0)
    }

    /// Check whether a tool call is covered by a prior scope grant
    /// (conversation-level, all-tools wildcard, or per-tool).
    ///
    /// Reserved for future HITL approval-scope integration.
    #[allow(dead_code)]
    pub fn is_approved(
        &self,
        run_id: &str,
        conversation_id: &str,
        tool_name: &str,
    ) -> Result<bool, StoreError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT 1 FROM tr_approvals
             WHERE run_id = ? AND (tool_name = ? OR tool_name = '*') AND conversation_id = ?
             LIMIT 1",
        )?;
        let mut rows = stmt.query(params![run_id, tool_name, conversation_id])?;
        Ok(rows.next()?.is_some())
    }

    /// Revoke all approvals for a run.
    pub fn revoke_run_approvals(&self, run_id: &str) -> Result<(), StoreError> {
        let conn = self.lock()?;
        conn.execute("DELETE FROM tr_approvals WHERE run_id = ?", params![run_id])?;
        Ok(())
    }
}

// ── transaction-scoped helpers (private) ─────────────────────────────────

fn append_event_tx(
    tx: &rusqlite::Transaction<'_>,
    run_id: &str,
    task_id: Option<&str>,
    step_id: Option<&str>,
    event_type: RuntimeEventKind,
    payload: serde_json::Value,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO tr_events (run_id, task_id, step_id, event_type, payload, timestamp)
         VALUES (?,?,?,?,?,?)",
        params![
            run_id,
            task_id,
            step_id,
            event_type.as_str(),
            payload.to_string(),
            Utc::now().to_rfc3339(),
        ],
    )?;
    Ok(())
}

fn json_u64(value: &serde_json::Value, key: &str) -> u64 {
    value.get(key).and_then(|v| v.as_u64()).unwrap_or(0)
}

fn json_bool(value: &serde_json::Value, key: &str, default: bool) -> bool {
    value.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
}

fn json_string(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string())
}

fn insert_plan_task_tx(
    tx: &rusqlite::Transaction<'_>,
    plan_id: &str,
    run_id: &str,
    t: &PlanTask,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO tr_plan_tasks
            (id, plan_id, run_id, title, description, kind, agent_role, domain_profile,
             depends_on, parallel_group, files, allowed_tools, verification,
             retry_count, max_retries, failure_fingerprint, status)
         VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        params![
            t.id,
            plan_id,
            run_id,
            t.title,
            t.description,
            t.kind.as_str(),
            t.agent_role,
            t.domain_profile.as_str(),
            serde_json::to_string(&t.depends_on)?,
            t.parallel_group,
            serde_json::to_string(&t.files)?,
            serde_json::to_string(&t.allowed_tools)?,
            serde_json::to_string(&t.verification)?,
            t.retry_count,
            t.max_retries,
            t.failure_fingerprint,
            t.status.as_str(),
        ],
    )?;
    Ok(())
}

fn load_plan_tasks(conn: &Connection, plan_id: &str) -> Result<Vec<PlanTask>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT id, title, description, kind, agent_role, domain_profile,
                depends_on, parallel_group, files, allowed_tools, verification,
                retry_count, max_retries, failure_fingerprint, status
         FROM tr_plan_tasks WHERE plan_id = ? ORDER BY rowid ASC",
    )?;
    let rows = stmt.query_map(params![plan_id], |row| {
        Ok(PlanTask {
            id: row.get(0)?,
            title: row.get(1)?,
            description: row.get(2)?,
            kind: PlanTaskKind::from_str(&row.get::<_, String>(3)?)
                .unwrap_or(PlanTaskKind::ReadOnlyReview),
            agent_role: row.get(4)?,
            domain_profile: DomainProfile::from_str(&row.get::<_, String>(5)?).unwrap_or_default(),
            depends_on: serde_json::from_str(&row.get::<_, String>(6)?).unwrap_or_default(),
            parallel_group: row.get(7)?,
            files: serde_json::from_str(&row.get::<_, String>(8)?).unwrap_or_default(),
            allowed_tools: serde_json::from_str(&row.get::<_, String>(9)?).unwrap_or_default(),
            verification: serde_json::from_str(&row.get::<_, String>(10)?).unwrap_or_default(),
            retry_count: row.get(11)?,
            max_retries: row.get(12)?,
            failure_fingerprint: row.get(13)?,
            status: TodoStatus::from_str(&row.get::<_, String>(14)?).unwrap_or_default(),
        })
    })?;
    Ok(rows.filter_map(Result::ok).collect())
}

/// Read a run AND take a write lock on its row (via `SELECT ... ` within the
/// open tx). Returns (current_status_str, run). Caller is inside a tx so the
/// subsequent UPDATE + event append are atomic with this read.
fn load_run_for_update(
    tx: &rusqlite::Transaction<'_>,
    run_id: &str,
) -> Result<(String, TaskRun), StoreError> {
    let mut stmt = tx.prepare(
        "SELECT run_id, workspace_id, conversation_id, root_message_id,
                domain_profile, status, goal, plan_id, created_at, updated_at
         FROM tr_runs WHERE run_id = ?",
    )?;
    let mut rows = stmt.query(params![run_id])?;
    let row = rows
        .next()?
        .ok_or(StoreError::RunNotFound(run_id.to_string()))?;
    let run = decode_run(&row)?;
    Ok((run.status.as_str().to_string(), run))
}

fn decode_run(row: &Row<'_>) -> rusqlite::Result<TaskRun> {
    let domain_str: String = row.get(4)?;
    let status_str: String = row.get(5)?;
    let created: String = row.get(8)?;
    let updated: String = row.get(9)?;
    Ok(TaskRun {
        run_id: row.get(0)?,
        workspace_id: row.get(1)?,
        conversation_id: row.get(2)?,
        root_message_id: row.get(3)?,
        domain_profile: DomainProfile::from_str(&domain_str).unwrap_or_default(),
        status: TaskRunStatus::from_str(&status_str).unwrap_or_default(),
        goal: row.get(6)?,
        plan_id: row.get(7)?,
        created_at: parse_dt(created),
        updated_at: parse_dt(updated),
    })
}

fn parse_dt(s: String) -> DateTime<Utc> {
    chrono::DateTime::parse_from_rfc3339(&s)
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(|e| {
            tracing::warn!(raw = %s, error = %e, "parse_dt: unparseable timestamp, falling back to now");
            Utc::now()
        })
}

fn parse_opt_dt(s: Option<String>) -> Option<DateTime<Utc>> {
    s.filter(|s| !s.is_empty()).map(parse_dt)
}

fn default_db_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home)
        .join(".echo-agent")
        .join("task_runtime.db")
}

// The compile-time test that proves the transaction invariant:
// a state change without an event would leave the DB inconsistent.
// We assert both rows land together.
#[cfg(test)]
mod tests {
    use super::super::classify::{Classification, ComplexityLabel};
    use super::super::planner::generate_parallel_readonly_plan;
    use super::*;

    fn fresh() -> TaskRuntimeStore {
        TaskRuntimeStore::new_in_memory().expect("in-memory store")
    }

    #[test]
    fn create_run_emits_run_created_event() {
        let s = fresh();
        let run = s
            .create_run(
                "r1",
                "ws",
                "c1",
                "m1",
                DomainProfile::AiCoding,
                "review runtime",
            )
            .unwrap();
        assert_eq!(run.status, TaskRunStatus::Pending);
        let evs = s.list_events("r1", 0).unwrap();
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].event_type, RuntimeEventKind::RunCreated);
    }

    #[test]
    fn transition_run_appends_status_event_atomically() {
        let s = fresh();
        s.create_run("r1", "ws", "c1", "m1", DomainProfile::General, "g")
            .unwrap();
        let run = s.transition_run("r1", TaskRunStatus::Planning).unwrap();
        assert_eq!(run.status, TaskRunStatus::Planning);
        let evs = s.list_events("r1", 0).unwrap();
        // RunCreated + RunStatusChanged
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[1].event_type, RuntimeEventKind::RunStatusChanged);
    }

    #[test]
    fn illegal_transition_is_rejected_and_leaves_no_event() {
        let s = fresh();
        s.create_run("r1", "ws", "c1", "m1", DomainProfile::General, "g")
            .unwrap();
        let before = s.list_events("r1", 0).unwrap().len();
        let err = s.transition_run("r1", TaskRunStatus::Running).unwrap_err();
        assert!(matches!(err, StoreError::IllegalTransition { .. }));
        // No new event was appended — the tx rolled back.
        assert_eq!(s.list_events("r1", 0).unwrap().len(), before);
    }

    #[test]
    fn attach_plan_creates_tasks_and_todos() {
        let s = fresh();
        s.create_run("r1", "ws", "c1", "m1", DomainProfile::General, "g")
            .unwrap();
        s.transition_run("r1", TaskRunStatus::Planning).unwrap();
        let plan = TaskPlan {
            plan_id: "p1".into(),
            run_id: "r1".into(),
            domain_profile: DomainProfile::General,
            goal: "g".into(),
            assumptions: vec!["a".into()],
            risks: vec![],
            execution_mode: ExecutionMode::Parallel,
            tasks: vec![PlanTask {
                id: "t1".into(),
                title: "Review runtime".into(),
                kind: PlanTaskKind::ReadOnlyReview,
                agent_role: "code_reviewer".into(),
                ..Default::default()
            }],
        };
        s.attach_plan(&plan).unwrap();

        let loaded = s.get_plan("r1").unwrap().expect("plan");
        assert_eq!(loaded.tasks.len(), 1);
        assert_eq!(loaded.tasks[0].id, "t1");

        let todos = s.list_todos("r1").unwrap();
        assert_eq!(todos.len(), 1);
        assert_eq!(todos[0].task_id, "t1");
        assert_eq!(todos[0].status, TodoStatus::Pending);

        let run = s.get_run("r1").unwrap().unwrap();
        assert_eq!(run.status, TaskRunStatus::AwaitingPlanApproval);
        assert_eq!(run.plan_id.as_deref(), Some("p1"));
    }

    #[test]
    fn attach_parallel_readonly_plans_do_not_reuse_global_task_ids() {
        let s = fresh();
        let classification = Classification {
            complexity: ComplexityLabel::Complex,
            inferred_profile: DomainProfile::AiCoding,
            reason: "test".to_string(),
            signals: vec!["analysis".to_string()],
        };

        for run_id in ["r1", "r2"] {
            s.create_run(run_id, "ws", "c1", "m1", DomainProfile::AiCoding, "g")
                .unwrap();
            s.transition_run(run_id, TaskRunStatus::Planning).unwrap();
            let generated = generate_parallel_readonly_plan(
                run_id,
                "帮我分析当前目录的项目",
                &classification,
                &["project_explorer".to_string(), "summary_writer".to_string()],
            );
            s.attach_plan(&generated.plan).unwrap();
        }

        let first = s.get_plan("r1").unwrap().expect("first plan");
        let second = s.get_plan("r2").unwrap().expect("second plan");
        for first_task in &first.tasks {
            for second_task in &second.tasks {
                assert_ne!(first_task.id, second_task.id);
            }
        }
    }

    #[test]
    fn set_task_status_updates_task_todo_and_event_together() {
        let s = fresh();
        seed_plan(&s);
        s.set_task_status("r1", "t1", TodoStatus::Running, Some("code_reviewer"), None)
            .unwrap();
        let todos = s.list_todos("r1").unwrap();
        assert_eq!(todos[0].status, TodoStatus::Running);
        assert_eq!(todos[0].owner_agent.as_deref(), Some("code_reviewer"));
        assert!(todos[0].started_at.is_some());

        let evs = s.list_events("r1", 0).unwrap();
        assert!(
            evs.iter()
                .any(|e| e.event_type == RuntimeEventKind::TaskStarted)
        );
    }

    #[test]
    fn put_summary_upserts_and_get_summary_reads() {
        let s = fresh();
        seed_plan(&s);
        let sum = TaskExecutionSummary {
            run_id: "r1".into(),
            task_id: "t1".into(),
            worker_agent: "code_reviewer".into(),
            completed_work: vec!["read chat.rs".into()],
            files_read: vec!["chat.rs".into()],
            files_changed: vec![],
            decisions: vec!["route via TaskRuntime".into()],
            failures: vec![],
            verification: vec!["cargo check".into()],
            next_implications: vec!["implement router".into()],
            created_at: Utc::now(),
        };
        s.put_summary(&sum).unwrap();
        let got = s.get_summary("r1", "t1").unwrap().unwrap();
        assert_eq!(got.completed_work, vec!["read chat.rs".to_string()]);
        assert_eq!(got.next_implications.len(), 1);
    }

    #[test]
    fn latest_run_for_conversation_orders_by_created_desc() {
        let s = fresh();
        s.create_run("r1", "ws", "c1", "m1", DomainProfile::General, "g1")
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        s.create_run("r2", "ws", "c1", "m2", DomainProfile::General, "g2")
            .unwrap();
        let latest = s.latest_run_for_conversation("c1").unwrap().unwrap();
        assert_eq!(latest.run_id, "r2");
    }

    fn seed_plan(s: &TaskRuntimeStore) {
        s.create_run("r1", "ws", "c1", "m1", DomainProfile::General, "g")
            .unwrap();
        s.transition_run("r1", TaskRunStatus::Planning).unwrap();
        let plan = TaskPlan {
            plan_id: "p1".into(),
            run_id: "r1".into(),
            domain_profile: DomainProfile::General,
            goal: "g".into(),
            assumptions: vec![],
            risks: vec![],
            execution_mode: ExecutionMode::Parallel,
            tasks: vec![PlanTask {
                id: "t1".into(),
                title: "Review runtime".into(),
                kind: PlanTaskKind::ReadOnlyReview,
                agent_role: "code_reviewer".into(),
                ..Default::default()
            }],
        };
        s.attach_plan(&plan).unwrap();
        // approve -> ready -> running so todos can transition freely
        s.transition_run("r1", TaskRunStatus::Ready).unwrap();
        s.transition_run("r1", TaskRunStatus::Running).unwrap();
    }
}

// ── Usage records persistence ──────────────────────────────────────────

impl TaskRuntimeStore {
    /// Insert a usage record.
    pub fn insert_usage_record(
        &self,
        record: &super::types::UsageRecord,
    ) -> Result<(), StoreError> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT OR REPLACE INTO tr_usage_records
             (id, session_id, run_id, worker_id, model, provider, route_kind,
              input_tokens, output_tokens, cached_input_tokens, cache_creation_input_tokens,
              usage_reported, system_prompt_hash, tools_schema_hash, cwd_hash,
              worker_prompt_hash, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
            rusqlite::params![
                record.id,
                record.session_id,
                record.run_id,
                record.worker_id,
                record.model,
                record.provider,
                record.route_kind,
                record.input_tokens as i64,
                record.output_tokens as i64,
                record.cached_input_tokens as i64,
                record.cache_creation_input_tokens as i64,
                record.usage_reported as i32,
                record.system_prompt_hash,
                record.tools_schema_hash,
                record.cwd_hash,
                record.worker_prompt_hash,
                record.created_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    /// Query usage records with optional filters.
    pub fn query_usage_records(
        &self,
        filter: &super::types::UsageQueryFilter,
    ) -> Result<Vec<super::types::UsageRecord>, StoreError> {
        use super::types::UsageRecord;
        let conn = self.lock()?;
        let mut sql = String::from(
            "SELECT id, session_id, run_id, worker_id, model, provider, route_kind,
                    input_tokens, output_tokens, cached_input_tokens, cache_creation_input_tokens,
                    usage_reported, system_prompt_hash, tools_schema_hash, cwd_hash,
                    worker_prompt_hash, created_at
             FROM tr_usage_records WHERE 1=1",
        );
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(ref v) = filter.session_id {
            params.push(Box::new(v.clone()));
            sql.push_str(&format!(" AND session_id = ?{}", params.len()));
        }
        if let Some(ref v) = filter.run_id {
            params.push(Box::new(v.clone()));
            sql.push_str(&format!(" AND run_id = ?{}", params.len()));
        }
        if let Some(ref v) = filter.model {
            params.push(Box::new(v.clone()));
            sql.push_str(&format!(" AND model = ?{}", params.len()));
        }
        if let Some(ref v) = filter.provider {
            params.push(Box::new(v.clone()));
            sql.push_str(&format!(" AND provider = ?{}", params.len()));
        }
        if let Some(ref v) = filter.route_kind {
            params.push(Box::new(v.clone()));
            sql.push_str(&format!(" AND route_kind = ?{}", params.len()));
        }
        if let Some(ref v) = filter.created_after {
            params.push(Box::new(v.to_rfc3339()));
            sql.push_str(&format!(" AND created_at >= ?{}", params.len()));
        }
        if let Some(ref v) = filter.created_before {
            params.push(Box::new(v.to_rfc3339()));
            sql.push_str(&format!(" AND created_at <= ?{}", params.len()));
        }

        sql.push_str(" ORDER BY created_at DESC");

        if let Some(limit) = filter.limit {
            params.push(Box::new(limit as i64));
            sql.push_str(&format!(" LIMIT ?{}", params.len()));
        }
        if let Some(offset) = filter.offset {
            params.push(Box::new(offset as i64));
            sql.push_str(&format!(" OFFSET ?{}", params.len()));
        }

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(param_refs.as_slice(), |row| {
            Ok(UsageRecord {
                id: row.get(0)?,
                session_id: row.get(1)?,
                run_id: row.get(2)?,
                worker_id: row.get(3)?,
                model: row.get(4)?,
                provider: row.get(5)?,
                route_kind: row.get(6)?,
                input_tokens: row.get::<_, i64>(7)? as u64,
                output_tokens: row.get::<_, i64>(8)? as u64,
                cached_input_tokens: row.get::<_, i64>(9)? as u64,
                cache_creation_input_tokens: row.get::<_, i64>(10)? as u64,
                usage_reported: row.get::<_, i32>(11)? != 0,
                system_prompt_hash: row.get(12)?,
                tools_schema_hash: row.get(13)?,
                cwd_hash: row.get(14)?,
                worker_prompt_hash: row.get(15)?,
                created_at: {
                    let s: String = row.get(16)?;
                    chrono::DateTime::parse_from_rfc3339(&s)
                        .map(|dt| dt.with_timezone(&chrono::Utc))
                        .unwrap_or_else(|_| chrono::Utc::now())
                },
            })
        })?;

        let mut records = Vec::new();
        for row in rows {
            records.push(row.map_err(StoreError::Sqlite)?);
        }
        Ok(records)
    }

    /// Get an end-of-run usage summary from persisted records.
    pub fn get_run_usage_summary(
        &self,
        run_id: &str,
    ) -> Result<Option<super::types::RunUsageSummary>, StoreError> {
        use super::types::{ModelUsageSummary, RunUsageSummary};
        let records = self.query_usage_records(&super::types::UsageQueryFilter {
            run_id: Some(run_id.to_string()),
            ..Default::default()
        })?;

        if records.is_empty() {
            return Ok(None);
        }

        let mut total_input = 0u64;
        let mut total_output = 0u64;
        let mut total_cached = 0u64;
        let mut total_cache_write = 0u64;
        let mut model_map: std::collections::HashMap<String, (u64, u64, u64, u64)> =
            std::collections::HashMap::new();

        for r in &records {
            total_input += r.input_tokens;
            total_output += r.output_tokens;
            total_cached += r.cached_input_tokens;
            total_cache_write += r.cache_creation_input_tokens;

            let entry = model_map.entry(r.model.clone()).or_insert((0, 0, 0, 0));
            entry.0 += 1; // llm_calls
            entry.1 += r.input_tokens;
            entry.2 += r.output_tokens;
            entry.3 += r.cached_input_tokens;
        }

        let cache_read_rate = if total_input > 0 {
            total_cached as f64 / total_input as f64
        } else {
            0.0
        };

        let model_breakdown: Vec<ModelUsageSummary> = model_map
            .into_iter()
            .map(|(model, (calls, inp, out, cached))| ModelUsageSummary {
                model,
                llm_calls: calls,
                input_tokens: inp,
                output_tokens: out,
                cached_input_tokens: cached,
            })
            .collect();

        let top_low_hit_reasons = if cache_read_rate < 0.1 && total_input > 0 {
            vec!["cache read rate below 10% — check system prompt stability and tools schema consistency".to_string()]
        } else {
            vec![]
        };

        Ok(Some(RunUsageSummary {
            run_id: Some(run_id.to_string()),
            total_input_tokens: total_input,
            total_output_tokens: total_output,
            total_cached_input_tokens: total_cached,
            total_cache_creation_input_tokens: total_cache_write,
            cache_read_rate,
            llm_calls: records.len() as u64,
            model_breakdown,
            top_low_hit_reasons,
        }))
    }

    // ── Conversation events (replay support) ───────────────────────────

    pub fn append_conversation_event(
        &self,
        conversation_id: &str,
        event_type: &str,
        payload: &str,
    ) -> Result<(), StoreError> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO tr_conversation_events (conversation_id, event_type, payload, timestamp)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                conversation_id,
                event_type,
                payload,
                chrono::Utc::now().to_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn list_conversation_events(
        &self,
        conversation_id: &str,
        since_seq: Option<i64>,
    ) -> Result<Vec<serde_json::Value>, StoreError> {
        let conn = self.lock()?;
        let sql = if let Some(seq) = since_seq {
            format!(
                "SELECT seq, event_type, payload, timestamp FROM tr_conversation_events WHERE conversation_id = ?1 AND seq > {seq} ORDER BY seq"
            )
        } else {
            "SELECT seq, event_type, payload, timestamp FROM tr_conversation_events WHERE conversation_id = ?1 ORDER BY seq".to_string()
        };
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params![conversation_id], |row| {
            Ok(serde_json::json!({
                "seq": row.get::<_, i64>(0)?,
                "event_type": row.get::<_, String>(1)?,
                "payload": row.get::<_, String>(2)?,
                "timestamp": row.get::<_, String>(3)?,
            }))
        })?;
        let mut events = Vec::new();
        for row in rows {
            events.push(row.map_err(StoreError::Sqlite)?);
        }
        Ok(events)
    }
}
