//! Cron scheduler adapter — wires the framework's `SchedulerRunner` to the
//! CLI's `AgentHandle` and TaskRuntime.
//!
//! The framework's runner is generic over a `FireFn` callback; this module
//! provides [`build_fire_fn`] which constructs one that launches TaskRuntime runs.
//!
//! Phase 3.1: ALL cron tasks route through the unified TaskRuntime executor
//! (`drive_existing_cron_run`). The legacy `[plan]` prefix is stripped for backward
//! compatibility but no longer selects a separate path — simple prompts are
//! answered directly by the agent and materialized as a one-task Plan with
//! completion evidence; complex prompts drive task_create + task_execute.

use crate::agent_handle::AgentHandle;
use crate::agent_pool::AgentPool;
use crate::tasks::task_runtime::TaskRuntimeStore;
use crate::tasks::task_runtime::executor::{create_unattended_run, drive_existing_cron_run};
use echo_agent::agent::CancellationToken;
use echo_agent::scheduler::{
    CronTask, CronTaskStore, FireFn, SchedulerRunner as FrameworkSchedulerRunner,
};
use std::sync::Arc;

/// The CLI's scheduler runner is the framework's `SchedulerRunner`.
pub type SchedulerRunner = FrameworkSchedulerRunner;

/// Legacy plan-orchestration marker prefix. Phase 3.1: stripped for backward
/// compatibility but no longer routes — all cron tasks go through
/// the canonical supervised cron driver regardless.
const PLAN_MARKER: &str = "[plan]";

/// Build a `FireFn` that dispatches cron task execution.
///
/// Phase 3.1+: ALL cron prompts route through the unified supervised
/// TaskRuntime executor, `Unattended`). The `[plan]` prefix is stripped for
/// backward compatibility. Simple prompts are answered directly by the agent
/// and materialized as a one-task Plan with completion evidence; complex prompts
/// drive task_create + task_execute.
///
/// Phase 3.5: the dead-in-practice `runtime_store=None` fallback (legacy
/// `BackgroundTaskService::submit` + `execute_direct`) has been removed —
/// `AppState` always constructs a `TaskRuntimeStore` at boot.
///
/// Phase C: cron now runs on a POOL-ACQUIRED per-run agent (not the shared
/// primary chat agent). This fixes the latent `working_dir` override bug —
/// each cron run's worktree binding lives on its own agent, so overlapping
/// runs no longer clobber each other's working_dir (previously masked only by
/// the agent's execution_mutex). When no pool is configured, falls back to the
/// shared `agent` (the pre-C behavior, still correct for single-agent setups).
pub fn build_fire_fn(
    agent: AgentHandle,
    task_runtime_store: Option<Arc<TaskRuntimeStore>>,
    pool: Option<Arc<AgentPool>>,
    webhook_emitter: Option<Arc<crate::webhook::WebhookEmitter>>,
    review_integration: Option<Arc<crate::evolution::ReviewIntegration>>,
) -> FireFn {
    build_fire_fn_with_cancel(
        agent,
        task_runtime_store,
        pool,
        webhook_emitter,
        review_integration,
        CancellationToken::new(),
    )
}

fn build_fire_fn_with_cancel(
    agent: AgentHandle,
    task_runtime_store: Option<Arc<TaskRuntimeStore>>,
    pool: Option<Arc<AgentPool>>,
    webhook_emitter: Option<Arc<crate::webhook::WebhookEmitter>>,
    review_integration: Option<Arc<crate::evolution::ReviewIntegration>>,
    scheduler_cancel: CancellationToken,
) -> FireFn {
    Arc::new(move |task: CronTask| {
        let fallback_agent = agent.clone();
        let runtime_store = task_runtime_store.clone();
        let pool = pool.clone();
        let webhook_emitter = webhook_emitter.clone();
        let review_integration = review_integration.clone();
        let scheduler_cancel = scheduler_cancel.clone();
        Box::pin(async move {
            let store = runtime_store.ok_or_else(|| {
                echo_agent::error::ReactError::Other(
                    "TaskRuntimeStore not configured — cron cannot run".into(),
                )
            })?;
            // [plan] prefix strip for backward compat; no longer routes.
            let prompt = task
                .prompt
                .strip_prefix(PLAN_MARKER)
                .map(str::trim)
                .unwrap_or(&task.prompt);
            if prompt.is_empty() {
                return Err(echo_agent::error::ReactError::Other(
                    "cron prompt is empty (after [plan] strip)".into(),
                ));
            }
            let fire_id = uuid::Uuid::new_v4().to_string();
            let run_id = uuid::Uuid::new_v4().to_string();
            let cancel = scheduler_cancel.child_token();
            let admission = store
                .reserve_run_driver_admission(run_id.clone(), cancel.clone())
                .map_err(|error| {
                    echo_agent::error::ReactError::Other(format!(
                        "cron TaskRun driver admission failed: {error}"
                    ))
                })?;
            // Keep TaskRun creation/execution, memory and pool acquisition, and
            // final projection on one workspace generation. The canonical
            // driver reservation is acquired first so shutdown cannot overtake
            // this accepted fire before it is registered with the supervisor.
            let generation_lease = store.lease_active_workspace_generation().map_err(|error| {
                echo_agent::error::ReactError::Other(format!(
                    "task runtime generation admission failed: {error}"
                ))
            })?;
            let mut registration = store
                .register_run_driver::<String>(admission, generation_lease)
                .map_err(|error| {
                    echo_agent::error::ReactError::Other(format!(
                        "cron TaskRun driver registration failed: {error}"
                    ))
                })?;
            registration.mark_preparation_started();
            if let Err(error) =
                create_unattended_run(&store, &run_id, "cron", &task.id, &fire_id, prompt)
            {
                registration.fail_preparation(error.to_string());
                return Err(echo_agent::error::ReactError::Other(format!(
                    "cron TaskRun creation failed: {error}"
                )));
            }
            let owned_store = store.clone();
            let owned_run_id = run_id.clone();
            let owned_task = task.clone();
            let owned_fire_id = fire_id.clone();
            let owned_prompt = prompt.to_string();
            let owned_cancel = cancel.clone();
            let waiter = registration.start(move |mut receipt_owner| async move {
                // The exact driver owns memory settlement before pool
                // admission. A configured integration never falls back
                // to an unpinned manager while a rebind is in progress.
                let memory_generation = review_integration
                    .as_ref()
                    .map(|integration| integration.lease_generation())
                    .transpose()
                    .map_err(|error| format!("cron memory generation unavailable: {error}"))?;
                if let Some(generation) = memory_generation.as_ref() {
                    receipt_owner.retain(generation.clone());
                }
                let layer_manager = memory_generation
                    .as_ref()
                    .map(crate::evolution::ReviewGenerationLease::layer_manager)
                    .transpose()
                    .map_err(|error| format!("cron memory layer unavailable: {error}"))?;

                // Pool admission follows TaskRuntime and memory generation admission.
                let run_agent = match &pool {
                    Some(pool) => {
                        let run_key = format!("__cron__:{}:{owned_fire_id}", owned_task.id);
                        let acquired = pool
                            .acquire(&run_key)
                            .await
                            .map_err(|error| format!("cron pool acquire failed: {error}"))?;
                        let agent = acquired.agent();
                        receipt_owner.retain(pool.retain_for_supervised_run(run_key, acquired));
                        register_task_execute_on_agent(&agent, owned_store.clone()).await;
                        agent
                    }
                    None => fallback_agent,
                };
                if let Some(layer_manager) = layer_manager {
                    run_agent
                        .write(|agent| agent.install_memory_layer_manager(layer_manager))
                        .await;
                }
                let result = drive_existing_cron_run(
                    owned_store.clone(),
                    run_agent,
                    owned_run_id.clone(),
                    &owned_task.id,
                    &owned_fire_id,
                    &owned_prompt,
                    owned_cancel,
                )
                .await
                .map_err(|error| format!("cron run failed: {error}"));
                let settled_run_id = result?;
                if let Some(emitter) = webhook_emitter.as_ref() {
                    emitter.emit(crate::webhook::WebhookEvent::CronTaskCompleted {
                        task_id: owned_task.id.clone(),
                        task_name: owned_task.name.clone(),
                        result_summary: format!("cron run {settled_run_id} finished"),
                    });
                }
                Ok(format!(
                    "cron run {settled_run_id} finished for task {}",
                    owned_task.id
                ))
            });
            match waiter.await {
                Ok(Ok(result)) => Ok(result),
                Ok(Err(error)) => Err(echo_agent::error::ReactError::Other(error)),
                Err(error) => Err(echo_agent::error::ReactError::Other(format!(
                    "cron TaskRun result waiter failed: {error}"
                ))),
            }
        }) as futures::future::BoxFuture<'static, echo_agent::error::Result<String>>
    })
}

/// Register the `task_execute` tool on a cron run's pool-acquired agent
/// (Phase C). The normal shared pool registry already contains a
/// conversation-aware tool; this fills the gap for standalone runners.
async fn register_task_execute_on_agent(agent_handle: &AgentHandle, store: Arc<TaskRuntimeStore>) {
    use crate::tasks::task_runtime::ExecuteTaskTool;
    if agent_handle
        .read(|agent| agent.tool_names().iter().any(|name| name == "task_execute"))
        .await
    {
        return;
    }
    let tool = ExecuteTaskTool::new(store, agent_handle.clone());
    let added = agent_handle
        .write(|agent| {
            agent.add_tool(Box::new(tool));
            true
        })
        .await;
    if added {
        tracing::debug!("Registered task_execute tool on cron run agent");
    } else {
        tracing::warn!("Failed to register task_execute on cron run agent (write lock poisoned)");
    }
}

/// Convenience constructor: build a `SchedulerRunner` with the CLI's
/// preferred wiring (agent + optional background task service).
///
/// U1c phase-1 → Phase 3.1: `task_runtime_store` enables the unified
/// cron TaskRuntime path (all cron, not just `[plan]`).
/// Phase C: `pool` enables per-run agent isolation (recommended when an
/// `AgentPool` exists); falls back to the shared `agent` when `None`.
pub async fn new_scheduler_runner(
    store: CronTaskStore,
    cancel: echo_agent::agent::CancellationToken,
    agent: AgentHandle,
    task_runtime_store: Option<Arc<TaskRuntimeStore>>,
    pool: Option<Arc<AgentPool>>,
    webhook_emitter: Option<Arc<crate::webhook::WebhookEmitter>>,
    review_integration: Option<Arc<crate::evolution::ReviewIntegration>>,
) -> echo_agent::error::Result<SchedulerRunner> {
    let fire_fn = build_fire_fn_with_cancel(
        agent,
        task_runtime_store,
        pool,
        webhook_emitter,
        review_integration,
        cancel.clone(),
    );
    SchedulerRunner::new(store, cancel, fire_fn).await
}

#[cfg(test)]
mod tests {
    use super::*; // AgentHandle, Arc, CronTask, TaskRuntimeStore, CancellationToken, ...
    use crate::tasks::task_runtime::TaskRunStatus;
    use echo_agent::agent::react::builder::ReactAgentBuilder;
    use echo_agent::testing::MockLlmClient;

    /// Phase 3.1: 非 `[plan]` cron 必须经 supervised TaskRuntime driver(在 store 建 run),
    /// 而非旧 `agent.chat`/`execute_direct`。mock LLM 返回纯文本(无 tool call),
    /// agent 直接作答,driver 的 direct 分支自动转 Completed。
    #[tokio::test]
    async fn build_fire_fn_routes_non_plan_cron_to_supervised_run() -> Result<(), String> {
        let llm = MockLlmClient::new().with_response("ok");
        let agent = ReactAgentBuilder::new()
            .llm_client(Arc::new(llm))
            .system_prompt("test")
            .build()
            .map_err(|error| error.to_string())?;
        let handle = AgentHandle::new(agent);
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
        store
            .rebind_shadow_root(workspace.path().join("tasks"), "cron-workspace")
            .await
            .map_err(|error| error.to_string())?;

        // task_service=None:Phase 3.1 前会逼非-[plan] prompt 走 execute_direct;
        // 3.1 后 runtime_store(此处置 Some)接管所有 prompt → supervised driver。
        let fire_fn = build_fire_fn(handle, Some(store.clone()), None, None, None);

        let task = CronTask::new("plain", "*/5 * * * *", "hello world");
        let result = fire_fn(task).await;
        assert!(result.is_ok(), "fire_fn should succeed: {:?}", result.err());

        let completed = store
            .list_runs_in(&[TaskRunStatus::Completed])
            .map_err(|error| error.to_string())?;
        assert_eq!(
            completed.len(),
            1,
            "非-[plan] cron 应经 supervised driver 建恰好 1 个 Completed run"
        );
        let completed_run = completed
            .first()
            .ok_or_else(|| "completed cron run is missing".to_string())?;
        assert_eq!(completed_run.workspace_id, "cron-workspace");
        Ok(())
    }

    /// `[plan]` 前缀仍经 supervised driver(marker strip,向后兼容)。
    #[tokio::test]
    async fn build_fire_fn_strips_plan_marker_and_routes_to_supervised_run() -> Result<(), String> {
        let llm = MockLlmClient::new().with_response("ok");
        let agent = ReactAgentBuilder::new()
            .llm_client(Arc::new(llm))
            .system_prompt("test")
            .build()
            .map_err(|error| error.to_string())?;
        let handle = AgentHandle::new(agent);
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let fire_fn = build_fire_fn(handle, Some(store.clone()), None, None, None);

        let task = CronTask::new("plan", "*/5 * * * *", "[plan] do the thing");
        let result = fire_fn(task).await;
        assert!(result.is_ok(), "fire_fn should succeed: {:?}", result.err());

        let completed = store
            .list_runs_in(&[TaskRunStatus::Completed])
            .map_err(|error| error.to_string())?;
        assert_eq!(
            completed.len(),
            1,
            "[plan] cron 应 strip marker 后建 1 个 Completed run"
        );
        Ok(())
    }

    #[tokio::test]
    async fn cron_stream_failure_is_persisted_and_returned_as_error() -> Result<(), String> {
        let llm = MockLlmClient::new().with_error(echo_agent::error::ReactError::Other(
            "provider unavailable".to_string(),
        ));
        let agent = ReactAgentBuilder::new()
            .llm_client(Arc::new(llm))
            .system_prompt("test")
            .build()
            .map_err(|error| error.to_string())?;
        let handle = AgentHandle::new(agent);
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let fire_fn = build_fire_fn(handle, Some(store.clone()), None, None, None);

        let task = CronTask::new("failing", "*/5 * * * *", "run this");
        let result = fire_fn(task).await;
        assert!(result.is_err(), "failed cron run must not report success");
        let failed = store
            .list_runs_in(&[TaskRunStatus::Failed])
            .map_err(|error| error.to_string())?;
        assert_eq!(failed.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn scheduler_cancellation_reaches_the_cron_task_run() -> Result<(), String> {
        let llm = MockLlmClient::new().with_response("should not complete");
        let agent = ReactAgentBuilder::new()
            .llm_client(Arc::new(llm))
            .system_prompt("test")
            .build()
            .map_err(|error| error.to_string())?;
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let scheduler_cancel = CancellationToken::new();
        scheduler_cancel.cancel();
        let fire_fn = build_fire_fn_with_cancel(
            AgentHandle::new(agent),
            Some(store.clone()),
            None,
            None,
            None,
            scheduler_cancel,
        );

        let result = fire_fn(CronTask::new("cancelled", "*/5 * * * *", "do work")).await;

        assert!(result.is_err());
        let cancelled = store
            .list_runs_in(&[TaskRunStatus::Cancelled])
            .map_err(|error| error.to_string())?;
        assert_eq!(cancelled.len(), 1);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn scheduler_reservation_is_boot_resumable_during_runtime_shutdown() -> Result<(), String>
    {
        let llm = MockLlmClient::new().with_response("should be cancelled");
        let agent = ReactAgentBuilder::new()
            .llm_client(Arc::new(llm))
            .system_prompt("test")
            .build()
            .map_err(|error| error.to_string())?;
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let (reservation_started, release_reservation) =
            store.park_next_run_driver_admission_for_test()?;
        let fire_fn = build_fire_fn(
            AgentHandle::new(agent),
            Some(store.clone()),
            None,
            None,
            None,
        );
        let fire = tokio::spawn(async move {
            fire_fn(CronTask::new("shutdown-race", "*/5 * * * *", "do work")).await
        });
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            tokio::task::spawn_blocking(move || {
                reservation_started.recv_timeout(std::time::Duration::from_secs(2))
            }),
        )
        .await
        .map_err(|_| "scheduler admission reservation was not observed".to_string())?
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;

        let shutdown_store = store.clone();
        let shutdown = tokio::spawn(async move { shutdown_store.shutdown_run_drivers().await });
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            store.wait_run_driver_shutdown_started(),
        )
        .await
        .map_err(|_| "TaskRuntime shutdown did not close admission".to_string())?;
        if shutdown.is_finished() {
            return Err("shutdown overtook the scheduler's accepted reservation".to_string());
        }
        release_reservation
            .send(())
            .map_err(|_| "scheduler admission reservation stopped waiting".to_string())?;

        let (fire_result, shutdown_result) =
            tokio::time::timeout(std::time::Duration::from_secs(2), async move {
                tokio::join!(fire, shutdown)
            })
            .await
            .map_err(|_| "scheduler/runtime shutdown race did not settle".to_string())?;
        let fire_result = fire_result.map_err(|error| error.to_string())?;
        assert!(fire_result.is_err());
        shutdown_result
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert_eq!(store.active_run_driver_count()?, 0);
        assert_eq!(store.active_run_driver_receipt_count()?, 0);
        let paused = store
            .list_runs_in(&[TaskRunStatus::Paused])
            .map_err(|error| error.to_string())?;
        assert_eq!(paused.len(), 1);
        let run_id = paused
            .first()
            .map(|run| run.run_id.as_str())
            .ok_or_else(|| "shutdown did not retain the accepted scheduler run".to_string())?;
        let pause_reason = store
            .get_run_state(run_id)
            .map_err(|error| error.to_string())?
            .and_then(|state| state.continuation)
            .and_then(|continuation| continuation.pause)
            .map(|pause| pause.reason);
        assert_eq!(
            pause_reason,
            Some(crate::tasks::task_runtime::RunPauseReason::BootRecovery)
        );
        Ok(())
    }
}
