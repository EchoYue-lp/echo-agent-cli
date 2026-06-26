//! Cron scheduler adapter — wires the framework's `SchedulerRunner` to the
//! CLI's `AgentHandle` and optional `BackgroundTaskService`.
//!
//! The framework's runner is generic over a `FireFn` callback; this module
//! provides [`build_fire_fn`] which constructs one that submits via
//! `BackgroundTaskService` when available, falling back to direct agent chat.
//!
//! U1c phase-1: when a cron task's prompt starts with `[plan]`, the task is
//! routed through the unified TaskRuntime executor (`launch_cron_run`) instead
//! of the legacy `agent.chat(prompt)` path. The `[plan]` prefix is stripped
//! before the prompt is passed to the executor.

use crate::agent_handle::AgentHandle;
use crate::tasks::background::BackgroundTaskKind;
use crate::tasks::service::BackgroundTaskService;
use crate::tasks::task_runtime::{TaskRuntimeStore, launch_cron_run};
use echo_agent::agent::{Agent, CancellationToken};
use echo_agent::scheduler::{
    CronTask, CronTaskStore, FireFn, SchedulerRunner as FrameworkSchedulerRunner,
};
use std::sync::Arc;

/// The CLI's scheduler runner is the framework's `SchedulerRunner`.
pub type SchedulerRunner = FrameworkSchedulerRunner;

/// Plan-orchestration marker prefix. Cron tasks whose prompt starts with
/// this string are routed through `launch_cron_run` (unified TaskRuntime
/// executor) rather than `agent.chat(prompt)`.
const PLAN_MARKER: &str = "[plan]";

/// Build a `FireFn` that dispatches cron task execution.
///
/// When `task_service` is `Some`, tasks are submitted as tracked background
/// `AgentChat` tasks (gaining progress tracking, retry, and persistence).
/// Otherwise the prompt is sent directly to the agent via `chat()`.
///
/// U1c phase-1: when `task_runtime_store` is `Some` and the prompt starts
/// with `[plan]`, the task is routed through `launch_cron_run` (unified
/// TaskRuntime executor with `ReadOnlyPlanNoShell` profile).
pub fn build_fire_fn(
    agent: AgentHandle,
    task_service: Option<Arc<BackgroundTaskService>>,
    task_runtime_store: Option<Arc<TaskRuntimeStore>>,
) -> FireFn {
    Arc::new(move |task: CronTask| {
        let agent = agent.clone();
        let service = task_service.clone();
        let runtime_store = task_runtime_store.clone();
        Box::pin(async move {
            // ── U1c phase-1: plan orchestration path ──
            if let Some(ref store) = runtime_store {
                if let Some(plan_prompt) = task.prompt.strip_prefix(PLAN_MARKER) {
                    let plan_prompt = plan_prompt.trim();
                    if plan_prompt.is_empty() {
                        return Err(echo_agent::error::ReactError::Other(
                            "[plan] marker found but prompt is empty".into(),
                        ));
                    }
                    let fire_id = uuid::Uuid::new_v4().to_string();
                    let cancel = CancellationToken::new();
                    match launch_cron_run(
                        store.clone(),
                        agent.clone(),
                        &task.id,
                        &fire_id,
                        plan_prompt,
                        cancel,
                    )
                    .await
                    {
                        Ok(()) => Ok(format!("[plan] cron run finished for task {}", task.id)),
                        Err(e) => Err(echo_agent::error::ReactError::Other(format!(
                            "[plan] run failed: {e}"
                        ))),
                    }
                } else if let Some(ref svc) = service {
                    // Legacy path: submit via BackgroundTaskService.
                    let kind = BackgroundTaskKind::AgentChat {
                        prompt: task.prompt.clone(),
                        session_id: None,
                    };
                    let description = format!("Cron [{}]: {}", task.name, task.prompt);
                    match svc
                        .submit(kind, &description, Some("cron".to_string()))
                        .await
                    {
                        Ok(task_id) => Ok(format!("Submitted as background task: {task_id}")),
                        Err(e) => {
                            tracing::warn!(
                                "BackgroundTaskService submit failed ({e}), falling back to direct execution"
                            );
                            execute_direct(&agent, &task).await
                        }
                    }
                } else {
                    execute_direct(&agent, &task).await
                }
            } else if let Some(ref svc) = service {
                // No runtime store — legacy path only.
                let kind = BackgroundTaskKind::AgentChat {
                    prompt: task.prompt.clone(),
                    session_id: None,
                };
                let description = format!("Cron [{}]: {}", task.name, task.prompt);
                match svc
                    .submit(kind, &description, Some("cron".to_string()))
                    .await
                {
                    Ok(task_id) => Ok(format!("Submitted as background task: {task_id}")),
                    Err(e) => {
                        tracing::warn!(
                            "BackgroundTaskService submit failed ({e}), falling back to direct execution"
                        );
                        execute_direct(&agent, &task).await
                    }
                }
            } else {
                execute_direct(&agent, &task).await
            }
        }) as futures::future::BoxFuture<'static, echo_agent::error::Result<String>>
    })
}

/// Direct execution fallback — sends the prompt to the agent synchronously.
async fn execute_direct(agent: &AgentHandle, task: &CronTask) -> echo_agent::error::Result<String> {
    let guard = agent.inner().read().await;
    guard.chat(&task.prompt).await
}

/// Convenience constructor: build a `SchedulerRunner` with the CLI's
/// preferred wiring (agent + optional background task service).
///
/// U1c phase-1: `task_runtime_store` enables the `[plan]` orchestration path.
pub fn new_scheduler_runner(
    store: CronTaskStore,
    cancel: echo_agent::agent::CancellationToken,
    agent: AgentHandle,
    task_service: Option<Arc<BackgroundTaskService>>,
    task_runtime_store: Option<Arc<TaskRuntimeStore>>,
) -> SchedulerRunner {
    let fire_fn = build_fire_fn(agent, task_service, task_runtime_store);
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
        let store = Arc::new(
            TaskRuntimeStore::new_in_memory().expect("in-memory store should init"),
        );

        // task_service=None:Phase 3.1 前会逼非-[plan] prompt 走 execute_direct;
        // 3.1 后 runtime_store(此处置 Some)接管所有 prompt → launch_cron_run。
        let fire_fn = build_fire_fn(handle, None, Some(store.clone()));

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
}
