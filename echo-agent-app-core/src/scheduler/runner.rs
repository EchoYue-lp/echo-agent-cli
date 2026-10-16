//! Cron scheduler adapter — wires the framework's `SchedulerRunner` to the
//! CLI's `AgentHandle` and TaskRuntime.
//!
//! The framework's runner is generic over a `FireFn` callback; this module
//! provides [`build_fire_fn`] which constructs one that launches TaskRuntime runs.
//!
//! All cron tasks route through the unified TaskRuntime executor
//! (`drive_existing_cron_run`). Prompts are passed through unchanged; simple
//! prompts are answered directly by the agent and materialized as a one-task
//! Plan with completion evidence, while complex prompts drive task creation and
//! execution.

use crate::agent_handle::AgentHandle;
use crate::agent_pool::AgentPool;
use crate::tasks::task_runtime::TaskRuntimeStore;
use crate::tasks::task_runtime::executor::{
    ExecError, create_unattended_run, drive_existing_cron_run,
};
use crate::tasks::task_runtime::store::RunDriverFailure;
use echo_agent::agent::CancellationToken;
use echo_agent::scheduler::{
    CronTask, CronTaskStore, FireFn, SchedulerRunner as FrameworkSchedulerRunner,
};
use std::sync::Arc;

/// The CLI's scheduler runner is the framework's `SchedulerRunner`.
pub type SchedulerRunner = FrameworkSchedulerRunner;

/// Build a `FireFn` that dispatches cron task execution.
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
            let prompt = task.prompt.as_str();
            if prompt.trim().is_empty() {
                return Err(echo_agent::error::ReactError::Other(
                    "cron prompt is empty".into(),
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
                .register_observed_run_driver::<String>(admission, generation_lease)
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
    use echo_agent::agent::ReactAgentBuilder;
    use echo_agent::testing::MockLlmClient;

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
    async fn build_fire_fn_routes_non_plan_cron_to_supervised_run() -> Result<(), String> {
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
            "cron 应经 supervised driver 建恰好 1 个 Completed run"
        );
        let completed_run = completed
            .first()
            .ok_or_else(|| "completed cron run is missing".to_string())?;
        assert_eq!(completed_run.workspace_id, "cron-workspace");
        Ok(())
    }

    #[tokio::test]
    async fn build_fire_fn_preserves_plan_marker_as_prompt_text() -> Result<(), String> {
        let llm = MockLlmClient::new().with_response("ok");
        let agent = ReactAgentBuilder::new()
            .llm_client(Arc::new(llm))
            .system_prompt("test")
            .build()
            .map_err(|error| error.to_string())?;
        let handle = AgentHandle::new(agent);
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let _resource_scope = store.test_resource_scope();
        let fire_fn = build_fire_fn(handle, Some(store.clone()), None, None, None);

        let task = CronTask::new("plan", "*/5 * * * *", "[plan] do the thing");
        let result = fire_fn(task).await;
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
        let _resource_scope = store.test_resource_scope();
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
        let _resource_scope = store.test_resource_scope();
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
}
