//! Cron scheduler adapter — wires the framework's `SchedulerRunner` to the
//! CLI's `AgentHandle` and TaskRuntime.
//!
//! The framework's runner is generic over a `FireFn` callback; this module
//! provides [`build_fire_fn`] which constructs one that launches TaskRuntime runs.
//!
//! Phase 3.1: ALL cron tasks route through the unified TaskRuntime executor
//! (`launch_cron_run`). The legacy `[plan]` prefix is stripped for backward
//! compatibility but no longer selects a separate path — simple prompts are
//! answered directly by the agent (auto-Completed when plan_execute isn't
//! called); complex prompts drive plan_create + plan_execute.

use crate::agent_handle::AgentHandle;
use crate::agent_pool::AgentPool;
use crate::tasks::task_runtime::{TaskRuntimeStore, launch_cron_run};
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
/// (auto-Completed); complex prompts drive plan_create + plan_execute.
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
) -> FireFn {
    Arc::new(move |task: CronTask| {
        let fallback_agent = agent.clone();
        let runtime_store = task_runtime_store.clone();
        let pool = pool.clone();
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
            let cancel = CancellationToken::new();

            // Phase C: acquire a per-run pool agent when available. The
            // run-scoped key means each cron run gets its OWN agent (never
            // reused), so the worktree working_dir binding in
            // drive_unattended_run is per-run and can't be clobbered by an
            // overlapping run. Pooled agents don't get ExecutePlanTool at
            // construction (built for subagents, §10.2), so register it here —
            // a cron run's agent plays the primary role (drives plan_create +
            // plan_execute), not a subagent role.
            let run_agent: AgentHandle = match &pool {
                Some(pool) => {
                    let run_key = format!("__cron__:{}:{fire_id}", task.id);
                    let acquired = pool.acquire(&run_key).await.map_err(|e| {
                        echo_agent::error::ReactError::Other(format!(
                            "cron pool acquire failed: {e}"
                        ))
                    })?;
                    register_plan_execute_on_agent(&acquired, store.clone()).await;
                    acquired
                }
                None => fallback_agent.clone(),
            };

            let result =
                launch_cron_run(store.clone(), run_agent, &task.id, &fire_id, prompt, cancel).await;

            // Release the per-run pool entry so it doesn't linger until the
            // 5-min idle evictor reaps it (drive_run_async notably does NOT
            // release — a pre-existing minor leak we don't repeat here).
            if let Some(pool) = &pool {
                pool.release(&format!("__cron__:{}:{fire_id}", task.id))
                    .await;
            }

            match result {
                Ok(run_id) => Ok(format!("cron run {run_id} finished for task {}", task.id)),
                Err(e) => Err(echo_agent::error::ReactError::Other(format!(
                    "cron run failed: {e}"
                ))),
            }
        }) as futures::future::BoxFuture<'static, echo_agent::error::Result<String>>
    })
}

/// Register the `plan_execute` tool on a cron run's pool-acquired agent
/// (Phase C). Mirrors `desktop.rs`'s primary-agent registration. Pooled agents
/// are built without it (subagent stance, §10.2), but a cron run's agent drives
/// plan_create + plan_execute and needs it.
async fn register_plan_execute_on_agent(agent_handle: &AgentHandle, store: Arc<TaskRuntimeStore>) {
    use crate::tasks::task_runtime::ExecutePlanTool;
    let tool = ExecutePlanTool::new(store, agent_handle.clone());
    let added = agent_handle
        .write(|agent| {
            agent.add_tool(Box::new(tool));
            true
        })
        .await;
    if added {
        tracing::debug!("Registered plan_execute tool on cron run agent");
    } else {
        tracing::warn!("Failed to register plan_execute on cron run agent (write lock poisoned)");
    }
}

/// Convenience constructor: build a `SchedulerRunner` with the CLI's
/// preferred wiring (agent + optional background task service).
///
/// U1c phase-1 → Phase 3.1: `task_runtime_store` enables the unified
/// cron→launch_cron_run path (all cron, not just `[plan]`).
/// Phase C: `pool` enables per-run agent isolation (recommended when an
/// `AgentPool` exists); falls back to the shared `agent` when `None`.
pub fn new_scheduler_runner(
    store: CronTaskStore,
    cancel: echo_agent::agent::CancellationToken,
    agent: AgentHandle,
    task_runtime_store: Option<Arc<TaskRuntimeStore>>,
    pool: Option<Arc<AgentPool>>,
) -> SchedulerRunner {
    let fire_fn = build_fire_fn(agent, task_runtime_store, pool);
    SchedulerRunner::new(store, cancel, fire_fn)
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
    async fn build_fire_fn_routes_non_plan_cron_to_launch_cron_run() {
        let llm = MockLlmClient::new().with_response("ok");
        let agent = ReactAgentBuilder::new()
            .llm_client(Arc::new(llm))
            .system_prompt("test")
            .build()
            .expect("test agent should build");
        let handle = AgentHandle::new(agent);
        let store =
            Arc::new(TaskRuntimeStore::new_in_memory().expect("in-memory store should init"));

        // task_service=None:Phase 3.1 前会逼非-[plan] prompt 走 execute_direct;
        // 3.1 后 runtime_store(此处置 Some)接管所有 prompt → launch_cron_run。
        let fire_fn = build_fire_fn(handle, Some(store.clone()), None);

        let task = CronTask::new("plain", "*/5 * * * *", "hello world");
        let result = fire_fn(task).await;
        assert!(result.is_ok(), "fire_fn should succeed: {:?}", result.err());

        let completed = store
            .list_runs_in(&[TaskRunStatus::Completed])
            .expect("list_runs_in should not error");
        assert_eq!(
            completed.len(),
            1,
            "非-[plan] cron 应经 launch_cron_run 建恰好 1 个 Completed run"
        );
    }

    /// `[plan]` 前缀仍经 launch_cron_run(marker strip,向后兼容)。
    #[tokio::test]
    async fn build_fire_fn_strips_plan_marker_and_routes_to_launch_cron_run() {
        let llm = MockLlmClient::new().with_response("ok");
        let agent = ReactAgentBuilder::new()
            .llm_client(Arc::new(llm))
            .system_prompt("test")
            .build()
            .expect("test agent should build");
        let handle = AgentHandle::new(agent);
        let store =
            Arc::new(TaskRuntimeStore::new_in_memory().expect("in-memory store should init"));
        let fire_fn = build_fire_fn(handle, Some(store.clone()), None);

        let task = CronTask::new("plan", "*/5 * * * *", "[plan] do the thing");
        let result = fire_fn(task).await;
        assert!(result.is_ok(), "fire_fn should succeed: {:?}", result.err());

        let completed = store
            .list_runs_in(&[TaskRunStatus::Completed])
            .expect("list_runs_in should not error");
        assert_eq!(
            completed.len(),
            1,
            "[plan] cron 应 strip marker 后建 1 个 Completed run"
        );
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
        let fire_fn = build_fire_fn(handle, Some(store.clone()), None);

        let task = CronTask::new("failing", "*/5 * * * *", "run this");
        let result = fire_fn(task).await;
        assert!(result.is_err(), "failed cron run must not report success");
        let failed = store
            .list_runs_in(&[TaskRunStatus::Failed])
            .map_err(|error| error.to_string())?;
        assert_eq!(failed.len(), 1);
        Ok(())
    }
}
