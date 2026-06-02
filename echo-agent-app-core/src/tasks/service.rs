//! BackgroundTaskService — unified lifecycle manager for all background work.
//!
//! Wraps the framework's `TaskManager` + `TaskExecutor` and provides a
//! high-level submit/cancel/list/resume API. The `TaskExecuteFn` closure
//! dispatches by `BackgroundTaskKind` tag to the appropriate handler.
//!
//! **No default timeout**: tasks run until they complete, are cancelled,
//! or the process exits (in which case they are resumed on next start).

use super::background::*;
use super::long_running::{HumanCheckpointGate, HumanCheckpointResponse};
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
    agent: AgentHandle,
    event_bus: Arc<TaskEventBus>,
    cancel: echo_agent::agent::CancellationToken,
    human_gate: Arc<HumanCheckpointGate>,
}

impl BackgroundTaskService {
    /// Create a new BackgroundTaskService.
    ///
    /// `store_backend` is the SQLite (or in-memory) store used for task
    /// persistence. Metadata is stored directly on the framework `Task`
    /// via `metadata_json`.
    pub async fn new(
        agent: AgentHandle,
        store_backend: Arc<dyn Store>,
        cancel: echo_agent::agent::CancellationToken,
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

        // Build the execute_fn that dispatches by kind — reads metadata_json
        // from the Task in the manager instead of a separate meta_store
        let agent_clone = agent.clone();
        let manager_clone = manager.clone();
        let execute_fn: TaskExecuteFn = Arc::new(move |ctx: TaskContext| {
            let agent = agent_clone.clone();
            let manager = manager_clone.clone();
            Box::pin(async move { dispatch_task(ctx, agent, manager).await })
        });

        // Create executor — no default timeout (tasks run until done)
        let config = TaskExecutorConfig {
            max_concurrent: 5,
            default_timeout_secs: 0, // 0 = no timeout
            enable_hooks: true,
            ..Default::default()
        };

        let executor =
            Arc::new(TaskExecutor::new(manager.clone(), config).with_execute_fn(execute_fn));

        Ok(Self {
            manager,
            executor,
            store,
            agent,
            event_bus,
            cancel,
            human_gate: Arc::new(HumanCheckpointGate::new()),
        })
    }

    /// Submit a new background task.
    ///
    /// Creates a framework `Task` with `BackgroundTaskMeta` stored as
    /// `metadata_json`, persists it to SQLite, and schedules it for execution.
    /// Returns the task ID.
    pub async fn submit(
        &self,
        kind: BackgroundTaskKind,
        description: &str,
        submitted_via: Option<String>,
    ) -> anyhow::Result<String> {
        let task_id = uuid::Uuid::new_v4().to_string();
        let meta = BackgroundTaskMeta::new(kind.clone(), submitted_via);

        // Create framework Task with metadata embedded directly
        let task = Task::new(task_id.clone(), description.to_string())
            .with_tags(vec![kind.tag()])
            .with_metadata(meta);

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
                let _ = self.manager.add_task(task);
                continue;
            }
            // Re-add non-terminal tasks — they'll be picked up by execute_all
            let _ = self.manager.add_task(task);
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

    /// Get the human checkpoint gate (for responding to checkpoint requests).
    pub fn human_gate(&self) -> &Arc<HumanCheckpointGate> {
        &self.human_gate
    }

    /// Respond to a pending human checkpoint request.
    pub async fn respond_to_checkpoint(
        &self,
        task_id: &str,
        selection: &str,
        instructions: Option<String>,
    ) -> bool {
        self.human_gate
            .respond(
                task_id,
                HumanCheckpointResponse {
                    selection: selection.to_string(),
                    instructions,
                },
            )
            .await
    }

    /// List pending human checkpoint requests.
    pub async fn pending_checkpoints(
        &self,
    ) -> Vec<(String, super::long_running::HumanCheckpointRequest)> {
        self.human_gate.pending().await
    }

    // ── Internal helpers ──

    async fn persist_all(&self) -> anyhow::Result<()> {
        let tasks = self.manager.get_all_tasks();
        self.store
            .save_all(&tasks)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to persist tasks: {e}"))
    }
}

// ── Task dispatch ──

/// Dispatch a task to the appropriate handler based on its kind (from Task.metadata_json).
async fn dispatch_task(
    ctx: TaskContext,
    agent: AgentHandle,
    manager: Arc<TaskManager>,
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

    match meta.kind {
        BackgroundTaskKind::AgentChat { .. } => execute_agent_chat(ctx, agent).await,
        BackgroundTaskKind::Cron { .. } => execute_cron(ctx, agent).await,
        BackgroundTaskKind::Workflow { .. } => execute_workflow(ctx, agent).await,
        BackgroundTaskKind::Research { .. } => execute_research(ctx, agent).await,
        BackgroundTaskKind::ResearchToWriting { .. } => {
            execute_research_to_writing(ctx, agent).await
        }
        BackgroundTaskKind::DataPipeline { .. } => execute_data_pipeline(ctx, agent).await,
        BackgroundTaskKind::WritingPipeline { .. } => execute_writing_pipeline(ctx, agent).await,
        BackgroundTaskKind::Composite { steps, strategy } => {
            execute_composite(ctx, agent, steps, strategy).await
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
        .read_async(|guard| Box::pin(async move { guard.chat(&prompt).await }))
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
        .read_async(|guard| Box::pin(async move { guard.execute(&prompt).await }))
        .await;

    result.map_err(|e| {
        echo_agent::error::ReactError::Other(format!("Workflow execution failed: {e}"))
    })
}

/// Execute a research pipeline using the Graph workflow.
async fn execute_research(
    ctx: TaskContext,
    agent: AgentHandle,
) -> Result<String, echo_agent::error::ReactError> {
    // Description is the research topic (clean, no kind prefix)
    let topic = ctx.description.clone();

    let result = super::pipelines::run_research(agent, &topic, 20).await;

    result
        .map_err(|e| echo_agent::error::ReactError::Other(format!("Research pipeline failed: {e}")))
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

    result.map_err(|e| {
        echo_agent::error::ReactError::Other(format!("Research-to-writing pipeline failed: {e}"))
    })
}

/// Execute a data analysis pipeline using the Graph workflow.
async fn execute_data_pipeline(
    ctx: TaskContext,
    agent: AgentHandle,
) -> Result<String, echo_agent::error::ReactError> {
    // Description is the dataset path (clean, no kind prefix)
    let dataset_path = ctx.description.clone();

    let result = super::pipelines::run_data_pipeline(agent, &dataset_path, 3).await;

    result.map_err(|e| {
        echo_agent::error::ReactError::Other(format!("Data analysis pipeline failed: {e}"))
    })
}

/// Execute a writing pipeline using the Graph workflow.
async fn execute_writing_pipeline(
    ctx: TaskContext,
    agent: AgentHandle,
) -> Result<String, echo_agent::error::ReactError> {
    // Description is the writing topic (clean, no kind prefix)
    let topic = ctx.description.clone();

    let result = super::pipelines::run_writing_pipeline(agent, &topic).await;

    result
        .map_err(|e| echo_agent::error::ReactError::Other(format!("Writing pipeline failed: {e}")))
}

/// Execute a composite task by delegating to the framework's composite module.
///
/// Converts CLI-domain `CompositeStep` / `CompositeStrategy` into their
/// framework counterparts, then lets `echo_orchestration::tasks::composite`
/// handle the sequential / parallel orchestration.
async fn execute_composite(
    _ctx: TaskContext,
    agent: AgentHandle,
    steps: Vec<super::background::CompositeStep>,
    strategy: super::background::CompositeStrategy,
) -> Result<String, echo_agent::error::ReactError> {
    use echo_agent::tasks::composite::{
        CompositePlan as FrameworkCompositePlan, CompositeStep as FrameworkCompositeStep,
        CompositeStrategy as FrameworkCompositeStrategy,
        execute_composite as framework_execute_composite,
    };

    // Convert CLI CompositeStep → Framework CompositeStep
    let framework_steps: Vec<FrameworkCompositeStep> = steps
        .iter()
        .enumerate()
        .map(|(i, step)| {
            let agent = agent.clone();
            let kind = step.kind.clone();
            let step_id = step
                .description
                .clone()
                .unwrap_or_else(|| format!("step_{}", i + 1));
            FrameworkCompositeStep {
                id: step_id.clone(),
                name: step_id,
                execute_fn: Arc::new(move |ctx| {
                    let agent = agent.clone();
                    let kind = kind.clone();
                    Box::pin(async move { dispatch_sub_task(ctx, agent, &kind).await })
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

    // Join results into a single string (backward compatible with the previous
    // manual implementation).
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

/// Dispatch a sub-task within a composite task.
async fn dispatch_sub_task(
    ctx: TaskContext,
    agent: AgentHandle,
    kind: &super::background::BackgroundTaskKind,
) -> Result<String, echo_agent::error::ReactError> {
    use super::background::BackgroundTaskKind;

    match kind {
        BackgroundTaskKind::AgentChat { .. } => execute_agent_chat(ctx, agent).await,
        BackgroundTaskKind::Cron { .. } => execute_cron(ctx, agent).await,
        BackgroundTaskKind::Workflow { .. } => execute_workflow(ctx, agent).await,
        BackgroundTaskKind::Research { .. } => execute_research(ctx, agent).await,
        BackgroundTaskKind::ResearchToWriting { .. } => {
            execute_research_to_writing(ctx, agent).await
        }
        BackgroundTaskKind::DataPipeline { .. } => execute_data_pipeline(ctx, agent).await,
        BackgroundTaskKind::WritingPipeline { .. } => execute_writing_pipeline(ctx, agent).await,
        BackgroundTaskKind::Composite { .. } => Err(echo_agent::error::ReactError::Other(
            "Nested composite tasks are not supported".to_string(),
        )),
    }
}
