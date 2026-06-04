//! BackgroundTaskService — pure task lifecycle manager.
//!
//! Manages task submission, scheduling, persistence, progress tracking, and
//! cancellation. Does NOT hold any Agent reference — agent execution is
//! delegated to a `TaskExecuteFn` closure provided at construction time.
//!
//! ## Architecture
//!
//! The `TaskExecuteFn` closure is constructed in `AppState::start_task_service()`
//! and captures: agent, subagent_executor. This keeps all
//! Agent-related concerns outside the service itself.
//!
//! ## Concurrency
//!
//! All agent execution (foreground chat + background tasks) is serialized
//! internally by `ReactAgent`'s `execution_mutex`. Only one caller can use
//! the agent at a time.

use super::background::*;
use super::*;
use crate::agent_handle::AgentHandle;
use dashmap::DashMap;
use echo_agent::agent::Agent;
use echo_agent::memory::Store;
use echo_agent::tasks::progress::TaskProgress;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing;

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
    /// Latest progress for each task, updated by ProgressBridge via TaskEventBus.
    /// Frontends (Tauri, CLI) can query this to get real-time progress.
    latest_progress: Arc<DashMap<String, TaskProgress>>,
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
        let store = Arc::new(SqliteTaskStore::new(store_backend));

        // Create event bus with logging listener
        let manager = Arc::new(TaskManager::with_logging_and_events());

        // Get event bus reference from manager for external subscribers
        let event_bus = manager
            .event_bus()
            .cloned()
            .expect("with_logging_and_events always creates an event bus");
        let event_bus = Arc::new(event_bus);

        // Build the TaskExecuteFn closure — captures agent + manager + event_bus
        let execute_fn: TaskExecuteFn = {
            let agent = agent.clone();
            let manager = manager.clone();
            let event_bus = event_bus.clone();
            Arc::new(move |ctx: TaskContext| {
                let agent = agent.clone();
                let manager = manager.clone();
                let event_bus = event_bus.clone();
                Box::pin(async move { dispatch_task(ctx, agent, manager, event_bus).await })
            })
        };

        // Create executor — max_concurrent=1 since all tasks share one agent
        let executor_config = TaskExecutorConfig {
            max_concurrent: 1,
            default_timeout_secs: 0, // 0 = no timeout
            enable_hooks: true,
            ..Default::default()
        };

        let executor = Arc::new(
            TaskExecutor::new(manager.clone(), executor_config).with_execute_fn(execute_fn),
        );

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
            latest_progress,
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

    /// Get the underlying TaskManager (for advanced use).
    pub fn manager(&self) -> &Arc<TaskManager> {
        &self.manager
    }

    // ── HITL checkpoint stubs ──
    //
    // These are stubs for the legacy HumanCheckpointGate interface from
    // LongRunningTaskRunner. The HITL system is being redesigned to use
    // the main Agent's HumanLoopProvider directly.

    /// List pending human checkpoint requests (currently always empty).
    pub async fn pending_checkpoints(
        &self,
    ) -> Vec<(String, super::long_running::HumanCheckpointRequest)> {
        Vec::new()
    }

    /// Respond to a pending human checkpoint request (currently always returns false).
    pub async fn respond_to_checkpoint(
        &self,
        _task_id: &str,
        _selection: &str,
        _instructions: Option<String>,
    ) -> bool {
        false
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

/// Dispatch a task using the **shared main Agent**.
///
/// Execution serialization is handled internally by ReactAgent's
/// `execution_mutex` — no external mutex needed.
///
/// This function is captured in the TaskExecuteFn closure constructed in
/// `AppState::start_task_service()`.
async fn dispatch_task(
    ctx: TaskContext,
    agent: crate::agent_handle::AgentHandle,
    manager: Arc<TaskManager>,
    event_bus: Arc<TaskEventBus>,
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
        return execute_composite(ctx, agent, steps.clone(), strategy.clone()).await;
    }

    let prompt = meta.kind.to_prompt();

    tracing::info!(
        task_id = %ctx.task_id,
        mode = %meta.kind.mode_name(),
        prompt_len = prompt.len(),
        "Executing task on shared Agent"
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

    // Execute on the shared main agent — ReactAgent serializes internally
    let result = agent
        .read_async(|a| {
            let prompt = prompt.clone();
            Box::pin(async move { a.execute(&prompt).await })
        })
        .await;

    // Clean up callback
    bridge.disable();
    agent
        .write(|a| {
            a.remove_callbacks_by_type_name("ProgressBridge");
        })
        .await;

    result.map_err(|e| echo_agent::error::ReactError::Other(format!("Agent execution failed: {e}")))
}

/// Execute a composite task by delegating to the framework's composite module.
///
/// Each sub-step runs on the shared main agent via `dispatch_sub_task`.
async fn execute_composite(
    _ctx: TaskContext,
    agent: crate::agent_handle::AgentHandle,
    steps: Vec<super::background::CompositeStep>,
    strategy: super::background::CompositeStrategy,
) -> Result<String, echo_agent::error::ReactError> {
    use echo_agent::tasks::composite::{
        CompositePlan as FrameworkCompositePlan, CompositeStep as FrameworkCompositeStep,
        CompositeStrategy as FrameworkCompositeStrategy,
        execute_composite as framework_execute_composite,
    };

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

/// Dispatch a sub-task within a composite task using the shared main agent.
///
/// Execution serialization is handled internally by ReactAgent.
async fn dispatch_sub_task(
    _ctx: TaskContext,
    agent: crate::agent_handle::AgentHandle,
    kind: &super::background::BackgroundTaskKind,
) -> Result<String, echo_agent::error::ReactError> {
    use super::background::BackgroundTaskKind;

    if let BackgroundTaskKind::Composite { .. } = kind {
        return Err(echo_agent::error::ReactError::Other(
            "Nested composite tasks are not supported".to_string(),
        ));
    }

    let prompt = kind.to_prompt();

    // Execute on the shared main agent — ReactAgent serializes internally
    let result = agent
        .read_async(|a| {
            let prompt = prompt.clone();
            Box::pin(async move { a.execute(&prompt).await })
        })
        .await;

    result.map_err(|e| {
        echo_agent::error::ReactError::Other(format!("Sub-task execution failed: {e}"))
    })
}
