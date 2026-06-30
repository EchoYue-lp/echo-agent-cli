//! BackgroundTaskService — pure task lifecycle manager.
//!
//! Manages task submission, scheduling, persistence, progress tracking, and
//! cancellation. The service struct itself does not store an Agent reference;
//! instead, agent execution is delegated to a `TaskExecuteFn` closure that
//! acquires an agent from a task-scoped provider at execution time.
//!
//! ## Architecture
//!
//! The `TaskExecuteFn` closure is constructed in `AppState::start_task_service()`
//! and captures: an agent provider, manager, and event bus. This keeps all
//! Agent-related concerns outside the service itself.
//!
//! ## Concurrency
//!
//! When an `AgentPool` is available, each background task gets a distinct
//! pooled worker agent so ready tasks can run concurrently. Without a pool,
//! the service falls back to the legacy single-agent serialized path.

use super::background::*;
use super::*;
use crate::agent_handle::AgentHandle;
use async_trait::async_trait;
use dashmap::DashMap;
use echo_agent::agent::Agent;
use echo_agent::memory::Store;
use echo_agent::tasks::progress::TaskProgress;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing;

/// Runtime execution settings for background tasks.
#[derive(Debug, Clone)]
pub struct BackgroundTaskServiceConfig {
    /// Maximum top-level background tasks that may execute at once.
    pub max_concurrent: usize,
    /// Number of agent slots reserved for foreground/multi-session work.
    pub reserve_foreground_agents: usize,
    /// Maximum parallel steps inside a single composite task.
    pub composite_parallelism: usize,
}

impl Default for BackgroundTaskServiceConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 1,
            reserve_foreground_agents: 0,
            composite_parallelism: 1,
        }
    }
}

/// A unified task projection merging framework `Task` (pipeline) and `TaskRun`
/// (AgentChat/Composite). Phase 3.4: frontend reads this instead of raw `Task`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct UnifiedTaskInfo {
    pub id: String,
    pub description: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    pub result: Option<String>,
    pub error: Option<String>,
    pub kind: Option<String>,
    pub source: &'static str, // "framework" | "run"
    pub dependencies: Vec<String>,
    pub priority: u8,
}

#[async_trait]
trait TaskAgentProvider: Send + Sync {
    async fn acquire_for_task(
        &self,
        task_key: &str,
    ) -> Result<TaskAgentLease, echo_agent::error::ReactError>;
}

struct TaskAgentLease {
    agent: AgentHandle,
    release: Option<TaskAgentRelease>,
    provider: &'static str,
}

impl TaskAgentLease {
    fn agent(&self) -> AgentHandle {
        self.agent.clone()
    }

    fn provider(&self) -> &'static str {
        self.provider
    }

    async fn release(mut self) {
        if let Some(release) = self.release.take() {
            release.release().await;
        }
    }
}

impl Drop for TaskAgentLease {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            tokio::spawn(async move {
                release.release().await;
            });
        }
    }
}

enum TaskAgentRelease {
    Pool {
        pool: Arc<crate::agent_pool::AgentPool>,
        key: String,
    },
    #[cfg(test)]
    Test {
        released: Arc<std::sync::atomic::AtomicUsize>,
    },
}

impl TaskAgentRelease {
    async fn release(self) {
        match self {
            TaskAgentRelease::Pool { pool, key } => {
                pool.release(&key).await;
            }
            #[cfg(test)]
            TaskAgentRelease::Test { released } => {
                released.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        }
    }
}

struct SingleAgentTaskProvider {
    agent: AgentHandle,
}

#[async_trait]
impl TaskAgentProvider for SingleAgentTaskProvider {
    async fn acquire_for_task(
        &self,
        _task_key: &str,
    ) -> Result<TaskAgentLease, echo_agent::error::ReactError> {
        Ok(TaskAgentLease {
            agent: self.agent.clone(),
            release: None,
            provider: "single",
        })
    }
}

struct PoolTaskAgentProvider {
    pool: Arc<crate::agent_pool::AgentPool>,
}

#[async_trait]
impl TaskAgentProvider for PoolTaskAgentProvider {
    async fn acquire_for_task(
        &self,
        task_key: &str,
    ) -> Result<TaskAgentLease, echo_agent::error::ReactError> {
        let key = format!("__task__:{task_key}");
        let agent = self.pool.acquire(&key).await.map_err(|e| {
            echo_agent::error::ReactError::Other(format!("Failed to acquire task agent: {e}"))
        })?;
        Ok(TaskAgentLease {
            agent,
            release: Some(TaskAgentRelease::Pool {
                pool: self.pool.clone(),
                key,
            }),
            provider: "pool",
        })
    }
}

/// Pure task lifecycle manager.
///
/// Created once per process and shared across all modes (web, cli, tui, tauri).
/// Does NOT hold an AgentHandle — all agent operations are in the dispatch closure.
pub struct BackgroundTaskService {
    manager: Arc<TaskManager>,
    executor: Arc<TaskExecutor>,
    store: Arc<SqliteTaskStore>,
    event_bus: Arc<TaskEventBus>,
    cancel: echo_agent::agent::CancellationToken,
    config: BackgroundTaskServiceConfig,
    /// Latest progress for each task, updated by the TaskEventBus subscriber
    /// (framework/pipeline tasks only; run-sourced tasks get progress from
    /// the TaskRuntimeStore plan via the frontend).
    /// Frontends (Tauri, CLI) can query this to get real-time progress.
    latest_progress: Arc<DashMap<String, TaskProgress>>,
    /// HITL provider for background tasks — routes approval/input requests to frontends.
    hitl_provider: Arc<super::hitl_provider::BackgroundTaskHumanProvider>,
    /// Phase 3.4: TaskRuntimeStore backing AgentChat/Composite runs (which
    /// bypass the framework Task). `None` only when the caller explicitly
    /// disabled it (some tests); production always `Some` (AppState injects one).
    task_runtime_store: Option<Arc<super::task_runtime::TaskRuntimeStore>>,
    /// Agent provider reused to drive AgentChat/Composite runs (the spawned
    /// driver leases an agent from it). Same provider the executor uses.
    agent_provider: Arc<dyn TaskAgentProvider>,
}

impl BackgroundTaskService {
    /// Create a new BackgroundTaskService.
    ///
    /// `agent` is captured in the TaskExecuteFn closure.
    /// The service itself does NOT hold any Agent reference.
    pub async fn new(
        agent: AgentHandle,
        store_backend: Arc<dyn Store>,
        cancel: echo_agent::agent::CancellationToken,
        task_runtime_store: Option<Arc<super::task_runtime::TaskRuntimeStore>>,
    ) -> anyhow::Result<Self> {
        Self::with_hooks(agent, store_backend, cancel, None, task_runtime_store).await
    }

    /// Create with optional task hook bridge for YAML hook integration.
    pub async fn with_hooks(
        agent: AgentHandle,
        store_backend: Arc<dyn Store>,
        cancel: echo_agent::agent::CancellationToken,
        task_hooks: Option<Arc<dyn echo_agent::workspace::orchestration::tasks::TaskHooks>>,
        task_runtime_store: Option<Arc<super::task_runtime::TaskRuntimeStore>>,
    ) -> anyhow::Result<Self> {
        Self::with_agent_provider(
            Arc::new(SingleAgentTaskProvider { agent }),
            BackgroundTaskServiceConfig::default(),
            store_backend,
            cancel,
            task_hooks,
            task_runtime_store,
        )
        .await
    }

    /// Create with an AgentPool so top-level background tasks can execute in parallel.
    pub async fn with_pool(
        pool: Arc<crate::agent_pool::AgentPool>,
        store_backend: Arc<dyn Store>,
        cancel: echo_agent::agent::CancellationToken,
        task_hooks: Option<Arc<dyn echo_agent::workspace::orchestration::tasks::TaskHooks>>,
        task_runtime_store: Option<Arc<super::task_runtime::TaskRuntimeStore>>,
    ) -> anyhow::Result<Self> {
        let max_concurrent = pool.background_task_concurrency();
        let reserve_foreground_agents = pool.foreground_agent_reserve();
        let composite_parallelism = pool.composite_parallelism();
        Self::with_agent_provider(
            Arc::new(PoolTaskAgentProvider { pool }),
            BackgroundTaskServiceConfig {
                max_concurrent,
                reserve_foreground_agents,
                composite_parallelism,
            },
            store_backend,
            cancel,
            task_hooks,
            task_runtime_store,
        )
        .await
    }

    async fn with_agent_provider(
        agent_provider: Arc<dyn TaskAgentProvider>,
        service_config: BackgroundTaskServiceConfig,
        store_backend: Arc<dyn Store>,
        cancel: echo_agent::agent::CancellationToken,
        task_hooks: Option<Arc<dyn echo_agent::workspace::orchestration::tasks::TaskHooks>>,
        task_runtime_store: Option<Arc<super::task_runtime::TaskRuntimeStore>>,
    ) -> anyhow::Result<Self> {
        let store = Arc::new(SqliteTaskStore::new(store_backend));

        // Create event bus with logging listener
        let manager = Arc::new(TaskManager::with_logging_and_events());

        // Get event bus reference from manager for external subscribers
        let event_bus = manager.event_bus().cloned().ok_or_else(|| {
            anyhow::anyhow!(
                "TaskManager did not create an event bus; background task service cannot start"
            )
        })?;
        let event_bus = Arc::new(event_bus);

        // Create HITL provider for background tasks.
        let (hitl_provider, _hitl_rx) = super::hitl_provider::BackgroundTaskHumanProvider::new();
        let hitl_provider = Arc::new(hitl_provider);

        // Build the TaskExecuteFn closure — captures provider + manager + event_bus
        let execute_fn: TaskExecuteFn = {
            let agent_provider = agent_provider.clone();
            let manager = manager.clone();
            let event_bus = event_bus.clone();
            let hitl_provider = hitl_provider.clone();
            let service_config = service_config.clone();
            let task_runtime_store = task_runtime_store.clone();
            Arc::new(move |ctx: TaskContext| {
                let agent_provider = agent_provider.clone();
                let manager = manager.clone();
                let event_bus = event_bus.clone();
                let hitl_provider = hitl_provider.clone();
                let service_config = service_config.clone();
                let task_runtime_store = task_runtime_store.clone();
                Box::pin(async move {
                    dispatch_task(
                        ctx,
                        agent_provider,
                        manager,
                        event_bus,
                        hitl_provider,
                        service_config,
                        task_runtime_store,
                    )
                    .await
                })
            })
        };

        // Create executor. Pool-backed services can run multiple tasks concurrently;
        // single-agent services keep the legacy serialized behavior.
        let executor_config = TaskExecutorConfig {
            max_concurrent: service_config.max_concurrent.max(1),
            default_timeout_secs: 0, // 0 = no timeout
            enable_hooks: true,
            ..Default::default()
        };

        let mut executor =
            TaskExecutor::new(manager.clone(), executor_config).with_execute_fn(execute_fn);
        if let Some(hooks) = task_hooks {
            executor = executor.with_task_hook(hooks);
        }
        let executor = Arc::new(executor);

        // Progress cache — updated by a background subscriber of TaskEventBus.
        let latest_progress = Arc::new(DashMap::new());
        {
            let mut rx = event_bus.subscribe();
            let cache = latest_progress.clone();
            tokio::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(event) => {
                            match event.as_ref() {
                                TaskEvent::Progress { task_id, progress } => {
                                    cache.insert(task_id.clone(), progress.clone());
                                }
                                // Clean up cache on terminal events
                                TaskEvent::Completed { task_id, .. }
                                | TaskEvent::Failed { task_id, .. } => {
                                    let cache = cache.clone();
                                    let tid = task_id.clone();
                                    tokio::spawn(async move {
                                        tokio::time::sleep(std::time::Duration::from_secs(60))
                                            .await;
                                        cache.remove(&tid);
                                    });
                                }
                                _ => {}
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            });
        }

        Ok(Self {
            manager,
            executor,
            store,
            event_bus,
            cancel,
            config: service_config,
            latest_progress,
            hitl_provider,
            task_runtime_store,
            agent_provider,
        })
    }

    /// Submit a new background task.
    ///
    /// Creates a framework `Task` with `BackgroundTaskMeta` stored as
    /// `metadata_json`, persists it to SQLite, and schedules it for execution.
    /// Returns the task ID.
    ///
    /// `priority` (0–10, default 5) controls scheduling order when multiple
    /// tasks are ready. `depends_on` lists task IDs that must complete before
    /// this task starts.
    pub async fn submit(
        &self,
        kind: BackgroundTaskKind,
        description: &str,
        submitted_via: Option<String>,
    ) -> anyhow::Result<String> {
        self.submit_with_options(kind, description, submitted_via, None, Vec::new())
            .await
    }

    /// Submit with explicit priority and dependency list.
    pub async fn submit_with_options(
        &self,
        kind: BackgroundTaskKind,
        description: &str,
        submitted_via: Option<String>,
        priority: Option<u8>,
        depends_on: Vec<String>,
    ) -> anyhow::Result<String> {
        // Phase 3.5: submit_with_options is now pipeline-only. AgentChat/
        // Composite use submit_run/submit_dag directly (callers construct
        // PlanTask/Vec<PlanTask>). Pipeline variants (Research/DataPipeline/
        // WritingPipeline/ResearchToWriting) still create a framework Task
        // here and execute via the executor loop + dispatch_task.
        let task_id = uuid::Uuid::new_v4().to_string();
        let prio = priority.unwrap_or(5).min(10);
        let meta = BackgroundTaskMeta::new(kind.clone(), submitted_via)
            .with_priority(prio)
            .with_dependencies(depends_on.clone());

        // Create framework Task with metadata, priority, and dependencies
        let task = Task::new(task_id.clone(), description.to_string())
            .with_tags(vec![kind.tag()])
            .with_priority(prio)
            .with_dependencies(depends_on)
            .with_metadata(meta);

        // Add to manager (also persists to store via event)
        self.manager.add_task(task);

        // Persist to SQLite
        self.persist_all().await?;

        tracing::info!(
            task_id = %task_id,
            kind = %kind.display_name(),
            priority = prio,
            "Background task submitted"
        );
        Ok(task_id)
    }

    /// Phase 3.4 cancel: framework Task → executor.cancel_task; Run →
    /// best-effort transition to Cancelled via the store (caller may also
    /// cancel via AppState.run_cancel_tokens). Returns true if cancelled.
    pub async fn cancel(&self, id: &str) -> bool {
        // Framework path.
        if self.manager.get_task(id).is_some() {
            let cancelled = self.executor.cancel_task(id);
            if cancelled {
                let _ = self.persist_all().await;
                tracing::info!(task_id = %id, "Background task cancelled (framework)");
            }
            return cancelled;
        }
        // Run path: transition to Cancelled if non-terminal.
        if let Some(store) = &self.task_runtime_store {
            use super::task_runtime::TaskRunStatus;
            if let Ok(Some(run)) = store.get_run(id) {
                let is_terminal = matches!(
                    run.status,
                    TaskRunStatus::Completed | TaskRunStatus::Failed | TaskRunStatus::Cancelled
                );
                if !is_terminal {
                    let _ = store.transition_run(id, TaskRunStatus::Cancelled);
                    tracing::info!(run_id = %id, "Run cancelled (store transition)");
                    return true;
                }
            }
        }
        false
    }

    /// List all tasks, optionally filtered by status.
    pub fn list(&self, status_filter: Option<TaskStatus>) -> Vec<Task> {
        match status_filter {
            Some(status) => self
                .manager
                .get_all_tasks()
                .into_iter()
                .filter(|t| t.status == status)
                .collect(),
            None => self.manager.get_all_tasks(),
        }
    }

    /// Get a single task with its metadata.
    ///
    /// Reads `BackgroundTaskMeta` from the task's `metadata_json` field.
    pub fn get(&self, task_id: &str) -> Option<(Task, Option<BackgroundTaskMeta>)> {
        let task = self.manager.get_task(task_id)?;
        let meta = task
            .metadata_json
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok());
        Some((task, meta))
    }

    // ── Phase 3.4: Unified list / get ────────────────────────────────────

    /// Phase 3.4: list all tasks merging framework `Task` (pipeline) and
    /// `TaskRun` (AgentChat/Composite). Background runs are identified by
    /// conversation_id starting with `"background:"`. `status_filter`
    /// matches against the canonical status string.
    pub fn list_unified(&self, status_filter: Option<&str>) -> Vec<UnifiedTaskInfo> {
        let mut out = Vec::new();
        for t in self.manager.get_all_tasks() {
            let info = task_to_unified(&t, "framework");
            if pass_filter(&info.status, status_filter) {
                out.push(info);
            }
        }
        if let Some(store) = &self.task_runtime_store
            && let Ok(runs) = store.list_runs_in(&all_run_statuses())
        {
            for r in runs {
                if !r.conversation_id.starts_with("background:") {
                    continue;
                }
                let info = run_to_unified(&r, "run");
                if pass_filter(&info.status, status_filter) {
                    out.push(info);
                }
            }
        }
        out
    }

    /// Phase 3.4: unified lookup — tries framework Task first, then Run.
    pub fn get_unified(&self, id: &str) -> Option<UnifiedTaskInfo> {
        if let Some(t) = self.manager.get_task(id) {
            return Some(task_to_unified(&t, "framework"));
        }
        if let Some(store) = &self.task_runtime_store
            && let Ok(Some(r)) = store.get_run(id)
        {
            return Some(run_to_unified(&r, "run"));
        }
        None
    }

    /// Resume pending/in-progress tasks from SQLite after restart.
    ///
    /// Loads all tasks from the SQLite store, adds them back to the
    /// in-memory manager, and re-schedules non-terminal tasks.
    pub async fn resume_pending(&self) -> anyhow::Result<usize> {
        let tasks = match self.store.load_all().await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("Failed to load tasks from store: {e}");
                Vec::new()
            }
        };
        let mut resumed = 0;

        for task in tasks {
            if task.status.is_terminal() {
                // Re-add terminal tasks for history but don't execute
                self.manager.add_task(task);
                continue;
            }
            // Re-add non-terminal tasks — they'll be picked up by execute_all
            self.manager.add_task(task);
            resumed += 1;
        }

        if resumed > 0 {
            tracing::info!(
                count = resumed,
                "Resumed pending background tasks from store"
            );
        }
        Ok(resumed)
    }

    /// Start the background execution loop.
    ///
    /// Spawns a tokio task that runs `executor.execute_all()` in a loop
    /// until cancelled.
    pub fn spawn(self: Arc<Self>) {
        let svc = self.clone();
        let cancel = self.cancel.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        tracing::info!("BackgroundTaskService execution loop cancelled");
                        break;
                    }
                    result = svc.executor.execute_all() => {
                        match result {
                            Ok(_) => {
                                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                            }
                            Err(e) => {
                                tracing::error!("Background task execution error: {e}");
                                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                            }
                        }
                    }
                }
            }
        });
    }

    /// Subscribe to task events (for SSE streaming).
    pub fn subscribe_events(&self) -> broadcast::Receiver<Arc<TaskEvent>> {
        self.event_bus.subscribe()
    }

    /// Get the latest progress for a task (from TaskEventBus subscriber;
    /// framework/pipeline tasks only — run-sourced tasks return None here).
    pub fn get_progress(&self, task_id: &str) -> Option<TaskProgress> {
        self.latest_progress.get(task_id).map(|v| v.clone())
    }

    /// Runtime execution settings for this service.
    pub fn config(&self) -> &BackgroundTaskServiceConfig {
        &self.config
    }

    /// Get the underlying TaskManager (for advanced use).
    pub fn manager(&self) -> &Arc<TaskManager> {
        &self.manager
    }

    /// Get the HITL provider (for subscribing to HITL events from frontends).
    pub fn hitl_provider(&self) -> &Arc<super::hitl_provider::BackgroundTaskHumanProvider> {
        &self.hitl_provider
    }

    async fn persist_all(&self) -> anyhow::Result<()> {
        let tasks = self.manager.get_all_tasks();
        self.store
            .save_all(&tasks)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to persist tasks: {e}"))
    }

    /// Phase 3.4: submit an AgentChat-style prompt as a Run (no framework Task).
    ///
    /// Creates the run synchronously (so the id is returnable to the caller),
    /// then spawns a driver that calls `drive_unattended_run`. Returns the
    /// `run_id` immediately. The agent's ReAct loop may call `task_create` +
    /// `execute_plan`; a simple prompt auto-Completes (Q5).
    ///
    /// Per-run `CancellationToken` registration lives on `AppState.tasks
    /// .run_cancel_tokens` (keyed `__run__:{run_id}`), which this service does
    /// NOT own. The Tauri `cancel_task` command inserts/looks up the token
    /// there (Task 3). The driver uses a child of the service's own cancel
    /// token for in-run cancellation; process-wide cancel propagates via the
    /// parent.
    pub async fn submit_run(
        &self,
        prompt: &str,
        description: &str,
        source_kind: &str,
        source_id: &str,
    ) -> anyhow::Result<String> {
        use super::task_runtime::{AttendedMode, DomainProfile, TaskRouteKind, TaskRunStatus};

        let store = self.task_runtime_store.clone().ok_or_else(|| {
            anyhow::anyhow!("TaskRuntimeStore not configured; cannot submit AgentChat run")
        })?;
        let lease = self
            .agent_provider
            .acquire_for_task(&format!("{source_kind}:{source_id}"))
            .await
            .map_err(|e| anyhow::anyhow!("acquire agent: {e}"))?;
        let agent = lease.agent();
        install_background_hitl_provider(&agent, self.hitl_provider.clone()).await;
        // Phase C: pooled agents are built without ExecutePlanTool (worker
        // stance, §10.2), but a submit_run's agent drives task_create +
        // execute_plan (primary role). Register it, mirroring the cron path's
        // register_execute_plan_on_agent. Without this a complex AgentChat
        // prompt can't execute its plan via execute_plan (silent degrade).
        {
            use super::task_runtime::ExecutePlanTool;
            let tool = ExecutePlanTool::new(store.clone(), agent.clone());
            agent
                .write(|a| {
                    a.add_tool(Box::new(tool));
                    true
                })
                .await;
        }

        let fire_id = uuid::Uuid::new_v4().to_string();
        let run_id = uuid::Uuid::new_v4().to_string();
        let conversation_id = format!("{source_kind}:{source_id}:{fire_id}");
        let goal = if description.trim().is_empty() {
            prompt
        } else {
            description
        };
        store
            .create_run(
                &run_id,
                "default",
                &conversation_id,
                "", // root_message_id — no chat message for background run
                DomainProfile::General,
                goal,
                TaskRouteKind::ParallelReadonlyDelegation.as_str(),
                AttendedMode::Unattended,
            )
            .map_err(|e| anyhow::anyhow!("create_run: {e}"))?;
        store
            .transition_run(&run_id, TaskRunStatus::Running)
            .map_err(|e| anyhow::anyhow!("transition_run: {e}"))?;

        let child_cancel = self.cancel.child_token();
        let store_clone = store.clone();
        let agent_clone = agent.clone();
        let prompt_owned = prompt.to_string();
        let run_id_owned = run_id.clone();
        let source_kind_owned = source_kind.to_string();
        tokio::spawn(async move {
            let result = super::task_runtime::drive_unattended_run(
                store_clone,
                agent_clone,
                &run_id_owned,
                &source_kind_owned,
                &fire_id,
                &prompt_owned,
                child_cancel,
                super::task_runtime::UnattendedWriteMode::default(), // D7 stage 2: Worktree
                super::task_runtime::worktree::git_repo_root(std::path::Path::new(".")).ok(),
            )
            .await;
            lease.release().await;
            if let Err(e) = result {
                tracing::error!(run_id = %run_id_owned, error = %e, "AgentChat run driver failed");
            }
        });
        tracing::info!(run_id = %run_id, "AgentChat run submitted (Phase 3.4)");
        Ok(run_id)
    }

    /// Phase 3.4: submit a pre-constructed `PlanTask` DAG as a Run (Composite
    /// path). Creates the run + attaches the plan, then spawns a driver that
    /// calls `execute_run` (the DAG executor). Returns the `run_id`.
    pub async fn submit_dag(
        &self,
        plan_tasks: Vec<super::task_runtime::PlanTask>,
        description: &str,
        source_kind: &str,
        source_id: &str,
    ) -> anyhow::Result<String> {
        use super::task_runtime::{
            AttendedMode, DomainProfile, ExecutionMode, TaskPlan, TaskRouteKind, TaskRunStatus,
        };

        let store = self.task_runtime_store.clone().ok_or_else(|| {
            anyhow::anyhow!("TaskRuntimeStore not configured; cannot submit DAG run")
        })?;
        let lease = self
            .agent_provider
            .acquire_for_task(&format!("{source_kind}:{source_id}"))
            .await
            .map_err(|e| anyhow::anyhow!("acquire agent: {e}"))?;
        let agent = lease.agent();
        install_background_hitl_provider(&agent, self.hitl_provider.clone()).await;

        let run_id = uuid::Uuid::new_v4().to_string();
        let goal = if description.trim().is_empty() {
            "composite"
        } else {
            description
        };
        let conversation_id = format!("{source_kind}:{source_id}:{}", uuid::Uuid::new_v4());
        store
            .create_run(
                &run_id,
                "default",
                &conversation_id,
                "",
                DomainProfile::General,
                goal,
                TaskRouteKind::ParallelReadonlyDelegation.as_str(),
                AttendedMode::Unattended,
            )
            .map_err(|e| anyhow::anyhow!("create_run: {e}"))?;
        store
            .transition_run(&run_id, TaskRunStatus::Running)
            .map_err(|e| anyhow::anyhow!("transition_run: {e}"))?;

        let plan = TaskPlan {
            plan_id: uuid::Uuid::new_v4().to_string(),
            run_id: run_id.clone(),
            domain_profile: DomainProfile::General,
            goal: goal.to_string(),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::default(),
            tasks: plan_tasks,
        };
        store
            .attach_plan(&plan)
            .map_err(|e| anyhow::anyhow!("attach_plan: {e}"))?;

        // Child of self.cancel so graceful shutdown propagates to the detached
        // DAG driver (mirrors submit_run). Per-run cancel via
        // AppState.run_cancel_tokens is wired by the Tauri cancel_task command.
        let cancel = self.cancel.child_token();
        let store_clone = store.clone();
        let run_id_owned = run_id.clone();
        tokio::spawn(async move {
            let outcome = super::task_runtime::execute_run(
                store_clone,
                Some(agent),
                None, // reviewer_llm — background run has no GUI review gate
                None, // layer_manager — no memory-layer evolution off a background run
                None, // run_store — no trace persistence (mirrors legacy execute_composite)
                None, // trace_sink — no worker://trace event stream
                &run_id_owned,
                cancel,
                // B5.1: no memory write for background DAG runs (layer_manager is
                // None anyway); explicit None for clarity.
                super::task_runtime::MemoryPolicy::None,
            )
            .await;
            lease.release().await;
            match outcome {
                Ok(o) => tracing::info!(run_id = %run_id_owned, ?o, "DAG run finished"),
                Err(e) => tracing::error!(run_id = %run_id_owned, error = %e, "DAG run failed"),
            }
        });
        tracing::info!(run_id = %run_id, "Composite DAG run submitted (Phase 3.4)");
        Ok(run_id)
    }
}

// ── Phase 3.4: UnifiedTaskInfo helpers ──────────────────────────────────

fn all_run_statuses() -> Vec<super::task_runtime::TaskRunStatus> {
    use super::task_runtime::TaskRunStatus;
    vec![
        TaskRunStatus::Pending,
        TaskRunStatus::Running,
        TaskRunStatus::Paused,
        TaskRunStatus::Cancelled,
        TaskRunStatus::Failed,
        TaskRunStatus::Completed,
    ]
}

fn pass_filter(status: &str, filter: Option<&str>) -> bool {
    match filter {
        Some(f) => status == f,
        None => true,
    }
}

fn task_to_unified(t: &Task, source: &'static str) -> UnifiedTaskInfo {
    let kind = t.tags.iter().find(|x| x.starts_with("bg:kind:")).cloned();
    let (status, error) = status_to_str(&t.status);
    // Task.created_at/updated_at are u64 unix seconds.
    let created_at = chrono::DateTime::from_timestamp(t.created_at as i64, 0)
        .map(|d| d.to_rfc3339())
        .unwrap_or_default();
    let updated_at = chrono::DateTime::from_timestamp(t.updated_at as i64, 0)
        .map(|d| d.to_rfc3339())
        .unwrap_or_default();
    UnifiedTaskInfo {
        id: t.id.clone(),
        description: t.description.clone(),
        status,
        error,
        created_at,
        updated_at,
        result: t.result.clone(),
        kind,
        source,
        dependencies: t.dependencies.clone(),
        priority: t.priority,
    }
}

fn run_to_unified(r: &super::task_runtime::TaskRun, source: &'static str) -> UnifiedTaskInfo {
    use super::task_runtime::TaskRunStatus;
    let kind = match r.conversation_id.split(':').next() {
        Some("background") => Some("bg:kind:agent_chat".to_string()),
        _ => None,
    };
    let status = match r.status {
        TaskRunStatus::Pending => "pending",
        TaskRunStatus::Running => "in_progress",
        TaskRunStatus::Paused => "paused",
        TaskRunStatus::Cancelled => "cancelled",
        TaskRunStatus::Failed => "failed",
        TaskRunStatus::Completed => "completed",
    }
    .to_string();
    UnifiedTaskInfo {
        id: r.run_id.clone(),
        description: r.goal.clone(),
        status,
        created_at: r.created_at.to_rfc3339(),
        updated_at: r.updated_at.to_rfc3339(),
        result: None,
        error: None,
        kind,
        source,
        dependencies: Vec::new(),
        priority: 5,
    }
}

fn status_to_str(s: &TaskStatus) -> (String, Option<String>) {
    use echo_agent::tasks::TaskStatus::*;
    match s {
        Pending => ("pending".into(), None),
        InProgress => ("in_progress".into(), None),
        Completed => ("completed".into(), None),
        Cancelled => ("cancelled".into(), None),
        Failed(e) => ("failed".into(), Some(e.clone())),
        Blocked(e) => ("blocked".into(), Some(e.clone())),
        TimedOut { error } => ("timed_out".into(), Some(error.clone())),
        Retrying {
            attempt,
            last_error,
        } => (
            "retrying".into(),
            Some(format!("attempt {attempt}: {last_error}")),
        ),
        Skipped => ("skipped".into(), None),
        Paused(reason) => ("paused".into(), Some(reason.clone())),
    }
}

// ── Unified Task Dispatch ──

/// Dispatch a task using a task-scoped agent from the provider.
///
/// Pool-backed providers return a distinct worker per task, allowing ready
/// tasks to execute concurrently. Single-agent providers preserve the legacy
/// serialized behavior.
///
/// This function is captured in the TaskExecuteFn closure constructed in
/// `AppState::start_task_service()`.
async fn dispatch_task(
    ctx: TaskContext,
    agent_provider: Arc<dyn TaskAgentProvider>,
    manager: Arc<TaskManager>,
    _event_bus: Arc<TaskEventBus>,
    hitl_provider: Arc<super::hitl_provider::BackgroundTaskHumanProvider>,
    _service_config: BackgroundTaskServiceConfig,
    _task_runtime_store: Option<Arc<super::task_runtime::TaskRuntimeStore>>,
) -> Result<String, echo_agent::error::ReactError> {
    let task = manager.get_task(&ctx.task_id).ok_or_else(|| {
        echo_agent::error::ReactError::Other(format!(
            "Task not found in manager: task_id={}",
            ctx.task_id
        ))
    })?;

    let meta: BackgroundTaskMeta =
        serde_json::from_value(task.metadata_json.clone().ok_or_else(|| {
            echo_agent::error::ReactError::Other(format!(
                "No metadata found for task_id={}",
                ctx.task_id
            ))
        })?)
        .map_err(|e| {
            echo_agent::error::ReactError::Other(format!(
                "Failed to deserialize metadata for task_id={}: {}",
                ctx.task_id, e
            ))
        })?;

    // Phase 3.5: AgentChat/Composite no longer create framework Tasks (submit
    // short-circuits them to Runs). Only pipeline variants reach dispatch_task.
    let lease = agent_provider.acquire_for_task(&ctx.task_id).await?;
    let agent = lease.agent();
    install_background_hitl_provider(&agent, hitl_provider.clone()).await;
    log_task_runtime_model(&ctx.task_id, &agent, lease.provider()).await;
    tracing::debug!(
        task_id = %ctx.task_id,
        workspace_write_policy = ?meta.workspace_write_policy,
        sandbox_execution_policy = ?meta.sandbox_execution_policy,
        "Background task resource policy"
    );

    // Pipeline tasks — route to structured graph pipelines instead of generic prompt
    let result = match &meta.kind {
        BackgroundTaskKind::Research {
            topic,
            max_papers,
            output_format: _,
        } => {
            tracing::info!(
                task_id = %ctx.task_id,
                pipeline = "research",
                topic = %topic,
                max_papers = max_papers,
                "Executing research pipeline"
            );
            let config =
                super::pipelines::ResearchConfig::new(topic.as_str()).with_max_papers(*max_papers);
            let result = super::pipelines::run_research_with_config(agent.clone(), config).await;
            result.map_err(|e| {
                echo_agent::error::ReactError::Other(format!("Research pipeline failed: {e}"))
            })
        }

        BackgroundTaskKind::ResearchToWriting {
            topic,
            max_papers,
            audience,
            format,
            research_max_revisions,
            research_quality_threshold,
            writing_max_revisions,
            writing_quality_threshold,
        } => {
            tracing::info!(
                task_id = %ctx.task_id,
                pipeline = "research_to_writing",
                topic = %topic,
                max_papers = max_papers,
                "Executing research-to-writing pipeline"
            );
            let config = super::pipelines::ResearchToWritingConfig::new(topic.as_str())
                .with_max_papers(*max_papers)
                .with_audience(audience.as_str())
                .with_format(format.as_str())
                .with_research_revisions(*research_max_revisions, *research_quality_threshold)
                .with_writing_revisions(*writing_max_revisions, *writing_quality_threshold);
            let result = super::pipelines::run_research_to_writing(agent.clone(), config).await;
            result.map_err(|e| {
                echo_agent::error::ReactError::Other(format!(
                    "Research-to-writing pipeline failed: {e}"
                ))
            })
        }

        BackgroundTaskKind::DataPipeline {
            dataset_path,
            objective,
            max_charts,
        } => {
            tracing::info!(
                task_id = %ctx.task_id,
                pipeline = "data_analysis",
                dataset_path = %dataset_path,
                max_charts = max_charts,
                "Executing data analysis pipeline"
            );
            let mut config = super::pipelines::DataPipelineConfig::new(dataset_path.as_str())
                .with_max_charts(*max_charts);
            if let Some(obj) = objective {
                config = config.with_objective(obj.as_str());
            }
            let result =
                super::pipelines::run_data_pipeline_with_config(agent.clone(), config).await;
            result.map_err(|e| {
                echo_agent::error::ReactError::Other(format!("Data pipeline failed: {e}"))
            })
        }

        BackgroundTaskKind::WritingPipeline {
            topic,
            audience,
            format,
            max_revisions,
            quality_threshold,
        } => {
            tracing::info!(
                task_id = %ctx.task_id,
                pipeline = "writing",
                topic = %topic,
                audience = %audience,
                "Executing writing pipeline"
            );
            let config = super::pipelines::WritingPipelineConfig::new(topic.as_str())
                .with_audience(audience.as_str())
                .with_format(format.as_str())
                .with_max_revisions(*max_revisions)
                .with_quality_threshold(*quality_threshold);
            let result =
                super::pipelines::run_writing_pipeline_with_config(agent.clone(), config).await;
            result.map_err(|e| {
                echo_agent::error::ReactError::Other(format!("Writing pipeline failed: {e}"))
            })
        }
    };

    lease.release().await;
    result
}

async fn log_task_runtime_model(task_id: &str, agent: &AgentHandle, agent_provider: &'static str) {
    agent
        .read(|agent| {
            let llm_config = agent.llm_config();
            let auth_source = if llm_config.is_some() {
                "configured"
            } else {
                "fallback_env_or_models"
            };
            tracing::info!(
                task_id = %task_id,
                agent_provider = agent_provider,
                model = %agent.model_name(),
                llm_provider = ?llm_config.map(|config| &config.provider),
                has_base_url = llm_config
                    .map(|config| !config.base_url.is_empty())
                    .unwrap_or(false),
                auth_source = auth_source,
                "Background task runtime model"
            );
        })
        .await;
}

async fn install_background_hitl_provider(
    agent: &AgentHandle,
    hitl_provider: Arc<super::hitl_provider::BackgroundTaskHumanProvider>,
) {
    agent
        .write(|a| {
            a.set_human_loop_provider(
                hitl_provider as Arc<dyn echo_agent::human_loop::HumanLoopProvider>,
            )
        })
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::time::{Duration, Instant};

    fn create_test_agent_handle() -> AgentHandle {
        create_test_agent_handle_with_response("ok")
    }

    fn create_test_agent_handle_with_response(response: &str) -> AgentHandle {
        use echo_agent::agent::ReactAgentBuilder;
        use echo_agent::testing::MockLlmClient;

        let mock_llm = Arc::new(
            MockLlmClient::new()
                .with_model_name("test-model")
                .with_response(response),
        );
        let agent = ReactAgentBuilder::new()
            .model("test-model")
            .llm_client(mock_llm)
            .build()
            .expect("test agent should build");
        AgentHandle::new(agent)
    }

    #[tokio::test]
    async fn submit_agent_chat_creates_run_not_framework_task() {
        // Phase 3.4-2: AgentChat must bypass the framework Task and create a
        // Run in TaskRuntimeStore instead (asymmetric split, D3-2/Q1).
        let store =
            Arc::new(super::task_runtime::TaskRuntimeStore::new_in_memory().expect("store"));
        let agent = create_test_agent_handle();
        let svc = BackgroundTaskService::with_hooks(
            agent,
            Arc::new(echo_agent::memory::InMemoryStore::new())
                as Arc<dyn echo_agent::memory::Store>,
            echo_agent::agent::CancellationToken::new(),
            None,
            Some(store.clone()),
        )
        .await
        .expect("service should build");

        let run_id = svc
            .submit_run("hi", "test chat", "background", "ipc")
            .await
            .expect("submit_run should succeed");

        // The returned id keys a real Run in TaskRuntimeStore.
        assert!(store.get_run(&run_id).expect("get_run").is_some());
        // The framework Task store has NO task for this id.
        let framework_tasks = svc.list(None);
        assert!(
            framework_tasks.iter().all(|t| t.id != run_id),
            "AgentChat must not create a framework Task"
        );
    }

    #[tokio::test]
    async fn submit_composite_creates_dag_run() {
        // Phase 3.5: Composite variant is deleted; callers construct PlanTask
        // DAGs directly and call submit_dag. Sequential chain: s2 depends_on s1.
        use super::task_runtime::{PlanTask, PlanTaskKind};
        let store =
            Arc::new(super::task_runtime::TaskRuntimeStore::new_in_memory().expect("store"));
        let agent = create_test_agent_handle();
        let svc = BackgroundTaskService::with_hooks(
            agent,
            Arc::new(echo_agent::memory::InMemoryStore::new())
                as Arc<dyn echo_agent::memory::Store>,
            echo_agent::agent::CancellationToken::new(),
            None,
            Some(store.clone()),
        )
        .await
        .expect("service should build");

        let plan_tasks = vec![
            PlanTask {
                id: "s1".into(),
                title: "s1".into(),
                description: "a".into(),
                kind: PlanTaskKind::Implementation,
                agent_role: "implementer".into(),
                sort_order: 0,
                depends_on: vec![],
                ..Default::default()
            },
            PlanTask {
                id: "s2".into(),
                title: "s2".into(),
                description: "b".into(),
                kind: PlanTaskKind::Implementation,
                agent_role: "implementer".into(),
                sort_order: 1,
                depends_on: vec!["s1".into()],
                ..Default::default()
            },
        ];
        let run_id = svc
            .submit_dag(plan_tasks, "comp", "background", "ipc")
            .await
            .expect("submit_dag should succeed");

        let _run = store
            .get_run(&run_id)
            .expect("get_run")
            .expect("run exists");
        let plan = store
            .get_plan(&run_id)
            .expect("get_plan")
            .expect("plan exists");
        assert_eq!(plan.tasks.len(), 2);
        // Sequential chain: s2 depends_on s1.
        assert_eq!(plan.tasks[1].depends_on, vec!["s1".to_string()]);
    }

    #[tokio::test]
    async fn list_unified_merges_framework_tasks_and_runs() {
        // Phase 3.4: list_unified must show both pipeline framework Tasks AND
        // AgentChat Runs (asymmetric split, D3-2).
        let store =
            Arc::new(super::task_runtime::TaskRuntimeStore::new_in_memory().expect("store"));
        let agent = create_test_agent_handle();
        let svc = BackgroundTaskService::with_hooks(
            agent,
            Arc::new(echo_agent::memory::InMemoryStore::new())
                as Arc<dyn echo_agent::memory::Store>,
            echo_agent::agent::CancellationToken::new(),
            None,
            Some(store.clone()),
        )
        .await
        .expect("service should build");

        // AgentChat → Run (Phase 3.5: call submit_run directly; variant deleted).
        let chat_id = svc
            .submit_run("x", "chat", "background", "ipc")
            .await
            .expect("submit_run");
        // Research → framework Task.
        let _research_id = svc
            .submit(
                BackgroundTaskKind::Research {
                    topic: "t".into(),
                    max_papers: 1,
                    output_format: Default::default(),
                },
                "research",
                Some("t".into()),
            )
            .await
            .expect("submit research");

        let listed = svc.list_unified(None);
        let ids: Vec<&str> = listed.iter().map(|t| t.id.as_str()).collect();
        assert!(
            ids.contains(&chat_id.as_str()),
            "must include AgentChat run"
        );
        assert!(
            listed
                .iter()
                .any(|t| t.kind.as_deref() == Some("bg:kind:agent_chat")),
            "must have bg:kind:agent_chat"
        );
        assert!(
            listed
                .iter()
                .any(|t| t.kind.as_deref() == Some("bg:kind:research")),
            "must include Research framework task"
        );
        assert!(
            listed.iter().any(|t| t.source == "run"),
            "must have at least one run-sourced entry"
        );
        assert!(
            listed.iter().any(|t| t.source == "framework"),
            "must have at least one framework-sourced entry"
        );
    }

    #[test]
    fn default_background_task_service_config_is_serial() {
        let config = BackgroundTaskServiceConfig::default();
        assert_eq!(config.max_concurrent, 1);
        assert_eq!(config.reserve_foreground_agents, 0);
        assert_eq!(config.composite_parallelism, 1);
    }

    #[tokio::test]
    async fn single_agent_provider_reuses_the_same_agent_without_release() {
        let handle = create_test_agent_handle();
        let provider = SingleAgentTaskProvider {
            agent: handle.clone(),
        };

        let lease = provider.acquire_for_task("task-1").await.unwrap();

        assert!(Arc::ptr_eq(handle.inner(), lease.agent().inner()));
        assert!(lease.release.is_none());
    }

    #[tokio::test]
    async fn task_agent_lease_drop_releases_worker_as_fallback() {
        let released = Arc::new(AtomicUsize::new(0));
        let lease = TaskAgentLease {
            agent: create_test_agent_handle(),
            release: Some(TaskAgentRelease::Test {
                released: released.clone(),
            }),
            provider: "test",
        };

        drop(lease);
        tokio::time::sleep(Duration::from_millis(20)).await;

        assert_eq!(released.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn task_executor_runs_ready_tasks_concurrently() {
        let manager = Arc::new(TaskManager::with_logging_and_events());
        manager.add_task(Task::new("task-a".to_string(), "A".to_string()));
        manager.add_task(Task::new("task-b".to_string(), "B".to_string()));

        let execute_fn: TaskExecuteFn = Arc::new(|_ctx| {
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(150)).await;
                Ok("done".to_string())
            })
        });

        let executor = TaskExecutor::new(
            manager,
            TaskExecutorConfig {
                max_concurrent: 2,
                retry_delay_secs: 0,
                retry_jitter: false,
                default_timeout_secs: 0,
                ..Default::default()
            },
        )
        .with_execute_fn(execute_fn);

        let started = Instant::now();
        let results = executor.execute_ready_tasks().await.unwrap();

        assert_eq!(results.len(), 2);
        assert!(
            started.elapsed() < Duration::from_millis(260),
            "two 150ms tasks should run concurrently, elapsed={:?}",
            started.elapsed()
        );
    }
}
