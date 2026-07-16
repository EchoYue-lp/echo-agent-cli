//! File-backed canonical store for the TaskRuntime.
//!
//! U1c phase-0/0bc: the file system (`plan.json` + `events.jsonl`) is the
//! single source of truth for all task/plan data. Usage records and
//! conversation-replay events are held in memory (EKO is a local tool; these
//! are ephemeral and need not survive a restart). No SQLite dependency.
//!
//! Every state mutation appends a [`RuntimeTaskEvent`] to `events.jsonl` and
//! rebuilds `plan.json` from the full event stream.

use std::path::PathBuf;

use chrono::Utc;

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
    #[error("lock poisoned")]
    LockPoisoned,
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid plan: {0}")]
    InvalidPlan(String),
    #[error("file shadow: {0}")]
    Shadow(#[from] super::file_shadow::ShadowError),
}

/// File-backed TaskRuntime store. One instance per process; cheap to clone
/// behind `Arc`. The file system (plan.json + events.jsonl) is the read/write
/// authority for all task/plan data. Usage records and conversation-replay
/// events are kept in-memory (EKO is a local tool; these are ephemeral and
/// need not survive a restart — see AGENTS.md "no compat/recovery" stance).
pub struct TaskRuntimeStore {
    /// Per-task cancellation tokens (in-memory runtime state, not persisted).
    /// Key = `"{run_id}::{task_id}"`. `execute_task` registers a token when a
    /// task starts and removes it on completion; `remove_task` cancels the
    /// token of a running task so its subagent stops promptly (rather than the
    /// status flipping to Skipped while execution continues).
    task_cancel_tokens:
        std::sync::Mutex<std::collections::HashMap<String, echo_agent::agent::CancellationToken>>,
    /// Active TaskRun driver tokens. Every entry point registers here so pause
    /// and cancel target the real executor instead of a surface-local map.
    run_cancel_tokens:
        std::sync::Mutex<std::collections::HashMap<String, echo_agent::agent::CancellationToken>>,
    /// File shadow (U1c phase-0/0bc). The read/write authority for all task data.
    shadow: std::sync::Arc<super::file_shadow::FileTaskShadow>,
    /// In-memory LLM usage records (token spend per subagent call). Not persisted
    /// —重启清零,符合 EKO 本地工具定位(usage 是参考指标,非账本)。
    usage_records: std::sync::Mutex<Vec<super::types::UsageRecord>>,
    /// Per-run plan/state 写互斥锁 (F2-1 / F3-3 / F3-4)。
    ///
    /// insert_task / attach_plan / update_plan_task / transition_run 都是
    /// "读文件 → 改 → 重写文件"三步, 此前无锁 → EKO 写工具默认并行执行
    /// (react_loop.rs:415, 仅 approval 工具串行), plan_create + plan_execute
    /// 可能并发覆写 plan.json。加 per-run Mutex 串行化同一 run 的所有 plan/run
    /// 变更 (对标调研结论: 进程内 Mutex 兜底, 同时防崩溃中态)。不同 run 互不影响。
    plan_locks: dashmap::DashMap<String, std::sync::Arc<std::sync::Mutex<()>>>,
}

/// RAII registration for one active TaskRun driver. Nested drivers for the
/// same run restore the previous token when they finish (for example an
/// unattended ReAct driver invoking `plan_execute`).
pub struct RunCancellationRegistration {
    store: std::sync::Arc<TaskRuntimeStore>,
    run_id: String,
    token: echo_agent::agent::CancellationToken,
    previous: Option<echo_agent::agent::CancellationToken>,
}

impl Drop for RunCancellationRegistration {
    fn drop(&mut self) {
        if let Ok(mut map) = self.store.run_cancel_tokens.lock() {
            if self.token.is_cancelled() {
                map.remove(&self.run_id);
            } else if let Some(previous) = self.previous.take() {
                map.insert(self.run_id.clone(), previous);
            } else {
                map.remove(&self.run_id);
            }
        }
    }
}

impl TaskRuntimeStore {
    /// Create the store at the default location.
    ///
    /// task/plan data lives under the file shadow root (`~/.echo-agent/tasks/`);
    /// usage/conversation-events are in-memory. No database is opened, so this
    /// does not fail in practice — the `Result` is kept for call-site compat.
    pub fn new() -> anyhow::Result<Self> {
        Self::open()
    }

    /// Create the store. No path is needed anymore (no SQLite); the file shadow
    /// root is the real storage location. Kept as `open()` with no args for
    /// call-site compatibility with the old `open(path)` constructor.
    pub fn open() -> anyhow::Result<Self> {
        let shadow = std::sync::Arc::new(super::file_shadow::FileTaskShadow::new(
            super::file_shadow::FileTaskShadow::default_root(),
        ));
        Ok(Self {
            task_cancel_tokens: std::sync::Mutex::new(std::collections::HashMap::new()),
            run_cancel_tokens: std::sync::Mutex::new(std::collections::HashMap::new()),
            shadow,
            usage_records: std::sync::Mutex::new(Vec::new()),
            plan_locks: dashmap::DashMap::new(),
        })
    }

    /// In-memory store for tests / fallback. The file shadow is backed by a
    /// per-process temp dir so every test exercises the file-authority path.
    pub fn new_in_memory() -> anyhow::Result<Self> {
        let shadow_root = std::env::temp_dir().join(format!(
            "echo-agent-task-runtime-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        Self::new_in_memory_with_shadow_root(shadow_root)
    }

    /// In-memory store whose file shadow is rooted at `shadow_root`. Tests use
    /// this (with a `tempfile::tempdir()` root) so they can read the written
    /// `events.jsonl` / `plan.json` back directly and so runs are isolated
    /// under a known directory. Replaces the old `attach_shadow` test hook.
    pub fn new_in_memory_with_shadow_root(shadow_root: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let shadow = std::sync::Arc::new(super::file_shadow::FileTaskShadow::new(shadow_root));
        Ok(Self {
            task_cancel_tokens: std::sync::Mutex::new(std::collections::HashMap::new()),
            run_cancel_tokens: std::sync::Mutex::new(std::collections::HashMap::new()),
            shadow,
            usage_records: std::sync::Mutex::new(Vec::new()),
            plan_locks: dashmap::DashMap::new(),
        })
    }

    /// 在持有某 run 的 plan/state 写锁期间执行闭包 (F2-1 / F3-3 / F3-4)。
    ///
    /// 用 closure 模式而非返回 Guard: std::sync::MutexGuard 借自 &Mutex, 而
    /// Mutex 在 Arc 内, Arc 作为局部变量时 Guard 跨函数返回即悬垂 (自引用
    /// struct 在 Rust 里无法直接表达)。closure 把锁的获取与释放封装在内部,
    /// 闭包体内是临界区。insert_task / attach_plan / update_plan_task /
    /// transition_run 用它包裹"读改写"全程。
    fn with_run_lock<R, E>(&self, run_id: &str, f: impl FnOnce() -> Result<R, E>) -> Result<R, E> {
        let arc = self
            .plan_locks
            .entry(run_id.to_string())
            .or_insert_with(|| std::sync::Arc::new(std::sync::Mutex::new(())))
            .clone();
        // 持锁调用闭包; poison 时恢复 (与 working_dir 同款 into_inner, 不 panic)。
        let _guard = arc.lock().unwrap_or_else(|e| e.into_inner());
        f()
    }

    /// Build a `FileTaskStore` over the shadow, for read delegation.
    fn file_store(&self) -> super::file_store::FileTaskStore {
        super::file_store::FileTaskStore::new((*self.shadow).clone())
    }

    // ── Runs ────────────────────────────────────────────────────────────

    /// Create a new run in `Pending` and emit `RunCreated`. Returns the
    /// existing run when `run_id` is already present.
    #[allow(clippy::too_many_arguments)] // run identity + routing fields all thread through
    pub fn create_run(
        &self,
        run_id: &str,
        workspace_id: &str,
        conversation_id: &str,
        root_message_id: &str,
        domain_profile: DomainProfile,
        goal: &str,
        route: &str,
        attended_mode: AttendedMode,
    ) -> Result<TaskRun, StoreError> {
        self.with_run_lock(run_id, || {
            if let Some(existing) = self.get_run(run_id)? {
                return Ok(existing);
            }

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
                route: route.to_string(),
                attended_mode,
                attachments: Vec::new(),
                created_at: now,
                updated_at: now,
            };

            // U1c phase-0/0bc step-2: file is the write authority. Append the
            // RunCreated event to events.jsonl and rebuild plan.json — no SQL
            // write.
            self.shadow.append_event_line(
                run.run_id.as_str(),
                None,
                None,
                RuntimeEventKind::RunCreated,
                serde_json::json!({
                    "goal": goal,
                    "domain_profile": domain_profile.as_str(),
                    "workspace_id": run.workspace_id,
                    "conversation_id": run.conversation_id,
                    "root_message_id": run.root_message_id,
                    "route": run.route,
                    "attended_mode": attended_mode.as_str(),
                    "created_at": echo_agent::utils::time::to_local(run.created_at).to_rfc3339(),
                }),
            )?;
            self.shadow.rewrite_plan(&run.run_id)?;
            Ok(run)
        })
    }

    /// Bind user-uploaded attachments to a run so plan-level subagents see the
    /// same images/files as the main agent.
    ///
    /// Follows the event-sourcing pattern: append a `RunAttachmentsUpdated`
    /// event then rewrite plan.json so subsequent `get_run` reads reflect it.
    pub fn set_run_attachments(
        &self,
        run_id: &str,
        attachments: &[crate::attachments::AttachmentRef],
    ) -> Result<(), StoreError> {
        self.with_run_lock(run_id, || {
            // Validate the run exists (mirrors set_task_status / transition_run).
            self.get_run(run_id)?
                .ok_or(StoreError::RunNotFound(run_id.to_string()))?;
            self.shadow.append_event_line(
                run_id,
                None,
                None,
                RuntimeEventKind::RunAttachmentsUpdated,
                serde_json::json!({ "attachments": attachments }),
            )?;
            self.shadow.rewrite_plan(run_id)?;
            Ok(())
        })
    }

    /// Atomically transition a run to `next` and append `RunStatusChanged`.
    /// Rejects illegal transitions (see [`TaskRunStatus::can_transition_to`]).
    pub fn transition_run(&self, run_id: &str, next: TaskRunStatus) -> Result<TaskRun, StoreError> {
        // F3-3/F3-4: 串行化"读→验证→写", 防并发 transition 丢更新 + 崩溃中态。
        // 用 closure 包裹临界区 (见 with_run_lock 说明)。
        self.with_run_lock(run_id, || {
            // U1c phase-0/0bc step-2: file is the read/write authority. Read the
            // current run from the file to validate the transition, then append the
            // status-changed event + rewrite plan.json. No SQL write.
            let run = self
                .get_run(run_id)?
                .ok_or(StoreError::RunNotFound(run_id.to_string()))?;
            let current = run.status;
            if !current.can_transition_to(next) {
                return Err(StoreError::IllegalTransition {
                    run_id: run_id.to_string(),
                    from: current.as_str().to_string(),
                    to: next.as_str().to_string(),
                });
            }
            let now = Utc::now();
            self.shadow.append_event_line(
                run_id,
                None,
                None,
                RuntimeEventKind::RunStatusChanged,
                serde_json::json!({ "from": current.as_str(), "to": next.as_str() }),
            )?;
            if next == TaskRunStatus::Cancelled {
                self.shadow.append_event_line(
                    run_id,
                    None,
                    None,
                    RuntimeEventKind::RunCancelled,
                    serde_json::json!({}),
                )?;
            }
            self.shadow.rewrite_plan(run_id)?;
            let mut run = run;
            run.status = next;
            run.updated_at = now;
            Ok(run)
        })
    }

    /// Resume a paused run: `Paused → Running`.
    ///
    /// The caller (IPC layer) is responsible for re-launching the executor
    /// after this succeeds — the store only handles the state transition.
    pub fn resume_task_run(&self, run_id: &str) -> Result<TaskRun, StoreError> {
        self.transition_run(run_id, TaskRunStatus::Running)
    }

    // ── Task-level cancellation (gap-2 fix) ───────────────────────────────
    // These are in-memory runtime tokens, NOT persisted. They let remove_task
    // stop a running task's subagent promptly instead of leaving it executing
    // after the status has flipped to Skipped.

    /// Register a cancellation token for a task that is about to start running.
    /// Called by the executor before dispatching the subagent. The token is a
    /// child of the run-level cancel, so run cancel still propagates.
    pub fn register_task_cancel_token(
        &self,
        run_id: &str,
        task_id: &str,
        token: echo_agent::agent::CancellationToken,
    ) {
        let key = format!("{run_id}::{task_id}");
        if let Ok(mut map) = self.task_cancel_tokens.lock() {
            map.insert(key, token);
        }
    }

    /// Remove a task's cancellation token after it completes (success/fail).
    /// Called by the executor when execute_task returns.
    pub fn unregister_task_cancel_token(&self, run_id: &str, task_id: &str) {
        let key = format!("{run_id}::{task_id}");
        if let Ok(mut map) = self.task_cancel_tokens.lock() {
            map.remove(&key);
        }
    }

    /// Cancel a specific task's subagent (if running). Called by remove_task /
    /// update_task when a running task is being skipped or fundamentally
    /// changed. No-op if the task isn't currently running (no token registered).
    pub fn cancel_task(&self, run_id: &str, task_id: &str) {
        let key = format!("{run_id}::{task_id}");
        if let Ok(mut map) = self.task_cancel_tokens.lock() {
            #[allow(clippy::collapsible_if)]
            // nested let-Ok/let-Some reads clearer than a let-chain
            if let Some(token) = map.remove(&key) {
                token.cancel();
            }
        }
    }

    /// Register the active driver token and automatically restore/remove it
    /// when the returned guard is dropped.
    pub fn register_run_cancellation(
        self: &std::sync::Arc<Self>,
        run_id: &str,
        token: echo_agent::agent::CancellationToken,
    ) -> Result<RunCancellationRegistration, StoreError> {
        let previous = self
            .run_cancel_tokens
            .lock()
            .map_err(|_| StoreError::LockPoisoned)?
            .insert(run_id.to_string(), token.clone());
        Ok(RunCancellationRegistration {
            store: self.clone(),
            run_id: run_id.to_string(),
            token,
            previous,
        })
    }

    fn cancel_active_run(&self, run_id: &str) -> bool {
        if let Ok(mut map) = self.run_cancel_tokens.lock() {
            #[allow(clippy::collapsible_if)]
            // nested let-Ok/let-Some reads clearer than a let-chain
            if let Some(token) = map.remove(run_id) {
                token.cancel();
                return true;
            }
        }
        false
    }

    /// Request cancellation through the single TaskRuntime control path.
    /// Active runs are stopped through their driver token so the executor owns
    /// the terminal transition. Runs without a driver may only be cancelled
    /// directly when they are not executing.
    pub fn request_cancel(&self, run_id: &str) -> Result<bool, StoreError> {
        if self.cancel_active_run(run_id) {
            return Ok(true);
        }
        let Some(run) = self.get_run(run_id)? else {
            return Ok(false);
        };
        match run.status {
            TaskRunStatus::Pending | TaskRunStatus::Paused | TaskRunStatus::Failed => {
                self.transition_run(run_id, TaskRunStatus::Cancelled)?;
                Ok(true)
            }
            TaskRunStatus::Running | TaskRunStatus::Cancelled | TaskRunStatus::Completed => {
                Ok(false)
            }
        }
    }

    /// Pause an actively driven run. The status changes first, then the same
    /// run-scoped token used for cancellation stops in-flight workers. The
    /// executor observes the durable Paused status and leaves the run resumable.
    pub fn request_pause(&self, run_id: &str) -> Result<bool, StoreError> {
        let Some(run) = self.get_run(run_id)? else {
            return Ok(false);
        };
        if run.status != TaskRunStatus::Running {
            return Ok(false);
        }
        let token = self
            .run_cancel_tokens
            .lock()
            .map_err(|_| StoreError::LockPoisoned)?
            .remove(run_id);
        let Some(token) = token else {
            return Ok(false);
        };
        if let Err(error) = self.transition_run(run_id, TaskRunStatus::Paused) {
            self.run_cancel_tokens
                .lock()
                .map_err(|_| StoreError::LockPoisoned)?
                .insert(run_id.to_string(), token);
            return Err(error);
        }
        token.cancel();
        Ok(true)
    }

    /// Insert a new task into the plan, optionally after a given task id.
    /// Works in any non-terminal run state.
    /// Validates dependency integrity and acyclicity. Emits `PlanEdited`.
    pub fn insert_task(
        &self,
        run_id: &str,
        after_task_id: Option<String>,
        task: PlanTask,
    ) -> Result<(), StoreError> {
        // F2-1: 串行化该 run 的 plan 变更, 防 plan_create 并发覆写 plan.json。
        self.with_run_lock(run_id, || {
            // U1c phase-0/0bc step-2: file authority. Read the current plan from
            // the file (bootstrapping an empty plan if none exists), validate deps,
            // then append PlanEdited{insert} + rewrite plan.json. No SQL write.
            let run = self
                .get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            // Load current plan/tasks from the file (None if no plan yet).
            let current_plan = self.get_plan(run_id)?;
            let existing_tasks: Vec<PlanTask> = current_plan
                .as_ref()
                .map(|p| p.tasks.clone())
                .unwrap_or_default();

            // Lazy bootstrap: if no plan exists, emit a PlanGenerated first
            // (matching the SQL path that creates an empty plan row). The run
            // goal comes from the run header (already on file via create_run).
            if current_plan.is_none() {
                let new_plan_id = uuid::Uuid::new_v4().to_string();
                self.shadow.append_event_line(
                    run_id,
                    None,
                    None,
                    RuntimeEventKind::PlanGenerated,
                    serde_json::json!({
                        "plan_id": new_plan_id,
                        "task_count": 0,
                        "bootstrapped": true,
                        "domain_profile": run.domain_profile.as_str(),
                        "goal": run.goal,
                        "assumptions": Vec::<String>::new(),
                        "risks": Vec::<String>::new(),
                        "execution_mode": "parallel",
                    }),
                )?;
            }

            // Build the new task list: insert after `after_task_id` (or front).
            let mut new_tasks = existing_tasks.clone();
            let insert_pos = after_task_id
                .as_ref()
                .and_then(|id| new_tasks.iter().position(|t| &t.id == id))
                .map(|i| i + 1)
                .unwrap_or(0);
            let mut task_with_order = task.clone();
            task_with_order.sort_order = insert_pos as i64;
            new_tasks.insert(insert_pos, task_with_order.clone());

            // Validate deps (dangling refs + cycle detection).
            if let Err(errs) = super::planner::validate_plan_deps(&new_tasks) {
                return Err(StoreError::InvalidPlan(errs.join("; ")));
            }

            // Sprint 7: plan-time file-overlap advisory. Non-blocking — the write
            // semaphore already serializes all writers, so this is a scheduling
            // hint for when parallel writes are enabled (Sprint 8/9), surfaced
            // early so the user sees the plan risk at edit time.
            let report = super::planner::analyze_file_ownership(&new_tasks);
            if report.has_overlap() {
                for pair in &report.overlap_pairs {
                    tracing::warn!(
                        run_id,
                        task_a = %pair.task_a,
                        task_b = %pair.task_b,
                        shared = ?pair.shared,
                        "plan: writer tasks share files (will serialize; disjoint files enable parallel worktrees)"
                    );
                }
            }

            self.shadow.append_event_line(
                run_id,
                None,
                None,
                RuntimeEventKind::PlanEdited,
                serde_json::json!({
                    "action": "insert",
                    "task_id": task_with_order.id,
                    "after_task_id": after_task_id,
                    // Full PlanTask body so events.jsonl can rebuild plan.json.
                    "task": task_with_order,
                }),
            )?;
            self.shadow.rewrite_plan(run_id)?;
            Ok(())
        })
    }

    /// Soft-delete a task: set its status to `Skipped`. The task remains in
    /// the plan (for audit) but is no longer scheduled. Emits `PlanEdited`.
    pub fn remove_task(&self, run_id: &str, task_id: &str) -> Result<(), StoreError> {
        // If the task is currently running, cancel its worker FIRST (before
        // flipping status), so execution stops promptly instead of continuing
        // after the status says Skipped.
        let currently_running = self
            .list_todos(run_id)
            .ok()
            .and_then(|todos| {
                todos
                    .into_iter()
                    .find(|t| t.task_id == task_id)
                    .map(|t| t.status == TodoStatus::Running)
            })
            .unwrap_or(false);
        if currently_running {
            self.cancel_task(run_id, task_id);
        }
        self.set_task_status(
            run_id,
            task_id,
            TodoStatus::Skipped,
            None,
            Some("removed by user"),
        )?;
        // Emit PlanEdited (the set_task_status already emits TaskSkipped).
        // U1c phase-0/0bc step-2: file authority — append + rewrite, no SQL.
        self.shadow.append_event_line(
            run_id,
            None,
            None,
            RuntimeEventKind::PlanEdited,
            serde_json::json!({ "action": "remove", "task_id": task_id }),
        )?;
        self.shadow.rewrite_plan(run_id)?;
        Ok(())
    }

    /// Update a task with a partial patch. Only `Pending`/`Blocked` tasks can
    /// be fully updated; `Running` tasks can only change `title`/`description`
    /// (to avoid runtime tearing). `Completed`/`Failed`/`Skipped` tasks reject
    /// any update. Emits `PlanEdited`.
    pub fn update_task(
        &self,
        run_id: &str,
        task_id: &str,
        patch: TaskPatch,
    ) -> Result<(), StoreError> {
        self.with_run_lock(run_id, || {
        // U1c phase-0/0bc step-2: file authority. Read the task status + plan
        // from the file, validate (state guard + cycle on deps change), then
        // append PlanEdited{update} + rewrite plan.json. No SQL write.
        let plan = self
            .get_plan(run_id)?
            .ok_or(StoreError::PlanNotFound(run_id.to_string()))?;
        let current_task = plan
            .tasks
            .iter()
            .find(|t| t.id == task_id)
            .ok_or_else(|| StoreError::TaskNotFound(task_id.to_string()))?;
        let current_status = current_task.status;

        // State guard.
        match current_status {
            TodoStatus::Completed | TodoStatus::Failed | TodoStatus::Skipped => {
                return Err(StoreError::InvalidPlan(format!(
                    "cannot update task in terminal status {:?}",
                    current_status
                )));
            }
            TodoStatus::Running => {
                #[allow(clippy::collapsible_match)]
                // guard is a multi-field ||, not a single pattern; collapsing reads worse
                if patch.kind.is_some() || patch.depends_on.is_some() || patch.agent_role.is_some()
                {
                    return Err(StoreError::InvalidPlan(
                        "cannot change kind/depends_on/agent_role of a Running task".into(),
                    ));
                }
            }
            _ => {} // Pending/Blocked: all fields mutable.
        }

        // Re-validate cycle after changing deps.
        if let Some(deps) = &patch.depends_on {
            let mut tasks = plan.tasks.clone();
            if let Some(t) = tasks.iter_mut().find(|t| t.id == task_id) {
                t.depends_on = deps.clone();
            }
            if let Err(errs) = super::planner::validate_plan_deps(&tasks) {
                return Err(StoreError::InvalidPlan(errs.join("; ")));
            }
            // Sprint 7: plan-time file-overlap advisory (non-blocking; see insert_task).
            let report = super::planner::analyze_file_ownership(&tasks);
            if report.has_overlap() {
                for pair in &report.overlap_pairs {
                    tracing::warn!(
                        run_id,
                        task_a = %pair.task_a,
                        task_b = %pair.task_b,
                        shared = ?pair.shared,
                        "plan: writer tasks share files (will serialize; disjoint files enable parallel worktrees)"
                    );
                }
            }
        }

        self.shadow.append_event_line(
            run_id,
            None,
            None,
            RuntimeEventKind::PlanEdited,
            serde_json::json!({
                "action": "update",
                "task_id": task_id,
                // Applied patch fields so events.jsonl can rebuild plan.json.
                "patch": patch,
            }),
        )?;
        self.shadow.rewrite_plan(run_id)?;
        Ok(())
        })
    }

    /// Reorder non-terminal tasks. `new_order` must be a permutation of all
    /// task ids that are not in a terminal state (Completed/Failed/Skipped).
    /// Emits `PlanEdited`.
    pub fn reorder_tasks(&self, run_id: &str, new_order: Vec<String>) -> Result<(), StoreError> {
        self.with_run_lock(run_id, || {
            // U1c phase-0/0bc step-2: file authority. Read tasks from file,
            // validate new_order is a permutation of non-terminal task ids, then
            // append PlanEdited{reorder} + rewrite plan.json. No SQL write.
            let plan = self
                .get_plan(run_id)?
                .ok_or(StoreError::PlanNotFound(run_id.to_string()))?;
            let non_terminal: std::collections::HashSet<String> = plan
                .tasks
                .iter()
                .filter(|t| {
                    !matches!(
                        t.status,
                        TodoStatus::Completed | TodoStatus::Failed | TodoStatus::Skipped
                    )
                })
                .map(|t| t.id.clone())
                .collect();
            let new_set: std::collections::HashSet<String> = new_order.iter().cloned().collect();

            if non_terminal != new_set {
                return Err(StoreError::InvalidPlan(
                    "new_order must be a permutation of all non-terminal task ids".into(),
                ));
            }

            self.shadow.append_event_line(
                run_id,
                None,
                None,
                RuntimeEventKind::PlanEdited,
                serde_json::json!({
                    "action": "reorder",
                    // Full new ordering (task ids) so events.jsonl can rebuild plan.json.
                    "new_order": new_order,
                }),
            )?;
            self.shadow.rewrite_plan(run_id)?;
            Ok(())
        })
    }

    /// Attach a generated plan to a run, replacing any prior plan. Plan review
    /// is an artifact/tool interaction and does not introduce run states.
    pub fn attach_plan(&self, plan: &TaskPlan) -> Result<(), StoreError> {
        // F2-1: 串行化 plan 变更。
        self.with_run_lock(&plan.run_id, || {
            self.get_run(&plan.run_id)?
                .ok_or_else(|| StoreError::RunNotFound(plan.run_id.clone()))?;

            // U1c phase-0/0bc step-2: file authority. PlanGenerated carries the
            // full plan envelope + task bodies; the rebuilder reconstructs the
            // plan from it. No SQL write.
            self.shadow.append_event_line(
                plan.run_id.as_str(),
                None,
                None,
                RuntimeEventKind::PlanGenerated,
                serde_json::json!({
                    "plan_id": plan.plan_id,
                    "task_count": plan.tasks.len(),
                    "domain_profile": plan.domain_profile.as_str(),
                    "goal": plan.goal,
                    "assumptions": plan.assumptions,
                    "risks": plan.risks,
                    "execution_mode": plan.execution_mode,
                    // Full task bodies: attach_plan is the authoritative plan-creation path
                    // so PlanGenerated must carry the tasks for events.jsonl to rebuild plan.json.
                    "tasks": plan.tasks,
                }),
            )?;
            self.shadow.rewrite_plan(&plan.run_id)?;
            Ok(())
        })
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
        self.with_run_lock(run_id, || {
            // U1c phase-0/0bc step-2: file authority. Validate the task exists
            // (read plan from file), then append the Task*/TodoUpdated event with
            // explicit started_at/completed_at and rewrite plan.json. No SQL write.
            let plan = self
                .get_plan(run_id)?
                .ok_or(StoreError::PlanNotFound(run_id.to_string()))?;
            if !plan.tasks.iter().any(|t| t.id == task_id) {
                return Err(StoreError::TaskNotFound(task_id.to_string()));
            }
            let now = echo_agent::utils::time::now_local().to_rfc3339();
            let started = matches!(status, TodoStatus::Running);
            let finished = matches!(
                status,
                TodoStatus::Completed | TodoStatus::Failed | TodoStatus::Skipped
            );
            let kind = match status {
                TodoStatus::Running => RuntimeEventKind::TaskStarted,
                TodoStatus::Completed => RuntimeEventKind::TaskCompleted,
                TodoStatus::Failed => RuntimeEventKind::TaskFailed,
                TodoStatus::Skipped => RuntimeEventKind::TaskSkipped,
                TodoStatus::Blocked => RuntimeEventKind::TaskBlocked,
                TodoStatus::Pending => RuntimeEventKind::TodoUpdated,
            };
            self.shadow.append_event_line(
                run_id,
                Some(task_id),
                None,
                kind,
                serde_json::json!({
                    "status": status.as_str(),
                    "owner_agent": owner_agent,
                    "summary": summary,
                    // Explicit timestamps so events.jsonl can rebuild todo runtime fields
                    // without relying on the event `timestamp` as a proxy.
                    "started_at": if started { Some(now.as_str()) } else { None },
                    "completed_at": if finished { Some(now.as_str()) } else { None },
                }),
            )?;
            self.shadow.rewrite_plan(run_id)?;
            Ok(())
        })
    }

    /// Update a plan task's mutable fields (title, description, retry_count,
    /// failure_fingerprint, status) in place. Used by the review gate when a
    /// NeedsFix outcome produces a fix variant of a task — the fix shape must
    /// be persisted so a process restart doesn't lose retry progress or the
    /// review-informed brief. The task id is unchanged so downstream
    /// depends_on keeps resolving. Emits a `Note` event for traceability.
    pub fn update_plan_task(&self, run_id: &str, task: &PlanTask) -> Result<(), StoreError> {
        // F2-1: 串行化 plan 变更。
        self.with_run_lock(run_id, || {
            // U1c phase-0/0bc step-2: file authority. Validate the task exists, then
            // emit Note{fix_task_persisted} carrying the full task body so the
            // rebuilder can replace the task (see event_rebuild Note handler). No SQL.
            let plan = self
                .get_plan(run_id)?
                .ok_or(StoreError::PlanNotFound(run_id.to_string()))?;
            if !plan.tasks.iter().any(|t| t.id == task.id) {
                return Err(StoreError::TaskNotFound(task.id.clone()));
            }
            self.shadow.append_event_line(
                run_id,
                Some(task.id.as_str()),
                None,
                RuntimeEventKind::Note,
                serde_json::json!({
                    "kind": "fix_task_persisted",
                    "retry_count": task.retry_count,
                    "failure_fingerprint": task.failure_fingerprint,
                    // Full task body so events.jsonl can rebuild plan.json
                    // (the rebuilder replaces the matching task).
                    "task": task,
                }),
            )?;
            self.shadow.rewrite_plan(run_id)?;
            Ok(())
        })
    }

    // ── Reviews, artifacts, summaries ───────────────────────────────────

    pub fn add_review(&self, r: &ReviewResult) -> Result<(), StoreError> {
        self.with_run_lock(&r.run_id, || {
            // U1c phase-0/0bc step-2: file authority. Review* carries the full
            // review so FileTaskStore.list_reviews can derive it. No SQL.
            let kind = match r.outcome {
                ReviewOutcome::Pass => RuntimeEventKind::ReviewPassed,
                ReviewOutcome::NeedsFix => RuntimeEventKind::ReviewNeedsFix,
                ReviewOutcome::Blocked => RuntimeEventKind::ReviewBlocked,
            };
            self.shadow.append_event_line(
                r.run_id.as_str(),
                Some(r.task_id.as_str()),
                None,
                kind,
                serde_json::json!({
                    "review_id": r.id,
                    "reviewer": r.reviewer_agent,
                    "outcome": r.outcome.as_str(),
                    "issues": r.issues,
                    "failure_fingerprint": r.failure_fingerprint,
                    "created_fix_task_id": r.created_fix_task_id,
                    "created_at": echo_agent::utils::time::to_local(r.created_at).to_rfc3339(),
                }),
            )?;
            self.shadow.rewrite_plan(&r.run_id)?;
            Ok(())
        })
    }

    pub fn add_artifact(&self, a: &Artifact) -> Result<(), StoreError> {
        self.with_run_lock(&a.run_id, || {
            // U1c phase-0/0bc step-2: file authority. ArtifactProduced carries the
            // full artifact so FileTaskStore.list_artifacts can derive it. No SQL.
            self.shadow.append_event_line(
                a.run_id.as_str(),
                a.task_id.as_deref(),
                None,
                RuntimeEventKind::ArtifactProduced,
                serde_json::json!({
                    "artifact_id": a.id,
                    "kind": a.kind.as_str(),
                    "title": a.title,
                    "task_id": a.task_id,
                }),
            )?;
            self.shadow.rewrite_plan(&a.run_id)?;
            Ok(())
        })
    }

    /// Persist or overwrite the per-task execution summary. Primary key is
    /// `(run_id, task_id)` so a re-execution replaces the prior summary. The
    /// write is transactional and appends a `Note` event so the GUI and the
    /// recovery path can tell when a summary was updated (consistent with the
    /// "every state-relevant change writes a TaskEvent" invariant).
    pub fn put_summary(&self, s: &TaskExecutionSummary) -> Result<(), StoreError> {
        self.with_run_lock(&s.run_id, || {
            // U1c phase-0/0bc step-2: file authority. Note{summary_persisted}
            // carries the full summary so FileTaskStore.get_summary can derive it.
            self.shadow.append_event_line(
                s.run_id.as_str(),
                Some(s.task_id.as_str()),
                None,
                RuntimeEventKind::Note,
                serde_json::json!({
                    "kind": "summary_persisted",
                    // Full summary so events.jsonl can rebuild plan.json task summaries.
                    "summary": s,
                }),
            )?;
            self.shadow.rewrite_plan(&s.run_id)?;
            Ok(())
        })
    }

    // ── Read paths (used by Tauri query commands + recovery) ────────────

    pub fn get_run(&self, run_id: &str) -> Result<Option<TaskRun>, StoreError> {
        // U1c phase-0/0bc step-2: read delegates to the file store (file authority).
        self.file_store()
            .get_run(run_id)
            .map_err(|e| StoreError::InvalidPlan(format!("file read: {e}")))
    }

    /// Read just the `route` column for a given run. Returns `None` when the
    /// run does not exist.
    pub fn get_run_route(&self, run_id: &str) -> Result<Option<String>, StoreError> {
        // U1c phase-0/0bc step-2: delegate to file store, project the route field.
        self.file_store()
            .get_run(run_id)
            .map_err(|e| StoreError::InvalidPlan(format!("file read: {e}")))
            .map(|r| r.map(|r| r.route))
    }

    /// Latest run for a conversation (used by GUI to bind a chat to its run).
    pub fn latest_run_for_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<Option<TaskRun>, StoreError> {
        self.file_store()
            .latest_run_for_conversation(conversation_id)
            .map_err(|e| StoreError::InvalidPlan(format!("file read: {e}")))
    }

    /// Find an in-progress (Running or Paused) run for a conversation, if any.
    /// Used by the interrupt-detection logic: if a user sends a new message
    /// while a run is still executing, the system should prompt them rather
    /// than silently starting a second run.
    pub fn find_in_progress_run_by_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<Option<TaskRun>, StoreError> {
        self.file_store()
            .find_in_progress_run_by_conversation(conversation_id)
            .map_err(|e| StoreError::InvalidPlan(format!("file read: {e}")))
    }

    pub fn list_runs_in(&self, statuses: &[TaskRunStatus]) -> Result<Vec<TaskRun>, StoreError> {
        self.file_store()
            .list_runs_in(statuses)
            .map_err(|e| StoreError::InvalidPlan(format!("file read: {e}")))
    }

    /// Boot-time recovery of runs interrupted by a process restart (P1-8).
    ///
    /// Boot-time recovery of runs interrupted by a process restart.
    ///
    /// A run left in `Running` when the process died has durable plan/task/tool
    /// facts but no live driver. Move it to `Paused` so the normal resume path
    /// can re-read the plan and skip completed work. Pending/Paused are left
    /// untouched.
    /// Returns the number of runs recovered.
    ///
    /// Safe to call on an empty/fresh store (no-op).
    pub fn recover_incomplete(&self) -> usize {
        const INTERRUPTED: &[TaskRunStatus] = &[TaskRunStatus::Running];
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
            match self.transition_run(&run.run_id, TaskRunStatus::Paused) {
                Ok(_) => {
                    if let Ok(todos) = self.list_todos(&run.run_id) {
                        for todo in todos
                            .into_iter()
                            .filter(|todo| todo.status == TodoStatus::Running)
                        {
                            if let Err(error) = self.set_task_status(
                                &run.run_id,
                                &todo.task_id,
                                TodoStatus::Pending,
                                None,
                                Some("interrupted; pending resume"),
                            ) {
                                tracing::warn!(
                                    run_id = %run.run_id,
                                    task_id = %todo.task_id,
                                    %error,
                                    "recover_incomplete: failed to reset running task"
                                );
                            }
                        }
                    }
                    tracing::info!(
                        run_id = %run.run_id,
                        from = %run.status.as_str(),
                        "recovered interrupted run -> Paused at boot"
                    );
                }
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
                    "recover_incomplete: failed to transition run to Paused"
                ),
            }
        }
        count
    }

    pub fn get_plan(&self, run_id: &str) -> Result<Option<TaskPlan>, StoreError> {
        self.file_store()
            .get_plan(run_id)
            .map_err(|e| StoreError::InvalidPlan(format!("file read: {e}")))
    }

    pub fn list_todos(&self, run_id: &str) -> Result<Vec<TodoItem>, StoreError> {
        self.file_store()
            .list_todos(run_id)
            .map_err(|e| StoreError::InvalidPlan(format!("file read: {e}")))
    }

    pub fn list_events(
        &self,
        run_id: &str,
        since_seq: i64,
    ) -> Result<Vec<RuntimeTaskEvent>, StoreError> {
        self.file_store()
            .list_events(run_id, since_seq)
            .map_err(|e| StoreError::InvalidPlan(format!("file read: {e}")))
    }

    pub fn list_artifacts(&self, run_id: &str) -> Result<Vec<Artifact>, StoreError> {
        self.file_store()
            .list_artifacts(run_id)
            .map_err(|e| StoreError::InvalidPlan(format!("file read: {e}")))
    }

    pub fn list_reviews(
        &self,
        run_id: &str,
        task_id: &str,
    ) -> Result<Vec<ReviewResult>, StoreError> {
        // FileTaskStore.list_reviews returns all reviews for a run; filter
        // by task_id to match the SQL signature.
        self.file_store()
            .list_reviews(run_id)
            .map_err(|e| StoreError::InvalidPlan(format!("file read: {e}")))
            .map(|rs| rs.into_iter().filter(|r| r.task_id == task_id).collect())
    }

    pub fn get_summary(
        &self,
        run_id: &str,
        task_id: &str,
    ) -> Result<Option<TaskExecutionSummary>, StoreError> {
        self.file_store()
            .get_summary(run_id, task_id)
            .map_err(|e| StoreError::InvalidPlan(format!("file read: {e}")))
    }

    /// Append a free-form `Note` event for diagnostics / trace breadcrumbs.
    pub fn note(
        &self,
        run_id: &str,
        task_id: Option<&str>,
        message: &str,
    ) -> Result<(), StoreError> {
        // U1c phase-0/0bc step-2: file authority. A plain Note{message} does
        // not affect plan.json (the rebuilder only mutates the plan for
        // Note{kind: fix_task_persisted | summary_persisted}), so we skip the
        // rewrite — appending the event is enough.
        self.shadow.append_event_line(
            run_id,
            task_id,
            None,
            RuntimeEventKind::Note,
            serde_json::json!({ "message": message }),
        )?;
        Ok(())
    }

    /// Persist trigger/scheduling metadata without expanding the TaskRun state
    /// model. Consumers may rebuild this projection from the append-only event.
    pub fn record_trigger_metadata(
        &self,
        run_id: &str,
        source: &str,
        kind: &str,
        prompt: &str,
        priority: u8,
        dependencies: &[String],
    ) -> Result<(), StoreError> {
        self.shadow.append_event_line(
            run_id,
            None,
            None,
            RuntimeEventKind::Note,
            serde_json::json!({
                "kind": "trigger_metadata",
                "source": source,
                "task_kind": kind,
                "prompt": prompt,
                "priority": priority.min(10),
                "dependencies": dependencies,
            }),
        )?;
        Ok(())
    }

    pub fn record_execution_path(
        &self,
        run_id: &str,
        requested_mode: &str,
        observed_path: &str,
    ) -> Result<(), StoreError> {
        self.shadow.append_event_line(
            run_id,
            None,
            None,
            RuntimeEventKind::Note,
            serde_json::json!({
                "kind": "execution_path",
                "requested_mode": requested_mode,
                "observed_path": observed_path,
            }),
        )?;
        Ok(())
    }

    /// Persist a provider-reported LLM usage event for a subagent.
    ///
    /// This is intentionally a low-frequency structured event rather than raw
    /// token streaming. The event goes to the file authority (events.jsonl) for
    /// traceability; the usage record is held in memory for the GUI's token
    /// metrics (EKO is a local tool — usage is an ephemeral reference, not a
    /// ledger, and need not survive a restart).
    pub fn record_worker_llm_usage(
        &self,
        run_id: &str,
        task_id: &str,
        worker_id: &str,
        agent_name: &str,
        title: &str,
        payload: serde_json::Value,
    ) -> Result<(), StoreError> {
        // WorkerLlmUsage does not affect plan.json (the rebuilder ignores it),
        // so we append the event for traceability but skip the plan rewrite.
        self.shadow.append_event_line(
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
        // Hold the usage record in memory for query_usage_records / summaries.
        let record = super::types::UsageRecord {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: json_string(&payload, "session_id").unwrap_or_else(|| run_id.to_string()),
            run_id: Some(run_id.to_string()),
            worker_id: Some(worker_id.to_string()),
            model: json_string(&payload, "model").unwrap_or_else(|| "unknown".to_string()),
            provider: json_string(&payload, "provider"),
            route_kind: json_string(&payload, "route_kind")
                .or_else(|| Some("task_runtime".to_string())),
            input_tokens: json_u64(&payload, "prompt_tokens"),
            output_tokens: json_u64(&payload, "completion_tokens"),
            cached_input_tokens: json_u64(&payload, "cached_prompt_tokens"),
            cache_creation_input_tokens: json_u64(&payload, "cache_creation_prompt_tokens"),
            usage_reported: json_bool(&payload, "usage_reported", true),
            system_prompt_hash: json_string(&payload, "system_prompt_hash"),
            tools_schema_hash: json_string(&payload, "tools_schema_hash"),
            cwd_hash: json_string(&payload, "cwd_hash"),
            worker_prompt_hash: json_string(&payload, "worker_prompt_hash"),
            created_at: Utc::now(),
        };
        if let Ok(mut records) = self.usage_records.lock() {
            records.push(record);
        }
        Ok(())
    }
}

// ── JSON helpers for usage-record extraction ────────────────────────────

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

// The compile-time test that proves the transaction invariant:
// a state change without an event would leave the DB inconsistent.
// We assert both rows land together.
#[cfg(test)]
#[allow(clippy::items_after_test_module)] // usage-record impls below are production code kept here for locality with their tests; reordering is pure churn
mod tests {
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
                "",
                AttendedMode::Attended,
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
        let run = s.transition_run("r1", TaskRunStatus::Running).unwrap();
        assert_eq!(run.status, TaskRunStatus::Running);
        let evs = s.list_events("r1", 0).unwrap();
        // RunCreated + RunStatusChanged
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[1].event_type, RuntimeEventKind::RunStatusChanged);
    }

    #[test]
    fn illegal_transition_is_rejected_and_leaves_no_event() {
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
        // First transition to Running (was Pending → now legal).
        s.transition_run("r1", TaskRunStatus::Running).unwrap();
        // Running → Completed is legal. Now test that Completed → Running is
        // illegal (terminal state → non-terminal is always rejected).
        let _before = s.list_events("r1", 0).unwrap().len();
        s.transition_run("r1", TaskRunStatus::Completed).unwrap();
        let before_terminal = s.list_events("r1", 0).unwrap().len();
        let err = s.transition_run("r1", TaskRunStatus::Running).unwrap_err();
        assert!(matches!(err, StoreError::IllegalTransition { .. }));
        // No new event was appended — the tx rolled back.
        assert_eq!(s.list_events("r1", 0).unwrap().len(), before_terminal);
    }

    #[test]
    fn attach_plan_creates_tasks_and_todos() {
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
        // attach_plan no longer changes the run status; caller decides.
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
        // attach_plan no longer transitions status; run stays Pending.
        assert_eq!(run.status, TaskRunStatus::Pending);
        assert_eq!(run.plan_id.as_deref(), Some("p1"));
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
            suggested_tasks: vec![],
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
        s.create_run(
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
        std::thread::sleep(std::time::Duration::from_millis(10));
        s.create_run(
            "r2",
            "ws",
            "c1",
            "m2",
            DomainProfile::General,
            "g2",
            "",
            AttendedMode::Attended,
        )
        .unwrap();
        let latest = s.latest_run_for_conversation("c1").unwrap().unwrap();
        assert_eq!(latest.run_id, "r2");
    }

    fn seed_plan(s: &TaskRuntimeStore) {
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
        s.transition_run("r1", TaskRunStatus::Running).unwrap();
    }

    #[test]
    fn resume_task_run_transitions_paused_to_running() {
        let s = fresh();
        seed_plan(&s);
        // Simulate user interrupt: Running -> Paused.
        s.transition_run("r1", TaskRunStatus::Paused).unwrap();
        let run = s.get_run("r1").unwrap().unwrap();
        assert_eq!(run.status, TaskRunStatus::Paused);

        // Resume: Paused -> Running.
        let run = s.resume_task_run("r1").unwrap();
        assert_eq!(run.status, TaskRunStatus::Running);

        // Event log contains the Paused and Running transitions.
        let evs = s.list_events("r1", 0).unwrap();
        let status_changes: Vec<_> = evs
            .iter()
            .filter(|e| e.event_type == RuntimeEventKind::RunStatusChanged)
            .collect();
        assert!(status_changes.len() >= 2);
    }

    #[test]
    fn boot_recovery_pauses_run_and_preserves_completed_tasks() -> Result<(), StoreError> {
        let s = fresh();
        seed_plan(&s);
        s.set_task_status(
            "r1",
            "t1",
            TodoStatus::Completed,
            Some("explorer"),
            Some("verified"),
        )?;

        assert_eq!(s.recover_incomplete(), 1);
        let run = s
            .get_run("r1")?
            .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?;
        assert_eq!(run.status, TaskRunStatus::Paused);
        let todos = s.list_todos("r1")?;
        let task = todos
            .iter()
            .find(|todo| todo.task_id == "t1")
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        assert_eq!(task.status, TodoStatus::Completed);
        assert_eq!(task.summary.as_deref(), Some("verified"));
        Ok(())
    }

    #[test]
    fn pause_request_stops_driver_and_keeps_run_resumable() -> Result<(), StoreError> {
        let store = std::sync::Arc::new(fresh());
        seed_plan(&store);
        store.set_task_status("r1", "t1", TodoStatus::Running, Some("worker"), None)?;
        let token = echo_agent::agent::CancellationToken::new();
        let _registration = store.register_run_cancellation("r1", token.clone())?;

        assert!(store.request_pause("r1")?);
        assert!(token.is_cancelled());
        let run = store
            .get_run("r1")?
            .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?;
        assert_eq!(run.status, TaskRunStatus::Paused);
        Ok(())
    }

    #[test]
    fn boot_recovery_requeues_orphaned_running_task() -> Result<(), StoreError> {
        let store = fresh();
        seed_plan(&store);
        store.set_task_status("r1", "t1", TodoStatus::Running, Some("worker"), None)?;

        assert_eq!(store.recover_incomplete(), 1);
        let todo = store
            .list_todos("r1")?
            .into_iter()
            .find(|todo| todo.task_id == "t1")
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        assert_eq!(todo.status, TodoStatus::Pending);
        assert_eq!(todo.summary.as_deref(), Some("interrupted; pending resume"));
        Ok(())
    }

    #[test]
    fn find_in_progress_run_by_conversation_returns_running() {
        let s = fresh();
        seed_plan(&s); // run "r1" in conversation "c1" is now Running.
        let found = s.find_in_progress_run_by_conversation("c1").unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().run_id, "r1");
    }

    #[test]
    fn find_in_progress_run_by_conversation_returns_paused() {
        let s = fresh();
        seed_plan(&s);
        s.transition_run("r1", TaskRunStatus::Paused).unwrap();
        let found = s.find_in_progress_run_by_conversation("c1").unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().run_id, "r1");
    }

    #[test]
    fn find_in_progress_run_by_conversation_returns_none_for_completed() {
        let s = fresh();
        seed_plan(&s);
        s.transition_run("r1", TaskRunStatus::Completed).unwrap();
        let found = s.find_in_progress_run_by_conversation("c1").unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn insert_task_adds_to_plan_and_emits_plan_edited() {
        let s = fresh();
        seed_plan(&s);
        // Plan has one task "t1". Insert "t2" after "t1".
        let t2 = PlanTask {
            id: "t2".into(),
            title: "Second task".into(),
            kind: PlanTaskKind::Implementation,
            depends_on: vec!["t1".into()],
            ..Default::default()
        };
        s.insert_task("r1", Some("t1".into()), t2).unwrap();

        let plan = s.get_plan("r1").unwrap().unwrap();
        assert_eq!(plan.tasks.len(), 2);
        assert_eq!(plan.tasks[0].id, "t1");
        assert_eq!(plan.tasks[1].id, "t2");

        let todos = s.list_todos("r1").unwrap();
        assert!(todos.iter().any(|t| t.task_id == "t2"));

        let evs = s.list_events("r1", 0).unwrap();
        assert!(
            evs.iter()
                .any(|e| e.event_type == RuntimeEventKind::PlanEdited)
        );
    }

    #[test]
    fn insert_task_rejects_missing_run() -> std::result::Result<(), String> {
        let s = TaskRuntimeStore::new_in_memory().map_err(|e| e.to_string())?;
        let task = PlanTask {
            id: "t1".into(),
            title: "orphan task".into(),
            kind: PlanTaskKind::Investigation,
            ..Default::default()
        };

        let err = s
            .insert_task("missing-run", None, task)
            .err()
            .ok_or_else(|| "insert_task unexpectedly succeeded without a run".to_string())?;
        assert!(matches!(err, StoreError::RunNotFound(run_id) if run_id == "missing-run"));

        let events = s.list_events("missing-run", 0).map_err(|e| e.to_string())?;
        assert!(events.is_empty());
        Ok(())
    }

    #[test]
    fn remove_task_soft_deletes_via_skipped() {
        let s = fresh();
        seed_plan(&s);
        s.remove_task("r1", "t1").unwrap();
        let todos = s.list_todos("r1").unwrap();
        let t1 = todos.iter().find(|t| t.task_id == "t1").unwrap();
        assert_eq!(t1.status, TodoStatus::Skipped);
    }

    #[test]
    fn update_task_applies_title_patch() {
        let s = fresh();
        seed_plan(&s);
        s.update_task(
            "r1",
            "t1",
            TaskPatch {
                title: Some("Updated title".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let plan = s.get_plan("r1").unwrap().unwrap();
        assert_eq!(plan.tasks[0].title, "Updated title");
    }

    #[test]
    fn update_task_rejects_terminal_status() {
        let s = fresh();
        seed_plan(&s);
        s.set_task_status("r1", "t1", TodoStatus::Completed, None, None)
            .unwrap();
        let err = s
            .update_task(
                "r1",
                "t1",
                TaskPatch {
                    title: Some("nope".into()),
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert!(matches!(err, StoreError::InvalidPlan(_)));
    }

    // ── review #4: intent-visible tests that validation fires on the FILE
    //    authority path (not just transitively). Each asserts the error is
    //    returned AND no event line was appended — proving the file-path
    //    validation branch rejected before writing. ──────────────────────

    /// `transition_run` rejects an illegal transition on the file path and
    /// appends no event. (Completed → Running is always illegal.)
    #[test]
    fn file_path_rejects_illegal_transition_and_appends_no_event() {
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
        s.transition_run("r1", TaskRunStatus::Running).unwrap();
        s.transition_run("r1", TaskRunStatus::Completed).unwrap();
        let before = s.list_events("r1", 0).unwrap().len();
        let err = s.transition_run("r1", TaskRunStatus::Running).unwrap_err();
        assert!(matches!(err, StoreError::IllegalTransition { .. }));
        // No new event appended — the file-path validation rejected before writing.
        assert_eq!(s.list_events("r1", 0).unwrap().len(), before);
    }

    /// `insert_task` rejects a dependency cycle on the file path and appends no
    /// event. t1 depends on t2, t2 depends on t1 → cycle.
    #[test]
    fn file_path_rejects_dependency_cycle_and_appends_no_event() {
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
        // t1 with no deps (bootstraps the plan).
        s.insert_task(
            "r1",
            None,
            PlanTask {
                id: "t1".into(),
                depends_on: Vec::new(),
                ..sample_task_body("t1")
            },
        )
        .unwrap();
        // t2 depends on t1 (legal).
        s.insert_task(
            "r1",
            None,
            PlanTask {
                id: "t2".into(),
                depends_on: vec!["t1".into()],
                ..sample_task_body("t2")
            },
        )
        .unwrap();
        let before = s.list_events("r1", 0).unwrap().len();
        // Now make t1 depend on t2 → cycle. update_task must reject on the file path.
        let err = s
            .update_task(
                "r1",
                "t1",
                TaskPatch {
                    depends_on: Some(vec!["t2".into()]),
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert!(matches!(err, StoreError::InvalidPlan(_)));
        assert_eq!(s.list_events("r1", 0).unwrap().len(), before);
    }

    /// `set_task_status` rejects an unknown task on the file path and appends
    /// no event.
    #[test]
    fn file_path_rejects_unknown_task_and_appends_no_event() {
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
        let before = s.list_events("r1", 0).unwrap().len();
        let err = s
            .set_task_status("r1", "nope", TodoStatus::Running, None, None)
            .unwrap_err();
        assert!(matches!(err, StoreError::TaskNotFound(_)));
        assert_eq!(s.list_events("r1", 0).unwrap().len(), before);
    }

    /// Helper: a minimal `PlanTask` body with the given id and sane defaults,
    /// for the cycle test above (avoids repeating the full struct literal).
    fn sample_task_body(id: &str) -> PlanTask {
        PlanTask {
            id: id.to_string(),
            title: format!("task {id}"),
            description: format!("do {id}"),
            kind: PlanTaskKind::Investigation,
            agent_role: "explorer".to_string(),
            domain_profile: DomainProfile::General,
            depends_on: Vec::new(),
            parallel_group: None,
            files: Vec::new(),
            allowed_tools: vec!["read_file".to_string()],
            verification: Vec::new(),
            retry_count: 0,
            max_retries: 3,
            failure_fingerprint: None,
            status: TodoStatus::Pending,
            sort_order: 0,
        }
    }
}

// ── Usage records (in-memory) ──────────────────────────────────────────

impl TaskRuntimeStore {
    /// Insert a usage record (in-memory; not persisted across restarts).
    pub fn insert_usage_record(
        &self,
        record: &super::types::UsageRecord,
    ) -> Result<(), StoreError> {
        if let Ok(mut records) = self.usage_records.lock() {
            // INSERT OR REPLACE semantics: replace an existing record with the
            // same id, else push. (Matches the old SQL `ON CONFLICT` behavior.)
            if let Some(existing) = records.iter_mut().find(|r| r.id == record.id) {
                *existing = record.clone();
            } else {
                records.push(record.clone());
            }
        }
        Ok(())
    }

    /// Query usage records with optional filters. In-memory equivalent of the
    /// old SQL `SELECT ... WHERE ... ORDER BY created_at DESC LIMIT/OFFSET`.
    pub fn query_usage_records(
        &self,
        filter: &super::types::UsageQueryFilter,
    ) -> Result<Vec<super::types::UsageRecord>, StoreError> {
        let records = self
            .usage_records
            .lock()
            .map_err(|_| StoreError::LockPoisoned)?;
        let mut out: Vec<super::types::UsageRecord> = records
            .iter()
            .filter(|r| {
                filter
                    .session_id
                    .as_deref()
                    .is_none_or(|v| r.session_id == v)
            })
            .filter(|r| {
                filter
                    .run_id
                    .as_deref()
                    .is_none_or(|v| r.run_id.as_deref() == Some(v))
            })
            .filter(|r| filter.model.as_deref().is_none_or(|v| r.model == v))
            .filter(|r| {
                filter
                    .provider
                    .as_deref()
                    .is_none_or(|v| r.provider.as_deref() == Some(v))
            })
            .filter(|r| {
                filter
                    .route_kind
                    .as_deref()
                    .is_none_or(|v| r.route_kind.as_deref() == Some(v))
            })
            .filter(|r| {
                filter
                    .created_after
                    .is_none_or(|after| r.created_at >= after)
            })
            .filter(|r| {
                filter
                    .created_before
                    .is_none_or(|before| r.created_at <= before)
            })
            .cloned()
            .collect();
        // ORDER BY created_at DESC (stable sort preserves insertion order on ties).
        out.sort_by_key(|a| std::cmp::Reverse(a.created_at));
        // LIMIT / OFFSET.
        let offset = filter.offset.unwrap_or(0) as usize;
        if offset >= out.len() {
            return Ok(Vec::new());
        }
        let limited = if let Some(limit) = filter.limit {
            out[offset..].iter().take(limit as usize).cloned().collect()
        } else {
            out[offset..].to_vec()
        };
        Ok(limited)
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
}
