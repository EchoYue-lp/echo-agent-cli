//! Background task trigger adapter backed exclusively by EKO TaskRuntime.
//!
//! CLI, Tauri, cron-style background work and structured pipelines all create
//! an EKO TaskRun and use the same product persistence, recovery, and terminal
//! contracts. DAG execution itself is delegated to the framework's generic
//! runtime executor.

use std::sync::Arc;

use async_trait::async_trait;
use echo_agent::agent::CancellationToken;
use echo_agent::tasks::progress::TaskProgress;

use super::background::BackgroundTaskKind;
use super::task_runtime::{
    AttendedMode, DomainProfile, ExecutePlanTool, ExecutionMode, MemoryPolicy, PlanTask,
    RecoveryBlocker, RecoveryDecision, TaskPlan, TaskRun, TaskRunStatus, TaskRuntimeStore,
    TodoStatus, UnattendedWriteMode,
};
use crate::agent_handle::AgentHandle;

#[derive(Debug, Clone)]
pub struct BackgroundTaskServiceConfig {
    pub max_concurrent: usize,
    pub reserve_foreground_agents: usize,
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

struct PromptRunRequest<'a> {
    prompt: &'a str,
    description: &'a str,
    source: &'a str,
    task_kind: &'a str,
    priority: u8,
    dependencies: Vec<String>,
    domain_profile: DomainProfile,
}

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
    pub source: &'static str,
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
}

impl TaskAgentLease {
    fn agent(&self) -> AgentHandle {
        self.agent.clone()
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
            match tokio::runtime::Handle::try_current() {
                Ok(handle) => {
                    handle.spawn(async move {
                        release.release().await;
                    });
                }
                Err(error) => {
                    tracing::warn!(%error, "task agent lease dropped outside Tokio runtime");
                }
            }
        }
    }
}

enum TaskAgentRelease {
    Pool {
        pool: Arc<crate::agent_pool::AgentPool>,
        key: String,
    },
}

impl TaskAgentRelease {
    async fn release(self) {
        match self {
            Self::Pool { pool, key } => pool.release(&key).await,
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
        let agent = self.pool.acquire(&key).await.map_err(|error| {
            echo_agent::error::ReactError::Other(format!("Failed to acquire task agent: {error}"))
        })?;
        Ok(TaskAgentLease {
            agent,
            release: Some(TaskAgentRelease::Pool {
                pool: self.pool.clone(),
                key,
            }),
        })
    }
}

pub struct BackgroundTaskService {
    cancel: CancellationToken,
    config: BackgroundTaskServiceConfig,
    task_runtime_store: Arc<TaskRuntimeStore>,
    agent_provider: Arc<dyn TaskAgentProvider>,
    run_semaphore: Arc<tokio::sync::Semaphore>,
}

impl BackgroundTaskService {
    pub async fn new(
        agent: AgentHandle,
        cancel: CancellationToken,
        task_runtime_store: Option<Arc<TaskRuntimeStore>>,
    ) -> anyhow::Result<Self> {
        Self::with_agent_provider(
            Arc::new(SingleAgentTaskProvider { agent }),
            BackgroundTaskServiceConfig::default(),
            cancel,
            task_runtime_store,
        )
    }

    pub async fn with_pool(
        pool: Arc<crate::agent_pool::AgentPool>,
        cancel: CancellationToken,
        task_runtime_store: Option<Arc<TaskRuntimeStore>>,
    ) -> anyhow::Result<Self> {
        let config = BackgroundTaskServiceConfig {
            max_concurrent: pool.background_task_concurrency(),
            reserve_foreground_agents: pool.foreground_agent_reserve(),
            composite_parallelism: pool.composite_parallelism(),
        };
        Self::with_agent_provider(
            Arc::new(PoolTaskAgentProvider { pool }),
            config,
            cancel,
            task_runtime_store,
        )
    }

    fn with_agent_provider(
        agent_provider: Arc<dyn TaskAgentProvider>,
        config: BackgroundTaskServiceConfig,
        cancel: CancellationToken,
        task_runtime_store: Option<Arc<TaskRuntimeStore>>,
    ) -> anyhow::Result<Self> {
        let task_runtime_store = task_runtime_store
            .ok_or_else(|| anyhow::anyhow!("TaskRuntimeStore is required for background tasks"))?;
        Ok(Self {
            cancel,
            run_semaphore: Arc::new(tokio::sync::Semaphore::new(config.max_concurrent.max(1))),
            config,
            task_runtime_store,
            agent_provider,
        })
    }

    pub async fn submit(
        &self,
        kind: BackgroundTaskKind,
        description: &str,
        submitted_via: Option<String>,
    ) -> anyhow::Result<String> {
        self.submit_with_options(kind, description, submitted_via, None, Vec::new())
            .await
    }

    pub async fn submit_with_options(
        &self,
        kind: BackgroundTaskKind,
        description: &str,
        submitted_via: Option<String>,
        priority: Option<u8>,
        depends_on: Vec<String>,
    ) -> anyhow::Result<String> {
        let domain_profile = kind.domain_profile();
        let prompt = kind.to_prompt();
        let task_kind = kind.tag();
        let source = submitted_via.unwrap_or_else(|| "background".to_string());
        self.submit_prompt_run(PromptRunRequest {
            prompt: &prompt,
            description,
            source: &source,
            task_kind: &task_kind,
            priority: priority.unwrap_or(5),
            dependencies: depends_on,
            domain_profile,
        })
        .await
    }

    pub async fn submit_run(
        &self,
        prompt: &str,
        description: &str,
        source_kind: &str,
        source_id: &str,
    ) -> anyhow::Result<String> {
        let task_kind = format!("bg:kind:{source_kind}_agent_chat");
        self.submit_prompt_run(PromptRunRequest {
            prompt,
            description,
            source: source_id,
            task_kind: &task_kind,
            priority: 5,
            dependencies: Vec::new(),
            domain_profile: DomainProfile::General,
        })
        .await
    }

    async fn submit_prompt_run(&self, request: PromptRunRequest<'_>) -> anyhow::Result<String> {
        let run_id = uuid::Uuid::new_v4().to_string();
        let conversation_id = format!("background:{}:{}", request.source, uuid::Uuid::new_v4());
        let goal = if request.description.trim().is_empty() {
            request.prompt
        } else {
            request.description
        };
        self.task_runtime_store.create_run(
            &run_id,
            "default",
            &conversation_id,
            "",
            request.domain_profile,
            goal,
            request.task_kind,
            AttendedMode::Unattended,
        )?;
        self.task_runtime_store.record_trigger_metadata(
            &run_id,
            request.source,
            request.task_kind,
            request.prompt,
            request.priority,
            &request.dependencies,
        )?;
        self.start_run_driver(
            run_id.clone(),
            request.prompt.to_string(),
            request.dependencies,
        )?;
        Ok(run_id)
    }

    pub async fn submit_dag(
        &self,
        plan_tasks: Vec<PlanTask>,
        description: &str,
        source_kind: &str,
        source_id: &str,
    ) -> anyhow::Result<String> {
        let run_id = uuid::Uuid::new_v4().to_string();
        let conversation_id = format!("background:{source_id}:{}", uuid::Uuid::new_v4());
        let goal = if description.trim().is_empty() {
            "composite"
        } else {
            description
        };
        let task_kind = format!("bg:kind:{source_kind}_composite");
        self.task_runtime_store.create_run(
            &run_id,
            "default",
            &conversation_id,
            "",
            DomainProfile::General,
            goal,
            &task_kind,
            AttendedMode::Unattended,
        )?;
        self.task_runtime_store.attach_plan(&TaskPlan {
            plan_id: uuid::Uuid::new_v4().to_string(),
            run_id: run_id.clone(),
            revision: 1,
            domain_profile: DomainProfile::General,
            goal: goal.to_string(),
            assumptions: Vec::new(),
            risks: Vec::new(),
            execution_mode: ExecutionMode::default(),
            tasks: plan_tasks,
        })?;
        self.task_runtime_store.record_trigger_metadata(
            &run_id,
            source_id,
            &task_kind,
            goal,
            5,
            &[],
        )?;
        self.start_run_driver(run_id.clone(), goal.to_string(), Vec::new())?;
        Ok(run_id)
    }

    fn start_run_driver(
        &self,
        run_id: String,
        prompt: String,
        dependencies: Vec<String>,
    ) -> anyhow::Result<()> {
        let store = self.task_runtime_store.clone();
        let cancel = self.cancel.child_token();
        let cancel_registration = store
            .register_run_cancellation(&run_id, cancel.clone())
            .map_err(|error| anyhow::anyhow!("register run cancellation: {error}"))?;
        let agent_provider = self.agent_provider.clone();
        let run_semaphore = self.run_semaphore.clone();
        tokio::spawn(async move {
            let _cancel_registration = cancel_registration;
            if let Err(error) = wait_for_dependencies(&store, &dependencies, &cancel).await {
                finish_pre_execution_failure(&store, &run_id, &error, cancel.is_cancelled());
                return;
            }
            if cancel.is_cancelled() {
                finish_pre_execution_failure(&store, &run_id, "run cancelled", true);
                return;
            }
            let _run_permit = tokio::select! {
                _ = cancel.cancelled() => {
                    finish_pre_execution_failure(&store, &run_id, "run cancelled", true);
                    return;
                }
                permit = run_semaphore.acquire_owned() => match permit {
                    Ok(permit) => permit,
                    Err(error) => {
                        finish_pre_execution_failure(
                            &store,
                            &run_id,
                            &format!("background concurrency closed: {error}"),
                            false,
                        );
                        return;
                    }
                }
            };
            if let Err(error) = transition_to_running(&store, &run_id) {
                tracing::warn!(run_id = %run_id, %error, "background run could not start");
                return;
            }
            let lease = match agent_provider.acquire_for_task(&run_id).await {
                Ok(lease) => lease,
                Err(error) => {
                    finish_running_failure(&store, &run_id, &format!("acquire agent: {error}"));
                    return;
                }
            };
            let agent = lease.agent();
            // Wire reviewer LLM from the background agent. Implementation/
            // Debugging tasks (and any task with acceptance_criteria) now
            // require a review pass; without a reviewer the Skipped branch
            // would otherwise Paused the run forever (M7 forbids auto-pass).
            let reviewer_llm = agent.read(|a| a.llm_client().cloned()).await;
            let result = match store.get_plan(&run_id) {
                Ok(Some(_)) => super::task_runtime::execute_run(
                    store.clone(),
                    Some(agent),
                    reviewer_llm,
                    None,
                    None,
                    None,
                    &run_id,
                    cancel,
                    MemoryPolicy::None,
                )
                .await
                .map(|_| run_id.clone()),
                Ok(None) => {
                    register_plan_execute(&agent, store.clone()).await;
                    super::task_runtime::drive_unattended_run(
                        store.clone(),
                        agent,
                        &run_id,
                        "background",
                        &run_id,
                        &prompt,
                        cancel,
                        UnattendedWriteMode::default(),
                    )
                    .await
                }
                Err(error) => Err(super::task_runtime::ExecError::Other(format!(
                    "read plan before background execution: {error}"
                ))),
            };
            lease.release().await;
            if let Err(error) = result {
                finish_running_failure(&store, &run_id, &error.to_string());
            }
        });
        Ok(())
    }

    pub async fn cancel(&self, id: &str) -> bool {
        self.task_runtime_store
            .request_cancel(id)
            .is_ok_and(|cancelled| cancelled)
    }

    pub fn pause(&self, id: &str) -> anyhow::Result<bool> {
        self.task_runtime_store
            .request_pause(id)
            .map_err(Into::into)
    }

    pub fn resume(&self, id: &str) -> anyhow::Result<()> {
        let run = self
            .task_runtime_store
            .get_run(id)?
            .ok_or_else(|| anyhow::anyhow!("task run not found: {id}"))?;
        if !run.conversation_id.starts_with("background:") {
            return Err(anyhow::anyhow!(
                "task run is not owned by the background service: {id}"
            ));
        }
        let metadata = trigger_metadata(&self.task_runtime_store, id);
        let prompt = metadata.prompt.unwrap_or(run.goal);
        self.task_runtime_store.resume_task_run(id)?;
        if let Err(error) = self.start_run_driver(id.to_string(), prompt, metadata.dependencies) {
            let _ = self
                .task_runtime_store
                .transition_run(id, TaskRunStatus::Paused);
            return Err(error);
        }
        Ok(())
    }

    pub fn recovery_blockers(&self, id: &str) -> anyhow::Result<Vec<RecoveryBlocker>> {
        self.task_runtime_store
            .list_recovery_blockers(id)
            .map_err(Into::into)
    }

    pub fn resolve_recovery_task(
        &self,
        id: &str,
        task_id: &str,
        decision: RecoveryDecision,
    ) -> anyhow::Result<()> {
        self.task_runtime_store
            .resolve_recovery_task(id, task_id, decision)
            .map_err(Into::into)
    }

    /// Atomically retry a Blocked/Failed task on a Paused/Failed run. Mirrors
    /// the Tauri `retry_blocked_task` command and the GUI retry button so
    /// CLI/TUI users get the same acceptance-retry semantics.
    pub fn retry_blocked_task(&self, run_id: &str, task_id: &str) -> anyhow::Result<u32> {
        let run = self
            .task_runtime_store
            .get_run(run_id)?
            .ok_or_else(|| anyhow::anyhow!("task run not found: {run_id}"))?;
        if !run.conversation_id.starts_with("background:") {
            return Err(anyhow::anyhow!(
                "task run is not owned by the background service: {run_id}"
            ));
        }
        let metadata = trigger_metadata(&self.task_runtime_store, run_id);
        let prompt = metadata.prompt.unwrap_or(run.goal);
        let next = self
            .task_runtime_store
            .retry_blocked_task(run_id, task_id)?;
        if let Err(error) = self.start_run_driver(run_id.to_string(), prompt, metadata.dependencies)
        {
            let _ = self
                .task_runtime_store
                .transition_run(run_id, TaskRunStatus::Paused);
            return Err(error);
        }
        Ok(next)
    }

    pub fn list_unified(&self, status_filter: Option<&str>) -> Vec<UnifiedTaskInfo> {
        let Ok(runs) = self.task_runtime_store.list_runs_in(&all_run_statuses()) else {
            return Vec::new();
        };
        runs.into_iter()
            .filter(|run| run.conversation_id.starts_with("background:"))
            .map(|run| run_to_unified(&self.task_runtime_store, &run))
            .filter(|task| status_filter.is_none_or(|status| task.status == status))
            .collect()
    }

    pub fn get_unified(&self, id: &str) -> Option<UnifiedTaskInfo> {
        self.task_runtime_store
            .get_run(id)
            .ok()
            .flatten()
            .filter(|run| run.conversation_id.starts_with("background:"))
            .map(|run| run_to_unified(&self.task_runtime_store, &run))
    }

    pub async fn resume_pending(&self) -> anyhow::Result<usize> {
        let runs = self
            .task_runtime_store
            .list_runs_in(&[TaskRunStatus::Pending, TaskRunStatus::Paused])?;
        let mut resumed = 0usize;
        for run in runs
            .into_iter()
            .filter(|run| run.conversation_id.starts_with("background:"))
            .filter(|run| {
                run.status == TaskRunStatus::Pending
                    || was_recovered_at_boot(&self.task_runtime_store, &run.run_id)
            })
        {
            let blockers = self
                .task_runtime_store
                .list_recovery_blockers(&run.run_id)?;
            if !blockers.is_empty() {
                tracing::warn!(
                    run_id = %run.run_id,
                    blocker_count = blockers.len(),
                    "background run requires recovery decision before resume"
                );
                continue;
            }
            let metadata = trigger_metadata(&self.task_runtime_store, &run.run_id);
            let prompt = metadata.prompt.unwrap_or(run.goal);
            if run.status == TaskRunStatus::Paused {
                self.task_runtime_store.resume_task_run(&run.run_id)?;
            }
            self.start_run_driver(run.run_id, prompt, metadata.dependencies)?;
            resumed = resumed.saturating_add(1);
        }
        Ok(resumed)
    }

    pub fn spawn(self: Arc<Self>) {
        tokio::spawn(async move {
            match self.resume_pending().await {
                Ok(count) if count > 0 => {
                    tracing::info!(count, "resumed background TaskRuntime runs")
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(%error, "background run recovery failed"),
            }
        });
    }

    pub fn get_progress(&self, run_id: &str) -> Option<TaskProgress> {
        let run = self.task_runtime_store.get_run(run_id).ok().flatten()?;
        let todos = self.task_runtime_store.list_todos(run_id).ok()?;
        let total = todos.len();
        let completed = todos
            .iter()
            .filter(|todo| {
                matches!(
                    todo.status,
                    TodoStatus::Completed | TodoStatus::Failed | TodoStatus::Skipped
                )
            })
            .count();
        let percentage = if total == 0 {
            if run.status == TaskRunStatus::Completed {
                100.0
            } else {
                0.0
            }
        } else {
            (completed as f64 / total as f64) * 100.0
        };
        Some(TaskProgress {
            task_id: run_id.to_string(),
            percentage,
            current_phase: run_status_string(run.status).to_string(),
            phase_index: completed.min(total),
            total_phases: total,
            message: todos
                .iter()
                .find(|todo| todo.status == TodoStatus::Running)
                .map(|todo| todo.title.clone()),
            eta_secs: None,
            updated_at: run.updated_at,
        })
    }

    pub fn config(&self) -> &BackgroundTaskServiceConfig {
        &self.config
    }
}

fn was_recovered_at_boot(store: &TaskRuntimeStore, run_id: &str) -> bool {
    let Ok(events) = store.list_events(run_id, 0) else {
        return false;
    };
    events.iter().rev().any(|event| {
        event
            .payload
            .get("message")
            .and_then(|value| value.as_str())
            .is_some_and(|message| message.starts_with("recovered from running"))
    })
}

#[derive(Default)]
struct TriggerMetadata {
    kind: Option<String>,
    prompt: Option<String>,
    priority: u8,
    dependencies: Vec<String>,
}

fn trigger_metadata(store: &TaskRuntimeStore, run_id: &str) -> TriggerMetadata {
    let Ok(events) = store.list_events(run_id, 0) else {
        return TriggerMetadata::default();
    };
    events
        .iter()
        .rev()
        .find(|event| {
            event.payload.get("kind").and_then(|value| value.as_str()) == Some("trigger_metadata")
        })
        .map(|event| TriggerMetadata {
            kind: event
                .payload
                .get("task_kind")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            prompt: event
                .payload
                .get("prompt")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            priority: event
                .payload
                .get("priority")
                .and_then(|value| value.as_u64())
                .and_then(|value| u8::try_from(value).ok())
                .unwrap_or(5)
                .min(10),
            dependencies: event
                .payload
                .get("dependencies")
                .and_then(|value| value.as_array())
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|value| value.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
        })
        .unwrap_or_default()
}

fn run_to_unified(store: &TaskRuntimeStore, run: &TaskRun) -> UnifiedTaskInfo {
    let metadata = trigger_metadata(store, &run.run_id);
    let todos = store.list_todos(&run.run_id).unwrap_or_default();
    let summaries: Vec<String> = todos
        .iter()
        .filter_map(|todo| todo.summary.clone())
        .filter(|summary| !summary.trim().is_empty())
        .collect();
    let result = (!summaries.is_empty()).then(|| summaries.join("\n"));
    let error = todos
        .iter()
        .find(|todo| todo.status == TodoStatus::Failed)
        .and_then(|todo| todo.summary.clone());
    UnifiedTaskInfo {
        id: run.run_id.clone(),
        description: run.goal.clone(),
        status: run_status_string(run.status).to_string(),
        created_at: echo_agent::utils::time::to_local(run.created_at).to_rfc3339(),
        updated_at: echo_agent::utils::time::to_local(run.updated_at).to_rfc3339(),
        result,
        error,
        kind: metadata.kind.or_else(|| Some(run.route.clone())),
        source: "run",
        dependencies: metadata.dependencies,
        priority: if metadata.priority == 0 {
            5
        } else {
            metadata.priority
        },
    }
}

fn run_status_string(status: TaskRunStatus) -> &'static str {
    match status {
        TaskRunStatus::Pending => "pending",
        TaskRunStatus::Running => "in_progress",
        TaskRunStatus::Paused => "paused",
        TaskRunStatus::Cancelled => "cancelled",
        TaskRunStatus::Failed => "failed",
        TaskRunStatus::Completed => "completed",
    }
}

fn all_run_statuses() -> [TaskRunStatus; 6] {
    [
        TaskRunStatus::Pending,
        TaskRunStatus::Running,
        TaskRunStatus::Paused,
        TaskRunStatus::Cancelled,
        TaskRunStatus::Failed,
        TaskRunStatus::Completed,
    ]
}

async fn wait_for_dependencies(
    store: &TaskRuntimeStore,
    dependencies: &[String],
    cancel: &CancellationToken,
) -> Result<(), String> {
    loop {
        if cancel.is_cancelled() {
            return Err("run cancelled while waiting for dependencies".to_string());
        }
        let mut waiting = false;
        for dependency in dependencies {
            let run = store
                .get_run(dependency)
                .map_err(|error| format!("read dependency {dependency}: {error}"))?
                .ok_or_else(|| format!("dependency run not found: {dependency}"))?;
            match run.status {
                TaskRunStatus::Completed => {}
                TaskRunStatus::Failed | TaskRunStatus::Cancelled => {
                    return Err(format!(
                        "dependency {dependency} ended {}",
                        run.status.as_str()
                    ));
                }
                TaskRunStatus::Pending | TaskRunStatus::Running | TaskRunStatus::Paused => {
                    waiting = true;
                }
            }
        }
        if !waiting {
            return Ok(());
        }
        tokio::select! {
            _ = cancel.cancelled() => return Err("run cancelled while waiting for dependencies".to_string()),
            _ = tokio::time::sleep(std::time::Duration::from_millis(250)) => {}
        }
    }
}

fn transition_to_running(store: &TaskRuntimeStore, run_id: &str) -> Result<(), String> {
    let run = store
        .get_run(run_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("run not found: {run_id}"))?;
    match run.status {
        TaskRunStatus::Pending | TaskRunStatus::Paused | TaskRunStatus::Failed => store
            .transition_run(run_id, TaskRunStatus::Running)
            .map(|_| ())
            .map_err(|error| error.to_string()),
        TaskRunStatus::Running => Ok(()),
        TaskRunStatus::Cancelled | TaskRunStatus::Completed => {
            Err(format!("run {run_id} is already {}", run.status.as_str()))
        }
    }
}

fn finish_pre_execution_failure(
    store: &TaskRuntimeStore,
    run_id: &str,
    error: &str,
    cancelled: bool,
) {
    let Ok(Some(run)) = store.get_run(run_id) else {
        return;
    };
    if run.status == TaskRunStatus::Cancelled || run.status == TaskRunStatus::Completed {
        return;
    }
    if cancelled {
        let _ = store.transition_run(run_id, TaskRunStatus::Cancelled);
        return;
    }
    let _ = store.note(run_id, None, error);
    if run.status != TaskRunStatus::Running {
        let _ = store.transition_run(run_id, TaskRunStatus::Running);
    }
    let _ = store.transition_run(run_id, TaskRunStatus::Failed);
}

fn finish_running_failure(store: &TaskRuntimeStore, run_id: &str, error: &str) {
    let _ = store.note(run_id, None, error);
    if let Ok(Some(run)) = store.get_run(run_id)
        && run.status == TaskRunStatus::Running
    {
        let _ = store.transition_run(run_id, TaskRunStatus::Failed);
    }
}

async fn register_plan_execute(agent: &AgentHandle, store: Arc<TaskRuntimeStore>) {
    let tool = ExecutePlanTool::new(store, agent.clone());
    agent
        .write(|agent| {
            agent.add_tool(Box::new(tool));
        })
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::task_runtime::PlanTaskKind;

    fn test_agent() -> Result<AgentHandle, String> {
        let llm = Arc::new(
            echo_agent::testing::MockLlmClient::new()
                .with_model_name("test-model")
                .with_response("done"),
        );
        echo_agent::agent::ReactAgentBuilder::new()
            .model("test-model")
            .llm_client(llm)
            .build()
            .map(AgentHandle::new)
            .map_err(|error| error.to_string())
    }

    #[tokio::test]
    async fn pipeline_submission_creates_only_task_runtime_run() -> Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let service = BackgroundTaskService::new(
            test_agent()?,
            CancellationToken::new(),
            Some(store.clone()),
        )
        .await
        .map_err(|error| error.to_string())?;
        let run_id = service
            .submit(
                BackgroundTaskKind::Research {
                    topic: "runtime".to_string(),
                    max_papers: 2,
                    output_format: Default::default(),
                },
                "research runtime",
                Some("test".to_string()),
            )
            .await
            .map_err(|error| error.to_string())?;
        let run = store
            .get_run(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "run missing".to_string())?;
        assert!(run.conversation_id.starts_with("background:"));
        assert_eq!(run.route, "bg:kind:research");
        Ok(())
    }

    #[tokio::test]
    async fn dependency_wait_can_be_cancelled_through_runtime_store() -> Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        store
            .create_run(
                "dependency",
                "default",
                "background:test:dependency",
                "",
                DomainProfile::General,
                "dependency",
                "bg:kind:test",
                AttendedMode::Unattended,
            )
            .map_err(|error| error.to_string())?;
        let service = BackgroundTaskService::new(
            test_agent()?,
            CancellationToken::new(),
            Some(store.clone()),
        )
        .await
        .map_err(|error| error.to_string())?;
        let run_id = service
            .submit_with_options(
                BackgroundTaskKind::WritingPipeline {
                    topic: "runtime".to_string(),
                    audience: "engineers".to_string(),
                    format: "report".to_string(),
                    max_revisions: 1,
                    quality_threshold: 70,
                },
                "write runtime report",
                Some("test".to_string()),
                Some(7),
                vec!["dependency".to_string()],
            )
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            store
                .request_cancel(&run_id)
                .map_err(|error| error.to_string())?
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let run = store
            .get_run(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "run missing".to_string())?;
        assert_eq!(run.status, TaskRunStatus::Cancelled);
        Ok(())
    }

    #[tokio::test]
    async fn retry_registers_driver_immediately_instead_of_leaving_fake_running()
    -> Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        store
            .create_run(
                "dependency",
                "default",
                "background:test:dependency",
                "",
                DomainProfile::General,
                "dependency",
                "bg:kind:test",
                AttendedMode::Unattended,
            )
            .map_err(|error| error.to_string())?;
        store
            .create_run(
                "retry-run",
                "default",
                "background:test:retry-run",
                "",
                DomainProfile::General,
                "retry run",
                "bg:kind:test",
                AttendedMode::Unattended,
            )
            .map_err(|error| error.to_string())?;
        let task = PlanTask {
            id: "retry-task".to_string(),
            title: "Retry task".to_string(),
            kind: PlanTaskKind::Investigation,
            agent_role: "researcher".to_string(),
            max_retries: 2,
            ..PlanTask::default()
        };
        store
            .attach_plan(&TaskPlan {
                plan_id: "retry-plan".to_string(),
                run_id: "retry-run".to_string(),
                revision: 1,
                domain_profile: DomainProfile::General,
                goal: "retry run".to_string(),
                assumptions: Vec::new(),
                risks: Vec::new(),
                execution_mode: ExecutionMode::Sequential,
                tasks: vec![task],
            })
            .map_err(|error| error.to_string())?;
        store
            .transition_run("retry-run", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store
            .set_task_status(
                "retry-run",
                "retry-task",
                TodoStatus::Failed,
                Some("researcher"),
                Some("execution failed"),
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run("retry-run", TaskRunStatus::Failed)
            .map_err(|error| error.to_string())?;
        store
            .record_trigger_metadata(
                "retry-run",
                "test",
                "research",
                "retry run",
                5,
                &["dependency".to_string()],
            )
            .map_err(|error| error.to_string())?;

        let service = BackgroundTaskService::new(
            test_agent()?,
            CancellationToken::new(),
            Some(store.clone()),
        )
        .await
        .map_err(|error| error.to_string())?;
        let attempt = service
            .retry_blocked_task("retry-run", "retry-task")
            .map_err(|error| error.to_string())?;
        assert_eq!(attempt, 1);
        if !store
            .request_pause("retry-run")
            .map_err(|error| error.to_string())?
        {
            return Err("retry did not register an active run driver".to_string());
        }
        Ok(())
    }

    #[test]
    fn auto_resume_only_accepts_boot_recovery_pause() -> Result<(), String> {
        let store = TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?;
        store
            .create_run(
                "run",
                "default",
                "background:test:run",
                "",
                DomainProfile::General,
                "goal",
                "bg:kind:test",
                AttendedMode::Unattended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run("run", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store
            .transition_run("run", TaskRunStatus::Paused)
            .map_err(|error| error.to_string())?;
        assert!(!was_recovered_at_boot(&store, "run"));

        store
            .resume_task_run("run")
            .map_err(|error| error.to_string())?;
        assert_eq!(store.recover_incomplete(), 1);
        assert!(was_recovered_at_boot(&store, "run"));
        Ok(())
    }
}
