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
    /// Latest progress for each task, updated by ProgressBridge via TaskEventBus.
    /// Frontends (Tauri, CLI) can query this to get real-time progress.
    latest_progress: Arc<DashMap<String, TaskProgress>>,
    /// HITL provider for background tasks — routes approval/input requests to frontends.
    hitl_provider: Arc<super::hitl_provider::BackgroundTaskHumanProvider>,
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
    ) -> anyhow::Result<Self> {
        Self::with_hooks(agent, store_backend, cancel, None).await
    }

    /// Create with optional task hook bridge for YAML hook integration.
    pub async fn with_hooks(
        agent: AgentHandle,
        store_backend: Arc<dyn Store>,
        cancel: echo_agent::agent::CancellationToken,
        task_hooks: Option<Arc<dyn echo_agent::workspace::orchestration::tasks::TaskHooks>>,
    ) -> anyhow::Result<Self> {
        Self::with_agent_provider(
            Arc::new(SingleAgentTaskProvider { agent }),
            BackgroundTaskServiceConfig::default(),
            store_backend,
            cancel,
            task_hooks,
        )
        .await
    }

    /// Create with an AgentPool so top-level background tasks can execute in parallel.
    pub async fn with_pool(
        pool: Arc<crate::agent_pool::AgentPool>,
        store_backend: Arc<dyn Store>,
        cancel: echo_agent::agent::CancellationToken,
        task_hooks: Option<Arc<dyn echo_agent::workspace::orchestration::tasks::TaskHooks>>,
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
        )
        .await
    }

    async fn with_agent_provider(
        agent_provider: Arc<dyn TaskAgentProvider>,
        service_config: BackgroundTaskServiceConfig,
        store_backend: Arc<dyn Store>,
        cancel: echo_agent::agent::CancellationToken,
        task_hooks: Option<Arc<dyn echo_agent::workspace::orchestration::tasks::TaskHooks>>,
    ) -> anyhow::Result<Self> {
        let store = Arc::new(SqliteTaskStore::new(store_backend));

        // Create event bus with logging listener
        let manager = Arc::new(TaskManager::with_logging_and_events());

        // Get event bus reference from manager for external subscribers
        let event_bus = manager
            .event_bus()
            .cloned()
            .expect("with_logging_and_events always creates an event bus");
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
            Arc::new(move |ctx: TaskContext| {
                let agent_provider = agent_provider.clone();
                let manager = manager.clone();
                let event_bus = event_bus.clone();
                let hitl_provider = hitl_provider.clone();
                let service_config = service_config.clone();
                Box::pin(async move {
                    dispatch_task(
                        ctx,
                        agent_provider,
                        manager,
                        event_bus,
                        hitl_provider,
                        service_config,
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

    /// Cancel a running or pending task.
    pub async fn cancel(&self, task_id: &str) -> bool {
        let executor_cancelled = self.executor.cancel_task(task_id);
        if executor_cancelled {
            let _ = self.persist_all().await;
            tracing::info!(task_id = %task_id, "Background task cancelled");
        }
        executor_cancelled
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

    /// Get the latest progress for a task (updated by ProgressBridge).
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

    // ── HITL checkpoint integration ──

    /// List pending human checkpoint requests from background tasks.
    pub async fn pending_checkpoints(
        &self,
    ) -> Vec<(String, super::long_running::HumanCheckpointRequest)> {
        let request_ids = self.hitl_provider.pending_request_ids();
        let mut result = Vec::new();
        for id in request_ids {
            if let Some(event) = self.hitl_provider.get_pending(&id) {
                let request = echo_agent::human_loop::HumanLoopRequest {
                    kind: echo_agent::human_loop::HumanLoopKind::Selection,
                    prompt: event.prompt,
                    tool_name: event.tool_name,
                    args: event.args,
                    risk_level: None,
                    timeout: None,
                    task_id: Some(event.task_id),
                    options: event.options,
                    context: event.context,
                    phase: event.phase,
                };
                result.push((event.request_id, request));
            }
        }
        result
    }

    /// Respond to a pending human checkpoint request.
    pub async fn respond_to_checkpoint(
        &self,
        request_id: &str,
        selection: &str,
        instructions: Option<String>,
    ) -> bool {
        let response = echo_agent::human_loop::HumanLoopResponse::Selection {
            selection: selection.to_string(),
            instructions,
        };
        self.hitl_provider.respond(request_id, response)
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
    event_bus: Arc<TaskEventBus>,
    hitl_provider: Arc<super::hitl_provider::BackgroundTaskHumanProvider>,
    service_config: BackgroundTaskServiceConfig,
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

    // Composite tasks need special handling — they orchestrate sub-tasks
    if let BackgroundTaskKind::Composite { steps, strategy } = &meta.kind {
        return execute_composite(
            ctx,
            agent_provider,
            hitl_provider,
            service_config.composite_parallelism,
            steps.clone(),
            strategy.clone(),
        )
        .await;
    }

    // Cron and Workflow are defined but not yet implemented — return clear error
    match &meta.kind {
        BackgroundTaskKind::Cron { .. } => {
            return Err(echo_agent::error::ReactError::Other(
                "Cron task kind is defined but not yet implemented. Use /cron command for scheduled tasks.".into(),
            ));
        }
        BackgroundTaskKind::Workflow { .. } => {
            return Err(echo_agent::error::ReactError::Other(
                "Workflow task kind is defined but not yet implemented. Use /workflow command instead.".into(),
            ));
        }
        _ => {}
    }

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

        // Non-pipeline tasks fall through to generic prompt execution
        _ => execute_prompt_task(ctx, agent, event_bus, &meta.kind).await,
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

async fn execute_prompt_task(
    ctx: TaskContext,
    agent: AgentHandle,
    event_bus: Arc<TaskEventBus>,
    kind: &BackgroundTaskKind,
) -> Result<String, echo_agent::error::ReactError> {
    let prompt = kind.to_prompt();

    tracing::info!(
        task_id = %ctx.task_id,
        mode = %kind.mode_name(),
        prompt_len = prompt.len(),
        "Executing task on background worker agent"
    );

    // Register ProgressBridge callback (brief write lock)
    let bridge = Arc::new(super::progress_bridge::ProgressBridge::new(
        ctx.task_id.clone(),
        event_bus,
        0, // unlimited; progress uses diminishing curve
    ));
    let bridge_clone = bridge.clone();
    agent
        .write(|a| {
            a.add_callback(bridge_clone);
        })
        .await;

    let result = agent
        .read_async(|a| {
            let prompt = prompt.clone();
            Box::pin(async move { a.execute(&prompt).await })
        })
        .await;

    // Clean up callback. Pool workers execute one task at a time; this remains
    // task-local for pooled execution and preserves legacy single-agent behavior.
    bridge.disable();
    agent
        .write(|a| {
            a.remove_callbacks_by_type_name_and_id("ProgressBridge", &ctx.task_id);
        })
        .await;

    result.map_err(|e| echo_agent::error::ReactError::Other(format!("Agent execution failed: {e}")))
}

/// Execute a composite task by delegating to the framework's composite module.
///
/// Each sub-step runs on the shared main agent via `dispatch_sub_task`.
async fn execute_composite(
    ctx: TaskContext,
    agent_provider: Arc<dyn TaskAgentProvider>,
    hitl_provider: Arc<super::hitl_provider::BackgroundTaskHumanProvider>,
    composite_parallelism: usize,
    steps: Vec<super::background::CompositeStep>,
    strategy: super::background::CompositeStrategy,
) -> Result<String, echo_agent::error::ReactError> {
    use echo_agent::tasks::composite::{
        CompositePlan as FrameworkCompositePlan, CompositeStep as FrameworkCompositeStep,
        CompositeStrategy as FrameworkCompositeStrategy,
        execute_composite as framework_execute_composite,
    };

    let composite_limiter = Arc::new(tokio::sync::Semaphore::new(composite_parallelism.max(1)));
    let framework_steps: Vec<FrameworkCompositeStep> = steps
        .iter()
        .enumerate()
        .map(|(i, step)| {
            let agent_provider = agent_provider.clone();
            let hitl_provider = hitl_provider.clone();
            let parent_task_id = ctx.task_id.clone();
            let composite_limiter = composite_limiter.clone();
            let kind = step.kind.clone();
            let step_id = step
                .description
                .clone()
                .unwrap_or_else(|| format!("step_{}", i + 1));
            FrameworkCompositeStep {
                id: step_id.clone(),
                name: step_id,
                execute_fn: Arc::new(move |ctx| {
                    let agent_provider = agent_provider.clone();
                    let hitl_provider = hitl_provider.clone();
                    let parent_task_id = parent_task_id.clone();
                    let composite_limiter = composite_limiter.clone();
                    let kind = kind.clone();
                    Box::pin(async move {
                        let _permit = composite_limiter.acquire_owned().await.map_err(|e| {
                            echo_agent::error::ReactError::Other(format!(
                                "Composite parallelism limiter closed: {e}"
                            ))
                        })?;
                        dispatch_sub_task(
                            ctx,
                            agent_provider,
                            hitl_provider,
                            &parent_task_id,
                            &kind,
                        )
                        .await
                    })
                }),
                input_from: step.input_from.clone(),
            }
        })
        .collect();

    let framework_strategy = match strategy {
        super::background::CompositeStrategy::Sequential => FrameworkCompositeStrategy::Sequential,
        super::background::CompositeStrategy::Parallel => FrameworkCompositeStrategy::Parallel,
    };

    let plan = FrameworkCompositePlan {
        steps: framework_steps,
        strategy: framework_strategy,
    };

    let results = framework_execute_composite(plan).await?;

    Ok(format!(
        "Composite task completed {} steps:\n{}",
        results.len(),
        results
            .iter()
            .map(|(id, output)| format!("[{id}]: {output}"))
            .collect::<Vec<_>>()
            .join("\n---\n")
    ))
}

fn composite_step_task_key(parent_task_id: &str, step_id: &str) -> String {
    format!("{parent_task_id}:step:{step_id}")
}

/// Dispatch a sub-task within a composite task using a task-scoped worker agent.
async fn dispatch_sub_task(
    ctx: TaskContext,
    agent_provider: Arc<dyn TaskAgentProvider>,
    hitl_provider: Arc<super::hitl_provider::BackgroundTaskHumanProvider>,
    parent_task_id: &str,
    kind: &super::background::BackgroundTaskKind,
) -> Result<String, echo_agent::error::ReactError> {
    use super::background::BackgroundTaskKind;

    if let BackgroundTaskKind::Composite { .. } = kind {
        return Err(echo_agent::error::ReactError::Other(
            "Nested composite tasks are not supported".to_string(),
        ));
    }

    let prompt = kind.to_prompt();
    let task_key = composite_step_task_key(parent_task_id, &ctx.task_id);
    let lease = agent_provider.acquire_for_task(&task_key).await?;
    let agent = lease.agent();
    install_background_hitl_provider(&agent, hitl_provider).await;

    let result = agent
        .read_async(|a| {
            let prompt = prompt.clone();
            Box::pin(async move { a.execute(&prompt).await })
        })
        .await;

    lease.release().await;

    result.map_err(|e| {
        echo_agent::error::ReactError::Other(format!("Sub-task execution failed: {e}"))
    })
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

    #[test]
    fn composite_parallel_step_keys_are_parent_scoped() {
        assert_eq!(
            composite_step_task_key("parent-task", "a"),
            "parent-task:step:a"
        );
        assert_eq!(
            composite_step_task_key("parent-task", "b"),
            "parent-task:step:b"
        );
    }
}
