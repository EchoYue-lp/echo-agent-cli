//! File-backed canonical store for the TaskRuntime.
//!
//! The file system (`events.jsonl` plus deterministic `plan.json` and
//! `run-state.json` projections) is the source of truth for task/plan data. Usage records and
//! conversation-replay events are held in memory (EKO is a local tool; these
//! are ephemeral and need not survive a restart). No SQLite dependency.
//!
//! Every state mutation appends a [`RuntimeTaskEvent`] to `events.jsonl` and
//! refreshes only the affected projection from the full event stream.

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
    #[error("plan revision conflict for run {run_id}: expected {expected}, current {current}")]
    PlanConflict {
        run_id: String,
        expected: u64,
        current: u64,
    },
    #[error("file shadow: {0}")]
    Shadow(#[from] super::file_shadow::ShadowError),
    #[error("run {run_id} has unresolved recovery barriers: {details}")]
    RecoveryBlocked { run_id: String, details: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimWriteOutcome {
    Applied,
    Superseded,
}

/// File-backed TaskRuntime store. One instance per process; cheap to clone
/// behind `Arc`. The event stream is authoritative; plan and execution files
/// are deterministic read projections.
pub struct TaskRuntimeStore {
    /// Per-task cancellation tokens (in-memory runtime state, not persisted).
    /// Key = `"{run_id}::{task_id}"`. `execute_task` registers a token when a
    /// task starts and removes it on completion; runtime control actions use
    /// the token to stop that Subagent promptly.
    task_cancel_tokens:
        std::sync::Mutex<std::collections::HashMap<String, echo_agent::agent::CancellationToken>>,
    /// Active TaskRun driver tokens. Every entry point registers here so pause
    /// and cancel target the real executor instead of a surface-local map.
    run_cancel_tokens:
        std::sync::Mutex<std::collections::HashMap<String, echo_agent::agent::CancellationToken>>,
    /// File-backed event authority and deterministic projections.
    shadow: std::sync::Arc<super::file_shadow::FileTaskShadow>,
    /// Per-run plan/state 写互斥锁 (F2-1 / F3-3 / F3-4)。
    ///
    /// revision compare-and-commit / transition_run 都是
    /// "读事件 → 校验 → 追加 → 重建投影"事务, 必须按 run 串行化。
    /// Different runs keep independent locks.
    plan_locks: dashmap::DashMap<String, std::sync::Arc<std::sync::Mutex<()>>>,
}

/// RAII registration for one active TaskRun driver. Nested drivers for the
/// same run restore the previous token when they finish (for example an
/// unattended ReAct driver invoking `task_execute`).
pub struct RunCancellationRegistration {
    store: std::sync::Arc<TaskRuntimeStore>,
    run_id: String,
    token: echo_agent::agent::CancellationToken,
    previous: Option<echo_agent::agent::CancellationToken>,
}

#[cfg(test)]
fn validate_runtime_plan(tasks: &[PlanTask]) -> Result<(), StoreError> {
    let runtime_tasks = tasks.iter().map(PlanTask::to_task).collect::<Vec<_>>();
    echo_agent::tasks::PlanValidator::default()
        .validate_task_snapshot(&runtime_tasks)
        .map_err(|errors| StoreError::InvalidPlan(errors.join("; ")))
}

#[derive(Debug, Clone)]
struct ActiveSubagentBoundary {
    task_id: String,
    execution_id: String,
    replay_safe: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecoverableSubagentResult {
    pub(crate) result: SubagentTaskResult,
    pub(crate) full_output: String,
}

#[derive(Debug, Clone)]
struct ActiveToolBoundary {
    task_id: String,
    execution_id: Option<String>,
    call_id: String,
    tool_name: String,
    replay_safe: bool,
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
    /// task/plan data lives under the file shadow root (`~/.eko/tasks/`);
    /// No database is opened, so this
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
    /// `events.jsonl` / projection files back directly and so runs are isolated
    /// under a known directory. Replaces the old `attach_shadow` test hook.
    pub fn new_in_memory_with_shadow_root(shadow_root: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let shadow = std::sync::Arc::new(super::file_shadow::FileTaskShadow::new(shadow_root));
        Ok(Self {
            task_cancel_tokens: std::sync::Mutex::new(std::collections::HashMap::new()),
            run_cancel_tokens: std::sync::Mutex::new(std::collections::HashMap::new()),
            shadow,
            plan_locks: dashmap::DashMap::new(),
        })
    }

    /// 在持有某 run 的 plan/state 写锁期间执行闭包 (F2-1 / F3-3 / F3-4)。
    ///
    /// 用 closure 模式而非返回 Guard: std::sync::MutexGuard 借自 &Mutex, 而
    /// Mutex 在 Arc 内, Arc 作为局部变量时 Guard 跨函数返回即悬垂 (自引用
    /// struct 在 Rust 里无法直接表达)。closure 把锁的获取与释放封装在内部,
    /// 闭包体内是临界区。revision compare-and-commit / transition_run 用它包裹
    /// "读事件 → 校验 → 追加 → 重建投影"全程。
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
        let blockers = self.list_recovery_blockers(run_id)?;
        if !blockers.is_empty() {
            let details = blockers
                .iter()
                .map(|blocker| format!("{}: {}", blocker.task_id, blocker.reason))
                .collect::<Vec<_>>()
                .join("; ");
            return Err(StoreError::RecoveryBlocked {
                run_id: run_id.to_string(),
                details,
            });
        }
        self.transition_run(run_id, TaskRunStatus::Running)
    }

    /// Atomically mark a running run completed only when the latest committed
    /// revision is quiescent. A concurrent plan patch wins the same run lock
    /// and makes this return `false`, causing the executor to drain again.
    pub fn complete_run_if_quiescent(&self, run_id: &str) -> Result<bool, StoreError> {
        self.with_run_lock(run_id, || {
            let run = self
                .get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            if run.status == TaskRunStatus::Completed {
                return Ok(true);
            }
            if run.status != TaskRunStatus::Running {
                return Ok(false);
            }
            let plan = self
                .get_plan(run_id)?
                .ok_or_else(|| StoreError::PlanNotFound(run_id.to_string()))?;
            if plan
                .tasks
                .iter()
                .any(|task| !matches!(task.status, TodoStatus::Completed | TodoStatus::Skipped))
            {
                return Ok(false);
            }
            self.shadow.append_event_line(
                run_id,
                None,
                None,
                RuntimeEventKind::RunStatusChanged,
                serde_json::json!({
                    "from": TaskRunStatus::Running.as_str(),
                    "to": TaskRunStatus::Completed.as_str(),
                    "plan_revision": plan.revision,
                }),
            )?;
            self.shadow.rewrite_plan(run_id)?;
            Ok(true)
        })
    }

    // ── Task-level cancellation ────────────────────────────────────────────
    // These in-memory tokens let runtime control actions stop one Subagent
    // promptly without changing the immutable task specification.

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

    /// Cancel a specific task's Subagent if one is currently running.
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

    /// Whether this process currently owns a live driver for `run_id`.
    /// Persisted `Running` alone is insufficient because a killed/restarted
    /// process can leave that status behind; cleanup uses this in-memory fact
    /// to avoid touching a worktree that an active run still owns.
    pub fn is_run_active(&self, run_id: &str) -> bool {
        self.run_cancel_tokens
            .lock()
            .map(|map| map.contains_key(run_id))
            .unwrap_or(false)
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
    /// run-scoped token used for cancellation stops in-flight Subagents. The
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

    /// Unit-test fixture helper for committing a prepared initial plan.
    #[cfg(test)]
    pub(crate) fn attach_plan_for_test(&self, plan: &TaskPlan) -> Result<(), StoreError> {
        self.with_run_lock(&plan.run_id, || {
            let run = self
                .get_run(&plan.run_id)?
                .ok_or_else(|| StoreError::RunNotFound(plan.run_id.clone()))?;
            if matches!(
                run.status,
                TaskRunStatus::Completed | TaskRunStatus::Cancelled
            ) {
                return Err(StoreError::InvalidPlan(format!(
                    "cannot create a plan for terminal run {} ({:?})",
                    plan.run_id, run.status
                )));
            }
            if self.get_plan(&plan.run_id)?.is_some() {
                return Err(StoreError::InvalidPlan(
                    "plan already exists; submit a revisioned task_update".to_string(),
                ));
            }
            if plan.tasks.iter().any(|task| {
                task.status != TodoStatus::Pending
                    || task.retry_count != 0
                    || task.failure_fingerprint.is_some()
            }) {
                return Err(StoreError::InvalidPlan(
                    "initial plan tasks must have pending execution state".to_string(),
                ));
            }
            validate_runtime_plan(&plan.tasks)?;
            let mut committed = plan.clone();
            committed.revision = 1;
            self.shadow.append_event_line(
                plan.run_id.as_str(),
                None,
                None,
                RuntimeEventKind::PlanRevisionCommitted,
                serde_json::json!({
                    "base_revision": 0,
                    "reason": "initial complete plan",
                    "plan": committed.specification(),
                }),
            )?;
            self.shadow.rewrite_plan(&plan.run_id)?;
            Ok(())
        })
    }

    /// Load the product-neutral framework graph without projecting rich task
    /// execution states through EKO's smaller UI status enum.
    pub(crate) fn load_revisioned_task_graph(
        &self,
        run_id: &str,
    ) -> Result<Option<echo_agent::tasks::RevisionedTaskGraph>, StoreError> {
        let Some(plan) = self.shadow.read_plan(run_id)? else {
            return Ok(None);
        };
        let state = self
            .shadow
            .read_run_state(run_id)?
            .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
        let mut executions = state
            .tasks
            .into_iter()
            .map(|execution| (execution.task_id.clone(), execution))
            .collect::<std::collections::HashMap<_, _>>();
        let mut tasks = Vec::with_capacity(plan.tasks.len());
        for spec in plan.tasks {
            let execution = executions
                .remove(&spec.id)
                .unwrap_or_else(|| EkoTaskExecution::pending(spec.id.clone()));
            let metadata = serde_json::to_value(EkoTaskMetadata {
                domain_profile: spec.domain_profile,
                parallel_group: spec.parallel_group,
                sort_order: spec.sort_order,
            })?;
            tasks.push(echo_agent::tasks::Task {
                spec: echo_agent::tasks::TaskSpec {
                    id: spec.id,
                    title: spec.title,
                    description: spec.description,
                    kind: spec.kind.to_task_kind(),
                    agent_role: spec.agent_role,
                    depends_on: spec.depends_on,
                    files: spec.files,
                    allowed_tools: spec.allowed_tools,
                    required_artifacts: spec.required_artifacts,
                    execution_checks: spec.execution_checks,
                    acceptance_criteria: spec.acceptance_criteria,
                    max_retries: spec.max_retries,
                    metadata,
                },
                execution: echo_agent::tasks::TaskExecution {
                    task_id: execution.task_id,
                    status: execution.status,
                    retry_count: execution.retry_count,
                    failure_fingerprint: execution.failure_fingerprint,
                    claim: execution.claim,
                },
            });
        }
        let context_metadata = serde_json::to_value(EkoPlanMetadata {
            plan_id: plan.plan_id,
            domain_profile: plan.domain_profile,
        })?;
        Ok(Some(echo_agent::tasks::RevisionedTaskGraph {
            snapshot: echo_agent::tasks::RuntimePlanSnapshot {
                revision: plan.revision,
                tasks,
            },
            context: echo_agent::tasks::TaskGraphContext {
                goal: plan.goal,
                assumptions: plan.assumptions,
                risks: plan.risks,
                execution_mode: match plan.execution_mode {
                    ExecutionMode::Sequential => {
                        echo_agent::tasks::TaskGraphExecutionMode::Sequential
                    }
                    ExecutionMode::Parallel => echo_agent::tasks::TaskGraphExecutionMode::Parallel,
                },
                metadata: context_metadata,
            },
        }))
    }

    /// Persist one framework-computed graph candidate with optimistic
    /// concurrency. Patch semantics and DAG validation have already run in
    /// `TaskRevisionService`; this adapter only validates EKO metadata and
    /// commits the file event/projections atomically.
    pub(crate) fn compare_and_commit_revisioned_task_graph(
        &self,
        run_id: &str,
        commit: echo_agent::tasks::TaskGraphCommit,
    ) -> Result<echo_agent::tasks::RevisionedTaskGraph, StoreError> {
        self.with_run_lock(run_id, || {
            let run = self
                .get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            if matches!(
                run.status,
                TaskRunStatus::Completed | TaskRunStatus::Cancelled
            ) {
                return Err(StoreError::InvalidPlan(format!(
                    "cannot modify terminal run {} ({:?})",
                    run_id, run.status
                )));
            }
            let current = self.load_revisioned_task_graph(run_id)?;
            let current_revision = current.as_ref().map(|graph| graph.snapshot.revision);
            if current_revision != commit.expected_revision {
                return Err(StoreError::PlanConflict {
                    run_id: run_id.to_string(),
                    expected: commit.expected_revision.unwrap_or_default(),
                    current: current_revision.unwrap_or_default(),
                });
            }
            let expected_revision = commit
                .expected_revision
                .unwrap_or_default()
                .checked_add(1)
                .ok_or_else(|| StoreError::InvalidPlan("plan revision overflow".to_string()))?;
            if commit.next.snapshot.revision != expected_revision {
                return Err(StoreError::InvalidPlan(format!(
                    "invalid next plan revision: expected {expected_revision}, got {}",
                    commit.next.snapshot.revision
                )));
            }
            let plan_metadata: EkoPlanMetadata =
                serde_json::from_value(commit.next.context.metadata.clone())?;
            let mut specifications = Vec::with_capacity(commit.next.snapshot.tasks.len());
            for task in &commit.next.snapshot.tasks {
                if task.spec.id != task.execution.task_id {
                    return Err(StoreError::InvalidPlan(format!(
                        "task spec id '{}' does not match execution id '{}'",
                        task.spec.id, task.execution.task_id
                    )));
                }
                let metadata: EkoTaskMetadata = serde_json::from_value(task.spec.metadata.clone())?;
                specifications.push(EkoTaskSpec {
                    id: task.spec.id.clone(),
                    title: task.spec.title.clone(),
                    description: task.spec.description.clone(),
                    kind: PlanTaskKind::from_task_kind(task.spec.kind),
                    agent_role: task.spec.agent_role.clone(),
                    domain_profile: metadata.domain_profile,
                    depends_on: task.spec.depends_on.clone(),
                    parallel_group: metadata.parallel_group,
                    files: task.spec.files.clone(),
                    allowed_tools: task.spec.allowed_tools.clone(),
                    required_artifacts: task.spec.required_artifacts.clone(),
                    execution_checks: task.spec.execution_checks.clone(),
                    acceptance_criteria: task.spec.acceptance_criteria.clone(),
                    max_retries: task.spec.max_retries,
                    sort_order: metadata.sort_order,
                });
            }
            if commit.expected_revision.is_none()
                && commit.next.snapshot.tasks.iter().any(|task| {
                    task.execution.status != echo_agent::tasks::TaskStatus::Pending
                        || task.execution.retry_count != 0
                        || task.execution.failure_fingerprint.is_some()
                        || task.execution.claim.is_some()
                })
            {
                return Err(StoreError::InvalidPlan(
                    "initial plan tasks must have pending execution state".to_string(),
                ));
            }
            let plan = PlanRevision {
                plan_id: plan_metadata.plan_id,
                run_id: run_id.to_string(),
                revision: commit.next.snapshot.revision,
                domain_profile: plan_metadata.domain_profile,
                goal: commit.next.context.goal,
                assumptions: commit.next.context.assumptions,
                risks: commit.next.context.risks,
                execution_mode: match commit.next.context.execution_mode {
                    echo_agent::tasks::TaskGraphExecutionMode::Sequential => {
                        ExecutionMode::Sequential
                    }
                    echo_agent::tasks::TaskGraphExecutionMode::Parallel => ExecutionMode::Parallel,
                },
                tasks: specifications,
            };
            self.shadow.append_event_line(
                run_id,
                None,
                None,
                RuntimeEventKind::PlanRevisionCommitted,
                serde_json::json!({
                    "base_revision": commit.expected_revision.unwrap_or_default(),
                    "reason": commit.reason,
                    "skipped_task_ids": commit.effects.skipped_task_ids,
                    "reset_task_ids": commit.effects.reset_task_ids,
                    "plan": plan,
                }),
            )?;
            self.shadow.rewrite_plan(run_id)?;
            self.load_revisioned_task_graph(run_id)?
                .ok_or_else(|| StoreError::PlanNotFound(run_id.to_string()))
        })
    }

    /// Unit-test convenience for exercising the canonical framework patch
    /// engine through EKO's file commit adapter.
    #[cfg(test)]
    pub(crate) fn apply_task_patch_for_test(
        &self,
        run_id: &str,
        request: &TaskUpdateRequest,
    ) -> Result<TaskPlan, StoreError> {
        self.get_run(run_id)?
            .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
        let current = self
            .load_revisioned_task_graph(run_id)?
            .ok_or_else(|| StoreError::PlanNotFound(run_id.to_string()))?;
        if current.snapshot.revision != request.base_revision {
            return Err(StoreError::PlanConflict {
                run_id: run_id.to_string(),
                expected: request.base_revision,
                current: current.snapshot.revision,
            });
        }
        if request.reason.trim().is_empty() {
            return Err(StoreError::InvalidPlan(
                "task_update requires a non-empty reason".to_string(),
            ));
        }
        let patch = request
            .to_task_plan_patch()
            .map_err(StoreError::InvalidPlan)?;
        let application = echo_agent::tasks::TaskPatchEngine::apply_operations(
            &current.snapshot.tasks,
            patch.operations,
            false,
        )
        .map_err(|error| StoreError::InvalidPlan(error.to_string()))?;
        echo_agent::tasks::PlanValidator::default()
            .validate_task_snapshot(&application.tasks)
            .map_err(|errors| StoreError::InvalidPlan(errors.join("; ")))?;
        let next_revision = current
            .snapshot
            .revision
            .checked_add(1)
            .ok_or_else(|| StoreError::InvalidPlan("plan revision overflow".to_string()))?;
        self.compare_and_commit_revisioned_task_graph(
            run_id,
            echo_agent::tasks::TaskGraphCommit {
                expected_revision: Some(current.snapshot.revision),
                next: echo_agent::tasks::RevisionedTaskGraph {
                    snapshot: echo_agent::tasks::RuntimePlanSnapshot {
                        revision: next_revision,
                        tasks: application.tasks,
                    },
                    context: current.context,
                },
                reason: patch.reason,
                effects: application.effects,
            },
        )?;
        self.get_plan(run_id)?
            .ok_or_else(|| StoreError::PlanNotFound(run_id.to_string()))
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
            self.append_task_status_event(run_id, task_id, status, owner_agent, summary, None)
        })
    }

    /// Atomically claim a Pending task from one exact plan revision.
    pub fn claim_task(
        &self,
        run_id: &str,
        expected_task: &echo_agent::tasks::Task,
        expected_revision: u64,
    ) -> Result<echo_agent::tasks::RuntimeTaskClaimOutcome, StoreError> {
        self.with_run_lock(run_id, || {
            let plan = self
                .get_plan(run_id)?
                .ok_or_else(|| StoreError::PlanNotFound(run_id.to_string()))?;
            if plan.revision != expected_revision {
                return Ok(echo_agent::tasks::RuntimeTaskClaimOutcome::ReloadSnapshot);
            }
            let Some(task) = plan
                .tasks
                .iter()
                .find(|task| task.id == expected_task.spec.id)
            else {
                return Ok(echo_agent::tasks::RuntimeTaskClaimOutcome::ReloadSnapshot);
            };
            let current = task.to_task();
            if task.status != TodoStatus::Pending || current.spec != expected_task.spec {
                return Ok(echo_agent::tasks::RuntimeTaskClaimOutcome::ReloadSnapshot);
            }
            let claim = echo_agent::tasks::TaskClaim {
                revision: expected_revision,
                attempt: task.retry_count.saturating_add(1),
                spec_hash: current
                    .spec
                    .stable_hash()
                    .map_err(StoreError::InvalidPlan)?,
            };
            self.append_task_status_event(
                run_id,
                &task.id,
                TodoStatus::Running,
                Some(&task.agent_role),
                None,
                Some(&claim),
            )?;
            Ok(echo_agent::tasks::RuntimeTaskClaimOutcome::Claimed(claim))
        })
    }

    /// Commit a status only if the same claimed attempt is still Running.
    pub fn set_claimed_task_status(
        &self,
        run_id: &str,
        task_id: &str,
        claim: &echo_agent::tasks::TaskClaim,
        status: TodoStatus,
        owner_agent: Option<&str>,
        summary: Option<&str>,
    ) -> Result<ClaimWriteOutcome, StoreError> {
        self.with_run_lock(run_id, || {
            let plan = self
                .get_plan(run_id)?
                .ok_or_else(|| StoreError::PlanNotFound(run_id.to_string()))?;
            let Some(task) = plan.tasks.iter().find(|task| task.id == task_id) else {
                return Ok(ClaimWriteOutcome::Superseded);
            };
            if task.status != TodoStatus::Running || task.claim.as_ref() != Some(claim) {
                return Ok(ClaimWriteOutcome::Superseded);
            }
            self.append_task_status_event(
                run_id,
                task_id,
                status,
                owner_agent,
                summary,
                Some(claim),
            )?;
            Ok(ClaimWriteOutcome::Applied)
        })
    }

    /// Atomically requeue one failed claimed attempt and advance its retry
    /// counter without exposing an unclaimed Pending window.
    pub fn requeue_claimed_task(
        &self,
        run_id: &str,
        task_id: &str,
        claim: &echo_agent::tasks::TaskClaim,
        failure_fingerprint: Option<&str>,
        summary: &str,
    ) -> Result<ClaimWriteOutcome, StoreError> {
        self.with_run_lock(run_id, || {
            let plan = self
                .get_plan(run_id)?
                .ok_or_else(|| StoreError::PlanNotFound(run_id.to_string()))?;
            let Some(task) = plan.tasks.iter().find(|task| task.id == task_id) else {
                return Ok(ClaimWriteOutcome::Superseded);
            };
            if task.status != TodoStatus::Running || task.claim.as_ref() != Some(claim) {
                return Ok(ClaimWriteOutcome::Superseded);
            }
            let next = task.retry_count.saturating_add(1);
            self.shadow.append_event_line(
                run_id,
                Some(task_id),
                None,
                RuntimeEventKind::TodoUpdated,
                serde_json::json!({
                    "status": TodoStatus::Pending.as_str(),
                    "status_detail": null,
                    "owner_agent": task.agent_role,
                    "summary": summary,
                    "retry_count": next,
                    "failure_fingerprint": failure_fingerprint,
                    "claim": null,
                    "started_at": null,
                    "completed_at": null,
                }),
            )?;
            self.shadow.rewrite_plan(run_id)?;
            Ok(ClaimWriteOutcome::Applied)
        })
    }

    pub fn task_claim_is_current(
        &self,
        run_id: &str,
        task_id: &str,
        claim: &echo_agent::tasks::TaskClaim,
    ) -> Result<bool, StoreError> {
        let plan = self
            .get_plan(run_id)?
            .ok_or_else(|| StoreError::PlanNotFound(run_id.to_string()))?;
        Ok(plan.tasks.iter().any(|task| {
            task.id == task_id
                && task.status == TodoStatus::Running
                && task.claim.as_ref() == Some(claim)
        }))
    }

    fn append_task_status_event(
        &self,
        run_id: &str,
        task_id: &str,
        status: TodoStatus,
        owner_agent: Option<&str>,
        summary: Option<&str>,
        claim: Option<&echo_agent::tasks::TaskClaim>,
    ) -> Result<(), StoreError> {
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
        let status_detail = matches!(status, TodoStatus::Failed | TodoStatus::Blocked)
            .then(|| summary.unwrap_or_else(|| status.as_str()));
        self.shadow.append_event_line(
            run_id,
            Some(task_id),
            None,
            kind,
            serde_json::json!({
                "status": status.as_str(),
                "status_detail": status_detail,
                "owner_agent": owner_agent,
                "summary": summary,
                "claim": claim,
                "started_at": if started { Some(now.as_str()) } else { None },
                "completed_at": if finished { Some(now.as_str()) } else { None },
            }),
        )?;
        self.shadow.rewrite_plan(run_id)?;
        Ok(())
    }

    /// Bump execution retry metadata without mutating the task specification.
    pub fn increment_retry_count(
        &self,
        run_id: &str,
        task_id: &str,
        failure_fingerprint: Option<&str>,
    ) -> Result<u32, StoreError> {
        self.with_run_lock(run_id, || {
            let plan = self
                .get_plan(run_id)?
                .ok_or(StoreError::PlanNotFound(run_id.to_string()))?;
            let task = plan
                .tasks
                .iter()
                .find(|t| t.id == task_id)
                .ok_or_else(|| StoreError::TaskNotFound(task_id.to_string()))?;
            let next = task.retry_count.saturating_add(1);
            self.shadow.append_event_line(
                run_id,
                Some(task_id),
                None,
                RuntimeEventKind::TodoUpdated,
                serde_json::json!({
                    "status": task.status.as_str(),
                    "retry_count": next,
                    "failure_fingerprint": failure_fingerprint,
                }),
            )?;
            self.shadow.rewrite_plan(run_id)?;
            Ok(next)
        })
    }

    /// Atomically retry a Blocked/Failed task in a Paused/Failed run.
    ///
    /// Performs the full guard → retry_count bump → Pending → Running
    /// transition under a single per-run write lock, so concurrent
    /// retry_blocked_task callers cannot both pass the budget check and
    /// double-bump retry_count. Returns the new attempt number on success,
    /// or a StoreError on any precondition failure (run/task not in a
    /// retryable state, retry budget exhausted). The caller is responsible
    /// for spawning the executor after this returns Ok.
    pub fn retry_blocked_task(&self, run_id: &str, task_id: &str) -> Result<u32, StoreError> {
        self.with_run_lock(run_id, || {
            // 1. Run must be Paused or Failed (the states acceptance failure
            //    produces). Any other status is a concurrent retry / misuse.
            let run = self
                .get_run(run_id)?
                .ok_or_else(|| StoreError::RunNotFound(run_id.to_string()))?;
            if !matches!(run.status, TaskRunStatus::Paused | TaskRunStatus::Failed) {
                return Err(StoreError::InvalidPlan(format!(
                    "run {} is {:?}; retry requires Paused or Failed",
                    run_id, run.status
                )));
            }
            // 2. Task must be Blocked or Failed.
            let plan = self
                .get_plan(run_id)?
                .ok_or_else(|| StoreError::PlanNotFound(run_id.to_string()))?;
            let task = plan
                .tasks
                .iter()
                .find(|t| t.id == task_id)
                .cloned()
                .ok_or_else(|| StoreError::TaskNotFound(task_id.to_string()))?;
            if !matches!(task.status, TodoStatus::Blocked | TodoStatus::Failed) {
                return Err(StoreError::InvalidPlan(format!(
                    "task {} is {:?}; retry requires Blocked or Failed",
                    task_id, task.status
                )));
            }
            // 3. Budget check.
            if task.retry_count >= task.max_retries {
                return Err(StoreError::InvalidPlan(format!(
                    "task {} retry budget exhausted ({}/{})",
                    task_id, task.retry_count, task.max_retries
                )));
            }

            // 4. Atomic retry_count bump + Pending transition under the same
            //    lock. Title/description unchanged; attempt id derives from
            //    retry_count+1 at dispatch time.
            let next = task.retry_count.saturating_add(1);
            self.shadow.append_event_line(
                run_id,
                Some(task_id),
                None,
                RuntimeEventKind::TodoUpdated,
                serde_json::json!({
                    "owner_agent": task.agent_role,
                    "started_at": null,
                    "completed_at": null,
                    "status": "pending",
                    "retry_count": next,
                    "failure_fingerprint": task.failure_fingerprint,
                    "summary": format!("user-initiated retry (attempt {next})"),
                }),
            )?;

            // A hard task failure propagates `Blocked` to its downstream
            // dependents. Retrying only the failed node would leave those
            // descendants permanently unschedulable because the DAG frontier
            // accepts Pending tasks only. Reset precisely the descendants whose
            // persisted blocker was created by that upstream-failure propagation;
            // acceptance/review blockers keep their independent Blocked state.
            let todos = self.list_todos(run_id)?;
            let upstream_blocked: std::collections::HashSet<String> = todos
                .iter()
                .filter(|todo| {
                    todo.status == TodoStatus::Blocked
                        && todo.summary.as_deref() == Some("blocked: upstream task failed")
                })
                .map(|todo| todo.task_id.clone())
                .collect();
            let mut recovered = std::collections::HashSet::from([task_id.to_string()]);
            let mut descendants = Vec::new();
            loop {
                let mut changed = false;
                for candidate in &plan.tasks {
                    if candidate.status != TodoStatus::Blocked
                        || !upstream_blocked.contains(&candidate.id)
                        || recovered.contains(&candidate.id)
                        || !candidate
                            .depends_on
                            .iter()
                            .any(|dep| recovered.contains(dep))
                    {
                        continue;
                    }
                    let still_blocked = candidate.depends_on.iter().any(|dep_id| {
                        plan.tasks
                            .iter()
                            .find(|dep| dep.id == *dep_id)
                            .is_some_and(|dep| {
                                matches!(dep.status, TodoStatus::Failed | TodoStatus::Blocked)
                                    && !recovered.contains(dep_id)
                            })
                    });
                    if still_blocked {
                        continue;
                    }
                    recovered.insert(candidate.id.clone());
                    descendants.push(candidate.clone());
                    changed = true;
                }
                if !changed {
                    break;
                }
            }
            for descendant in descendants {
                self.shadow.append_event_line(
                    run_id,
                    Some(&descendant.id),
                    None,
                    RuntimeEventKind::TodoUpdated,
                    serde_json::json!({
                        "owner_agent": descendant.agent_role,
                        "started_at": null,
                        "completed_at": null,
                        "status": "pending",
                        "summary": format!("unblocked after retrying upstream task {task_id}"),
                    }),
                )?;
            }
            self.shadow.append_event_line(
                run_id,
                None,
                None,
                RuntimeEventKind::Note,
                serde_json::json!({
                    "message": format!("user retried blocked task {task_id} (attempt {next})"),
                }),
            )?;
            self.shadow.rewrite_plan(run_id)?;

            // 5. Run → Running (still under the lock so a racing caller sees
            //    the new state and fails the run-status guard above).
            self.transition_run_locked(run_id, TaskRunStatus::Running)?;
            Ok(next)
        })
    }

    /// Run-status transition without re-acquiring the per-run lock (for use
    /// inside another `with_run_lock` closure). Validates the transition
    /// and appends the event; does NOT itself call with_run_lock.
    fn transition_run_locked(
        &self,
        run_id: &str,
        next: TaskRunStatus,
    ) -> Result<TaskRun, StoreError> {
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
        let now = chrono::Utc::now();
        self.shadow.append_event_line(
            run_id,
            None,
            None,
            RuntimeEventKind::RunStatusChanged,
            serde_json::json!({ "from": current.as_str(), "to": next.as_str() }),
        )?;
        self.shadow.rewrite_plan(run_id)?;
        let mut run = run;
        run.status = next;
        run.updated_at = now;
        Ok(run)
    }

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
                    "path": a.path,
                    "metadata": a.metadata,
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

    fn active_subagent_boundaries(
        &self,
        run_id: &str,
    ) -> Result<Vec<ActiveSubagentBoundary>, StoreError> {
        let mut active = std::collections::HashMap::<String, ActiveSubagentBoundary>::new();
        for event in self.list_events(run_id, 0)? {
            let Some(execution_id) = event.step_id.clone() else {
                continue;
            };
            match event.event_type {
                RuntimeEventKind::SubagentAssigned => {
                    let Some(task_id) = event.task_id.clone() else {
                        continue;
                    };
                    active.insert(
                        execution_id.clone(),
                        ActiveSubagentBoundary {
                            task_id,
                            execution_id,
                            replay_safe: json_bool(&event.payload, "replay_safe", false),
                        },
                    );
                }
                RuntimeEventKind::SubagentReleased => {
                    active.remove(&execution_id);
                }
                _ => {}
            }
        }
        Ok(active.into_values().collect())
    }

    fn active_tool_boundaries(&self, run_id: &str) -> Result<Vec<ActiveToolBoundary>, StoreError> {
        let mut active = std::collections::HashMap::<(String, String), ActiveToolBoundary>::new();
        for event in self.list_events(run_id, 0)? {
            let Some(task_id) = event.task_id.clone() else {
                continue;
            };
            let call_id = json_string(&event.payload, "call_id")
                .or_else(|| event.step_id.clone())
                .unwrap_or_default();
            if call_id.is_empty() {
                continue;
            }
            let key = (task_id.clone(), call_id.clone());
            match event.event_type {
                RuntimeEventKind::ToolStarted => {
                    active.insert(
                        key,
                        ActiveToolBoundary {
                            task_id,
                            execution_id: json_string(&event.payload, "execution_id"),
                            call_id,
                            tool_name: json_string(&event.payload, "tool_name")
                                .unwrap_or_else(|| "unknown".to_string()),
                            replay_safe: json_bool(&event.payload, "replay_safe", false),
                        },
                    );
                }
                RuntimeEventKind::ToolCompleted | RuntimeEventKind::ToolFailed => {
                    active.remove(&key);
                }
                _ => {}
            }
        }
        Ok(active.into_values().collect())
    }

    fn record_recovery_blocker(
        &self,
        run_id: &str,
        task_id: &str,
        execution_id: Option<&str>,
        call_id: Option<&str>,
        tool_name: Option<&str>,
        reason: &str,
    ) -> Result<(), StoreError> {
        self.shadow.append_event_line(
            run_id,
            Some(task_id),
            execution_id,
            RuntimeEventKind::RecoveryBlocked,
            serde_json::json!({
                "execution_id": execution_id,
                "call_id": call_id,
                "tool_name": tool_name,
                "reason": reason,
            }),
        )?;
        Ok(())
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
                    let plan = self.get_plan(&run.run_id).ok().flatten();
                    let active_subagents = self
                        .active_subagent_boundaries(&run.run_id)
                        .unwrap_or_default();
                    let active_tools = self.active_tool_boundaries(&run.run_id).unwrap_or_default();
                    if let Ok(todos) = self.list_todos(&run.run_id) {
                        for todo in todos
                            .into_iter()
                            .filter(|todo| todo.status == TodoStatus::Running)
                        {
                            let task = plan.as_ref().and_then(|plan| {
                                plan.tasks.iter().find(|task| task.id == todo.task_id)
                            });
                            let execution_id = task.and_then(|task| {
                                task.claim
                                    .as_ref()
                                    .map(|claim| claim.execution_id(&run.run_id, &task.id))
                            });
                            let completed_subagent = execution_id.as_deref().and_then(|id| {
                                self.recoverable_subagent_result(&run.run_id, &todo.task_id, id)
                                    .ok()
                                    .flatten()
                            });

                            let active_tool = active_tools
                                .iter()
                                .find(|boundary| {
                                    boundary.task_id == todo.task_id && !boundary.replay_safe
                                })
                                .cloned();
                            let active_subagent = active_subagents
                                .iter()
                                .find(|boundary| {
                                    boundary.task_id == todo.task_id && !boundary.replay_safe
                                })
                                .cloned();

                            let (next_status, summary) = if completed_subagent.is_some() {
                                (
                                    TodoStatus::Pending,
                                    "Subagent completed before interruption; pending review",
                                )
                            } else if active_tool.is_some() || active_subagent.is_some() {
                                (
                                    TodoStatus::Blocked,
                                    "mutating side effect is indeterminate after restart",
                                )
                            } else {
                                (TodoStatus::Pending, "interrupted; pending resume")
                            };

                            if let Err(error) = self.set_task_status(
                                &run.run_id,
                                &todo.task_id,
                                next_status,
                                None,
                                Some(summary),
                            ) {
                                tracing::warn!(
                                    run_id = %run.run_id,
                                    task_id = %todo.task_id,
                                    %error,
                                    "recover_incomplete: failed to reset running task"
                                );
                                continue;
                            }

                            if next_status == TodoStatus::Blocked {
                                let (boundary_execution_id, call_id, tool_name) =
                                    if let Some(tool) = active_tool {
                                        (
                                            tool.execution_id,
                                            Some(tool.call_id),
                                            Some(tool.tool_name),
                                        )
                                    } else if let Some(subagent) = active_subagent {
                                        (Some(subagent.execution_id), None, None)
                                    } else {
                                        (execution_id, None, None)
                                    };
                                if let Err(error) = self.record_recovery_blocker(
                                    &run.run_id,
                                    &todo.task_id,
                                    boundary_execution_id.as_deref(),
                                    call_id.as_deref(),
                                    tool_name.as_deref(),
                                    summary,
                                ) {
                                    tracing::warn!(
                                        run_id = %run.run_id,
                                        task_id = %todo.task_id,
                                        %error,
                                        "recover_incomplete: failed to persist recovery blocker"
                                    );
                                }
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

    /// Persist the boundary immediately before a task Subagent starts model/tool
    /// execution. A matching [`record_subagent_released`](Self::record_subagent_released)
    /// makes the Subagent result recoverable without dispatching it again.
    #[allow(clippy::too_many_arguments)]
    pub fn record_subagent_assigned(
        &self,
        run_id: &str,
        task_id: &str,
        execution_id: &str,
        agent_name: &str,
        attempt: u32,
        replay_safe: bool,
    ) -> Result<(), StoreError> {
        self.shadow.append_event_line(
            run_id,
            Some(task_id),
            Some(execution_id),
            RuntimeEventKind::SubagentAssigned,
            serde_json::json!({
                "execution_id": execution_id,
                "agent_name": agent_name,
                "attempt": attempt,
                "replay_safe": replay_safe,
            }),
        )?;
        Ok(())
    }

    /// Persist a Subagent terminal fact with the structured result needed for resume.
    pub fn record_subagent_released(
        &self,
        run_id: &str,
        task_id: &str,
        execution_id: &str,
        status: &str,
        result: Option<&SubagentTaskResult>,
        full_output: Option<&str>,
    ) -> Result<(), StoreError> {
        let summary = result.map(|value| bounded_event_text(&value.summary, 2_000));
        self.shadow.append_event_line(
            run_id,
            Some(task_id),
            Some(execution_id),
            RuntimeEventKind::SubagentReleased,
            serde_json::json!({
                "execution_id": execution_id,
                "status": status,
                "summary": summary,
                "result": result,
                "full_output": full_output,
            }),
        )?;
        Ok(())
    }

    /// Persist a tool dispatch before execution. Raw arguments are deliberately
    /// excluded from the durable event to avoid leaking secrets or inflating
    /// the run file; `call_id` is the idempotency/correlation key.
    pub fn record_tool_started(
        &self,
        run_id: &str,
        task_id: &str,
        execution_id: &str,
        call_id: &str,
        tool_name: &str,
        replay_safe: bool,
    ) -> Result<(), StoreError> {
        self.shadow.append_event_line(
            run_id,
            Some(task_id),
            Some(call_id),
            RuntimeEventKind::ToolStarted,
            serde_json::json!({
                "execution_id": execution_id,
                "call_id": call_id,
                "tool_name": tool_name,
                "replay_safe": replay_safe,
            }),
        )?;
        Ok(())
    }

    /// Persist a tool terminal fact. The result preview is diagnostic only;
    /// canonical tool output remains in the agent checkpoint/transcript.
    #[allow(clippy::too_many_arguments)]
    pub fn record_tool_finished(
        &self,
        run_id: &str,
        task_id: &str,
        execution_id: &str,
        call_id: &str,
        tool_name: &str,
        success: bool,
        result: &str,
        failure: Option<&echo_agent::tools::ToolFailure>,
    ) -> Result<(), StoreError> {
        let event_type = if success {
            RuntimeEventKind::ToolCompleted
        } else {
            RuntimeEventKind::ToolFailed
        };
        self.shadow.append_event_line(
            run_id,
            Some(task_id),
            Some(call_id),
            event_type,
            serde_json::json!({
                "execution_id": execution_id,
                "call_id": call_id,
                "tool_name": tool_name,
                "success": success,
                "result_preview": bounded_event_text(result, 500),
                "result_chars": result.chars().count(),
                "failure": failure,
            }),
        )?;
        Ok(())
    }

    /// Return a completed Subagent result for this exact attempt. A later
    /// SubagentAssigned with the same id clears an older terminal fact, which is
    /// how an explicitly confirmed retry avoids reusing stale output.
    pub(crate) fn recoverable_subagent_result(
        &self,
        run_id: &str,
        task_id: &str,
        execution_id: &str,
    ) -> Result<Option<RecoverableSubagentResult>, StoreError> {
        let mut result = None;
        for event in self.list_events(run_id, 0)? {
            if event.task_id.as_deref() != Some(task_id)
                || event.step_id.as_deref() != Some(execution_id)
            {
                continue;
            }
            match event.event_type {
                RuntimeEventKind::SubagentAssigned => result = None,
                RuntimeEventKind::SubagentReleased => {
                    result =
                        if json_string(&event.payload, "status").as_deref() == Some("completed") {
                            event
                                .payload
                                .get("result")
                                .cloned()
                                .and_then(|value| {
                                    serde_json::from_value::<SubagentTaskResult>(value).ok()
                                })
                                .map(|result| RecoverableSubagentResult {
                                    full_output: json_string(&event.payload, "full_output")
                                        .filter(|output| !output.trim().is_empty())
                                        .unwrap_or_else(|| result.summary.clone()),
                                    result,
                                })
                        } else {
                            None
                        };
                }
                _ => {}
            }
        }
        Ok(result)
    }

    /// Current unresolved recovery barriers, folded from append-only events.
    pub fn list_recovery_blockers(&self, run_id: &str) -> Result<Vec<RecoveryBlocker>, StoreError> {
        let mut blockers = std::collections::BTreeMap::<String, RecoveryBlocker>::new();
        for event in self.list_events(run_id, 0)? {
            match event.event_type {
                RuntimeEventKind::RecoveryBlocked => {
                    let Some(task_id) = event.task_id.clone() else {
                        continue;
                    };
                    blockers.insert(
                        task_id.clone(),
                        RecoveryBlocker {
                            run_id: run_id.to_string(),
                            task_id,
                            execution_id: json_string(&event.payload, "execution_id"),
                            call_id: json_string(&event.payload, "call_id"),
                            tool_name: json_string(&event.payload, "tool_name"),
                            reason: json_string(&event.payload, "reason")
                                .unwrap_or_else(|| "mutating side effect is indeterminate".into()),
                        },
                    );
                }
                RuntimeEventKind::RecoveryResolved => {
                    if let Some(task_id) = event.task_id.as_ref() {
                        blockers.remove(task_id);
                    }
                }
                _ => {}
            }
        }
        // The blocked Todo projection is itself durable. If the dedicated
        // RecoveryBlocked append was interrupted after TaskBlocked landed,
        // synthesize the barrier so resume still fails closed.
        for todo in self.list_todos(run_id)?.into_iter().filter(|todo| {
            todo.status == TodoStatus::Blocked
                && todo.summary.as_deref()
                    == Some("mutating side effect is indeterminate after restart")
        }) {
            blockers
                .entry(todo.task_id.clone())
                .or_insert_with(|| RecoveryBlocker {
                    run_id: run_id.to_string(),
                    task_id: todo.task_id,
                    execution_id: None,
                    call_id: None,
                    tool_name: None,
                    reason: "mutating side effect is indeterminate after restart".to_string(),
                });
        }
        Ok(blockers.into_values().collect())
    }

    /// Resolve one recovery barrier after the user inspects the workspace.
    pub fn resolve_recovery_task(
        &self,
        run_id: &str,
        task_id: &str,
        decision: RecoveryDecision,
    ) -> Result<(), StoreError> {
        let blocker = self
            .list_recovery_blockers(run_id)?
            .into_iter()
            .find(|blocker| blocker.task_id == task_id)
            .ok_or_else(|| {
                StoreError::InvalidPlan(format!(
                    "task {task_id} has no unresolved recovery barrier"
                ))
            })?;

        // Persist the user's decision first. If the process stops before the
        // Todo mutation, the still-Blocked Todo synthesizes the barrier again
        // on the next read, so recovery continues to fail closed.
        self.shadow.append_event_line(
            run_id,
            Some(task_id),
            blocker.execution_id.as_deref(),
            RuntimeEventKind::RecoveryResolved,
            serde_json::json!({
                "decision": decision.as_str(),
                "previous_reason": blocker.reason,
            }),
        )?;
        match decision {
            RecoveryDecision::Retry => self.set_task_status(
                run_id,
                task_id,
                TodoStatus::Pending,
                None,
                Some("recovery retry confirmed by user"),
            )?,
            RecoveryDecision::Skip => self.set_task_status(
                run_id,
                task_id,
                TodoStatus::Skipped,
                None,
                Some("recovery skip confirmed by user"),
            )?,
        }
        Ok(())
    }
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

fn bounded_event_text(value: &str, max_chars: usize) -> String {
    let mut text = value.chars().take(max_chars).collect::<String>();
    if value.chars().count() > max_chars {
        text.push_str("...");
    }
    text
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
    fn artifact_round_trip_preserves_path_and_metadata() -> std::result::Result<(), String> {
        let store = TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?;
        store
            .create_run(
                "artifact-run",
                "ws",
                "conversation",
                "message",
                DomainProfile::General,
                "artifact round trip",
                "",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        let artifact = Artifact {
            id: "artifact-1".to_string(),
            run_id: "artifact-run".to_string(),
            task_id: None,
            kind: ArtifactKind::Trace,
            title: "Complete tool output".to_string(),
            path: Some("/tmp/tool-output.log".to_string()),
            metadata: serde_json::json!({
                "sha256": "abcdef",
                "retention": "conversation_or_30d",
            }),
        };
        store
            .add_artifact(&artifact)
            .map_err(|error| error.to_string())?;

        let artifacts = store
            .list_artifacts("artifact-run")
            .map_err(|error| error.to_string())?;
        let restored = artifacts
            .first()
            .ok_or_else(|| "artifact was not restored".to_string())?;
        assert_eq!(restored.path, artifact.path);
        assert_eq!(restored.metadata, artifact.metadata);
        Ok(())
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
            revision: 1,
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
        s.attach_plan_for_test(&plan).unwrap();

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
            subagent_name: "code_reviewer".into(),
            result: SubagentTaskResult {
                contract_version: 1,
                status: SubagentRunStatus::Completed,
                summary: "read chat.rs".into(),
                artifacts: Vec::new(),
                verification: vec![SubagentVerificationResult {
                    check: "cargo check".into(),
                    status: SubagentVerificationStatus::Passed,
                    details: String::new(),
                    source: SubagentVerificationSource::Observed,
                }],
                remaining_work: Vec::new(),
                touched_files: SubagentTouchedFiles {
                    read: vec!["chat.rs".into()],
                    written: Vec::new(),
                },
            },
            decisions: vec!["route via TaskRuntime".into()],
            next_implications: vec!["implement router".into()],
            suggested_tasks: vec![],
            created_at: Utc::now(),
        };
        s.put_summary(&sum).unwrap();
        let got = s.get_summary("r1", "t1").unwrap().unwrap();
        assert_eq!(got.result.summary, "read chat.rs");
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
            revision: 1,
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
        s.attach_plan_for_test(&plan).unwrap();
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
    fn retry_failed_upstream_restores_only_propagated_blocked_descendants() -> Result<(), StoreError>
    {
        let store = fresh();
        store.create_run(
            "retry-run",
            "ws",
            "c1",
            "m1",
            DomainProfile::General,
            "retry a failed dependency chain",
            "",
            AttendedMode::Attended,
        )?;
        store.attach_plan_for_test(&TaskPlan {
            plan_id: "retry-plan".to_string(),
            run_id: "retry-run".to_string(),
            revision: 1,
            domain_profile: DomainProfile::General,
            goal: "retry a failed dependency chain".to_string(),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Sequential,
            tasks: vec![
                PlanTask {
                    id: "upstream".to_string(),
                    agent_role: "implementer".to_string(),
                    max_retries: 2,
                    ..sample_task_body("upstream")
                },
                PlanTask {
                    id: "child".to_string(),
                    agent_role: "reviewer".to_string(),
                    depends_on: vec!["upstream".to_string()],
                    ..sample_task_body("child")
                },
                PlanTask {
                    id: "grandchild".to_string(),
                    agent_role: "explorer".to_string(),
                    depends_on: vec!["child".to_string()],
                    ..sample_task_body("grandchild")
                },
                PlanTask {
                    id: "acceptance-blocked".to_string(),
                    agent_role: "reviewer".to_string(),
                    ..sample_task_body("acceptance-blocked")
                },
            ],
        })?;
        store.transition_run("retry-run", TaskRunStatus::Running)?;
        store.set_task_status(
            "retry-run",
            "upstream",
            TodoStatus::Failed,
            Some("implementer"),
            Some("execution failed"),
        )?;
        for task_id in ["child", "grandchild"] {
            store.set_task_status(
                "retry-run",
                task_id,
                TodoStatus::Blocked,
                None,
                Some("blocked: upstream task failed"),
            )?;
        }
        store.set_task_status(
            "retry-run",
            "acceptance-blocked",
            TodoStatus::Blocked,
            Some("reviewer"),
            Some("review needs fix; awaiting explicit retry"),
        )?;
        store.transition_run("retry-run", TaskRunStatus::Failed)?;

        assert_eq!(store.retry_blocked_task("retry-run", "upstream")?, 1);
        let todos = store.list_todos("retry-run")?;
        for task_id in ["upstream", "child", "grandchild"] {
            let todo = todos
                .iter()
                .find(|todo| todo.task_id == task_id)
                .ok_or_else(|| StoreError::TaskNotFound(task_id.to_string()))?;
            assert_eq!(todo.status, TodoStatus::Pending, "{task_id}");
        }
        let upstream = todos
            .iter()
            .find(|todo| todo.task_id == "upstream")
            .ok_or_else(|| StoreError::TaskNotFound("upstream".to_string()))?;
        assert_eq!(upstream.owner_agent.as_deref(), Some("implementer"));
        let independent = todos
            .iter()
            .find(|todo| todo.task_id == "acceptance-blocked")
            .ok_or_else(|| StoreError::TaskNotFound("acceptance-blocked".to_string()))?;
        assert_eq!(independent.status, TodoStatus::Blocked);
        assert_eq!(
            store
                .get_run("retry-run")?
                .ok_or_else(|| StoreError::RunNotFound("retry-run".to_string()))?
                .status,
            TaskRunStatus::Running
        );
        Ok(())
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
        store.set_task_status("r1", "t1", TodoStatus::Running, Some("subagent"), None)?;
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
        store.set_task_status("r1", "t1", TodoStatus::Running, Some("subagent"), None)?;

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
    fn boot_recovery_reuses_completed_subagent_without_redispatch() -> Result<(), StoreError> {
        let store = fresh();
        seed_plan(&store);
        let task = store
            .get_plan("r1")?
            .ok_or_else(|| StoreError::PlanNotFound("r1".to_string()))?
            .tasks
            .first()
            .cloned()
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        let claim = match store.claim_task("r1", &task.to_task(), 1)? {
            echo_agent::tasks::RuntimeTaskClaimOutcome::Claimed(claim) => claim,
            echo_agent::tasks::RuntimeTaskClaimOutcome::ReloadSnapshot => {
                return Err(StoreError::InvalidPlan(
                    "fresh task claim unexpectedly required reload".to_string(),
                ));
            }
        };
        let execution_id = claim.execution_id("r1", "t1");
        store.record_subagent_assigned("r1", "t1", &execution_id, "subagent", 1, true)?;
        let result = SubagentTaskResult::terminal(
            SubagentRunStatus::Completed,
            "durable result",
            Vec::new(),
        );
        store.record_subagent_released(
            "r1",
            "t1",
            &execution_id,
            "completed",
            Some(&result),
            Some("durable full output"),
        )?;

        assert_eq!(store.recover_incomplete(), 1);
        assert_eq!(
            store.recoverable_subagent_result("r1", "t1", &execution_id)?,
            Some(RecoverableSubagentResult {
                result,
                full_output: "durable full output".to_string(),
            })
        );
        let todo = store
            .list_todos("r1")?
            .into_iter()
            .find(|todo| todo.task_id == "t1")
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        assert_eq!(todo.status, TodoStatus::Pending);
        assert_eq!(
            todo.summary.as_deref(),
            Some("Subagent completed before interruption; pending review")
        );
        assert!(store.list_recovery_blockers("r1")?.is_empty());
        Ok(())
    }

    #[test]
    fn mutating_in_doubt_subagent_blocks_resume_until_user_decides() -> Result<(), StoreError> {
        let store = fresh();
        seed_plan(&store);
        store.apply_task_patch_for_test(
            "r1",
            &TaskUpdateRequest {
                base_revision: 1,
                reason: "exercise mutating recovery".to_string(),
                operations: vec![TaskUpdateOperation::Update {
                    task_id: "t1".to_string(),
                    patch: TaskPatch {
                        kind: Some(PlanTaskKind::Implementation),
                        ..Default::default()
                    },
                }],
            },
        )?;
        store.set_task_status("r1", "t1", TodoStatus::Running, Some("subagent"), None)?;
        store.record_subagent_assigned("r1", "t1", "t1:1", "subagent", 1, false)?;
        store.record_tool_started("r1", "t1", "t1:1", "call-write", "write_file", false)?;

        assert_eq!(store.recover_incomplete(), 1);
        let blockers = store.list_recovery_blockers("r1")?;
        assert_eq!(blockers.len(), 1);
        assert_eq!(
            blockers.first().and_then(|b| b.call_id.as_deref()),
            Some("call-write")
        );
        assert!(matches!(
            store.resume_task_run("r1"),
            Err(StoreError::RecoveryBlocked { .. })
        ));

        store.resolve_recovery_task("r1", "t1", RecoveryDecision::Retry)?;
        assert!(store.list_recovery_blockers("r1")?.is_empty());
        let todo = store
            .list_todos("r1")?
            .into_iter()
            .find(|todo| todo.task_id == "t1")
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        assert_eq!(todo.status, TodoStatus::Pending);
        assert_eq!(store.resume_task_run("r1")?.status, TaskRunStatus::Running);
        Ok(())
    }

    #[test]
    fn tool_failure_boundary_persists_recovery_contract() -> Result<(), StoreError> {
        let store = fresh();
        seed_plan(&store);
        let failure = echo_agent::tools::ToolFailure::new(
            echo_agent::tools::ToolFailureCategory::PartialSideEffect,
        )
        .with_postcondition("verify target hash");

        store.record_tool_started("r1", "t1", "t1:1", "call-1", "write_file", false)?;
        store.record_tool_finished(
            "r1",
            "t1",
            "t1:1",
            "call-1",
            "write_file",
            false,
            "write interrupted",
            Some(&failure),
        )?;

        let event = store
            .list_events("r1", 0)?
            .into_iter()
            .find(|event| event.event_type == RuntimeEventKind::ToolFailed)
            .ok_or_else(|| StoreError::TaskNotFound("tool failure event".to_string()))?;
        assert_eq!(
            event
                .payload
                .get("failure")
                .and_then(|failure| failure.get("category"))
                .and_then(serde_json::Value::as_str),
            Some("partial_side_effect")
        );
        assert_eq!(
            event
                .payload
                .get("failure")
                .and_then(|failure| failure.get("postcondition"))
                .and_then(serde_json::Value::as_str),
            Some("verify target hash")
        );
        Ok(())
    }

    #[test]
    fn blocked_todo_restores_barrier_if_resolution_crashes_before_mutation()
    -> Result<(), StoreError> {
        let store = fresh();
        seed_plan(&store);
        store.apply_task_patch_for_test(
            "r1",
            &TaskUpdateRequest {
                base_revision: 1,
                reason: "exercise recovery barrier".to_string(),
                operations: vec![TaskUpdateOperation::Update {
                    task_id: "t1".to_string(),
                    patch: TaskPatch {
                        kind: Some(PlanTaskKind::Implementation),
                        ..Default::default()
                    },
                }],
            },
        )?;
        store.set_task_status("r1", "t1", TodoStatus::Running, Some("subagent"), None)?;
        store.record_subagent_assigned("r1", "t1", "t1:1", "subagent", 1, false)?;
        assert_eq!(store.recover_incomplete(), 1);

        // Simulate a process stop after RecoveryResolved was appended but
        // before resolve_recovery_task changed the durable Blocked Todo.
        store.shadow.append_event_line(
            "r1",
            Some("t1"),
            Some("t1:1"),
            RuntimeEventKind::RecoveryResolved,
            serde_json::json!({ "decision": "retry" }),
        )?;

        let blockers = store.list_recovery_blockers("r1")?;
        assert_eq!(blockers.len(), 1);
        assert_eq!(
            blockers.first().map(|blocker| blocker.task_id.as_str()),
            Some("t1")
        );
        assert!(matches!(
            store.resume_task_run("r1"),
            Err(StoreError::RecoveryBlocked { .. })
        ));
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
    fn task_update_inserts_task_and_commits_one_revision() {
        let s = fresh();
        seed_plan(&s);
        let t2 = PlanTask {
            id: "t2".into(),
            title: "Second task".into(),
            description: "implement the second task".into(),
            kind: PlanTaskKind::Implementation,
            agent_role: "implementer".into(),
            depends_on: vec!["t1".into()],
            ..Default::default()
        };
        let before = s.list_events("r1", 0).unwrap().len();
        let plan = s
            .apply_task_patch_for_test(
                "r1",
                &TaskUpdateRequest {
                    base_revision: 1,
                    reason: "new implementation dependency".to_string(),
                    operations: vec![TaskUpdateOperation::Insert {
                        after_task_id: Some("t1".to_string()),
                        task: t2.spec(),
                    }],
                },
            )
            .unwrap();

        assert_eq!(plan.revision, 2);
        assert_eq!(plan.tasks.len(), 2);
        assert_eq!(plan.tasks[0].id, "t1");
        assert_eq!(plan.tasks[1].id, "t2");
        let evs = s.list_events("r1", 0).unwrap();
        assert_eq!(evs.len(), before + 1);
        assert_eq!(
            evs.last().map(|event| event.event_type),
            Some(RuntimeEventKind::PlanRevisionCommitted)
        );
    }

    #[test]
    fn task_update_rejects_missing_run() -> std::result::Result<(), String> {
        let s = TaskRuntimeStore::new_in_memory().map_err(|e| e.to_string())?;
        let err = s
            .apply_task_patch_for_test(
                "missing-run",
                &TaskUpdateRequest {
                    base_revision: 1,
                    reason: "invalid".to_string(),
                    operations: vec![TaskUpdateOperation::Reorder {
                        task_ids: Vec::new(),
                    }],
                },
            )
            .err()
            .ok_or_else(|| "task_update unexpectedly succeeded without a run".to_string())?;
        assert!(matches!(err, StoreError::RunNotFound(run_id) if run_id == "missing-run"));
        Ok(())
    }

    #[test]
    fn task_update_rejects_stale_revision_without_appending_event() {
        let s = fresh();
        seed_plan(&s);
        let before = s.list_events("r1", 0).unwrap().len();
        let error = s
            .apply_task_patch_for_test(
                "r1",
                &TaskUpdateRequest {
                    base_revision: 0,
                    reason: "stale edit".to_string(),
                    operations: vec![TaskUpdateOperation::Skip {
                        task_id: "t1".to_string(),
                    }],
                },
            )
            .unwrap_err();
        assert!(matches!(error, StoreError::PlanConflict { .. }));
        assert_eq!(s.list_events("r1", 0).unwrap().len(), before);
    }

    #[test]
    fn claim_reloads_when_task_update_wins_revision_race() -> Result<(), StoreError> {
        let store = fresh();
        seed_plan(&store);
        let expected = store
            .get_plan("r1")?
            .ok_or_else(|| StoreError::PlanNotFound("r1".to_string()))?
            .tasks
            .first()
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?
            .to_task();
        store.apply_task_patch_for_test(
            "r1",
            &TaskUpdateRequest {
                base_revision: 1,
                reason: "skip before stale dispatch claims task".to_string(),
                operations: vec![TaskUpdateOperation::Skip {
                    task_id: "t1".to_string(),
                }],
            },
        )?;

        let outcome = store.claim_task("r1", &expected, 1)?;

        assert_eq!(
            outcome,
            echo_agent::tasks::RuntimeTaskClaimOutcome::ReloadSnapshot
        );
        let task = store
            .get_plan("r1")?
            .ok_or_else(|| StoreError::PlanNotFound("r1".to_string()))?
            .tasks
            .into_iter()
            .find(|task| task.id == "t1")
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        assert_eq!(task.status, TodoStatus::Skipped);
        assert!(task.claim.is_none());
        Ok(())
    }

    #[test]
    fn stale_claim_cannot_overwrite_cancelled_task() -> Result<(), StoreError> {
        let store = fresh();
        seed_plan(&store);
        let expected = store
            .get_plan("r1")?
            .ok_or_else(|| StoreError::PlanNotFound("r1".to_string()))?
            .tasks
            .first()
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?
            .to_task();
        let claim = match store.claim_task("r1", &expected, 1)? {
            echo_agent::tasks::RuntimeTaskClaimOutcome::Claimed(claim) => claim,
            echo_agent::tasks::RuntimeTaskClaimOutcome::ReloadSnapshot => {
                return Err(StoreError::InvalidPlan(
                    "fresh task claim unexpectedly required reload".to_string(),
                ));
            }
        };
        store.set_task_status(
            "r1",
            "t1",
            TodoStatus::Skipped,
            None,
            Some("cancelled by user"),
        )?;

        let outcome = store.set_claimed_task_status(
            "r1",
            "t1",
            &claim,
            TodoStatus::Completed,
            Some("code_reviewer"),
            Some("stale completion"),
        )?;

        assert_eq!(outcome, ClaimWriteOutcome::Superseded);
        let task = store
            .get_plan("r1")?
            .ok_or_else(|| StoreError::PlanNotFound("r1".to_string()))?
            .tasks
            .into_iter()
            .find(|task| task.id == "t1")
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        assert_eq!(task.status, TodoStatus::Skipped);
        Ok(())
    }

    #[test]
    fn patched_spec_uses_new_execution_identity_without_retry_bump() -> Result<(), StoreError> {
        let store = fresh();
        seed_plan(&store);
        let original = store
            .get_plan("r1")?
            .ok_or_else(|| StoreError::PlanNotFound("r1".to_string()))?
            .tasks
            .first()
            .cloned()
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        let old_claim = echo_agent::tasks::TaskClaim {
            revision: 1,
            attempt: 1,
            spec_hash: original
                .to_task()
                .spec
                .stable_hash()
                .map_err(StoreError::InvalidPlan)?,
        };
        let old_execution_id = old_claim.execution_id("r1", &original.id);
        let durable_result = SubagentTaskResult::terminal(
            SubagentRunStatus::Completed,
            "old spec result",
            Vec::new(),
        );
        store.record_subagent_assigned("r1", "t1", &old_execution_id, "code_reviewer", 1, true)?;
        store.record_subagent_released(
            "r1",
            "t1",
            &old_execution_id,
            "completed",
            Some(&durable_result),
            Some("old spec full output"),
        )?;
        store.set_task_status(
            "r1",
            "t1",
            TodoStatus::Blocked,
            Some("code_reviewer"),
            Some("requires a revised contract"),
        )?;
        let patched = store.apply_task_patch_for_test(
            "r1",
            &TaskUpdateRequest {
                base_revision: 1,
                reason: "change blocked task contract".to_string(),
                operations: vec![TaskUpdateOperation::Update {
                    task_id: "t1".to_string(),
                    patch: TaskPatch {
                        description: Some("review the revised runtime contract".to_string()),
                        ..Default::default()
                    },
                }],
            },
        )?;
        let patched_task = patched
            .tasks
            .first()
            .cloned()
            .ok_or_else(|| StoreError::TaskNotFound("t1".to_string()))?;
        assert_eq!(patched_task.retry_count, 0);
        let new_claim = match store.claim_task("r1", &patched_task.to_task(), patched.revision)? {
            echo_agent::tasks::RuntimeTaskClaimOutcome::Claimed(claim) => claim,
            echo_agent::tasks::RuntimeTaskClaimOutcome::ReloadSnapshot => {
                return Err(StoreError::InvalidPlan(
                    "patched task claim unexpectedly required reload".to_string(),
                ));
            }
        };
        let new_execution_id = new_claim.execution_id("r1", &patched_task.id);

        assert_ne!(old_execution_id, new_execution_id);
        assert_ne!(old_claim.spec_hash, new_claim.spec_hash);
        assert!(
            store
                .recoverable_subagent_result("r1", "t1", &old_execution_id)?
                .is_some()
        );
        assert!(
            store
                .recoverable_subagent_result("r1", "t1", &new_execution_id)?
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn task_update_skip_preserves_spec_and_updates_execution() {
        let s = fresh();
        seed_plan(&s);
        let plan = s
            .apply_task_patch_for_test(
                "r1",
                &TaskUpdateRequest {
                    base_revision: 1,
                    reason: "task no longer required".to_string(),
                    operations: vec![TaskUpdateOperation::Skip {
                        task_id: "t1".to_string(),
                    }],
                },
            )
            .unwrap();
        assert_eq!(plan.revision, 2);
        assert_eq!(plan.tasks.len(), 1);
        assert_eq!(plan.tasks[0].status, TodoStatus::Skipped);
    }

    #[test]
    fn task_update_update_requeues_blocked_task() {
        let s = fresh();
        seed_plan(&s);
        s.set_task_status(
            "r1",
            "t1",
            TodoStatus::Blocked,
            Some("reviewer"),
            Some("needs a clearer brief"),
        )
        .unwrap();
        let plan = s
            .apply_task_patch_for_test(
                "r1",
                &TaskUpdateRequest {
                    base_revision: 1,
                    reason: "clarify the blocked task".to_string(),
                    operations: vec![TaskUpdateOperation::Update {
                        task_id: "t1".to_string(),
                        patch: TaskPatch {
                            description: Some("Review the clarified runtime boundary".to_string()),
                            ..Default::default()
                        },
                    }],
                },
            )
            .unwrap();
        assert_eq!(plan.revision, 2);
        assert_eq!(plan.tasks[0].status, TodoStatus::Pending);
        assert_eq!(
            plan.tasks[0].description,
            "Review the clarified runtime boundary"
        );
    }

    #[test]
    fn completion_gate_rechecks_latest_plan_revision() -> Result<(), StoreError> {
        let s = fresh();
        seed_plan(&s);
        s.set_task_status("r1", "t1", TodoStatus::Completed, Some("explorer"), None)?;
        let follow_up = PlanTask {
            id: "t2".to_string(),
            title: "Verify follow-up".to_string(),
            description: "Verify evidence discovered by t1".to_string(),
            kind: PlanTaskKind::Verification,
            agent_role: "explorer".to_string(),
            depends_on: vec!["t1".to_string()],
            ..Default::default()
        };
        s.apply_task_patch_for_test(
            "r1",
            &TaskUpdateRequest {
                base_revision: 1,
                reason: "new evidence requires verification".to_string(),
                operations: vec![TaskUpdateOperation::Insert {
                    after_task_id: Some("t1".to_string()),
                    task: follow_up.spec(),
                }],
            },
        )?;
        assert!(!s.complete_run_if_quiescent("r1")?);
        s.set_task_status("r1", "t2", TodoStatus::Completed, Some("explorer"), None)?;
        assert!(s.complete_run_if_quiescent("r1")?);
        assert_eq!(
            s.get_run("r1")?
                .ok_or_else(|| StoreError::RunNotFound("r1".to_string()))?
                .status,
            TaskRunStatus::Completed
        );
        Ok(())
    }

    #[test]
    fn task_update_rejects_running_task_contract_change() -> Result<(), StoreError> {
        let store = fresh();
        seed_plan(&store);
        store.set_task_status("r1", "t1", TodoStatus::Running, Some("subagent"), None)?;
        let result = store.apply_task_patch_for_test(
            "r1",
            &TaskUpdateRequest {
                base_revision: 1,
                reason: "change active ownership".to_string(),
                operations: vec![TaskUpdateOperation::Update {
                    task_id: "t1".to_string(),
                    patch: TaskPatch {
                        files: Some(vec!["src/new-owner.rs".to_string()]),
                        ..Default::default()
                    },
                }],
            },
        );
        assert!(matches!(result, Err(StoreError::InvalidPlan(_))));
        Ok(())
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

    /// `task_update` rejects a dependency cycle and appends no revision event.
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
        s.attach_plan_for_test(&TaskPlan {
            plan_id: "p1".to_string(),
            run_id: "r1".to_string(),
            revision: 1,
            domain_profile: DomainProfile::General,
            goal: "g".to_string(),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::Parallel,
            tasks: vec![
                PlanTask {
                    id: "t1".into(),
                    depends_on: Vec::new(),
                    ..sample_task_body("t1")
                },
                PlanTask {
                    id: "t2".into(),
                    depends_on: vec!["t1".into()],
                    ..sample_task_body("t2")
                },
            ],
        })
        .unwrap();
        let before = s.list_events("r1", 0).unwrap().len();
        // Now make t1 depend on t2 → cycle.
        let err = s
            .apply_task_patch_for_test(
                "r1",
                &TaskUpdateRequest {
                    base_revision: 1,
                    reason: "introduce invalid cycle".to_string(),
                    operations: vec![TaskUpdateOperation::Update {
                        task_id: "t1".to_string(),
                        patch: TaskPatch {
                            depends_on: Some(vec!["t2".into()]),
                            ..Default::default()
                        },
                    }],
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
        seed_plan(&s);
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
}
