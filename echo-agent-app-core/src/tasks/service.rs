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
    AttendedMode, DomainProfile, ExecuteTaskTool, ExecutionMode, MemoryPolicy, PlanTask,
    RecoveryBlocker, RecoveryDecision, TaskPlan, TaskRetryPreparation, TaskRun, TaskRunBootOutcome,
    TaskRunBootReconciler, TaskRunStatus, TaskRuntimeBlockingAdapter, TaskRuntimeStore, TodoStatus,
    UnattendedWriteMode,
};
use crate::agent_handle::AgentHandle;
#[cfg(test)]
use crate::tasks::task_runtime::BootAutoResumeDecision;

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
    pool_receipt: Option<crate::agent_pool::OwnedRunPoolReceipt>,
}

impl TaskAgentLease {
    fn agent(&self) -> AgentHandle {
        self.agent.clone()
    }
}

impl super::task_runtime::store::RunDriverExecutionReceipt for TaskAgentLease {
    fn release(mut self: Box<Self>) -> futures::future::BoxFuture<'static, ()> {
        Box::pin(async move {
            if let Some(receipt) = self.pool_receipt.take() {
                super::task_runtime::store::RunDriverExecutionReceipt::release(Box::new(receipt))
                    .await;
            }
        })
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
            pool_receipt: None,
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
        let pool_execution = self.pool.acquire(&key).await.map_err(|error| {
            echo_agent::error::ReactError::Other(format!("Failed to acquire task agent: {error}"))
        })?;
        let agent = pool_execution.agent();
        let pool_receipt = self.pool.retain_for_supervised_run(key, pool_execution);
        Ok(TaskAgentLease {
            agent,
            pool_receipt: Some(pool_receipt),
        })
    }
}

pub struct BackgroundTaskService {
    cancel: CancellationToken,
    config: BackgroundTaskServiceConfig,
    task_runtime_store: Arc<TaskRuntimeStore>,
    agent_provider: Arc<dyn TaskAgentProvider>,
    run_semaphore: Arc<tokio::sync::Semaphore>,
    review_integration: Option<Arc<crate::evolution::ReviewIntegration>>,
    boot_reconciler: Arc<TaskRunBootReconciler>,
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
        let boot_reconciler = TaskRunBootReconciler::for_store(&task_runtime_store);
        Ok(Self {
            cancel,
            run_semaphore: Arc::new(tokio::sync::Semaphore::new(config.max_concurrent.max(1))),
            config,
            task_runtime_store,
            agent_provider,
            review_integration: None,
            boot_reconciler,
        })
    }

    pub fn with_review_integration(
        mut self,
        review_integration: Option<Arc<crate::evolution::ReviewIntegration>>,
    ) -> Self {
        self.review_integration = review_integration;
        self
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
        let cancel = self.cancel.child_token();
        let admission = self
            .task_runtime_store
            .reserve_run_driver_admission(run_id.clone(), cancel.clone())?;
        let generation_lease = self
            .task_runtime_store
            .lease_active_workspace_generation()?;
        let mut registration = self
            .task_runtime_store
            .register_run_driver::<()>(admission, generation_lease)?;
        let conversation_id = format!("background:{}:{}", request.source, uuid::Uuid::new_v4());
        let goal = if request.description.trim().is_empty() {
            request.prompt
        } else {
            request.description
        };
        registration.mark_preparation_started();
        let preparation = self
            .task_runtime_store
            .create_run_for_active_workspace(
                &run_id,
                &conversation_id,
                "",
                request.domain_profile,
                goal,
                request.task_kind,
                AttendedMode::Unattended,
            )
            .and_then(|_| {
                self.task_runtime_store
                    .configure_run_continuation(&run_id, true, true, None, None)
                    .map(|_| ())
            })
            .and_then(|_| {
                self.task_runtime_store.record_trigger_metadata(
                    &run_id,
                    request.source,
                    request.task_kind,
                    request.prompt,
                    request.priority,
                    &request.dependencies,
                )
            });
        if let Err(error) = preparation {
            registration.fail_preparation(error.to_string());
            return Err(error.into());
        }
        self.start_run_driver(
            run_id.clone(),
            request.prompt.to_string(),
            request.dependencies,
            registration,
            cancel,
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
        let cancel = self.cancel.child_token();
        let admission = self
            .task_runtime_store
            .reserve_run_driver_admission(run_id.clone(), cancel.clone())?;
        let generation_lease = self
            .task_runtime_store
            .lease_active_workspace_generation()?;
        let mut registration = self
            .task_runtime_store
            .register_run_driver::<()>(admission, generation_lease)?;
        let conversation_id = format!("background:{source_id}:{}", uuid::Uuid::new_v4());
        let goal = if description.trim().is_empty() {
            "composite"
        } else {
            description
        };
        let task_kind = format!("bg:kind:{source_kind}_composite");
        registration.mark_preparation_started();
        let run = match self.task_runtime_store.prepare_run_for_active_workspace(
            &run_id,
            &conversation_id,
            "",
            DomainProfile::General,
            goal,
            &task_kind,
            AttendedMode::Unattended,
        ) {
            Ok(run) => run,
            Err(error) => {
                registration.reject(error.to_string());
                return Err(error.into());
            }
        };
        if let Err(error) = super::task_runtime::revisioned_adapter::publish_eko_task_plan(
            self.task_runtime_store.clone(),
            run,
            super::task_runtime::store::InitialRunTriggerMetadata {
                source: source_id.to_string(),
                kind: task_kind.clone(),
                prompt: goal.to_string(),
                priority: 5,
                dependencies: Vec::new(),
            },
            Some((true, true, None, None)),
            TaskPlan {
                plan_id: uuid::Uuid::new_v4().to_string(),
                run_id: run_id.clone(),
                revision: 1,
                domain_profile: DomainProfile::General,
                goal_revision: 1,
                goal_sha256: super::task_runtime::task_goal_sha256(goal),
                assumptions: Vec::new(),
                risks: Vec::new(),
                execution_mode: ExecutionMode::default(),
                tasks: plan_tasks,
            },
        )
        .await
        {
            registration.reject(error.to_string());
            return Err(error.into());
        }
        self.start_run_driver(
            run_id.clone(),
            goal.to_string(),
            Vec::new(),
            registration,
            cancel,
        )?;
        Ok(run_id)
    }

    fn start_run_driver(
        &self,
        run_id: String,
        prompt: String,
        dependencies: Vec<String>,
        registration: super::task_runtime::store::RegisteredRunDriver<()>,
        cancel: echo_agent::agent::CancellationToken,
    ) -> anyhow::Result<()> {
        let store = self.task_runtime_store.clone();
        let agent_provider = self.agent_provider.clone();
        let review_integration = self.review_integration.clone();
        let run_semaphore = self.run_semaphore.clone();
        let result_waiter = registration.start(move |receipt_owner| {
            drive_background_run(
                store,
                agent_provider,
                review_integration,
                run_semaphore,
                run_id,
                prompt,
                dependencies,
                cancel,
                receipt_owner,
            )
        });
        drop(result_waiter);
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

    pub async fn resume(&self, id: &str) -> anyhow::Result<()> {
        let cancel = self.cancel.child_token();
        let admission = self
            .task_runtime_store
            .reserve_run_driver_admission(id.to_string(), cancel.clone())?;
        let generation_lease = self
            .task_runtime_store
            .lease_active_workspace_generation()?;
        let registration = self
            .task_runtime_store
            .register_run_driver::<()>(admission, generation_lease)?;
        let run_id = id.to_string();
        let store = self.task_runtime_store.clone();
        let agent_provider = self.agent_provider.clone();
        let review_integration = self.review_integration.clone();
        let run_semaphore = self.run_semaphore.clone();
        TaskRuntimeBlockingAdapter::new(store.clone())
            .run_owned("prepare exact background resume", move || {
                let mut registration = registration;
                let snapshot = store
                    .get_run_state(&run_id)?
                    .ok_or_else(|| super::task_runtime::StoreError::RunNotFound(run_id.clone()))?;
                if !snapshot.run.conversation_id.starts_with("background:") {
                    let error = super::task_runtime::StoreError::InvalidPlan(format!(
                        "task run is not owned by the background service: {run_id}"
                    ));
                    registration.reject(error.to_string());
                    return Err(error);
                }
                let metadata = trigger_metadata(&store, &run_id);
                let prompt = metadata.prompt.unwrap_or_else(|| snapshot.run.goal.clone());
                let expected =
                    crate::tasks::task_runtime::TaskRunResumeIdentity::capture(&snapshot);
                if let Err(error) = store.resume_task_run_expected(&expected) {
                    let detail = error.to_string();
                    if matches!(
                        error,
                        super::task_runtime::StoreError::ResumeOutcomeUnknown { .. }
                    ) {
                        registration.fail_preparation(detail);
                    } else {
                        registration.reject(detail);
                    }
                    return Err(error);
                }
                registration.mark_preparation_started();
                let dependencies = metadata.dependencies;
                let result_waiter = registration.start(move |receipt_owner| {
                    drive_background_run(
                        store,
                        agent_provider,
                        review_integration,
                        run_semaphore,
                        run_id,
                        prompt,
                        dependencies,
                        cancel,
                        receipt_owner,
                    )
                });
                drop(result_waiter);
                Ok(())
            })
            .await?;
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
    pub fn retry_blocked_task(
        &self,
        run_id: &str,
        task_id: &str,
    ) -> anyhow::Result<TaskRetryPreparation> {
        let cancel = self.cancel.child_token();
        let store = self.task_runtime_store.clone();
        let preflight_store = store.clone();
        let preflight_run_id = run_id.to_string();
        let agent_provider = self.agent_provider.clone();
        let review_integration = self.review_integration.clone();
        let run_semaphore = self.run_semaphore.clone();
        let driver_store = store.clone();
        let driver_run_id = run_id.to_string();
        let driver_cancel = cancel.clone();
        let (preparation, result_waiter) = store.spawn_supervised_task_retry(
            run_id.to_string(),
            task_id.to_string(),
            cancel,
            move || {
                let run = preflight_store.get_run(&preflight_run_id)?.ok_or_else(|| {
                    super::task_runtime::StoreError::RunNotFound(preflight_run_id.clone())
                })?;
                if !run.conversation_id.starts_with("background:") {
                    return Err(super::task_runtime::StoreError::InvalidPlan(format!(
                        "task run is not owned by the background service: {preflight_run_id}"
                    )));
                }
                let metadata = trigger_metadata(&preflight_store, &preflight_run_id);
                Ok((metadata.prompt.unwrap_or(run.goal), metadata.dependencies))
            },
            move |(prompt, dependencies), receipt_owner| {
                drive_background_run(
                    driver_store,
                    agent_provider,
                    review_integration,
                    run_semaphore,
                    driver_run_id,
                    prompt,
                    dependencies,
                    driver_cancel,
                    receipt_owner,
                )
            },
        )?;
        drop(result_waiter);
        Ok(preparation)
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
        self.boot_reconciler
            .recover_once()
            .await
            .map_err(anyhow::Error::msg)?;
        let mut runs = self
            .task_runtime_store
            .list_runs_in(&[TaskRunStatus::Pending, TaskRunStatus::Paused])?;
        runs.sort_by_key(|run| run.status == TaskRunStatus::Paused);
        let mut resumed = 0usize;
        for run in runs
            .into_iter()
            .filter(|run| run.conversation_id.starts_with("background:"))
        {
            let cancel = self.cancel.child_token();
            let admission = self
                .task_runtime_store
                .reserve_run_driver_admission(run.run_id.clone(), cancel.clone())?;
            let generation_lease = self
                .task_runtime_store
                .lease_active_workspace_generation()?;
            let mut registration = self
                .task_runtime_store
                .register_run_driver::<()>(admission, generation_lease)?;
            let metadata = trigger_metadata(&self.task_runtime_store, &run.run_id);
            let prompt = metadata.prompt.unwrap_or(run.goal);
            registration.mark_preparation_started();
            if run.status == TaskRunStatus::Paused {
                match self
                    .boot_reconciler
                    .resume(&run.run_id, true, false, &cancel)
                    .await
                    .map_err(anyhow::Error::msg)?
                {
                    TaskRunBootOutcome::Resumed(_) => {}
                    TaskRunBootOutcome::Blocked(blockers) => {
                        registration.reject(format!(
                            "boot auto-resume rejected for {}: {}",
                            run.run_id,
                            blockers
                                .iter()
                                .map(|blocker| blocker.as_str())
                                .collect::<Vec<_>>()
                                .join(",")
                        ));
                        continue;
                    }
                    TaskRunBootOutcome::Cancelled => {
                        registration
                            .reject(format!("boot auto-resume cancelled for {}", run.run_id));
                        return Ok(resumed);
                    }
                }
            }
            self.start_run_driver(
                run.run_id,
                prompt,
                metadata.dependencies,
                registration,
                cancel,
            )?;
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

#[allow(clippy::too_many_arguments)]
async fn drive_background_run(
    store: Arc<TaskRuntimeStore>,
    agent_provider: Arc<dyn TaskAgentProvider>,
    review_integration: Option<Arc<crate::evolution::ReviewIntegration>>,
    run_semaphore: Arc<tokio::sync::Semaphore>,
    run_id: String,
    prompt: String,
    dependencies: Vec<String>,
    cancel: CancellationToken,
    mut receipt_owner: super::task_runtime::store::RunDriverReceiptOwner,
) -> Result<(), String> {
    if let Err(error) = wait_for_dependencies(&store, &dependencies, &cancel).await {
        finish_pre_execution_failure(&store, &run_id, &error, cancel.is_cancelled())?;
        if cancel.is_cancelled() {
            return Ok(());
        }
        return Err(error);
    }
    if cancel.is_cancelled() {
        finish_pre_execution_failure(&store, &run_id, "run cancelled", true)?;
        return Ok(());
    }
    let _run_permit = tokio::select! {
        _ = cancel.cancelled() => {
            finish_pre_execution_failure(&store, &run_id, "run cancelled", true)?;
            return Ok(());
        }
        permit = run_semaphore.acquire_owned() => match permit {
            Ok(permit) => permit,
            Err(error) => {
                let message = format!("background concurrency closed: {error}");
                finish_pre_execution_failure(&store, &run_id, &message, false)?;
                return Err(message);
            }
        }
    };
    if let Err(error) = transition_to_running(&store, &run_id) {
        finish_pre_execution_failure(&store, &run_id, &error, false)?;
        return Err(error);
    }
    let memory_generation = review_integration
        .as_ref()
        .map(|integration| integration.lease_generation())
        .transpose()
        .map_err(|error| format!("memory generation unavailable: {error}"))?;
    if let Some(generation) = memory_generation.as_ref() {
        receipt_owner.retain(generation.clone());
    }
    let layer_manager = memory_generation
        .as_ref()
        .map(|generation| generation.create_layer_manager().map(Arc::new))
        .transpose()
        .map_err(|error| format!("memory layer unavailable: {error}"))?;
    let lease = match agent_provider.acquire_for_task(&run_id).await {
        Ok(lease) => lease,
        Err(error) => {
            let message = format!("acquire agent: {error}");
            finish_running_failure(&store, &run_id, &message)?;
            return Err(message);
        }
    };
    let agent = lease.agent();
    receipt_owner.retain(lease);
    if let Some(manager) = layer_manager.as_ref() {
        let manager = manager.clone();
        agent
            .write(|value| value.install_memory_layer_manager(manager))
            .await;
    }
    let reviewer_llm = agent.read(|agent| agent.llm_client().cloned()).await;
    let result = match store.get_plan(&run_id) {
        Ok(Some(_)) => super::task_runtime::execute_run(
            store.clone(),
            Some(agent),
            reviewer_llm,
            layer_manager,
            memory_generation,
            None,
            None,
            &run_id,
            cancel,
            MemoryPolicy::None,
            None,
        )
        .await
        .map(|_| run_id.clone()),
        Ok(None) => {
            register_task_execute(&agent, store.clone()).await;
            super::task_runtime::drive_unattended_run(
                store.clone(),
                agent,
                &run_id,
                "background",
                &run_id,
                &prompt,
                cancel,
                UnattendedWriteMode::default(),
                None,
            )
            .await
        }
        Err(error) => Err(super::task_runtime::ExecError::Other(format!(
            "read plan before background execution: {error}"
        ))),
    };
    if let Err(error) = result {
        let message = error.to_string();
        finish_running_failure(&store, &run_id, &message)?;
        return Err(message);
    }
    Ok(())
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
) -> Result<(), String> {
    let target = if cancelled {
        TaskRunStatus::Cancelled
    } else {
        TaskRunStatus::Failed
    };
    store
        .finalize_run(run_id, target, Some(error))
        .map(|_| ())
        .map_err(|settlement_error| settlement_error.to_string())
}

fn finish_running_failure(
    store: &TaskRuntimeStore,
    run_id: &str,
    error: &str,
) -> Result<(), String> {
    store
        .finalize_run(run_id, TaskRunStatus::Failed, Some(error))
        .map(|_| ())
        .map_err(|settlement_error| settlement_error.to_string())
}

async fn register_task_execute(agent: &AgentHandle, store: Arc<TaskRuntimeStore>) {
    if agent
        .read(|inner| inner.tool_names().iter().any(|name| name == "task_execute"))
        .await
    {
        return;
    }
    let tool = ExecuteTaskTool::new(store, agent.clone());
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

    fn prepare_background_retry_run(
        store: &TaskRuntimeStore,
        run_id: &str,
        task_id: &str,
        recovery: bool,
    ) -> Result<(), String> {
        store
            .create_run(
                run_id,
                "default",
                &format!("background:test:{run_id}"),
                "",
                DomainProfile::General,
                "retry run",
                "bg:kind:test",
                AttendedMode::Unattended,
            )
            .map_err(|error| error.to_string())?;
        store
            .attach_plan_for_test(&TaskPlan {
                plan_id: format!("{run_id}-plan"),
                run_id: run_id.to_string(),
                revision: 1,
                domain_profile: DomainProfile::General,
                goal_revision: 1,
                goal_sha256: crate::tasks::task_runtime::task_goal_sha256("retry run"),
                assumptions: Vec::new(),
                risks: Vec::new(),
                execution_mode: ExecutionMode::Sequential,
                tasks: vec![PlanTask {
                    id: task_id.to_string(),
                    title: "Retry task".to_string(),
                    kind: PlanTaskKind::Investigation,
                    agent_role: "researcher".to_string(),
                    max_retries: 2,
                    ..PlanTask::default()
                }],
            })
            .map_err(|error| error.to_string())?;
        store
            .transition_run(run_id, TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        let (task_status, summary, run_status) = if recovery {
            (
                TodoStatus::Blocked,
                "mutating side effect is indeterminate after restart",
                TaskRunStatus::Paused,
            )
        } else {
            (
                TodoStatus::Failed,
                "execution failed",
                TaskRunStatus::Failed,
            )
        };
        store
            .set_task_status(
                run_id,
                task_id,
                task_status,
                Some("researcher"),
                Some(summary),
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run(run_id, run_status)
            .map_err(|error| error.to_string())?;
        store
            .record_trigger_metadata(run_id, "test", "research", "retry run", 5, &[])
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    fn retry_snapshot(
        store: &TaskRuntimeStore,
        run_id: &str,
    ) -> Result<(TaskRunStatus, TodoStatus, u32, usize), String> {
        let run = store
            .get_run(run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("run missing: {run_id}"))?;
        let task = store
            .get_plan(run_id)
            .map_err(|error| error.to_string())?
            .and_then(|plan| plan.tasks.into_iter().next())
            .ok_or_else(|| format!("task missing: {run_id}"))?;
        Ok((
            run.status,
            task.status,
            task.retry_count,
            store
                .list_events(run_id, 0)
                .map_err(|error| error.to_string())?
                .len(),
        ))
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
        let continuation = store
            .get_run_state(&run_id)
            .map_err(|error| error.to_string())?
            .and_then(|snapshot| snapshot.continuation)
            .ok_or_else(|| "background continuation policy missing".to_string())?;
        assert!(continuation.enabled);
        assert!(continuation.auto_resume_after_restart);
        Ok(())
    }

    #[tokio::test]
    async fn dag_submission_crash_before_publish_leaves_no_visible_run_or_driver()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join("tasks");
        let store = Arc::new(
            TaskRuntimeStore::new_in_memory_with_shadow_root(root.clone())
                .map_err(|error| error.to_string())?,
        );
        store.fail_next_initial_publish_before_rename();
        let service = BackgroundTaskService::new(
            test_agent()?,
            CancellationToken::new(),
            Some(store.clone()),
        )
        .await
        .map_err(|error| error.to_string())?;

        let result = service
            .submit_dag(
                vec![PlanTask {
                    id: "inspect".to_string(),
                    title: "Inspect runtime".to_string(),
                    kind: PlanTaskKind::Investigation,
                    agent_role: "explorer".to_string(),
                    ..PlanTask::default()
                }],
                "inspect runtime",
                "test",
                "atomic-publish",
            )
            .await;
        assert!(result.is_err());
        store
            .clone()
            .shutdown_run_drivers()
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            store
                .active_run_driver_count()
                .map_err(|error| error.to_string())?,
            0
        );
        let has_visible_run = std::fs::read_dir(&root)
            .map_err(|error| error.to_string())?
            .filter_map(Result::ok)
            .any(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| !name.starts_with(".preparing-"))
            });
        assert!(!has_visible_run);
        drop(service);
        drop(store);

        let restarted = Arc::new(
            TaskRuntimeStore::new_in_memory_with_shadow_root(root.clone())
                .map_err(|error| error.to_string())?,
        );
        assert!(
            std::fs::read_dir(&root)
                .map_err(|error| error.to_string())?
                .filter_map(Result::ok)
                .all(|entry| entry
                    .file_name()
                    .to_str()
                    .is_none_or(|name| !name.starts_with(".preparing-")))
        );
        restarted
            .shutdown_run_drivers()
            .await
            .map_err(|error| error.to_string())?;
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
            .attach_plan_for_test(&TaskPlan {
                plan_id: "retry-plan".to_string(),
                run_id: "retry-run".to_string(),
                revision: 1,
                domain_profile: DomainProfile::General,
                goal_revision: 1,
                goal_sha256: crate::tasks::task_runtime::task_goal_sha256("retry run"),
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
        let preparation = service
            .retry_blocked_task("retry-run", "retry-task")
            .map_err(|error| error.to_string())?;
        assert_eq!(
            preparation,
            TaskRetryPreparation::Acceptance { next_attempt: 1 }
        );
        assert_eq!(store.active_run_driver_count()?, 1);
        store
            .shutdown_run_drivers()
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(store.active_run_driver_count()?, 0);
        Ok(())
    }

    #[tokio::test]
    async fn cli_retry_selects_recovery_without_acceptance_mutation() -> Result<(), String> {
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        prepare_background_retry_run(&store, "recovery-run", "recovery-task", true)?;
        let service = BackgroundTaskService::new(
            test_agent()?,
            CancellationToken::new(),
            Some(store.clone()),
        )
        .await
        .map_err(|error| error.to_string())?;

        let preparation = service
            .retry_blocked_task("recovery-run", "recovery-task")
            .map_err(|error| error.to_string())?;
        assert_eq!(preparation, TaskRetryPreparation::Recovery);
        assert!(
            store
                .list_recovery_blockers("recovery-run")
                .map_err(|error| error.to_string())?
                .is_empty()
        );
        let plan = store
            .get_plan("recovery-run")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "recovery plan missing".to_string())?;
        assert_eq!(
            plan.tasks.first().map(|task| task.retry_count),
            Some(1),
            "recovery retry must apply the canonical framework retry exactly once"
        );
        assert_eq!(
            store
                .list_events("recovery-run", 0)
                .map_err(|error| error.to_string())?
                .into_iter()
                .filter(|event| {
                    event.event_type
                        == crate::tasks::task_runtime::RuntimeEventKind::RecoveryResolved
                })
                .count(),
            1,
            "recovery retry must publish one resolution fact"
        );
        store
            .shutdown_run_drivers()
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    #[tokio::test]
    async fn cli_retry_admission_and_registration_failures_do_not_mutate_runtime()
    -> Result<(), String> {
        for failure in ["closed-admission", "registration"] {
            let store =
                Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
            let run_id = format!("{failure}-run");
            prepare_background_retry_run(&store, &run_id, "retry-task", false)?;
            let service = BackgroundTaskService::new(
                test_agent()?,
                CancellationToken::new(),
                Some(store.clone()),
            )
            .await
            .map_err(|error| error.to_string())?;
            let before = retry_snapshot(&store, &run_id)?;
            if failure == "closed-admission" {
                store
                    .shutdown_run_drivers()
                    .await
                    .map_err(|error| error.to_string())?;
            } else {
                store.fail_next_run_driver_registration_for_test();
            }

            if service.retry_blocked_task(&run_id, "retry-task").is_ok() {
                return Err(format!("{failure} retry unexpectedly succeeded"));
            }
            assert_eq!(before, retry_snapshot(&store, &run_id)?);
        }
        Ok(())
    }

    #[tokio::test]
    async fn auto_resume_only_accepts_boot_recovery_pause() -> Result<(), String> {
        let store = TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?;
        let workspace_id = store.active_workspace_id();
        store
            .create_run(
                "run",
                &workspace_id,
                "background:test:run",
                "",
                DomainProfile::General,
                "goal",
                "bg:kind:test",
                AttendedMode::Unattended,
            )
            .map_err(|error| error.to_string())?;
        store
            .attach_plan_for_test(&TaskPlan {
                plan_id: "run-plan".to_string(),
                run_id: "run".to_string(),
                revision: 1,
                domain_profile: DomainProfile::General,
                goal_revision: 1,
                goal_sha256: crate::tasks::task_runtime::task_goal_sha256("goal"),
                assumptions: Vec::new(),
                risks: Vec::new(),
                execution_mode: ExecutionMode::Sequential,
                tasks: vec![PlanTask {
                    id: "run-task".to_string(),
                    title: "Resume after recovery".to_string(),
                    ..Default::default()
                }],
            })
            .map_err(|error| error.to_string())?;
        store
            .transition_run("run", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        store
            .configure_run_continuation("run", true, true, None, None)
            .map_err(|error| error.to_string())?;
        store
            .transition_run("run", TaskRunStatus::Paused)
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            store
                .boot_auto_resume_decision("run", true, false)
                .map_err(|error| error.to_string())?,
            BootAutoResumeDecision::Blocked(blockers)
                if blockers.contains(&crate::tasks::task_runtime::store::BootAutoResumeBlocker::NotBootRecovery)
        ));

        store
            .resume_task_run("run")
            .map_err(|error| error.to_string())?;
        assert_eq!(
            store
                .recover_incomplete()
                .map_err(|error| error.to_string())?,
            1
        );
        assert!(matches!(
            store
                .boot_auto_resume_decision("run", true, false)
                .map_err(|error| error.to_string())?,
            BootAutoResumeDecision::Ready { .. }
        ));
        let store = Arc::new(store);
        let service = BackgroundTaskService::new(
            test_agent()?,
            CancellationToken::new(),
            Some(store.clone()),
        )
        .await
        .map_err(|error| error.to_string())?;
        assert_eq!(
            service
                .resume_pending()
                .await
                .map_err(|error| error.to_string())?,
            1
        );
        store
            .shutdown_run_drivers()
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }
}
