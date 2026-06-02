//! Cron scheduler adapter — wires the framework's `SchedulerRunner` to the
//! CLI's `AgentHandle` and optional `BackgroundTaskService`.
//!
//! The framework's runner is generic over a `FireFn` callback; this module
//! provides [`build_fire_fn`] which constructs one that submits via
//! `BackgroundTaskService` when available, falling back to direct agent chat.

use crate::agent_handle::AgentHandle;
use crate::tasks::background::BackgroundTaskKind;
use crate::tasks::service::BackgroundTaskService;
use echo_agent::agent::Agent;
use echo_agent::scheduler::{
    CronTask, CronTaskStore, FireFn, SchedulerRunner as FrameworkSchedulerRunner,
};
use std::sync::Arc;

/// The CLI's scheduler runner is the framework's `SchedulerRunner`.
pub type SchedulerRunner = FrameworkSchedulerRunner;

/// Build a `FireFn` that dispatches cron task execution.
///
/// When `task_service` is `Some`, tasks are submitted as tracked background
/// `AgentChat` tasks (gaining progress tracking, retry, and persistence).
/// Otherwise the prompt is sent directly to the agent via `chat()`.
pub fn build_fire_fn(
    agent: AgentHandle,
    task_service: Option<Arc<BackgroundTaskService>>,
) -> FireFn {
    Arc::new(move |task: CronTask| {
        let agent = agent.clone();
        let service = task_service.clone();
        Box::pin(async move {
            if let Some(ref svc) = service {
                // Submit via BackgroundTaskService for full lifecycle tracking.
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
pub fn new_scheduler_runner(
    store: CronTaskStore,
    cancel: echo_agent::agent::CancellationToken,
    agent: AgentHandle,
    task_service: Option<Arc<BackgroundTaskService>>,
) -> SchedulerRunner {
    let fire_fn = build_fire_fn(agent, task_service);
    SchedulerRunner::new(store, cancel, fire_fn)
}
