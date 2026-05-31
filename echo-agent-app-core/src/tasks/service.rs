//! BackgroundTaskService — unified lifecycle manager for all background work.
//!
//! Wraps the framework's `TaskManager` + `TaskExecutor` and provides a
//! high-level submit/cancel/list/resume API. The `TaskExecuteFn` closure
//! dispatches by `BackgroundTaskKind` tag to the appropriate handler.
//!
//! **No default timeout**: tasks run until they complete, are cancelled,
//! or the process exits (in which case they are resumed on next start).

use super::background::*;
use super::*;
use crate::agent_handle::AgentHandle;
use echo_agent::agent::Agent; // Import Agent trait to call chat()
use echo_agent::memory::Store;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing;

/// Central service for managing background tasks.
///
/// Created once per process and shared across all modes (web, cli, tui, tauri).
pub struct BackgroundTaskService {
    manager: Arc<TaskManager>,
    executor: Arc<TaskExecutor>,
    store: Arc<SqliteTaskStore>,
    meta_store: Arc<dyn Store>,
    agent: AgentHandle,
    event_bus: Arc<TaskEventBus>,
    cancel: echo_agent::agent::CancellationToken,
}

impl BackgroundTaskService {
    /// Create a new BackgroundTaskService.
    ///
    /// `store_backend` is the SQLite (or in-memory) store used for both
    /// task persistence and metadata storage.
    pub async fn new(
        agent: AgentHandle,
        store_backend: Arc<dyn Store>,
        cancel: echo_agent::agent::CancellationToken,
    ) -> anyhow::Result<Self> {
        let store = Arc::new(SqliteTaskStore::new(store_backend.clone()));
        let meta_store = store_backend.clone();

        // Create event bus with logging listener
        let manager = Arc::new(TaskManager::with_logging_and_events());

        // Get event bus reference from manager for external subscribers
        let event_bus = manager
            .event_bus()
            .cloned()
            .expect("with_logging_and_events always creates an event bus");
        let event_bus = Arc::new(event_bus);

        // Build the execute_fn that dispatches by kind — uses meta_store
        // lookup by task_id instead of fragile string parsing in description
        let agent_clone = agent.clone();
        let meta_clone: Arc<dyn Store> = store_backend.clone();
        let execute_fn: TaskExecuteFn = Arc::new(move |ctx: TaskContext| {
            let agent = agent_clone.clone();
            let meta_store: Arc<dyn Store> = meta_clone.clone();
            Box::pin(async move { dispatch_task(ctx, agent, meta_store).await })
        });

        // Create executor — no default timeout (tasks run until done)
        let config = TaskExecutorConfig {
            max_concurrent: 5,
            default_timeout_secs: 0, // 0 = no timeout
            enable_hooks: true,
            ..Default::default()
        };

        let executor = Arc::new(
            TaskExecutor::new(manager.clone(), config)
                .with_execute_fn(execute_fn),
        );

        Ok(Self {
            manager,
            executor,
            store,
            meta_store,
            agent,
            event_bus,
            cancel,
        })
    }

    /// Submit a new background task.
    ///
    /// Creates a framework `Task`, persists it to SQLite, stores the
    /// `BackgroundTaskMeta`, and schedules it for execution.
    /// Returns the task ID.
    pub async fn submit(
        &self,
        kind: BackgroundTaskKind,
        description: &str,
        submitted_via: Option<String>,
    ) -> anyhow::Result<String> {
        let task_id = uuid::Uuid::new_v4().to_string();
        let meta = BackgroundTaskMeta::new(kind.clone(), submitted_via);

        // Create framework Task — description is kept clean (no kind encoding)
        let task = Task::new(task_id.clone(), description.to_string())
            .with_tags(vec![kind.tag()]);
        // No timeout by default — tasks run until completion
        // No max_retries by default — let the caller decide

        // Persist meta
        self.save_meta(&task_id, &meta).await?;

        // Add to manager (also persists to store via event)
        self.manager.add_task(task);

        // Persist to SQLite
        self.persist_all().await?;

        tracing::info!(task_id = %task_id, kind = %kind.display_name(), "Background task submitted");
        Ok(task_id)
    }

    /// Cancel a running or pending task.
    pub async fn cancel(&self, task_id: &str) -> bool {
        // Cancel in executor if running (sync)
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
    pub async fn get(&self, task_id: &str) -> Option<(Task, Option<BackgroundTaskMeta>)> {
        let task = self.manager.get_task(task_id)?;
        let meta = self.load_meta(task_id).await.ok().flatten();
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
                let _ = self.manager.add_task(task);
                continue;
            }
            // Re-add non-terminal tasks — they'll be picked up by execute_all
            let _ = self.manager.add_task(task);
            resumed += 1;
        }

        if resumed > 0 {
            tracing::info!(count = resumed, "Resumed pending background tasks from store");
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
                                // All current tasks done, wait before checking for new ones
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

    /// Get the underlying TaskManager (for advanced use).
    pub fn manager(&self) -> &Arc<TaskManager> {
        &self.manager
    }

    /// Get the underlying AgentHandle.
    pub fn agent(&self) -> &AgentHandle {
        &self.agent
    }

    // ── Internal helpers ──

    fn meta_key(task_id: &str) -> String {
        format!("bg_meta:{task_id}")
    }

    async fn save_meta(&self, task_id: &str, meta: &BackgroundTaskMeta) -> anyhow::Result<()> {
        let value = serde_json::to_value(meta)?;
        self.meta_store
            .put(&["bg_meta"], &Self::meta_key(task_id), value)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to save meta: {e}"))
    }

    async fn load_meta(&self, task_id: &str) -> anyhow::Result<Option<BackgroundTaskMeta>> {
        match self
            .meta_store
            .get(&["bg_meta"], &Self::meta_key(task_id))
            .await
        {
            Ok(Some(item)) => {
                let meta: BackgroundTaskMeta = serde_json::from_value(item.value)?;
                Ok(Some(meta))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(anyhow::anyhow!("Failed to load meta: {e}")),
        }
    }

    async fn persist_all(&self) -> anyhow::Result<()> {
        let tasks = self.manager.get_all_tasks();
        self.store
            .save_all(&tasks)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to persist tasks: {e}"))
    }
}

// ── Task dispatch ──

/// Look up the BackgroundTaskKind for a given task_id from the meta_store.
async fn lookup_kind(
    meta_store: &Arc<dyn Store>,
    task_id: &str,
) -> Option<BackgroundTaskKind> {
    let key = format!("bg_meta:{task_id}");
    let item = meta_store.get(&["bg_meta"], &key).await.ok()??;
    let meta: BackgroundTaskMeta = serde_json::from_value(item.value).ok()?;
    Some(meta.kind)
}

/// Dispatch a task to the appropriate handler based on its kind (from meta_store).
async fn dispatch_task(
    ctx: TaskContext,
    agent: AgentHandle,
    meta_store: Arc<dyn Store>,
) -> Result<String, echo_agent::error::ReactError> {
    let kind = lookup_kind(&meta_store, &ctx.task_id)
        .await
        .ok_or_else(|| {
            echo_agent::error::ReactError::Other(format!(
                "No background task kind found for task_id={}",
                ctx.task_id
            ))
        })?;

    match kind {
        BackgroundTaskKind::AgentChat { .. } => execute_agent_chat(ctx, agent).await,
        BackgroundTaskKind::Cron { .. } => execute_cron(ctx, agent).await,
        BackgroundTaskKind::Workflow { .. } => execute_workflow(ctx, agent).await,
        BackgroundTaskKind::Research { .. } => execute_research(ctx, agent).await,
        BackgroundTaskKind::ResearchToWriting { .. } => execute_research_to_writing(ctx, agent).await,
        BackgroundTaskKind::DataPipeline { .. } => execute_data_pipeline(ctx, agent).await,
        BackgroundTaskKind::WritingPipeline { .. } => execute_writing_pipeline(ctx, agent).await,
        BackgroundTaskKind::Composite { steps, strategy } => {
            execute_composite(ctx, agent, meta_store, steps, strategy).await
        }
    }
}

/// Execute an AgentChat task: run the prompt through the agent.
async fn execute_agent_chat(
    ctx: TaskContext,
    agent: AgentHandle,
) -> Result<String, echo_agent::error::ReactError> {
    // Description is the clean prompt (no kind prefix)
    let prompt = ctx.description.clone();

    let result = agent
        .read_async(|guard| {
            Box::pin(async move { guard.chat(&prompt).await })
        })
        .await;

    result.map_err(|e| echo_agent::error::ReactError::Other(format!("Agent chat failed: {e}")))
}

/// Execute a Cron task: same as AgentChat for now.
async fn execute_cron(
    ctx: TaskContext,
    agent: AgentHandle,
) -> Result<String, echo_agent::error::ReactError> {
    // Cron tasks execute the prompt like AgentChat
    execute_agent_chat(ctx, agent).await
}

/// Execute a workflow task using the framework's Graph workflow engine.
async fn execute_workflow(
    ctx: TaskContext,
    agent: AgentHandle,
) -> Result<String, echo_agent::error::ReactError> {
    // The description contains the workflow definition as JSON or a prompt to execute
    let prompt = ctx.description.clone();
    let result = agent
        .read_async(|guard| {
            Box::pin(async move { guard.execute(&prompt).await })
        })
        .await;

    result.map_err(|e| echo_agent::error::ReactError::Other(format!("Workflow execution failed: {e}")))
}

/// Execute a research pipeline using the Graph workflow.
async fn execute_research(
    ctx: TaskContext,
    agent: AgentHandle,
) -> Result<String, echo_agent::error::ReactError> {
    // Description is the research topic (clean, no kind prefix)
    let topic = ctx.description.clone();

    let result = super::pipelines::run_research(agent, &topic, 20).await;

    result.map_err(|e| echo_agent::error::ReactError::Other(format!("Research pipeline failed: {e}")))
}

/// Execute a research-to-writing continuous workflow using the Graph workflow.
async fn execute_research_to_writing(
    ctx: TaskContext,
    agent: AgentHandle,
) -> Result<String, echo_agent::error::ReactError> {
    // Description is the research topic (clean, no kind prefix)
    let topic = ctx.description.clone();

    let config = super::pipelines::ResearchToWritingConfig::new(&topic);
    let result = super::pipelines::run_research_to_writing(agent, config).await;

    result.map_err(|e| echo_agent::error::ReactError::Other(format!("Research-to-writing pipeline failed: {e}")))
}

/// Execute a data analysis pipeline using the Graph workflow.
async fn execute_data_pipeline(
    ctx: TaskContext,
    agent: AgentHandle,
) -> Result<String, echo_agent::error::ReactError> {
    // Description is the dataset path (clean, no kind prefix)
    let dataset_path = ctx.description.clone();

    let result = super::pipelines::run_data_pipeline(agent, &dataset_path, 3).await;

    result.map_err(|e| echo_agent::error::ReactError::Other(format!("Data analysis pipeline failed: {e}")))
}

/// Execute a writing pipeline using the Graph workflow.
async fn execute_writing_pipeline(
    ctx: TaskContext,
    agent: AgentHandle,
) -> Result<String, echo_agent::error::ReactError> {
    // Description is the writing topic (clean, no kind prefix)
    let topic = ctx.description.clone();

    let result = super::pipelines::run_writing_pipeline(agent, &topic).await;

    result.map_err(|e| echo_agent::error::ReactError::Other(format!("Writing pipeline failed: {e}")))
}

/// Execute a composite task: chain multiple tasks together with dependencies.
async fn execute_composite(
    ctx: TaskContext,
    agent: AgentHandle,
    meta_store: Arc<dyn Store>,
    steps: Vec<super::background::CompositeStep>,
    strategy: super::background::CompositeStrategy,
) -> Result<String, echo_agent::error::ReactError> {
    use super::background::CompositeStrategy;

    match strategy {
        CompositeStrategy::Sequential => {
            let mut results: Vec<(String, String)> = Vec::new();
            for (i, step) in steps.iter().enumerate() {
                // Create a sub-context for this step
                let step_description = step.description.clone().unwrap_or_else(|| {
                    format!("Composite step {}: {:?}", i + 1, step.kind)
                });

                let step_id = format!("{}_step_{}", ctx.task_id, i);
                let step_ctx = TaskContext {
                    task_id: step_id.clone(),
                    description: step_description,
                    upstream_results: results.clone(),
                    upstream_errors: Vec::new(),
                    attempt: 1,
                };

                // Dispatch the sub-task
                let result = dispatch_sub_task(step_ctx, agent.clone(), meta_store.clone(), &step.kind).await?;
                results.push((step_id, result));
            }

            Ok(format!(
                "Composite task completed {} steps:\n{}",
                results.len(),
                results.iter().map(|(_, r)| r.as_str()).collect::<Vec<_>>().join("\n---\n")
            ))
        }
        CompositeStrategy::Parallel => {
            // For parallel execution, we'd use tokio::join! or similar
            // For now, fall back to sequential with a note
            let mut results: Vec<(String, String)> = Vec::new();
            for (i, step) in steps.iter().enumerate() {
                let step_description = step.description.clone().unwrap_or_else(|| {
                    format!("Composite step {}: {:?}", i + 1, step.kind)
                });

                let step_id = format!("{}_step_{}", ctx.task_id, i);
                let step_ctx = TaskContext {
                    task_id: step_id.clone(),
                    description: step_description,
                    upstream_results: Vec::new(),
                    upstream_errors: Vec::new(),
                    attempt: 1,
                };

                let result = dispatch_sub_task(step_ctx, agent.clone(), meta_store.clone(), &step.kind).await?;
                results.push((step_id, result));
            }

            Ok(format!(
                "Composite task completed {} parallel steps:\n{}",
                results.len(),
                results.iter().map(|(_, r)| r.as_str()).collect::<Vec<_>>().join("\n---\n")
            ))
        }
    }
}

/// Dispatch a sub-task within a composite task.
async fn dispatch_sub_task(
    ctx: TaskContext,
    agent: AgentHandle,
    meta_store: Arc<dyn Store>,
    kind: &super::background::BackgroundTaskKind,
) -> Result<String, echo_agent::error::ReactError> {
    use super::background::BackgroundTaskKind;

    match kind {
        BackgroundTaskKind::AgentChat { .. } => execute_agent_chat(ctx, agent).await,
        BackgroundTaskKind::Cron { .. } => execute_cron(ctx, agent).await,
        BackgroundTaskKind::Workflow { .. } => execute_workflow(ctx, agent).await,
        BackgroundTaskKind::Research { .. } => execute_research(ctx, agent).await,
        BackgroundTaskKind::ResearchToWriting { .. } => execute_research_to_writing(ctx, agent).await,
        BackgroundTaskKind::DataPipeline { .. } => execute_data_pipeline(ctx, agent).await,
        BackgroundTaskKind::WritingPipeline { .. } => execute_writing_pipeline(ctx, agent).await,
        BackgroundTaskKind::Composite { .. } => {
            Err(echo_agent::error::ReactError::Other(
                "Nested composite tasks are not supported".to_string(),
            ))
        }
    }
}
