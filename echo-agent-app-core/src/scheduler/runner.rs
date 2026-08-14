//! Cron scheduler adapter — wires the framework's `SchedulerRunner` to the
//! CLI's `AgentHandle` and TaskRuntime.
//!
//! The framework's runner is generic over a `FireFn` callback; this module
//! provides [`build_fire_fn`] which constructs one that launches TaskRuntime runs.
//!
//! Phase 3.1: ALL cron tasks route through the unified TaskRuntime executor
//! (`launch_cron_run`). The legacy `[plan]` prefix is stripped for backward
//! compatibility but no longer selects a separate path — simple prompts are
//! answered directly by the agent (auto-Completed when task_execute isn't
//! called); complex prompts drive task_create + task_execute.

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
/// `launch_cron_run` regardless.
const PLAN_MARKER: &str = "[plan]";

/// Build a `FireFn` that dispatches cron task execution.
///
/// Phase 3.1+: ALL cron prompts route through `launch_cron_run` (unified
/// TaskRuntime executor, `Unattended`). The `[plan]` prefix is stripped for
/// backward compatibility. Simple prompts are answered directly by the agent
/// (auto-Completed); complex prompts drive task_create + task_execute.
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
) -> FireFn {
    build_fire_fn_with_cancel(
        agent,
        task_runtime_store,
        pool,
        webhook_emitter,
        CancellationToken::new(),
    )
}

fn build_fire_fn_with_cancel(
    agent: AgentHandle,
    task_runtime_store: Option<Arc<TaskRuntimeStore>>,
    pool: Option<Arc<AgentPool>>,
    webhook_emitter: Option<Arc<crate::webhook::WebhookEmitter>>,
    scheduler_cancel: CancellationToken,
) -> FireFn {
    Arc::new(move |task: CronTask| {
        let fallback_agent = agent.clone();
        let runtime_store = task_runtime_store.clone();
        let pool = pool.clone();
        let webhook_emitter = webhook_emitter.clone();
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
            // Keep agent acquisition, TaskRun creation/execution, pool release,
            // and the final result projection on one workspace generation.
            // `switch_workspace`/`exit_workspace` can prepare concurrently but
            // cannot rebind the TaskRuntime file authority until this drops.
            let generation_lease = store.lease_active_workspace_generation().map_err(|error| {
                echo_agent::error::ReactError::Other(format!(
                    "task runtime generation admission failed: {error}"
                ))
            })?;
            let fire_id = uuid::Uuid::new_v4().to_string();
            let run_id = uuid::Uuid::new_v4().to_string();
            let cancel = scheduler_cancel.child_token();
            create_unattended_run(&store, &run_id, "cron", &task.id, &fire_id, prompt).map_err(
                |error| {
                    echo_agent::error::ReactError::Other(format!(
                        "cron TaskRun creation failed: {error}"
                    ))
                },
            )?;
            let owned_store = store.clone();
            let owned_run_id = run_id.clone();
            let owned_task = task.clone();
            let owned_fire_id = fire_id.clone();
            let owned_prompt = prompt.to_string();
            let owned_cancel = cancel.clone();
            let waiter = store
                .spawn_run_driver(
                    run_id.clone(),
                    cancel,
                    generation_lease,
                    move |receipt_owner| async move {
                        // Pool admission follows TaskRuntime generation admission.
                        let run_agent = match &pool {
                            Some(pool) => {
                                let run_key = format!("__cron__:{}:{owned_fire_id}", owned_task.id);
                                let acquired = pool.acquire(&run_key).await.map_err(|error| {
                                    format!("cron pool acquire failed: {error}")
                                })?;
                                let agent = acquired.agent();
                                receipt_owner
                                    .retain(pool.retain_for_supervised_run(run_key, acquired));
                                register_task_execute_on_agent(&agent, owned_store.clone()).await;
                                agent
                            }
                            None => fallback_agent,
                        };
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
                    },
                )
                .map_err(|error| {
                    echo_agent::error::ReactError::Other(format!(
                        "cron TaskRun driver admission failed: {error}"
                    ))
                })?;
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
/// cron→launch_cron_run path (all cron, not just `[plan]`).
/// Phase C: `pool` enables per-run agent isolation (recommended when an
/// `AgentPool` exists); falls back to the shared `agent` when `None`.
pub async fn new_scheduler_runner(
    store: CronTaskStore,
    cancel: echo_agent::agent::CancellationToken,
    agent: AgentHandle,
    task_runtime_store: Option<Arc<TaskRuntimeStore>>,
    pool: Option<Arc<AgentPool>>,
    webhook_emitter: Option<Arc<crate::webhook::WebhookEmitter>>,
) -> echo_agent::error::Result<SchedulerRunner> {
    let fire_fn = build_fire_fn_with_cancel(
        agent,
        task_runtime_store,
        pool,
        webhook_emitter,
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

    /// Phase 3.1: 非 `[plan]` cron 必须经 `launch_cron_run`(在 store 建 run),
    /// 而非旧 `agent.chat`/`execute_direct`。mock LLM 返回纯文本(无 tool call),
    /// agent 直接作答,`launch_cron_run` 的 `_` 分支自动转 Completed。
    #[tokio::test]
    async fn build_fire_fn_routes_non_plan_cron_to_launch_cron_run() -> Result<(), String> {
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
        // 3.1 后 runtime_store(此处置 Some)接管所有 prompt → launch_cron_run。
        let fire_fn = build_fire_fn(handle, Some(store.clone()), None, None);

        let task = CronTask::new("plain", "*/5 * * * *", "hello world");
        let result = fire_fn(task).await;
        assert!(result.is_ok(), "fire_fn should succeed: {:?}", result.err());

        let completed = store
            .list_runs_in(&[TaskRunStatus::Completed])
            .map_err(|error| error.to_string())?;
        assert_eq!(
            completed.len(),
            1,
            "非-[plan] cron 应经 launch_cron_run 建恰好 1 个 Completed run"
        );
        let completed_run = completed
            .first()
            .ok_or_else(|| "completed cron run is missing".to_string())?;
        assert_eq!(completed_run.workspace_id, "cron-workspace");
        Ok(())
    }

    /// `[plan]` 前缀仍经 launch_cron_run(marker strip,向后兼容)。
    #[tokio::test]
    async fn build_fire_fn_strips_plan_marker_and_routes_to_launch_cron_run() -> Result<(), String>
    {
        let llm = MockLlmClient::new().with_response("ok");
        let agent = ReactAgentBuilder::new()
            .llm_client(Arc::new(llm))
            .system_prompt("test")
            .build()
            .map_err(|error| error.to_string())?;
        let handle = AgentHandle::new(agent);
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let fire_fn = build_fire_fn(handle, Some(store.clone()), None, None);

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
        let fire_fn = build_fire_fn(handle, Some(store.clone()), None, None);

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
}
