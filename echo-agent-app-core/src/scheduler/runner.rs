//! Cron scheduler adapter — wires the framework's `SchedulerRunner` to the
//! CLI's `AgentHandle` and TaskRuntime.
//!
//! The framework's runner is generic over an `OccurrenceFireFn` callback; this
//! module provides [`build_occurrence_fire_fn`], which constructs one that
//! launches TaskRuntime runs.
//!
//! All cron tasks route through the unified TaskRuntime executor
//! (`drive_existing_cron_run`). Prompts are passed through unchanged; simple
//! prompts are answered directly by the agent and materialized as a one-task
//! Plan with completion evidence, while complex prompts drive task creation and
//! execution.
//!
//! # Idempotent occurrences
//!
//! The framework may deliver one durable occurrence more than once (owner loss,
//! duplicate tick) and gives every physical attempt a fresh `attempt_id`. Cron
//! delivery therefore never derives the run from the delivery moment:
//! [`OccurrenceIdentity`] hashes only the scheduling slot (workspace,
//! occurrence id, scheduler task, scheduled time — attempt and trigger fields
//! are excluded by construction), and
//! [`TaskRuntimeStore::get_or_create_occurrence_run`] admits exactly one run per
//! slot. A re-delivery reports `created == false` and must not execute the
//! prompt, change status, or append journal facts. The run id is the
//! idempotency key, so a duplicate cannot execute even when the two deliveries
//! race: the second one either reads the committed run or is refused by the
//! run-level admission fence — never by timing.

use crate::agent_handle::AgentHandle;
use crate::agent_pool::AgentPool;
use crate::tasks::task_runtime::executor::{
    ExecError, create_unattended_run, drive_existing_cron_run,
};
use crate::tasks::task_runtime::store::RunDriverFailure;
use crate::tasks::task_runtime::weixin_delivery::WeixinCronDelivery;
use crate::tasks::task_runtime::{
    AttendedMode, DomainProfile, OccurrenceIdentity, OccurrenceRunRequest, RunPlanPolicy, TaskRun,
    TaskRunExecutionProfile, TaskRunStatus, TaskRuntimeStore,
};
use echo_agent::agent::CancellationToken;
use echo_agent::scheduler::{
    CronTask, CronTaskStore, OccurrenceFireFn, SchedulerInvocation,
    SchedulerRunner as FrameworkSchedulerRunner,
};
use std::sync::Arc;

/// The CLI's scheduler runner is the framework's `SchedulerRunner`.
pub type SchedulerRunner = FrameworkSchedulerRunner;

/// Build the occurrence-aware callback that dispatches cron task execution.
///
/// All cron prompts route through the unified supervised TaskRuntime executor
/// in unattended mode. Simple prompts are answered directly by the agent
/// and materialized as a one-task Plan with completion evidence; complex prompts
/// drive task_create + task_execute.
///
/// `AppState` provides the required `TaskRuntimeStore`. When an `AgentPool` is
/// available, each cron run acquires its own Agent so worktree state cannot be
/// shared across overlapping runs; single-Agent embeddings use the supplied
/// fallback Agent.
pub fn build_occurrence_fire_fn(
    agent: AgentHandle,
    task_runtime_store: Option<Arc<TaskRuntimeStore>>,
    pool: Option<Arc<AgentPool>>,
    webhook_emitter: Option<Arc<crate::webhook::WebhookEmitter>>,
    review_integration: Option<Arc<crate::evolution::ReviewIntegration>>,
) -> OccurrenceFireFn {
    build_occurrence_fire_fn_with_weixin(
        agent,
        task_runtime_store,
        pool,
        webhook_emitter,
        review_integration,
        None,
    )
}

/// Optional EKO notification binding; the ordinary scheduler API remains
/// unchanged and unconfigured tasks acquire no external delivery behavior.
pub fn build_occurrence_fire_fn_with_weixin(
    agent: AgentHandle,
    task_runtime_store: Option<Arc<TaskRuntimeStore>>,
    pool: Option<Arc<AgentPool>>,
    webhook_emitter: Option<Arc<crate::webhook::WebhookEmitter>>,
    review_integration: Option<Arc<crate::evolution::ReviewIntegration>>,
    notifications: Option<Arc<dyn WeixinCronDelivery>>,
) -> OccurrenceFireFn {
    build_occurrence_fire_fn_with_cancel(
        agent,
        task_runtime_store,
        pool,
        webhook_emitter,
        review_integration,
        CancellationToken::new(),
        notifications,
    )
}

fn build_occurrence_fire_fn_with_cancel(
    agent: AgentHandle,
    task_runtime_store: Option<Arc<TaskRuntimeStore>>,
    pool: Option<Arc<AgentPool>>,
    webhook_emitter: Option<Arc<crate::webhook::WebhookEmitter>>,
    review_integration: Option<Arc<crate::evolution::ReviewIntegration>>,
    scheduler_cancel: CancellationToken,
    notifications: Option<Arc<dyn WeixinCronDelivery>>,
) -> OccurrenceFireFn {
    Arc::new(move |invocation: SchedulerInvocation| {
        let fallback_agent = agent.clone();
        let runtime_store = task_runtime_store.clone();
        let pool = pool.clone();
        let webhook_emitter = webhook_emitter.clone();
        let review_integration = review_integration.clone();
        let scheduler_cancel = scheduler_cancel.clone();
        let notifications = notifications.clone();
        Box::pin(async move {
            let store = runtime_store.ok_or_else(|| {
                echo_agent::error::ReactError::Other(
                    "TaskRuntimeStore not configured — cron cannot run".into(),
                )
            })?;
            // Capture the task definition carried by the occurrence. It is the
            // definition that was durable when the slot was persisted, not the
            // mutable current definition.
            let task = invocation.task.clone();
            let prompt = task.prompt.clone();
            if prompt.trim().is_empty() {
                return Err(echo_agent::error::ReactError::Other(
                    "cron prompt is empty".into(),
                ));
            }
            // The slot identity is derived from the durable occurrence only;
            // `attempt_id`, `attempt`, and `trigger` are deliberately ignored so
            // every retry of this slot lands on the same run.
            let identity = OccurrenceIdentity::from_scheduler_invocation(
                store.active_workspace_id(),
                &invocation,
            );
            let run_id = identity.run_id();
            // `attempt_id` remains the physical attempt identity for pool keys
            // and logs; it must never influence the run.
            let fire_id = invocation.attempt_id.clone();
            let request = cron_occurrence_request(&identity, &task, &prompt);
            let cancel = scheduler_cancel.child_token();
            // Keep the canonical driver reservation first so shutdown cannot
            // overtake this accepted fire before it is registered with the
            // supervisor. The reservation also refuses a second driver for the
            // same run while the first one is still unsettled.
            let admission = store
                .reserve_run_driver_admission(run_id.clone(), cancel.clone())
                .map_err(|error| {
                    echo_agent::error::ReactError::Other(format!(
                        "cron TaskRun driver admission failed: {error}"
                    ))
                })?;
            // Keep TaskRun creation/execution, memory and pool acquisition, and
            // final projection on one workspace generation.
            let generation_lease = store.lease_active_workspace_generation().map_err(|error| {
                echo_agent::error::ReactError::Other(format!(
                    "task runtime generation admission failed: {error}"
                ))
            })?;
            if store.active_workspace_id() != identity.workspace_id {
                return Err(echo_agent::error::ReactError::Other(
                    "cron workspace changed before generation admission".to_string(),
                ));
            }
            // Existing admissions own their original snapshot, including None.
            // In the create race, only the first RunCreated frame wins.
            let notification = if store
                .find_occurrence_run(&identity)
                .map_err(|error| echo_agent::error::ReactError::Other(error.to_string()))?
                .is_none()
            {
                match notifications.as_ref() {
                    Some(sender) => sender
                        .freeze_target(&invocation, &identity.workspace_id)
                        .await
                        .map_err(echo_agent::error::ReactError::Other)?,
                    None => None,
                }
            } else {
                None
            };
            if notification
                .as_ref()
                .is_some_and(|target| target.definition_id != task.definition_id)
            {
                return Err(echo_agent::error::ReactError::Other(
                    "Weixin notification definition does not match the frozen scheduler occurrence"
                        .to_string(),
                ));
            }
            let mut registration = store
                .register_observed_run_driver::<String>(admission, generation_lease)
                .map_err(|error| {
                    echo_agent::error::ReactError::Other(format!(
                        "cron TaskRun driver registration failed: {error}"
                    ))
                })?;
            // Authoritative admission: one run per occurrence slot. This is the
            // only place a cron run comes into existence. A re-delivery reads the
            // existing run and must not touch it, so it rejects the driver slot
            // instead of starting a driver; `Reject` performs no settlement.
            match store.get_or_create_occurrence_run_with_weixin(&request, notification.as_ref()) {
                Ok((_run, true)) => {
                    // This delivery owns the new run: bind continuation and
                    // Pending → Running through the shared unattended launcher
                    // contract before driving it. The occurrence admission above
                    // already created this exact run, so the helper's create
                    // step only re-validates the immutable run identity; it
                    // cannot create a second run for the slot.
                    registration.mark_preparation_started();
                    if let Err(error) = create_unattended_run(
                        &store,
                        &run_id,
                        "cron",
                        &task.id,
                        identity.occurrence_id.as_str(),
                        &prompt,
                    ) {
                        registration.fail_preparation(error.to_string());
                        return Err(echo_agent::error::ReactError::Other(format!(
                            "cron TaskRun admission failed: {error}"
                        )));
                    }
                }
                Ok((run, false)) => {
                    // Duplicate delivery of the same slot. Rejecting the
                    // reservation releases it without settlement, so the run's
                    // status, journal, and events stay exactly as the first
                    // delivery left them.
                    // A completion-to-intent crash can leave a finished run
                    // without notification facts. Recover only that obligation;
                    // never drive the prompt or read current route config.
                    #[cfg(feature = "channels")]
                    if run.status == TaskRunStatus::Completed {
                        deliver_weixin_without_changing_run(
                            &store,
                            &identity,
                            notifications.as_deref(),
                        )
                        .await;
                    }
                    registration.reject(format!(
                        "cron occurrence {} is already admitted as run {}",
                        identity.occurrence_id, run.run_id
                    ));
                    return duplicate_occurrence_outcome(&run, &identity.occurrence_id, &task.id);
                }
                Err(error) => {
                    // No run was created by this delivery; reject instead of
                    // failing preparation so no settlement debt is fabricated
                    // for a run that does not exist.
                    registration.reject(format!(
                        "cron occurrence {} admission failed: {error}",
                        identity.occurrence_id
                    ));
                    return Err(echo_agent::error::ReactError::Other(format!(
                        "cron TaskRun creation failed: {error}"
                    )));
                }
            }
            let owned_store = store.clone();
            let owned_run_id = run_id.clone();
            let owned_task = task.clone();
            let owned_fire_id = fire_id.clone();
            let owned_prompt = prompt.clone();
            let owned_cancel = cancel.clone();
            #[cfg(feature = "channels")]
            let owned_identity = identity.clone();
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
                    receipt_owner.retain(generation.clone())?;
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
                        receipt_owner.retain(pool.retain_for_supervised_run(run_key, acquired))?;
                        register_task_execute_on_agent(&agent, owned_store.clone()).await;
                        agent
                    }
                    None => fallback_agent,
                };
                if let Some(layer_manager) = layer_manager {
                    run_agent
                        .write(|agent| agent.install_memory_layer_manager(layer_manager))
                        .await
                        .map_err(|error| {
                            format!("cron memory layer installation failed: {error}")
                        })?;
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
                .map_err(cron_execution_error);
                let settled_run_id = result?;
                #[cfg(feature = "channels")]
                deliver_weixin_without_changing_run(
                    &owned_store,
                    &owned_identity,
                    notifications.as_deref(),
                )
                .await;
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
                Ok(Err(error)) => Err(error.into_react_error()),
                Err(error) => Err(echo_agent::error::ReactError::Other(format!(
                    "cron TaskRun result waiter failed: {error}"
                ))),
            }
        }) as futures::future::BoxFuture<'static, echo_agent::error::Result<String>>
    })
}

/// Immutable run request for one cron scheduling slot.
///
/// `conversation_id` is derived from the durable occurrence id, never from the
/// attempt id, so a retry reconstructs the same immutable run identity. The
/// remaining fields mirror the ordinary unattended launcher so both paths share
/// one executor and one completion gate.
fn cron_occurrence_request(
    identity: &OccurrenceIdentity,
    task: &CronTask,
    prompt: &str,
) -> OccurrenceRunRequest {
    OccurrenceRunRequest {
        identity: identity.clone(),
        conversation_id: crate::scheduler::cron_conversation_id(
            task.id.as_str(),
            identity.occurrence_id.as_str(),
        ),
        root_message_id: String::new(),
        domain_profile: DomainProfile::General,
        goal: prompt.to_string(),
        route: "parallel_readonly_delegation".to_string(),
        attended_mode: AttendedMode::Unattended,
        execution_profile: TaskRunExecutionProfile::orchestrated(RunPlanPolicy::AllowDirect),
    }
}

/// Result of a re-delivered occurrence whose run already exists.
///
/// A completed slot settles the callback with the same summary shape the first
/// delivery returned, so the cron task projection is not rewritten with a
/// synthetic message. Every other status stays an error: the framework must keep
/// the occurrence unsettled rather than record success for work its owner has
/// not finished (a fence-rejected, paused, or still-running run must never look
/// settled — R1).
fn duplicate_occurrence_outcome(
    run: &TaskRun,
    occurrence_id: &str,
    scheduler_task_id: &str,
) -> echo_agent::error::Result<String> {
    match run.status {
        TaskRunStatus::Completed => {
            tracing::info!(
                occurrence_id = %occurrence_id,
                run_id = %run.run_id,
                "cron occurrence re-delivered; reusing the completed run without re-executing"
            );
            Ok(format!(
                "cron run {} finished for task {scheduler_task_id}",
                run.run_id
            ))
        }
        TaskRunStatus::Failed => {
            tracing::warn!(
                occurrence_id = %occurrence_id,
                run_id = %run.run_id,
                "cron occurrence re-delivered after failure; refusing to re-execute"
            );
            Err(echo_agent::error::ReactError::Other(format!(
                "cron occurrence {occurrence_id} already failed as run {}; duplicate delivery never re-executes",
                run.run_id
            )))
        }
        TaskRunStatus::Cancelled => {
            tracing::warn!(
                occurrence_id = %occurrence_id,
                run_id = %run.run_id,
                "cron occurrence re-delivered after cancellation; refusing to re-execute"
            );
            Err(echo_agent::error::ReactError::Other(format!(
                "cron occurrence {occurrence_id} was cancelled as run {}; duplicate delivery never re-executes",
                run.run_id
            )))
        }
        TaskRunStatus::Pending | TaskRunStatus::Running | TaskRunStatus::Paused => {
            tracing::warn!(
                occurrence_id = %occurrence_id,
                run_id = %run.run_id,
                status = %run.status.as_str(),
                "cron occurrence re-delivered while its run is non-terminal; leaving the run to its owning driver"
            );
            Err(echo_agent::error::ReactError::Other(format!(
                "cron occurrence {occurrence_id} is already owned by non-terminal run {} ({}); duplicate delivery refused without touching it",
                run.run_id,
                run.status.as_str()
            )))
        }
    }
}

// Add ordinary cron context only after preserving the typed observation.
// The existing supervisor must see unknown ownership before any failure policy.
fn cron_execution_error(error: ExecError) -> RunDriverFailure {
    let ordinary = format!("cron run failed: {error}");
    match RunDriverFailure::from(error) {
        observation @ RunDriverFailure::Observation { .. } => observation,
        RunDriverFailure::Error(_) => RunDriverFailure::Error(ordinary),
    }
}

/// Register `task_execute` on a cron run's pool-acquired Agent. The normal
/// shared pool registry already contains a conversation-aware tool; this fills
/// the gap for standalone runners.
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
/// `task_runtime_store` owns the unified TaskRuntime path for every prompt.
/// `pool` enables per-run Agent isolation and the supplied Agent is the
/// fallback for single-Agent embeddings.
pub async fn new_scheduler_runner(
    store: CronTaskStore,
    cancel: echo_agent::agent::CancellationToken,
    agent: AgentHandle,
    task_runtime_store: Option<Arc<TaskRuntimeStore>>,
    pool: Option<Arc<AgentPool>>,
    webhook_emitter: Option<Arc<crate::webhook::WebhookEmitter>>,
    review_integration: Option<Arc<crate::evolution::ReviewIntegration>>,
) -> echo_agent::error::Result<SchedulerRunner> {
    let fire_fn = build_occurrence_fire_fn_with_cancel(
        agent,
        task_runtime_store,
        pool,
        webhook_emitter,
        review_integration,
        cancel.clone(),
        None,
    );
    // Occurrence-aware constructor: the callback receives the durable
    // occurrence identity (not just the mutable task), which is what makes the
    // TaskRun admission idempotent across retries.
    SchedulerRunner::new_with_occurrence_context(store, cancel, fire_fn).await
}

/// GUI background composition uses the same scheduler cancellation owner.
#[allow(clippy::too_many_arguments)]
pub async fn new_scheduler_runner_with_weixin(
    store: CronTaskStore,
    cancel: CancellationToken,
    agent: AgentHandle,
    task_runtime_store: Option<Arc<TaskRuntimeStore>>,
    pool: Option<Arc<AgentPool>>,
    webhook_emitter: Option<Arc<crate::webhook::WebhookEmitter>>,
    review_integration: Option<Arc<crate::evolution::ReviewIntegration>>,
    notifications: Option<Arc<dyn WeixinCronDelivery>>,
) -> echo_agent::error::Result<SchedulerRunner> {
    let fire_fn = build_occurrence_fire_fn_with_cancel(
        agent,
        task_runtime_store,
        pool,
        webhook_emitter,
        review_integration,
        cancel.clone(),
        notifications,
    );
    SchedulerRunner::new_with_occurrence_context(store, cancel, fire_fn).await
}

#[cfg(feature = "channels")]
async fn deliver_weixin_without_changing_run(
    store: &TaskRuntimeStore,
    identity: &OccurrenceIdentity,
    sender: Option<&dyn WeixinCronDelivery>,
) {
    if let Some(sender) = sender
        && let Err(error) = store.deliver_weixin_completion(identity, sender).await
    {
        // The TaskRun is already terminal. Delivery owns its independent
        // journal outcome; neither scheduler retry nor failure changes it.
        tracing::warn!(run_id = %identity.run_id(), %error, "cron Weixin notification remains independently unsettled");
    }
}

#[cfg(test)]
mod tests {
    use super::*; // AgentHandle, Arc, CronTask, TaskRuntimeStore, CancellationToken, ...
    use crate::tasks::task_runtime::TaskRunStatus;
    use echo_agent::agent::ReactAgentBuilder;
    use echo_agent::testing::MockLlmClient;

    /// First physical delivery of one durable occurrence.
    fn invocation(occurrence_id: &str, task: CronTask) -> SchedulerInvocation {
        SchedulerInvocation::new(
            occurrence_id,
            task,
            chrono::Utc::now(),
            echo_agent::scheduler::SchedulerTrigger::Scheduled,
        )
    }

    #[test]
    fn cron_observation_round_trip_keeps_exact_identity_and_ordinary_error_context() {
        let original = echo_agent::error::ReactError::ExecutionNotSettled {
            run_id: "cron-original-run".into(),
            execution_id: Some("cron-original-execution".into()),
        };
        let typed = cron_execution_error(ExecError::Framework(original));
        assert!(
            matches!(&typed, RunDriverFailure::Observation { run_id, execution_id } if run_id == "cron-original-run" && execution_id.as_deref() == Some("cron-original-execution"))
        );
        assert!(
            matches!(typed.into_react_error(), echo_agent::error::ReactError::ExecutionNotSettled { run_id, execution_id } if run_id == "cron-original-run" && execution_id.as_deref() == Some("cron-original-execution"))
        );
        let ordinary = cron_execution_error(ExecError::Other("provider unavailable".into()));
        assert!(
            matches!(ordinary.into_react_error(), echo_agent::error::ReactError::Other(message) if message == "cron run failed: provider unavailable")
        );
    }

    /// Cron 必须经 supervised TaskRuntime driver（在 store 建 run）。mock LLM
    /// 返回纯文本时，driver 的 direct 分支自动转 Completed。
    #[tokio::test]
    async fn occurrence_fire_fn_routes_non_plan_cron_to_supervised_run() -> Result<(), String> {
        let llm = MockLlmClient::new().with_response("ok");
        let agent = ReactAgentBuilder::new()
            .llm_client(Arc::new(llm))
            .system_prompt("test")
            .build()
            .map_err(|error| error.to_string())?;
        let handle = AgentHandle::new(agent);
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let _resource_scope = store.test_resource_scope();
        let workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
        store
            .rebind_shadow_root(workspace.path().join("tasks"), "cron-workspace")
            .await
            .map_err(|error| error.to_string())?;

        // runtime_store 接管所有 prompt，并统一进入 supervised driver。
        let fire_fn = build_occurrence_fire_fn(handle, Some(store.clone()), None, None, None);

        let task = CronTask::new("plain", "*/5 * * * *", "hello world");
        let result = fire_fn(invocation("cron-scheduled:plain", task)).await;
        assert!(result.is_ok(), "fire_fn should succeed: {:?}", result.err());

        let completed = store
            .list_runs_in(&[TaskRunStatus::Completed])
            .map_err(|error| error.to_string())?;
        assert_eq!(
            completed.len(),
            1,
            "cron 应经 supervised driver 建恰好 1 个 Completed run"
        );
        let completed_run = completed
            .first()
            .ok_or_else(|| "completed cron run is missing".to_string())?;
        assert_eq!(completed_run.workspace_id, "cron-workspace");
        Ok(())
    }

    #[tokio::test]
    async fn occurrence_fire_fn_preserves_plan_marker_as_prompt_text() -> Result<(), String> {
        let llm = MockLlmClient::new().with_response("ok");
        let agent = ReactAgentBuilder::new()
            .llm_client(Arc::new(llm))
            .system_prompt("test")
            .build()
            .map_err(|error| error.to_string())?;
        let handle = AgentHandle::new(agent);
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let _resource_scope = store.test_resource_scope();
        let fire_fn = build_occurrence_fire_fn(handle, Some(store.clone()), None, None, None);

        let task = CronTask::new("plan", "*/5 * * * *", "[plan] do the thing");
        let result = fire_fn(invocation("cron-scheduled:plan", task)).await;
        assert!(result.is_ok(), "fire_fn should succeed: {:?}", result.err());

        let completed = store
            .list_runs_in(&[TaskRunStatus::Completed])
            .map_err(|error| error.to_string())?;
        assert_eq!(completed.len(), 1, "cron 应建立 1 个 Completed run");
        let completed_run = completed
            .first()
            .ok_or_else(|| "completed cron run is missing".to_string())?;
        assert_eq!(completed_run.goal, "[plan] do the thing");
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
        let _resource_scope = store.test_resource_scope();
        let fire_fn = build_occurrence_fire_fn(handle, Some(store.clone()), None, None, None);

        let task = CronTask::new("failing", "*/5 * * * *", "run this");
        let result = fire_fn(invocation("cron-scheduled:failing", task)).await;
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
        let _resource_scope = store.test_resource_scope();
        let scheduler_cancel = CancellationToken::new();
        scheduler_cancel.cancel();
        let fire_fn = build_occurrence_fire_fn_with_cancel(
            AgentHandle::new(agent),
            Some(store.clone()),
            None,
            None,
            None,
            scheduler_cancel,
            None,
        );

        let result = fire_fn(invocation(
            "cron-scheduled:cancelled",
            CronTask::new("cancelled", "*/5 * * * *", "do work"),
        ))
        .await;

        assert!(result.is_err());
        let cancelled = store
            .list_runs_in(&[TaskRunStatus::Cancelled])
            .map_err(|error| error.to_string())?;
        assert_eq!(cancelled.len(), 1);
        Ok(())
    }

    // `worker_threads` is a fixed Tokio attribute, not an EKO execution role.
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
        let _resource_scope = store.test_resource_scope();
        let (reservation_started, release_reservation) =
            store.park_next_run_driver_admission_for_test()?;
        let fire_fn = build_occurrence_fire_fn(
            AgentHandle::new(agent),
            Some(store.clone()),
            None,
            None,
            None,
        );
        let fire = tokio::spawn(async move {
            fire_fn(invocation(
                "cron-scheduled:shutdown-race",
                CronTask::new("shutdown-race", "*/5 * * * *", "do work"),
            ))
            .await
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

        // Both branches do durable settlement after the race is released; parallel suites can
        // delay their scheduling without violating the already-observed admission ordering.
        let (fire_result, shutdown_result) =
            tokio::time::timeout(std::time::Duration::from_secs(10), async move {
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

    /// 同一 occurrence 重投只执行一次：第二次投递（新的 attempt_id、不同
    /// trigger）必须复用已提交的 run，不得再次调用模型、不得追加 journal
    /// 事实、不得写事件。幂等性来自 run_id 的构造（slot 身份哈希），不是
    /// 投递时序上的锁。
    #[tokio::test]
    async fn duplicate_occurrence_delivery_executes_exactly_once() -> Result<(), String> {
        let llm = Arc::new(MockLlmClient::new().with_response("ok"));
        let agent = ReactAgentBuilder::new()
            .llm_client(llm.clone())
            .system_prompt("test")
            .build()
            .map_err(|error| error.to_string())?;
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let _resource_scope = store.test_resource_scope();
        let workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
        store
            .rebind_shadow_root(workspace.path().join("tasks"), "cron-workspace")
            .await
            .map_err(|error| error.to_string())?;
        let fire_fn = build_occurrence_fire_fn(
            AgentHandle::new(agent),
            Some(store.clone()),
            None,
            None,
            None,
        );

        let task = CronTask::new("duplicate", "*/5 * * * *", "hello duplicate");
        let scheduled_at = chrono::Utc::now();
        let first = SchedulerInvocation::new(
            "cron-scheduled:duplicate",
            task.clone(),
            scheduled_at,
            echo_agent::scheduler::SchedulerTrigger::Scheduled,
        );
        fire_fn(first.clone())
            .await
            .map_err(|error| format!("first delivery failed: {error}"))?;

        let completed = store
            .list_runs_in(&[TaskRunStatus::Completed])
            .map_err(|error| error.to_string())?;
        assert_eq!(completed.len(), 1, "first delivery must create one run");
        let run_id = completed
            .first()
            .map(|run| run.run_id.clone())
            .ok_or_else(|| "completed run missing".to_string())?;
        assert!(
            run_id.starts_with("occ-"),
            "cron run id must be the occurrence digest, not a delivery uuid: {run_id}"
        );
        let before = store
            .get_run_state(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "run state missing".to_string())?;
        let events_before = store
            .list_events(&run_id, 0)
            .map_err(|error| error.to_string())?
            .len();
        let calls_before = llm.call_count();
        assert!(calls_before > 0, "the first delivery must reach the model");

        // Retry the same slot with a fresh attempt identity and a manual trigger;
        // neither may influence the run.
        let retry = SchedulerInvocation::new(
            "cron-scheduled:duplicate",
            task,
            scheduled_at,
            echo_agent::scheduler::SchedulerTrigger::Manual,
        );
        assert_ne!(retry.attempt_id, first.attempt_id);
        let delivered = fire_fn(retry)
            .await
            .map_err(|error| format!("completed slot must settle without re-executing: {error}"))?;
        assert!(delivered.contains(&run_id));

        assert_eq!(
            llm.call_count(),
            calls_before,
            "duplicate delivery must not execute the prompt again"
        );
        let after = store
            .get_run_state(&run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "run state missing after re-delivery".to_string())?;
        assert_eq!(after.run.status, TaskRunStatus::Completed);
        assert_eq!(
            after.journal_sequence, before.journal_sequence,
            "duplicate delivery must not append journal facts"
        );
        assert_eq!(
            store
                .list_events(&run_id, 0)
                .map_err(|error| error.to_string())?
                .len(),
            events_before,
            "duplicate delivery must not write runtime events"
        );
        let all = store
            .list_runs_in(&[
                TaskRunStatus::Pending,
                TaskRunStatus::Running,
                TaskRunStatus::Paused,
                TaskRunStatus::Completed,
                TaskRunStatus::Failed,
                TaskRunStatus::Cancelled,
            ])
            .map_err(|error| error.to_string())?;
        assert_eq!(all.len(), 1, "one occurrence slot owns exactly one run");
        // The canonical supervisor releases a rejected admission on its own
        // settlement task, after the callback returned. Wait for that
        // bookkeeping only — the execution/non-mutation assertions above are
        // already deterministic.
        let mut leaked = (usize::MAX, usize::MAX);
        for _ in 0..200 {
            leaked = (
                store
                    .active_run_driver_count()
                    .map_err(|error| error.to_string())?,
                store
                    .active_run_driver_receipt_count()
                    .map_err(|error| error.to_string())?,
            );
            if leaked == (0, 0) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(
            leaked,
            (0, 0),
            "the rejected duplicate admission must not leak a driver reservation"
        );
        Ok(())
    }

    /// 非终态 run（Paused）被 fence 保护：重投不得把它重新拉起、不得改状态、
    /// 不得写事件，并且必须返回错误，让 framework 不把该 occurrence 记为
    /// 已结算。
    #[tokio::test]
    async fn duplicate_occurrence_delivery_never_restarts_a_nonterminal_run() -> Result<(), String>
    {
        let llm = Arc::new(MockLlmClient::new().with_response("must not run"));
        let agent = ReactAgentBuilder::new()
            .llm_client(llm.clone())
            .system_prompt("test")
            .build()
            .map_err(|error| error.to_string())?;
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let _resource_scope = store.test_resource_scope();
        let workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
        store
            .rebind_shadow_root(workspace.path().join("tasks"), "cron-workspace")
            .await
            .map_err(|error| error.to_string())?;

        let task = CronTask::new("paused", "*/5 * * * *", "paused slot");
        let scheduled_at = chrono::Utc::now();
        let invocation = SchedulerInvocation::new(
            "cron-scheduled:paused",
            task.clone(),
            scheduled_at,
            echo_agent::scheduler::SchedulerTrigger::Scheduled,
        );
        // Seed the slot through the same admission the callback uses.
        let identity =
            OccurrenceIdentity::from_scheduler_invocation(store.active_workspace_id(), &invocation);
        let request = cron_occurrence_request(&identity, &task, task.prompt.as_str());
        let (run, created) = store
            .get_or_create_occurrence_run(&request)
            .map_err(|error| error.to_string())?;
        assert!(created);
        store
            .transition_run(&run.run_id, TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        assert!(
            store
                .request_pause(&run.run_id)
                .map_err(|error| error.to_string())?
        );
        let before = store
            .get_run_state(&run.run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "run state missing".to_string())?;
        assert_eq!(before.run.status, TaskRunStatus::Paused);
        let events_before = store
            .list_events(&run.run_id, 0)
            .map_err(|error| error.to_string())?
            .len();

        let fire_fn = build_occurrence_fire_fn(
            AgentHandle::new(agent),
            Some(store.clone()),
            None,
            None,
            None,
        );
        let retry = SchedulerInvocation::new(
            "cron-scheduled:paused",
            task,
            scheduled_at,
            echo_agent::scheduler::SchedulerTrigger::Scheduled,
        );
        let result = fire_fn(retry).await;
        assert!(
            result.is_err(),
            "a non-terminal occurrence must stay unsettled, not report success"
        );
        let error = match result {
            Ok(_) => return Err("non-terminal occurrence reported success".to_string()),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("non-terminal"),
            "error must identify the non-terminal owner: {error}"
        );

        assert_eq!(
            llm.call_count(),
            0,
            "no model call for a duplicate delivery"
        );
        let after = store
            .get_run_state(&run.run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "run state missing after re-delivery".to_string())?;
        assert_eq!(after.run.status, TaskRunStatus::Paused);
        assert_eq!(
            after.journal_sequence, before.journal_sequence,
            "duplicate delivery must not append journal facts"
        );
        assert_eq!(
            store
                .list_events(&run.run_id, 0)
                .map_err(|error| error.to_string())?
                .len(),
            events_before
        );
        Ok(())
    }

    #[tokio::test]
    async fn real_scheduler_fire_fn_keeps_task_unknown_and_original_occurrence_nonterminal()
    -> anyhow::Result<()> {
        use echo_agent::delivery::{DeliveryEvent, DeliveryOutcome};
        use echo_agent::error::ReactError;
        use echo_agent::llm::{ChatChunk, ChatRequest, ChatResponse, LlmClient};
        use futures::future::BoxFuture;
        use futures::stream::BoxStream;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use tokio::sync::Notify;

        // The gate is in the real Agent's provider call, not around FireFn.
        // The backend is test-owned async work; it is not an OS-process proof.
        struct ProviderGate {
            store: std::sync::Weak<TaskRuntimeStore>,
            unknown: bool,
            entered: Notify,
            output: Notify,
            physical_release: Arc<Notify>,
            backend: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
            capacity: Arc<tokio::sync::Semaphore>,
            physical_closed: Arc<AtomicBool>,
            calls: AtomicUsize,
            identity: std::sync::Mutex<Option<(String, Option<String>)>>,
        }

        impl ProviderGate {
            async fn admitted_identity(
                &self,
            ) -> echo_agent::error::Result<(String, Option<String>)> {
                if self.calls.fetch_add(1, Ordering::SeqCst) != 0 {
                    return Err(ReactError::Other("unexpected second provider call".into()));
                }
                let store = self
                    .store
                    .upgrade()
                    .ok_or_else(|| ReactError::Other("original store lost".into()))?;
                let runs = store
                    .list_runs_in(&[
                        TaskRunStatus::Pending,
                        TaskRunStatus::Running,
                        TaskRunStatus::Paused,
                        TaskRunStatus::Completed,
                        TaskRunStatus::Failed,
                        TaskRunStatus::Cancelled,
                    ])
                    .map_err(|error| ReactError::Other(error.to_string()))?;
                if runs.len() != 1 {
                    return Err(ReactError::Other(
                        "provider did not receive exactly one original TaskRun".into(),
                    ));
                }
                let run = runs
                    .first()
                    .ok_or_else(|| ReactError::Other("original TaskRun absent".into()))?;
                let snapshot = store
                    .get_run_state(&run.run_id)
                    .map_err(|error| ReactError::Other(error.to_string()))?
                    .ok_or_else(|| ReactError::Other("original Run state absent".into()))?;
                if snapshot.event_index.physical_owner_intents.len() != 1 {
                    return Err(ReactError::Other(
                        "provider started without its original durable intent".into(),
                    ));
                }
                let intent = snapshot
                    .event_index
                    .physical_owner_intents
                    .first()
                    .ok_or_else(|| ReactError::Other("original intent absent".into()))?;
                if intent.run_id != run.run_id || intent.physical_terminal_observed {
                    return Err(ReactError::Other(
                        "provider intent identity/finality mismatch".into(),
                    ));
                }
                let identity = (intent.run_id.clone(), intent.execution_id.clone());
                *self
                    .identity
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = Some(identity.clone());
                let permit = self
                    .capacity
                    .clone()
                    .acquire_owned()
                    .await
                    .map_err(|error| ReactError::Other(error.to_string()))?;
                let release = self.physical_release.clone();
                let closed = self.physical_closed.clone();
                let (started, ready) = tokio::sync::oneshot::channel();
                let backend = tokio::spawn(async move {
                    let _original_permit = permit;
                    let _ = started.send(());
                    release.notified().await;
                    closed.store(true, Ordering::SeqCst);
                });
                *self
                    .backend
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = Some(backend);
                ready
                    .await
                    .map_err(|error| ReactError::Other(error.to_string()))?;
                self.entered.notify_one();
                self.output.notified().await;
                Ok(identity)
            }

            fn response(&self, identity: (String, Option<String>)) -> MockLlmClient {
                if self.unknown {
                    MockLlmClient::new().with_error(ReactError::ExecutionNotSettled {
                        run_id: identity.0,
                        execution_id: identity.1,
                    })
                } else {
                    MockLlmClient::new().with_response("normal cron physical result")
                }
            }

            async fn finish_backend(&self) -> anyhow::Result<()> {
                self.physical_release.notify_one();
                let backend = self
                    .backend
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take()
                    .ok_or_else(|| anyhow::anyhow!("original provider backend absent"))?;
                tokio::time::timeout(std::time::Duration::from_secs(10), backend).await??;
                Ok(())
            }
        }

        impl LlmClient for ProviderGate {
            fn chat(
                &self,
                request: ChatRequest,
            ) -> BoxFuture<'_, echo_agent::error::Result<ChatResponse>> {
                Box::pin(async move {
                    let identity = self.admitted_identity().await?;
                    self.response(identity).chat(request).await
                })
            }
            fn chat_stream(
                &self,
                request: ChatRequest,
            ) -> BoxFuture<
                '_,
                echo_agent::error::Result<BoxStream<'static, echo_agent::error::Result<ChatChunk>>>,
            > {
                Box::pin(async move {
                    let identity = self.admitted_identity().await?;
                    self.response(identity).chat_stream(request).await
                })
            }
            fn model_name(&self) -> &str {
                "mock-model"
            }
        }

        struct ReleaseOnError(Arc<ProviderGate>);
        impl Drop for ReleaseOnError {
            fn drop(&mut self) {
                self.0.output.notify_one();
                self.0.physical_release.notify_one();
            }
        }

        type Event = DeliveryEvent<String, serde_json::Value>;
        fn original_events(path: &std::path::Path) -> anyhow::Result<Vec<Event>> {
            // Read only the real writer's batch records using its public Event
            // DTO. Opening a second FileEventJournal with a different payload
            // TypeId would replace the original authority and is not allowed.
            let mut events = Vec::new();
            for line in std::fs::read_to_string(path)?.lines() {
                let frame: serde_json::Value = serde_json::from_str(line)?;
                let records = frame
                    .get("records")
                    .and_then(serde_json::Value::as_array)
                    .ok_or_else(|| anyhow::anyhow!("original journal batch records absent"))?;
                for record in records {
                    let event = record
                        .get("event")
                        .ok_or_else(|| anyhow::anyhow!("original record event absent"))?;
                    events.push(serde_json::from_value(event.clone())?);
                }
            }
            Ok(events)
        }

        for unknown in [false, true] {
            let directory = tempfile::tempdir()?;
            let store = Arc::new(TaskRuntimeStore::new_in_memory()?);
            let _original_resource_root = store.test_resource_scope();
            store
                .rebind_shadow_root(directory.path().join("TaskRun"), "cron-real-chain")
                .await?;
            let gate = Arc::new(ProviderGate {
                store: Arc::downgrade(&store),
                unknown,
                entered: Notify::new(),
                output: Notify::new(),
                physical_release: Arc::new(Notify::new()),
                backend: std::sync::Mutex::new(None),
                capacity: Arc::new(tokio::sync::Semaphore::new(1)),
                physical_closed: Arc::new(AtomicBool::new(false)),
                calls: AtomicUsize::new(0),
                identity: std::sync::Mutex::new(None),
            });
            let _release_on_error = ReleaseOnError(gate.clone());
            let agent = AgentHandle::new(
                ReactAgentBuilder::new()
                    .llm_client(gate.clone())
                    .system_prompt("test")
                    .build()?,
            );
            let cancel = CancellationToken::new();
            let definition = directory.path().join("tasks.json");
            // CronTaskStore::occurrence_paths appends this suffix to the full
            // selected definition filename; the test does not invent a store.
            let journal = directory.path().join("tasks.json.occurrences.jsonl");
            let runner = Arc::new(
                new_scheduler_runner(
                    CronTaskStore::new().with_path(definition),
                    cancel,
                    agent,
                    Some(store.clone()),
                    None,
                    None,
                    None,
                )
                .await?,
            );
            let task = CronTask::new("real consumer chain", "* * * * *", "read-only answer");
            let task_id = task.id.clone();
            runner.add_task(task).await?;
            let original_runner = runner.clone();
            let original_task_id = task_id.clone();
            let manual =
                tokio::spawn(async move { original_runner.run_once(&original_task_id).await });
            tokio::time::timeout(std::time::Duration::from_secs(10), gate.entered.notified())
                .await?;
            let provider_identity = gate
                .identity
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone()
                .ok_or_else(|| anyhow::anyhow!("actual provider identity absent"))?;
            let before = store
                .get_run_state(&provider_identity.0)?
                .ok_or_else(|| anyhow::anyhow!("original run snapshot absent"))?;
            let before_run = store
                .get_run(&provider_identity.0)?
                .ok_or_else(|| anyhow::anyhow!("original run absent"))?;
            let entered_events = original_events(&journal)?;
            let claims: Vec<_> = entered_events
                .iter()
                .filter_map(|event| match event {
                    Event::Claimed {
                        message_id,
                        attempt_id,
                        ..
                    } => Some((message_id.clone(), attempt_id.clone())),
                    _ => None,
                })
                .collect();
            let original_claim = claims
                .first()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("actual scheduler claim absent"))?;
            let held = !manual.is_finished()
                && gate.capacity.available_permits() == 0
                && !gate.physical_closed.load(Ordering::SeqCst);
            if !unknown {
                gate.finish_backend().await?;
            }
            gate.output.notify_one();
            let result = tokio::time::timeout(std::time::Duration::from_secs(10), manual).await??;
            let after_run = store
                .get_run(&provider_identity.0)?
                .ok_or_else(|| anyhow::anyhow!("original run disappeared"))?;
            let after = store
                .get_run_state(&provider_identity.0)?
                .ok_or_else(|| anyhow::anyhow!("original state disappeared"))?;
            let task_events = store.list_events(&provider_identity.0, 0)?;
            let same_run_rejected = unknown
                && store
                    .reserve_run_driver_admission(
                        provider_identity.0.clone(),
                        CancellationToken::new(),
                    )
                    .is_err();
            let mut handle = runner.clone().spawn();
            let shutdown =
                tokio::time::timeout(std::time::Duration::from_secs(10), handle.shutdown()).await?;
            let repeated =
                tokio::time::timeout(std::time::Duration::from_secs(10), handle.join()).await?;
            let final_events = original_events(&journal)?;
            let final_tasks = runner.list_tasks().await;
            let final_task = final_tasks
                .iter()
                .find(|task| task.id == task_id)
                .ok_or_else(|| anyhow::anyhow!("original cron definition disappeared"))?;
            let callback_held_after_observation = gate.capacity.available_permits() == 0
                && !gate.physical_closed.load(Ordering::SeqCst);
            if unknown {
                gate.finish_backend().await?;
            }
            let original_backend_joined = gate.capacity.available_permits() == 1
                && gate.physical_closed.load(Ordering::SeqCst);
            // No store/occurrence terminal acknowledgement is fabricated from
            // the test's physical join. Unknown with no late source stays so.
            assert!(held && original_backend_joined);
            assert_eq!(gate.calls.load(Ordering::SeqCst), 1);
            assert_eq!(claims.len(), 1);
            let final_claims: Vec<_> = final_events
                .iter()
                .filter_map(|event| match event {
                    Event::Claimed {
                        message_id,
                        attempt_id,
                        ..
                    } => Some((message_id.clone(), attempt_id.clone())),
                    _ => None,
                })
                .collect();
            assert_eq!(final_claims, claims);
            assert_eq!(final_events.iter().filter(|event| matches!(event, Event::EffectStarted { message_id, attempt_id, .. } if message_id == &original_claim.0 && attempt_id == &original_claim.1)).count(), 1);
            let all_runs = store.list_runs_in(&[
                TaskRunStatus::Pending,
                TaskRunStatus::Running,
                TaskRunStatus::Paused,
                TaskRunStatus::Completed,
                TaskRunStatus::Failed,
                TaskRunStatus::Cancelled,
            ])?;
            assert_eq!(all_runs.len(), 1);
            if unknown {
                assert!(
                    matches!(result, Err(ReactError::ExecutionNotSettled { run_id, execution_id }) if run_id == provider_identity.0 && execution_id == provider_identity.1)
                );
                assert_eq!(after_run.status, before_run.status);
                assert!(!matches!(
                    after_run.status,
                    TaskRunStatus::Completed | TaskRunStatus::Failed | TaskRunStatus::Cancelled
                ));
                assert_eq!(
                    after.event_index.physical_owner_intents,
                    before.event_index.physical_owner_intents
                );
                assert!(store.has_unsettled_driver_resources() && same_run_rejected);
                assert!(callback_held_after_observation);
                assert!(!task_events.iter().any(|event| matches!(
                    event.event_type,
                    crate::tasks::task_runtime::RuntimeEventKind::RunCompleted
                        | crate::tasks::task_runtime::RuntimeEventKind::RunFailed
                        | crate::tasks::task_runtime::RuntimeEventKind::RunCancelled
                )));
                let exact_occurrence = |error: &echo_agent::error::Result<()>| {
                    matches!(error,
                    Err(ReactError::ExecutionNotSettled { run_id, execution_id: Some(attempt) })
                    if run_id == &original_claim.0 && attempt == &original_claim.1)
                };
                assert!(exact_occurrence(&shutdown) && exact_occurrence(&repeated));
                assert!(final_task.last_result.is_none() && final_task.last_run_at.is_none());
                assert!(
                    !final_events
                        .iter()
                        .any(|event| matches!(event, Event::TurnSettled { .. }))
                );
            } else {
                let answer = result?;
                assert_eq!(after_run.status, TaskRunStatus::Completed);
                assert!(after.event_index.physical_owner_intents.is_empty());
                assert!(!store.has_unsettled_driver_resources());
                assert_eq!(final_task.last_result.as_deref(), Some(answer.as_str()));
                assert!(final_task.last_run_at.is_some());
                assert!(shutdown.is_ok() && repeated.is_ok());
                assert_eq!(
                    final_events
                        .iter()
                        .filter(|event| matches!(event, Event::TurnSettled {
                    message_id, attempt_id, outcome: DeliveryOutcome::Completed, ..
                } if message_id == &original_claim.0 && attempt_id == &original_claim.1))
                        .count(),
                    1
                );
            }
            // Spawning and immediately cancelling the loop is not claimed to
            // exercise recovery/replay; that remains the framework's own proof.
            // This composition proves the existing claim and callback count
            // survive real shutdown/re-observation without false settlement.
        }
        Ok(())
    }

    #[cfg(feature = "channels")]
    struct RecordingWeixin {
        freeze_calls: std::sync::atomic::AtomicUsize,
        sends: std::sync::Mutex<Vec<(String, String)>>,
    }

    #[cfg(feature = "channels")]
    impl WeixinCronDelivery for RecordingWeixin {
        fn freeze_target<'a>(
            &'a self,
            invocation: &'a SchedulerInvocation,
            workspace_id: &'a str,
        ) -> futures::future::BoxFuture<
            'a,
            Result<
                Option<crate::tasks::task_runtime::weixin_delivery::WeixinDeliveryTarget>,
                String,
            >,
        > {
            Box::pin(async move {
                self.freeze_calls
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(Some(
                    crate::tasks::task_runtime::weixin_delivery::WeixinDeliveryTarget {
                        account_id: "frozen-account".to_string(),
                        credential_generation: "original-generation".to_string(),
                        peer_id: "frozen-peer".to_string(),
                        context_ref: "original-context-reference".to_string(),
                        workspace_id: workspace_id.to_string(),
                        channel_conversation: "original-direct-conversation".to_string(),
                        definition_id: invocation.task.definition_id.clone(),
                    },
                ))
            })
        }

        fn send<'a>(
            &'a self,
            target: &'a crate::tasks::task_runtime::weixin_delivery::WeixinDeliveryTarget,
            message_id: &'a str,
            text: &'a str,
        ) -> futures::future::BoxFuture<
            'a,
            Result<echo_agent::channels::weixin::WeixinSendOutcome, String>,
        > {
            Box::pin(async move {
                if target.account_id != "frozen-account"
                    || target.credential_generation != "original-generation"
                {
                    return Err("target drift".to_string());
                }
                self.sends
                    .lock()
                    .map_err(|_| "test send observer poisoned".to_string())?
                    .push((message_id.to_string(), text.to_string()));
                Ok(echo_agent::channels::weixin::WeixinSendOutcome::Accepted {
                    client_id: message_id.to_string(),
                    message_id: Some("fixture-server-message".to_string()),
                })
            })
        }
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn cron_completion_delivers_full_original_answer_once_across_duplicate_occurrence()
    -> Result<(), String> {
        let full_answer = format!("{}FULL-ANSWER-END", "中文🌱".repeat(600));
        let agent = ReactAgentBuilder::new()
            .llm_client(Arc::new(
                MockLlmClient::new().with_response(full_answer.clone()),
            ))
            .system_prompt("test")
            .build()
            .map_err(|error| error.to_string())?;
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let _resource_scope = store.test_resource_scope();
        let sender = Arc::new(RecordingWeixin {
            freeze_calls: std::sync::atomic::AtomicUsize::new(0),
            sends: std::sync::Mutex::new(Vec::new()),
        });
        let fire = build_occurrence_fire_fn_with_weixin(
            AgentHandle::new(agent),
            Some(store.clone()),
            None,
            None,
            None,
            Some(sender.clone()),
        );
        let mut task = CronTask::new("notify", "*/5 * * * *", "answer fully");
        task.definition_id = "notification-positive-definition".to_string();
        let invocation = invocation("cron-scheduled:weixin-one", task);
        let identity =
            OccurrenceIdentity::from_scheduler_invocation(store.active_workspace_id(), &invocation);
        fire(invocation.clone())
            .await
            .map_err(|error| error.to_string())?;
        fire(invocation).await.map_err(|error| error.to_string())?;
        assert_eq!(
            sender
                .freeze_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        let sends = sender
            .sends
            .lock()
            .map_err(|_| "test send observer poisoned".to_string())?;
        assert_eq!(
            sends.len(),
            1,
            "duplicate occurrence must not send or execute again"
        );
        let (_, text) = sends.first().ok_or("original send missing")?;
        assert!(
            text.contains(&full_answer),
            "bounded summary/finished label cannot replace original direct answer"
        );
        let receipt = store
            .weixin_notification_status(&identity.run_id())
            .map_err(|error| error.to_string())?
            .ok_or("notification receipt missing")?;
        assert_eq!(
            receipt.outcome,
            Some(echo_agent::delivery::DeliveryOutcome::Completed)
        );
        assert_eq!(
            receipt.reply_message_id.as_deref(),
            Some("fixture-server-message")
        );
        assert_eq!(
            store
                .get_run(&identity.run_id())
                .map_err(|error| error.to_string())?
                .ok_or("original run missing")?
                .status,
            TaskRunStatus::Completed
        );
        Ok(())
    }

    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn notification_remains_unsent_for_each_uncompleted_task_run() -> anyhow::Result<()> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        for status in [
            TaskRunStatus::Pending,
            TaskRunStatus::Running,
            TaskRunStatus::Paused,
            TaskRunStatus::Failed,
            TaskRunStatus::Cancelled,
        ] {
            let store = std::sync::Arc::new(TaskRuntimeStore::new_in_memory()?);
            let _resources = store.test_resource_scope();
            let sender = RecordingWeixin {
                freeze_calls: AtomicUsize::new(0),
                sends: std::sync::Mutex::new(Vec::new()),
            };
            let mut task = CronTask::new(
                "native completion boundary",
                "*/5 * * * *",
                "original prompt",
            );
            task.definition_id = "notification-status-definition".to_string();
            let invocation = invocation(&format!("native-not-completed-{}", status.as_str()), task);
            let identity = OccurrenceIdentity::from_scheduler_invocation(
                store.active_workspace_id(),
                &invocation,
            );
            let target = sender
                .freeze_target(&invocation, &identity.workspace_id)
                .await
                .map_err(anyhow::Error::msg)?
                .ok_or_else(|| anyhow::anyhow!("frozen target missing"))?;
            let request =
                cron_occurrence_request(&identity, &invocation.task, &invocation.task.prompt);
            let (_, created) =
                store.get_or_create_occurrence_run_with_weixin(&request, Some(&target))?;
            assert!(created);
            let run_id = identity.run_id();
            if status != TaskRunStatus::Pending {
                store.transition_run(&run_id, TaskRunStatus::Running)?;
                if status == TaskRunStatus::Paused {
                    assert!(store.request_pause(&run_id)?);
                } else if status != TaskRunStatus::Running {
                    store.transition_run(&run_id, status)?;
                }
            }
            let before = store
                .get_run_state(&run_id)?
                .ok_or_else(|| anyhow::anyhow!("original run missing"))?;
            assert!(
                store
                    .deliver_weixin_completion(&identity, &sender)
                    .await
                    .is_err()
            );
            let after = store
                .get_run_state(&run_id)?
                .ok_or_else(|| anyhow::anyhow!("original run lost"))?;
            assert_eq!(after.run.status, status);
            assert_eq!(before.journal_sequence, after.journal_sequence);
            assert_eq!(store.frozen_weixin_notification(&run_id)?, Some(target));
            assert!(store.weixin_notification_status(&run_id)?.is_none());
            assert!(
                sender
                    .sends
                    .lock()
                    .map_err(|_| anyhow::anyhow!("send observer poisoned"))?
                    .is_empty()
            );
            assert_eq!(sender.freeze_calls.load(Ordering::SeqCst), 1);
        }
        Ok(())
    }

    #[cfg(feature = "channels")]
    struct RestartNotificationSender {
        unknown: bool,
        freeze_calls: std::sync::atomic::AtomicUsize,
        send_calls: std::sync::atomic::AtomicUsize,
    }

    #[cfg(feature = "channels")]
    impl WeixinCronDelivery for RestartNotificationSender {
        fn freeze_target<'a>(
            &'a self,
            invocation: &'a SchedulerInvocation,
            workspace_id: &'a str,
        ) -> futures::future::BoxFuture<
            'a,
            Result<
                Option<crate::tasks::task_runtime::weixin_delivery::WeixinDeliveryTarget>,
                String,
            >,
        > {
            Box::pin(async move {
                self.freeze_calls
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(Some(
                    crate::tasks::task_runtime::weixin_delivery::WeixinDeliveryTarget {
                        account_id: "original-account".to_string(),
                        credential_generation: "original-generation".to_string(),
                        peer_id: "original-peer".to_string(),
                        context_ref: "opaque-original-context".to_string(),
                        workspace_id: workspace_id.to_string(),
                        channel_conversation: "original-direct-conversation".to_string(),
                        definition_id: invocation.task.definition_id.clone(),
                    },
                ))
            })
        }
        fn send<'a>(
            &'a self,
            target: &'a crate::tasks::task_runtime::weixin_delivery::WeixinDeliveryTarget,
            message_id: &'a str,
            text: &'a str,
        ) -> futures::future::BoxFuture<
            'a,
            Result<echo_agent::channels::weixin::WeixinSendOutcome, String>,
        > {
            Box::pin(async move {
                if target.account_id != "original-account"
                    || target.credential_generation != "original-generation"
                    || target.peer_id != "original-peer"
                    || !text.contains("original completed answer")
                {
                    return Err("fixture observed frozen target/result drift".to_string());
                }
                self.send_calls
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if self.unknown {
                    Ok(echo_agent::channels::weixin::WeixinSendOutcome::Unknown {
                        client_id: message_id.to_string(),
                        reason: "original admitted effect has no response".to_string(),
                    })
                } else {
                    Ok(echo_agent::channels::weixin::WeixinSendOutcome::Accepted {
                        client_id: message_id.to_string(),
                        message_id: Some("original-server-accepted".to_string()),
                    })
                }
            })
        }
    }

    /// This starts with the REAL Framework run_once owner. Its callback and
    /// occurrence both complete despite a failed notification journal append.
    /// New stores and the SAME public after-channel-start recovery entry must
    /// recover that obligation without invoking an Agent or scheduler fire.
    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn completed_framework_occurrence_recovers_original_notification_debt_after_restart()
    -> anyhow::Result<()> {
        use echo_agent::delivery::{DeliveryEvent, DeliveryOutcome, DeliveryPhase};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let cases = [
            (Some(DeliveryPhase::Persisted), false),
            (Some(DeliveryPhase::Claimed), false),
            (Some(DeliveryPhase::EffectStarted), false),
            (Some(DeliveryPhase::TurnSettled), false),
            (None, false),
            (None, true),
        ];
        for (index, (fault, unknown)) in cases.into_iter().enumerate() {
            let root = tempfile::tempdir()?;
            let registry_root = root.path().join("registry");
            let registry = Arc::new(
                crate::workspace::registry::WorkspaceRegistry::with_base_dir(
                    registry_root.clone(),
                )?,
            );
            // Alternate global and cold stored workspace scopes. Recovery
            // cannot depend on the GUI's currently selected workspace.
            let workspace_scope = index % 2 == 1;
            let (tasks_root, workspace_id) = if workspace_scope {
                let path = root.path().join("original-workspace");
                let workspace = registry.create_at(
                    "original",
                    crate::workspace::WorkspaceKind::General,
                    path.clone(),
                )?;
                (
                    crate::workspace::layout::WorkspaceLayout::tasks(&path),
                    workspace.id.to_string(),
                )
            } else {
                (root.path().join("global-tasks"), "global".to_string())
            };
            let llm =
                Arc::new(MockLlmClient::new().with_response("original completed answer 中文🌱"));
            let sender = Arc::new(RestartNotificationSender {
                unknown,
                freeze_calls: AtomicUsize::new(0),
                send_calls: AtomicUsize::new(0),
            });
            let definition = root.path().join("tasks.json");
            let (run_id, occurrence_id, original_identity) = {
                let store = Arc::new(TaskRuntimeStore::open_for_workspace(
                    &tasks_root,
                    &workspace_id,
                )?);
                let resource_root = store.test_resource_scope();
                let agent = AgentHandle::new(
                    ReactAgentBuilder::new()
                        .llm_client(llm.clone())
                        .system_prompt("original prompt")
                        .build()?,
                );
                let original = build_occurrence_fire_fn_with_weixin(
                    agent,
                    Some(store.clone()),
                    None,
                    None,
                    None,
                    Some(sender.clone()),
                );
                let fault_store = store.clone();
                let fire: OccurrenceFireFn = Arc::new(move |invocation| {
                    if let Some(phase) = fault {
                        let identity = OccurrenceIdentity::from_scheduler_invocation(
                            fault_store.active_workspace_id(),
                            &invocation,
                        );
                        TaskRuntimeStore::fail_weixin_phase_before_append_for_test(
                            &identity.run_id(),
                            phase,
                        );
                    }
                    original(invocation)
                });
                let runner = FrameworkSchedulerRunner::new_with_occurrence_context(
                    CronTaskStore::new().with_path(definition.clone()),
                    CancellationToken::new(),
                    fire,
                )
                .await?;
                let task = CronTask::new(
                    "restart debt",
                    "0 0 1 1 *",
                    "produce original completed answer",
                );
                let task_id = task.id.clone();
                runner.add_task(task).await?;
                let manual = runner.run_once_with_receipt(&task_id).await?;
                assert_eq!(llm.call_count(), 1);
                let runs = store.list_runs_in(&[TaskRunStatus::Completed])?;
                let run = runs
                    .first()
                    .ok_or_else(|| anyhow::anyhow!("original completed run missing"))?;
                assert_eq!(runs.len(), 1);
                let identity = store
                    .frozen_weixin_occurrence(&run.run_id)?
                    .ok_or_else(|| anyhow::anyhow!("original frozen occurrence missing"))?;
                assert_eq!(identity.occurrence_id, manual.occurrence.occurrence_id);
                let history = runner.read_occurrence_history(None, Some(512))?;
                assert!(history.events.iter().any(|record| matches!(&record.event,
                    DeliveryEvent::TurnSettled { message_id, outcome: DeliveryOutcome::Completed, .. }
                    if message_id == &manual.occurrence.occurrence_id)));
                if matches!(
                    fault,
                    Some(
                        DeliveryPhase::Persisted
                            | DeliveryPhase::Claimed
                            | DeliveryPhase::EffectStarted
                    )
                ) {
                    assert_eq!(sender.send_calls.load(Ordering::SeqCst), 0);
                    assert!(
                        !store
                            .weixin_notification_status(&run.run_id)?
                            .is_some_and(|record| record.terminal)
                    );
                }
                let result = (
                    run.run_id.clone(),
                    manual.occurrence.occurrence_id.clone(),
                    identity,
                );
                store.shutdown_run_drivers().await?;
                drop(runner);
                drop(store);
                drop(resource_root);
                result
            };
            // Original stores have closed. Neither the new Agent nor current
            // config has the old notification binding; only RunCreated does.
            let reopened = Arc::new(TaskRuntimeStore::open_for_workspace(
                &tasks_root,
                &workspace_id,
            )?);
            let resources = reopened.test_resource_scope();
            let agent = AgentHandle::new(
                ReactAgentBuilder::new()
                    .llm_client(llm.clone())
                    .system_prompt("must not execute after restart")
                    .build()?,
            );
            let mut state = crate::state::AppState::from_shared(
                agent.clone(),
                None,
                Arc::new(crate::hitl::HitlDispatcher::new()),
                None,
                None,
                Default::default(),
                Arc::new(crate::mcp_config_runtime::McpConfigRuntime::new(
                    root.path().join("mcp.json"),
                    Default::default(),
                )),
                crate::product_data_io::ProductDataIoService::new(),
            )?;
            state.workspace.registry = Arc::new(
                crate::workspace::registry::WorkspaceRegistry::with_base_dir(registry_root)?,
            );
            state
                .workspace
                .runtimes
                .bind_lifecycle_retention(&resources);
            state.tasks.runtime = if workspace_scope {
                let global = Arc::new(TaskRuntimeStore::open_for_workspace(
                    root.path().join("new-global-tasks"),
                    "global",
                )?);
                global.bind_lifecycle_retention(&resources);
                Some(global)
            } else {
                Some(reopened.clone())
            };
            let reopened_runner = Arc::new(
                FrameworkSchedulerRunner::new_with_occurrence_context(
                    CronTaskStore::new().with_path(definition),
                    CancellationToken::new(),
                    build_occurrence_fire_fn_with_weixin(
                        agent,
                        Some(reopened.clone()),
                        None,
                        None,
                        None,
                        Some(sender.clone()),
                    ),
                )
                .await?,
            );
            let history = reopened_runner.read_occurrence_history(None, Some(512))?;
            assert!(history.events.iter().any(|record| matches!(&record.event,
                DeliveryEvent::TurnSettled { message_id, outcome: DeliveryOutcome::Completed, .. }
                if message_id == &occurrence_id)));
            state.scheduler.runner = Some(reopened_runner);
            if index == 0 {
                // A second refusal during actual startup must be visible as
                // durable debt, not a successful recovery or failed prompt.
                TaskRuntimeStore::fail_weixin_phase_before_append_for_test(
                    &run_id,
                    DeliveryPhase::Persisted,
                );
                let pending = state
                    .recover_weixin_notifications_after_channel_start(
                        sender.clone(),
                        &CancellationToken::new(),
                    )
                    .await;
                assert!(!pending.pending.is_empty());
                assert_eq!(pending.recovered, 0);
                assert_eq!(sender.send_calls.load(Ordering::SeqCst), 0);
                assert!(reopened.weixin_notification_status(&run_id)?.is_none());
                assert_eq!(
                    reopened.frozen_weixin_occurrence(&run_id)?,
                    Some(original_identity.clone())
                );
                assert_eq!(
                    reopened
                        .get_run(&run_id)?
                        .ok_or_else(|| anyhow::anyhow!("pending debt lost its original run"))?
                        .status,
                    TaskRunStatus::Completed
                );
            }
            let report = state
                .recover_weixin_notifications_after_channel_start(
                    sender.clone(),
                    &CancellationToken::new(),
                )
                .await;
            assert!(
                report.pending.is_empty(),
                "startup notification debt remains: {:?}",
                report.pending
            );
            assert_eq!(report.scanned, 1);
            assert_eq!(
                llm.call_count(),
                1,
                "notification recovery must never re-execute a completed prompt"
            );
            assert_eq!(
                sender.freeze_calls.load(Ordering::SeqCst),
                1,
                "restart cannot reinterpret current config"
            );
            assert_eq!(
                sender.send_calls.load(Ordering::SeqCst),
                1,
                "only an unstarted notification may send"
            );
            let status = reopened
                .weixin_notification_status(&run_id)?
                .ok_or_else(|| anyhow::anyhow!("recovery receipt missing"))?;
            let expected = if unknown || fault == Some(DeliveryPhase::TurnSettled) {
                DeliveryOutcome::OutcomeUnknown
            } else {
                DeliveryOutcome::Completed
            };
            assert_eq!(status.outcome, Some(expected));
            assert_eq!(
                reopened
                    .get_run(&run_id)?
                    .ok_or_else(|| anyhow::anyhow!("original run missing"))?
                    .status,
                TaskRunStatus::Completed
            );
            assert_eq!(
                reopened.frozen_weixin_occurrence(&run_id)?,
                Some(original_identity)
            );
            let again = state
                .recover_weixin_notifications_after_channel_start(
                    sender.clone(),
                    &CancellationToken::new(),
                )
                .await;
            assert!(again.pending.is_empty());
            assert_eq!(sender.send_calls.load(Ordering::SeqCst), 1);
            assert_eq!(llm.call_count(), 1);
            state.shutdown_workspace_runtimes().await?;
            reopened.shutdown_run_drivers().await?;
            if let Some(global) = state.tasks.runtime.as_ref() {
                global.shutdown_run_drivers().await?;
            }
        }
        Ok(())
    }
    /// Exercise the same public runner methods used by GUI management. The
    /// original store must issue identities; no fixture backfills production.
    #[tokio::test]
    async fn gui_runner_definition_is_stable_per_revision_across_fire_edit_and_reopen()
    -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let definition_path = root.path().join("tasks.json");
        let captured = Arc::new(std::sync::Mutex::new(Vec::<CronTask>::new()));
        let observed = captured.clone();
        let callback: OccurrenceFireFn = Arc::new(move |invocation| {
            let observed = observed.clone();
            Box::pin(async move {
                observed
                    .lock()
                    .map_err(|_| {
                        echo_agent::error::ReactError::Other("definition observer poisoned".into())
                    })?
                    .push(invocation.task);
                Ok("original callback physically finished".to_string())
            })
        });
        let runner = FrameworkSchedulerRunner::new_with_occurrence_context(
            CronTaskStore::new().with_path(definition_path.clone()),
            CancellationToken::new(),
            callback.clone(),
        )
        .await?;
        let mut task = CronTask::new("definition lifecycle", "0 0 1 1 *", "original prompt");
        task.id = "definition-lifecycle-task".into();
        assert!(
            task.definition_id.is_empty(),
            "new input is not yet a stored definition"
        );
        runner.add_task(task.clone()).await?;
        let first = runner
            .list_tasks()
            .await
            .into_iter()
            .find(|current| current.id == task.id)
            .ok_or_else(|| anyhow::anyhow!("stored definition missing"))?;
        assert!(!first.definition_id.trim().is_empty());
        let first_fire = runner.run_once_with_receipt(&task.id).await?;
        let second_fire = runner.run_once_with_receipt(&task.id).await?;
        assert_ne!(
            first_fire.occurrence.occurrence_id,
            second_fire.occurrence.occurrence_id
        );
        assert!(
            runner
                .set_status(&task.id, echo_agent::scheduler::CronTaskStatus::Disabled)
                .await?
        );
        let disabled = runner
            .list_tasks()
            .await
            .into_iter()
            .find(|current| current.id == task.id)
            .ok_or_else(|| anyhow::anyhow!("disabled definition missing"))?;
        assert_eq!(disabled.definition_id, first.definition_id);
        assert!(
            runner
                .set_status(&task.id, echo_agent::scheduler::CronTaskStatus::Enabled)
                .await?
        );
        let current = runner
            .list_tasks()
            .await
            .into_iter()
            .find(|current| current.id == task.id)
            .ok_or_else(|| anyhow::anyhow!("enabled definition missing"))?;
        let edited = runner
            .edit_task(
                current,
                echo_agent::scheduler::CronTaskEdit {
                    name: "edited definition lifecycle".into(),
                    cron_expr: "0 0 2 1 *".into(),
                    prompt: "edited prompt".into(),
                },
            )
            .await?;
        assert_eq!(edited.id, first.id);
        assert_eq!(edited.created_at, first.created_at);
        assert!(!edited.definition_id.trim().is_empty());
        assert_ne!(edited.definition_id, first.definition_id);
        runner.run_once_with_receipt(&task.id).await?;
        drop(runner);
        let reopened = FrameworkSchedulerRunner::new_with_occurrence_context(
            CronTaskStore::new().with_path(definition_path),
            CancellationToken::new(),
            callback,
        )
        .await?;
        let restored = reopened
            .list_tasks()
            .await
            .into_iter()
            .find(|current| current.id == task.id)
            .ok_or_else(|| anyhow::anyhow!("reopened definition missing"))?;
        assert_eq!(restored.definition_id, edited.definition_id);
        assert_eq!(restored.id, edited.id);
        assert_eq!(restored.prompt, edited.prompt);
        reopened.run_once_with_receipt(&task.id).await?;
        let captured = captured
            .lock()
            .map_err(|_| anyhow::anyhow!("definition observer poisoned"))?;
        assert_eq!(captured.len(), 4);
        assert_eq!(
            captured
                .iter()
                .map(|value| value.definition_id.as_str())
                .collect::<Vec<_>>(),
            vec![
                first.definition_id.as_str(),
                first.definition_id.as_str(),
                edited.definition_id.as_str(),
                edited.definition_id.as_str()
            ]
        );
        assert!(captured.iter().all(|value| value.id == task.id));
        Ok(())
    }

    /// Bypass the positive invocation helper intentionally: malformed frozen
    /// definitions must never be repaired by the test or production adapter.
    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn empty_or_mismatched_frozen_definition_refuses_model_run_and_notification()
    -> anyhow::Result<()> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct DefinitionSender {
            definition: String,
            freeze_calls: AtomicUsize,
            sends: AtomicUsize,
        }
        impl WeixinCronDelivery for DefinitionSender {
            fn freeze_target<'a>(
                &'a self,
                _invocation: &'a SchedulerInvocation,
                workspace_id: &'a str,
            ) -> futures::future::BoxFuture<
                'a,
                Result<
                    Option<crate::tasks::task_runtime::weixin_delivery::WeixinDeliveryTarget>,
                    String,
                >,
            > {
                Box::pin(async move {
                    self.freeze_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(Some(
                        crate::tasks::task_runtime::weixin_delivery::WeixinDeliveryTarget {
                            account_id: "original-account".into(),
                            credential_generation: "original-generation".into(),
                            peer_id: "original-peer".into(),
                            context_ref: "original-context".into(),
                            workspace_id: workspace_id.into(),
                            channel_conversation: "original-direct-conversation".into(),
                            definition_id: self.definition.clone(),
                        },
                    ))
                })
            }
            fn send<'a>(
                &'a self,
                _target: &'a crate::tasks::task_runtime::weixin_delivery::WeixinDeliveryTarget,
                _message_id: &'a str,
                _text: &'a str,
            ) -> futures::future::BoxFuture<
                'a,
                Result<echo_agent::channels::weixin::WeixinSendOutcome, String>,
            > {
                Box::pin(async move {
                    self.sends.fetch_add(1, Ordering::SeqCst);
                    Err("unexpected send for invalid definition".into())
                })
            }
        }
        for (task_definition, frozen_definition, expected_error) in [
            ("", "", "destination is incomplete"),
            ("valid-original-definition", "", "definition does not match"),
            (
                "valid-original-definition",
                "foreign-definition",
                "definition does not match",
            ),
        ] {
            let store = Arc::new(TaskRuntimeStore::new_in_memory()?);
            let _resources = store.test_resource_scope();
            let model = Arc::new(MockLlmClient::new().with_response("must not execute"));
            let agent = AgentHandle::new(
                ReactAgentBuilder::new()
                    .llm_client(model.clone())
                    .system_prompt("offline definition control")
                    .build()?,
            );
            let sender = Arc::new(DefinitionSender {
                definition: frozen_definition.into(),
                freeze_calls: AtomicUsize::new(0),
                sends: AtomicUsize::new(0),
            });
            let fire = build_occurrence_fire_fn_with_weixin(
                agent,
                Some(store.clone()),
                None,
                None,
                None,
                Some(sender.clone()),
            );
            let mut task = CronTask::new("invalid definition control", "0 0 1 1 *", "must not run");
            task.id = "invalid-definition-task".into();
            task.definition_id = task_definition.into();
            let invocation = SchedulerInvocation::new(
                "invalid-definition-occurrence",
                task,
                chrono::Utc::now(),
                echo_agent::scheduler::SchedulerTrigger::Manual,
            );
            let identity = OccurrenceIdentity::from_scheduler_invocation(
                store.active_workspace_id(),
                &invocation,
            );
            for _ in 0..2 {
                let error = fire(invocation.clone())
                    .await
                    .err()
                    .ok_or_else(|| anyhow::anyhow!("invalid definition unexpectedly admitted"))?;
                assert!(
                    error.to_string().contains(expected_error),
                    "unexpected refusal: {error}"
                );
                assert!(store.get_run(&identity.run_id())?.is_none());
            }
            assert_eq!(model.call_count(), 0);
            assert_eq!(sender.freeze_calls.load(Ordering::SeqCst), 2);
            assert_eq!(sender.sends.load(Ordering::SeqCst), 0);
            store.shutdown_run_drivers().await?;
        }
        Ok(())
    }
    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn one_definition_two_slots_keep_original_target_and_each_slot_sends_once()
    -> anyhow::Result<()> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let model = Arc::new(
            MockLlmClient::new()
                .with_responses(["first original full answer", "second original full answer"]),
        );
        let agent = AgentHandle::new(
            ReactAgentBuilder::new()
                .llm_client(model.clone())
                .system_prompt("offline original slots")
                .build()?,
        );
        let store = Arc::new(TaskRuntimeStore::new_in_memory()?);
        let _resources = store.test_resource_scope();
        let sender = Arc::new(RecordingWeixin {
            freeze_calls: AtomicUsize::new(0),
            sends: std::sync::Mutex::new(Vec::new()),
        });
        let fire = build_occurrence_fire_fn_with_weixin(
            agent,
            Some(store.clone()),
            None,
            None,
            None,
            Some(sender.clone()),
        );
        let mut task = CronTask::new(
            "two original slots",
            "0 0 1 1 *",
            "produce original full answer",
        );
        task.id = "one-definition-task".into();
        task.definition_id = "one-original-definition".into();
        let mut runs = Vec::new();
        for occurrence in ["original-slot-one", "original-slot-two"] {
            let invocation = SchedulerInvocation::new(
                occurrence,
                task.clone(),
                chrono::Utc::now(),
                echo_agent::scheduler::SchedulerTrigger::Manual,
            );
            let identity = OccurrenceIdentity::from_scheduler_invocation(
                store.active_workspace_id(),
                &invocation,
            );
            fire(invocation.clone()).await?;
            fire(invocation).await?;
            let frozen = store
                .frozen_weixin_notification(&identity.run_id())?
                .ok_or_else(|| anyhow::anyhow!("original frozen route missing"))?;
            assert_eq!(frozen.definition_id, task.definition_id);
            assert_eq!(frozen.account_id, "frozen-account");
            assert_eq!(frozen.credential_generation, "original-generation");
            assert_eq!(frozen.peer_id, "frozen-peer");
            assert_eq!(frozen.context_ref, "original-context-reference");
            assert_eq!(
                store
                    .get_run(&identity.run_id())?
                    .ok_or_else(|| anyhow::anyhow!("original run missing"))?
                    .status,
                TaskRunStatus::Completed
            );
            runs.push(identity.run_id());
        }
        assert_ne!(runs.first(), runs.get(1));
        assert_eq!(model.call_count(), 2);
        assert_eq!(sender.freeze_calls.load(Ordering::SeqCst), 2);
        let sends = sender
            .sends
            .lock()
            .map_err(|_| anyhow::anyhow!("send observer poisoned"))?;
        assert_eq!(sends.len(), 2);
        assert_ne!(
            sends.first().map(|value| &value.0),
            sends.get(1).map(|value| &value.0)
        );
        assert!(
            sends
                .first()
                .is_some_and(|value| value.1.contains("first original full answer"))
        );
        assert!(
            sends
                .get(1)
                .is_some_and(|value| value.1.contains("second original full answer"))
        );
        Ok(())
    }
    /// Same GUI runner/edit/persistence path, then the same original callback
    /// and TaskRuntime notification ledger. Route settings are an offline
    /// transport fixture; the original journal alone owns admitted targets.
    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn edited_definition_reopens_without_retargeting_original_occurrence_receipt()
    -> anyhow::Result<()> {
        use crate::tasks::task_runtime::weixin_delivery::WeixinDeliveryTarget;
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct DefinitionRoutes {
            routes: std::sync::Mutex<std::collections::HashMap<String, WeixinDeliveryTarget>>,
            freeze_calls: AtomicUsize,
            sends: std::sync::Mutex<Vec<(WeixinDeliveryTarget, String, String)>>,
        }
        impl WeixinCronDelivery for DefinitionRoutes {
            fn freeze_target<'a>(
                &'a self,
                invocation: &'a SchedulerInvocation,
                _workspace_id: &'a str,
            ) -> futures::future::BoxFuture<'a, Result<Option<WeixinDeliveryTarget>, String>>
            {
                Box::pin(async move {
                    self.freeze_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(self
                        .routes
                        .lock()
                        .map_err(|_| "route fixture poisoned".to_string())?
                        .get(&invocation.task.definition_id)
                        .cloned())
                })
            }
            fn send<'a>(
                &'a self,
                target: &'a WeixinDeliveryTarget,
                message_id: &'a str,
                text: &'a str,
            ) -> futures::future::BoxFuture<
                'a,
                Result<echo_agent::channels::weixin::WeixinSendOutcome, String>,
            > {
                Box::pin(async move {
                    self.sends
                        .lock()
                        .map_err(|_| "send fixture poisoned".to_string())?
                        .push((target.clone(), message_id.into(), text.into()));
                    Ok(echo_agent::channels::weixin::WeixinSendOutcome::Accepted {
                        client_id: message_id.into(),
                        message_id: Some(format!("accepted-{message_id}")),
                    })
                })
            }
        }
        fn callback(
            store: Arc<TaskRuntimeStore>,
            model: Arc<MockLlmClient>,
            sender: Arc<DefinitionRoutes>,
            captured: Arc<std::sync::Mutex<Vec<SchedulerInvocation>>>,
        ) -> anyhow::Result<OccurrenceFireFn> {
            let agent = AgentHandle::new(
                ReactAgentBuilder::new()
                    .llm_client(model)
                    .system_prompt("offline persisted definition route")
                    .build()?,
            );
            let original = build_occurrence_fire_fn_with_weixin(
                agent,
                Some(store),
                None,
                None,
                None,
                Some(sender),
            );
            Ok(Arc::new(move |invocation| {
                let captured = captured.clone();
                let original = original.clone();
                Box::pin(async move {
                    captured
                        .lock()
                        .map_err(|_| {
                            echo_agent::error::ReactError::Other(
                                "invocation observer poisoned".into(),
                            )
                        })?
                        .push(invocation.clone());
                    original(invocation).await
                })
            }))
        }
        fn route(
            definition: &str,
            account: &str,
            generation: &str,
            peer: &str,
        ) -> WeixinDeliveryTarget {
            WeixinDeliveryTarget {
                definition_id: definition.into(),
                account_id: account.into(),
                credential_generation: generation.into(),
                peer_id: peer.into(),
                context_ref: format!("original-context-{peer}"),
                workspace_id: "global".into(),
                channel_conversation: format!("original-direct-{peer}"),
            }
        }
        let root = tempfile::tempdir()?;
        let tasks_path = root.path().join("tasks.json");
        let runtime_path = root.path().join("runs");
        let model = Arc::new(
            MockLlmClient::new()
                .with_responses(["original pre-edit answer", "original post-edit answer"]),
        );
        let sender = Arc::new(DefinitionRoutes {
            routes: std::sync::Mutex::new(std::collections::HashMap::new()),
            freeze_calls: AtomicUsize::new(0),
            sends: std::sync::Mutex::new(Vec::new()),
        });
        let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (old_invocation, old_identity, original_route, edited_definition) = {
            let store = Arc::new(TaskRuntimeStore::open_for_workspace(
                &runtime_path,
                "global",
            )?);
            let resources = store.test_resource_scope();
            let runner = FrameworkSchedulerRunner::new_with_occurrence_context(
                CronTaskStore::new().with_path(tasks_path.clone()),
                CancellationToken::new(),
                callback(
                    store.clone(),
                    model.clone(),
                    sender.clone(),
                    captured.clone(),
                )?,
            )
            .await?;
            let mut task = CronTask::new("original route", "0 0 1 1 *", "original pre-edit prompt");
            task.id = "edited-notification-task".into();
            runner.add_task(task.clone()).await?;
            let admitted = runner
                .list_tasks()
                .await
                .into_iter()
                .find(|value| value.id == task.id)
                .ok_or_else(|| anyhow::anyhow!("admitted scheduler definition missing"))?;
            assert!(!admitted.definition_id.trim().is_empty());
            let original_route = route(
                &admitted.definition_id,
                "original-account",
                "original-generation",
                "original-peer",
            );
            sender
                .routes
                .lock()
                .map_err(|_| anyhow::anyhow!("route fixture poisoned"))?
                .insert(admitted.definition_id.clone(), original_route.clone());
            let old_receipt = runner.run_once_with_receipt(&task.id).await?;
            let old_invocation = captured
                .lock()
                .map_err(|_| anyhow::anyhow!("invocation observer poisoned"))?
                .first()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("original invocation missing"))?;
            let old_identity = OccurrenceIdentity::from_scheduler_invocation(
                store.active_workspace_id(),
                &old_invocation,
            );
            assert_eq!(old_receipt.occurrence.definition_id, admitted.definition_id);
            assert_eq!(
                store.frozen_weixin_notification(&old_identity.run_id())?,
                Some(original_route.clone())
            );
            assert!(
                runner
                    .set_status(&task.id, echo_agent::scheduler::CronTaskStatus::Disabled)
                    .await?
            );
            assert!(
                runner
                    .set_status(&task.id, echo_agent::scheduler::CronTaskStatus::Enabled)
                    .await?
            );
            let current = runner
                .list_tasks()
                .await
                .into_iter()
                .find(|value| value.id == task.id)
                .ok_or_else(|| anyhow::anyhow!("enabled scheduler definition missing"))?;
            assert_eq!(current.definition_id, admitted.definition_id);
            let edited = runner
                .edit_task(
                    current,
                    echo_agent::scheduler::CronTaskEdit {
                        name: "edited route".into(),
                        cron_expr: "0 0 2 1 *".into(),
                        prompt: "original post-edit prompt".into(),
                    },
                )
                .await?;
            assert_eq!(edited.id, admitted.id);
            assert_eq!(edited.created_at, admitted.created_at);
            assert!(!edited.definition_id.trim().is_empty());
            assert_ne!(
                edited.definition_id, admitted.definition_id,
                "actual content edit owns a new definition revision"
            );
            let new_route = route(
                &edited.definition_id,
                "new-account",
                "new-generation",
                "new-peer",
            );
            {
                let mut routes = sender
                    .routes
                    .lock()
                    .map_err(|_| anyhow::anyhow!("route fixture poisoned"))?;
                routes.clear();
                routes.insert(edited.definition_id.clone(), new_route);
            }
            // The exact original TaskRun retains the old route and terminal receipt.
            assert_eq!(
                store.frozen_weixin_notification(&old_identity.run_id())?,
                Some(original_route.clone())
            );
            assert_eq!(
                store
                    .weixin_notification_status(&old_identity.run_id())?
                    .and_then(|record| record.outcome),
                Some(echo_agent::delivery::DeliveryOutcome::Completed)
            );
            store.shutdown_run_drivers().await?;
            drop(runner);
            drop(store);
            drop(resources);
            (old_invocation, old_identity, original_route, edited)
        };
        let reopened = Arc::new(TaskRuntimeStore::open_for_workspace(
            &runtime_path,
            "global",
        )?);
        let _resources = reopened.test_resource_scope();
        let fire = callback(reopened.clone(), model.clone(), sender.clone(), captured)?;
        let runner = FrameworkSchedulerRunner::new_with_occurrence_context(
            CronTaskStore::new().with_path(tasks_path),
            CancellationToken::new(),
            fire.clone(),
        )
        .await?;
        let restored = runner
            .list_tasks()
            .await
            .into_iter()
            .find(|value| value.id == edited_definition.id)
            .ok_or_else(|| anyhow::anyhow!("edited definition absent after physical reopen"))?;
        assert_eq!(restored.definition_id, edited_definition.definition_id);
        assert_eq!(restored.prompt, edited_definition.prompt);
        let old_before = reopened
            .get_run_state(&old_identity.run_id())?
            .ok_or_else(|| anyhow::anyhow!("old run absent"))?;
        let sends_before = sender
            .sends
            .lock()
            .map_err(|_| anyhow::anyhow!("send fixture poisoned"))?
            .len();
        let freezes_before = sender.freeze_calls.load(Ordering::SeqCst);
        fire(old_invocation).await?;
        assert_eq!(
            sender
                .sends
                .lock()
                .map_err(|_| anyhow::anyhow!("send fixture poisoned"))?
                .len(),
            sends_before
        );
        assert_eq!(
            sender.freeze_calls.load(Ordering::SeqCst),
            freezes_before,
            "old admission never reselects new route"
        );
        assert_eq!(
            model.call_count(),
            1,
            "old receipt cannot restart the original prompt"
        );
        assert_eq!(
            reopened.frozen_weixin_notification(&old_identity.run_id())?,
            Some(original_route.clone())
        );
        let old_after = reopened
            .get_run_state(&old_identity.run_id())?
            .ok_or_else(|| anyhow::anyhow!("old run lost"))?;
        assert_eq!(old_after.journal_sequence, old_before.journal_sequence);
        assert_eq!(old_after.run.status, TaskRunStatus::Completed);
        let new_receipt = runner.run_once_with_receipt(&restored.id).await?;
        assert_eq!(new_receipt.occurrence.definition_id, restored.definition_id);
        assert_ne!(
            new_receipt.occurrence.occurrence_id,
            old_identity.occurrence_id
        );
        assert_eq!(model.call_count(), 2);
        assert_eq!(sender.freeze_calls.load(Ordering::SeqCst), 2);
        let sends = sender
            .sends
            .lock()
            .map_err(|_| anyhow::anyhow!("send fixture poisoned"))?;
        assert_eq!(sends.len(), 2);
        assert_eq!(sends.first().map(|record| &record.0), Some(&original_route));
        assert!(
            sends
                .first()
                .is_some_and(|record| record.2.contains("original pre-edit answer"))
        );
        let newer = sends
            .get(1)
            .ok_or_else(|| anyhow::anyhow!("new definition notification absent"))?;
        assert_eq!(newer.0.definition_id, restored.definition_id);
        assert_eq!(newer.0.account_id, "new-account");
        assert_eq!(newer.0.credential_generation, "new-generation");
        assert_eq!(newer.0.peer_id, "new-peer");
        assert!(newer.2.contains("original post-edit answer"));
        drop(sends);
        reopened.shutdown_run_drivers().await?;
        Ok(())
    }

    /// 原送达future尚未返回时，原occurrence owner必须保留；不另造通知重试或调度器。
    #[cfg(feature = "channels")]
    #[tokio::test]
    async fn native_cron_notification_waits_for_original_physical_send_and_never_replays()
    -> anyhow::Result<()> {
        use echo_agent::agent::{AssistantMessageBlock, AssistantPhase};
        use echo_agent::llm::types::DeltaMessage;
        use echo_agent::testing::StreamChunk;
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct OriginalSendGate {
            recorded: RecordingWeixin,
            entered: tokio::sync::Notify,
            release: CancellationToken,
        }
        impl WeixinCronDelivery for OriginalSendGate {
            fn freeze_target<'a>(
                &'a self,
                invocation: &'a SchedulerInvocation,
                workspace_id: &'a str,
            ) -> futures::future::BoxFuture<
                'a,
                Result<
                    Option<crate::tasks::task_runtime::weixin_delivery::WeixinDeliveryTarget>,
                    String,
                >,
            > {
                self.recorded.freeze_target(invocation, workspace_id)
            }
            fn send<'a>(
                &'a self,
                target: &'a crate::tasks::task_runtime::weixin_delivery::WeixinDeliveryTarget,
                message_id: &'a str,
                text: &'a str,
            ) -> futures::future::BoxFuture<
                'a,
                Result<echo_agent::channels::weixin::WeixinSendOutcome, String>,
            > {
                Box::pin(async move {
                    self.entered.notify_one();
                    self.release.cancelled().await;
                    self.recorded.send(target, message_id, text).await
                })
            }
        }
        let answer = format!("{}NATIVE-NOTIFY-END", "原生完整回答🌱".repeat(600));
        let native = |phase, delta: String, sequence| AssistantMessageBlock {
            provider_source: None,
            block_id: format!("native-cron-{sequence}"),
            request_id: "native-cron-original-request".into(),
            sequence,
            phase,
            delta,
            complete: true,
            provider: Some("offline-native-provider".into()),
            reasoning: None,
        };
        let model = Arc::new(MockLlmClient::new().with_stream_script(vec![
            StreamChunk::Delta(DeltaMessage {
                role: Some("assistant".into()),
                provider_response: None,
                content: None,
                reasoning_content: None,
                reasoning_blocks: None,
                assistant_blocks: Some(vec![
                    native(
                        AssistantPhase::Reasoning,
                        "private-native-reasoning".into(),
                        1,
                    ),
                    native(AssistantPhase::FinalAnswer, answer.clone(), 2),
                ]),
                tool_calls: None,
            }),
            StreamChunk::Terminal {
                finish_reason: Some("stop".into()),
                usage: None,
            },
        ]));
        let agent = AgentHandle::new(
            ReactAgentBuilder::new()
                .llm_client(model.clone())
                .system_prompt("offline native cron")
                .build()?,
        );
        let store = Arc::new(TaskRuntimeStore::new_in_memory()?);
        let _resources = store.test_resource_scope();
        let sender = Arc::new(OriginalSendGate {
            recorded: RecordingWeixin {
                freeze_calls: AtomicUsize::new(0),
                sends: std::sync::Mutex::new(Vec::new()),
            },
            entered: tokio::sync::Notify::new(),
            release: CancellationToken::new(),
        });
        let fire = build_occurrence_fire_fn_with_weixin(
            agent,
            Some(store.clone()),
            None,
            None,
            None,
            Some(sender.clone()),
        );
        let mut task = CronTask::new("native send barrier", "0 0 1 1 *", "original native answer");
        task.definition_id = "native-original-definition".into();
        let invocation = invocation("native-original-notification-slot", task);
        let identity =
            OccurrenceIdentity::from_scheduler_invocation(store.active_workspace_id(), &invocation);
        let observe = async {
            let reached =
                tokio::time::timeout(std::time::Duration::from_secs(5), sender.entered.notified())
                    .await;
            if reached.is_err() {
                sender.release.cancel();
                return Err(anyhow::anyhow!(
                    "original send did not reach its physical barrier"
                ));
            }
            // 先收集原事实，先释放并join，再判断；断言失败不得遗留刻意阻塞的发送。
            let receipt_before = store.weixin_notification_status(&identity.run_id());
            let physically_owned_before = store.active_run_driver_count();
            let sends_before = sender
                .recorded
                .sends
                .lock()
                .map(|sends| sends.len())
                .map_err(|_| "send observer poisoned".to_string());
            let duplicate =
                tokio::time::timeout(std::time::Duration::from_secs(1), fire(invocation.clone()))
                    .await;
            sender.release.cancel();
            Ok((
                receipt_before,
                sends_before,
                duplicate,
                physically_owned_before,
            ))
        };
        let joined = tokio::time::timeout(std::time::Duration::from_secs(12), async {
            tokio::join!(fire(invocation.clone()), observe)
        })
        .await;
        sender.release.cancel();
        let (original, observed) = joined?;
        original?;
        let (receipt_before, sends_before, duplicate, physically_owned_before) = observed?;
        assert_eq!(physically_owned_before.map_err(anyhow::Error::msg)?, 1);
        assert!(!store.has_unsettled_driver_resources());
        assert_eq!(sends_before.map_err(anyhow::Error::msg)?, 0);
        assert!(receipt_before?.is_some_and(|receipt| receipt.outcome.is_none()));
        assert!(duplicate?.is_err(), "同一物理owner未收尾时不能重新准入");
        // 完成后的同slot重投仍仅观察原结果，不新调模型或发通知。
        fire(invocation).await?;
        assert_eq!(model.call_count(), 1);
        assert_eq!(sender.recorded.freeze_calls.load(Ordering::SeqCst), 1);
        let sends = sender
            .recorded
            .sends
            .lock()
            .map_err(|_| anyhow::anyhow!("send observer poisoned"))?;
        assert_eq!(sends.len(), 1);
        let (_, sent) = sends
            .first()
            .ok_or_else(|| anyhow::anyhow!("original send missing"))?;
        assert!(sent.contains(&answer));
        assert!(!sent.contains("private-native-reasoning"));
        assert_eq!(
            store
                .get_run(&identity.run_id())?
                .ok_or_else(|| anyhow::anyhow!("original TaskRun missing"))?
                .status,
            TaskRunStatus::Completed
        );
        assert_eq!(
            store
                .weixin_notification_status(&identity.run_id())?
                .ok_or_else(|| anyhow::anyhow!("original notification receipt missing"))?
                .outcome,
            Some(echo_agent::delivery::DeliveryOutcome::Completed)
        );
        drop(sends);
        store.shutdown_run_drivers().await?;
        assert_eq!(
            store
                .active_run_driver_count()
                .map_err(anyhow::Error::msg)?,
            0
        );
        Ok(())
    }
}
