//! Phase B2 — background/foreground Run driver (pool-per-run isolation).
//!
//! [`drive_run_async`] is the lower half that drives an already-created
//! TaskRuntime run to completion on an **isolated pool agent**, decoupled from
//! the front-desk chat agent. It is the bridge the `create_complex_task` tool
//! (Phase B3) and cron (Phase B5) spawn/await.
//!
//! Concurrency-safety (spec §5.6, verified against current code):
//! - `RunPayload` is fully owned + `Arc`'d ⇒ `Send + 'static`, so it can be
//!   `move`d into `tokio::spawn` for background runs (no borrowed refs).
//! - `AgentPool::acquire` returns an application execution receipt. The pool's
//!   map lock is released before the ReAct loop, while the receipt pins the
//!   workspace generation until the run reaches a terminal state.
//! - Background runs MUST use an independent `CancellationToken` (spec §5.5),
//!   never the chat turn's token — the caller (`create_complex_task`) supplies
//!   it via `RunPayload.cancel`.
//!
//! Terminal status is read from TaskRuntime after the Agent loop; completed and
//! cancelled chat-owned runs then perform the blocking memory write.

use std::sync::Arc;

use echo_agent::agent::CancellationToken;
use echo_agent::human_loop::HumanLoopProvider;

use crate::agent_pool::AgentPool;
use crate::tasks::task_runtime::executor::{
    ExecSink, RunOutcome, RunPlanPolicy, TaskRuntimeOperation, drive_agent_run,
};
use crate::tasks::task_runtime::memory_bridge::{MemoryEvent, MemoryPolicy};
use crate::tasks::task_runtime::store::TaskRuntimeStore;
use crate::tasks::task_runtime::types::{TaskRunStatus, UnattendedWriteMode};

/// Fully-owned payload for driving a Run in the background or foreground.
///
/// Every field is owned or `Arc`'d so the whole struct is `Send + 'static`
/// and can be moved into `tokio::spawn`.
pub struct RunPayload {
    pub run_id: String,
    pub pool: Arc<AgentPool>,
    pub store: Arc<TaskRuntimeStore>,
    /// Independent cancel token for this run (NOT the chat turn's token for
    /// background runs — see spec §5.5). Foreground runs may share the turn
    /// token, but using an independent one keeps the logic uniform.
    pub cancel: CancellationToken,
    pub memory_generation: Option<crate::evolution::ReviewGenerationLease>,
    /// Execution-flow sink forwarded into the independent Agent so its
    /// thinking/tool/token events reach the frontend's `execution://event`
    /// channel. `Some` for foreground (inline streaming to the chat sink);
    /// `None` for background (events go via Tauri emit from the run path).
    pub trace_sink: Option<ExecSink>,
    /// Full prompt for the independent primary Agent. Kept separate from the
    /// persisted user-facing Run goal so domain methodology does not pollute UI.
    pub prompt: String,
    /// Whether this Run must materialize a formal plan before completion.
    pub plan_policy: RunPlanPolicy,
    /// Surface-owned approval/input transport for an attended independent run.
    /// Unattended launchers pass `None`, so they never wait on an interactive
    /// provider that has no owner.
    pub human_loop_provider: Option<Arc<dyn HumanLoopProvider>>,
    /// Exact EKO product-data root and workspace lifetime authority captured
    /// by the originating surface. Background drivers never re-read focus.
    pub workspace_io: Option<crate::state::WorkspaceIoInvocation>,
    pub(crate) receipt_owner: crate::tasks::task_runtime::store::RunDriverReceiptOwner,
}

struct RunExecutionPayload {
    run_id: String,
    store: Arc<TaskRuntimeStore>,
    cancel: CancellationToken,
    memory_generation: Option<crate::evolution::ReviewGenerationLease>,
    trace_sink: Option<ExecSink>,
    prompt: String,
    plan_policy: RunPlanPolicy,
    workspace_io: Option<crate::state::WorkspaceIoInvocation>,
}

fn admitted_human_loop_provider(
    attended_mode: crate::tasks::task_runtime::AttendedMode,
    provider: Option<Arc<dyn HumanLoopProvider>>,
) -> Option<Arc<dyn HumanLoopProvider>> {
    match attended_mode {
        crate::tasks::task_runtime::AttendedMode::Attended => provider,
        crate::tasks::task_runtime::AttendedMode::Unattended => None,
    }
}

/// Drive an already-created TaskRuntime run to completion on an isolated pool
/// agent. The isolated Agent first runs ReAct so it can materialize a formal
/// plan through `task_create` + `task_execute`, or complete directly when the
/// Run policy allows it.
pub async fn drive_run_async(payload: RunPayload) -> Result<RunOutcome, String> {
    let RunPayload {
        run_id,
        pool,
        store,
        cancel,
        memory_generation,
        trace_sink,
        prompt,
        plan_policy,
        human_loop_provider,
        workspace_io,
        mut receipt_owner,
    } = payload;
    if let Some(generation) = memory_generation.as_ref() {
        receipt_owner.retain(generation.clone());
    }
    let settlement_store = store.clone();
    let settlement_run_id = run_id.clone();
    let settlement_cancel = cancel.clone();
    let pool_lease = match pool.acquire(&run_id).await {
        Ok(lease) => lease,
        Err(error) => {
            let message = format!("pool acquire failed for run {run_id}: {error}");
            settle_driver_error(store.clone(), &run_id, &message, cancel.is_cancelled()).await?;
            return Err(message);
        }
    };
    let pool_agent = pool_lease.agent();
    receipt_owner.retain(pool.retain_for_supervised_run(run_id.clone(), pool_lease));
    let hitl_run_id = run_id.clone();
    let attended_mode = TaskRuntimeOperation::new(store.clone())
        .run("load independent run HITL policy", move |store| {
            store
                .get_run(&hitl_run_id)?
                .map(|run| run.attended_mode)
                .ok_or(crate::tasks::task_runtime::StoreError::RunNotFound(
                    hitl_run_id,
                ))
        })
        .await
        .map_err(|error| format!("load independent run HITL policy failed: {error}"))?;
    if let Some(provider) = admitted_human_loop_provider(attended_mode, human_loop_provider) {
        pool_agent
            .write_async(|agent| {
                Box::pin(async move {
                    agent.set_human_loop_provider_preserving_approvals(provider);
                })
            })
            .await;
    }
    let execute_plan =
        crate::tasks::task_runtime::ExecuteTaskTool::new(store.clone(), pool_agent.clone())
            .with_workspace_io(workspace_io.clone());
    pool_agent
        .write(|agent| {
            agent.add_tool(Box::new(execute_plan));
        })
        .await;
    let result = drive_run_async_inner(
        RunExecutionPayload {
            run_id,
            store,
            cancel,
            memory_generation,
            trace_sink,
            prompt,
            plan_policy,
            workspace_io,
        },
        pool_agent,
    )
    .await;
    if let Err(error) = &result {
        settle_driver_error(
            settlement_store,
            &settlement_run_id,
            error,
            settlement_cancel.is_cancelled(),
        )
        .await?;
    }
    // The TaskRuntime supervisor releases the pool receipt only after its own
    // durable terminal readback (or after settlement debt retry succeeds).
    result
}

async fn drive_run_async_inner(
    payload: RunExecutionPayload,
    pool_agent: crate::agent_handle::AgentHandle,
) -> Result<RunOutcome, String> {
    let drive_result = drive_agent_run(
        payload.store.clone(),
        pool_agent,
        &payload.run_id,
        "create_complex_task",
        &payload.run_id,
        &payload.prompt,
        payload.cancel,
        UnattendedWriteMode::Disabled,
        payload.plan_policy,
        payload.trace_sink.clone(),
        payload.workspace_io,
    )
    .await;
    drive_result.map_err(|error| {
        format!(
            "agent-driven run failed for run {}: {error}",
            payload.run_id
        )
    })?;

    let load_run_id = payload.run_id.clone();
    let run =
        TaskRuntimeOperation::new(payload.store.clone())
            .run("load final supervised run status", move |store| {
                store.get_run(&load_run_id)?.ok_or(
                    crate::tasks::task_runtime::StoreError::RunNotFound(load_run_id),
                )
            })
            .await
            .map_err(|error| format!("read final run status failed: {error}"))?;
    let outcome = match run.status {
        TaskRunStatus::Completed => RunOutcome::Completed,
        TaskRunStatus::Cancelled => RunOutcome::Cancelled,
        TaskRunStatus::Failed => RunOutcome::Failed {
            failed_task_id: None,
            error: "agent-driven run failed".to_string(),
        },
        TaskRunStatus::Paused => RunOutcome::Paused {
            failed_task_id: None,
            error: "agent-driven run paused".to_string(),
        },
        status => {
            return Err(format!(
                "run {} ended in non-terminal status {}",
                payload.run_id,
                status.as_str()
            ));
        }
    };

    let memory_event = match &outcome {
        RunOutcome::Completed => Some(MemoryEvent::RunCompleted {
            run_id: payload.run_id.clone(),
            goal: run.goal.clone(),
        }),
        RunOutcome::Cancelled => Some(MemoryEvent::RunCancelledByUser {
            run_id: payload.run_id.clone(),
            goal: run.goal.clone(),
        }),
        RunOutcome::Failed { .. } | RunOutcome::Paused { .. } => None,
    };
    if let Some(event) = memory_event {
        crate::tasks::task_runtime::memory_bridge::write_memory_candidate_dispatch(
            MemoryPolicy::BestEffortSettled,
            payload.memory_generation.as_ref(),
            &payload.store,
            event,
        )
        .await;
    }
    Ok(outcome)
}

async fn settle_driver_error(
    store: Arc<TaskRuntimeStore>,
    run_id: &str,
    error: &str,
    cancelled: bool,
) -> Result<(), String> {
    let target = if cancelled {
        TaskRunStatus::Cancelled
    } else {
        TaskRunStatus::Failed
    };
    let run_id = run_id.to_string();
    let error = error.to_string();
    TaskRuntimeOperation::new(store)
        .run("settle supervised run error", move |store| {
            store.finalize_run(&run_id, target, Some(&error))
        })
        .await
        .map(|_| ())
        .map_err(|settlement_error| settlement_error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixedHumanLoopProvider;

    impl HumanLoopProvider for FixedHumanLoopProvider {
        fn request(
            &self,
            _request: echo_agent::human_loop::HumanLoopRequest,
        ) -> futures::future::BoxFuture<
            '_,
            echo_agent::error::Result<echo_agent::human_loop::HumanLoopResponse>,
        > {
            Box::pin(async { Ok(echo_agent::human_loop::HumanLoopResponse::Approved) })
        }
    }

    /// Compile-time guarantee that `RunPayload` is `Send + 'static` — the
    /// prerequisite for `tokio::spawn(drive_run_async(payload))` on the
    /// background path (spec §5.6 mine 1). If any field loses `Send`, this
    /// fails to compile.
    #[test]
    fn run_payload_is_send_and_static() {
        fn assert_send_static<T: Send + 'static>() {}
        assert_send_static::<RunPayload>();
    }

    #[test]
    fn independent_run_hitl_policy_preserves_only_attended_surface_provider() -> Result<(), String>
    {
        use crate::tasks::task_runtime::AttendedMode;

        let provider: Arc<dyn HumanLoopProvider> = Arc::new(FixedHumanLoopProvider);
        let attended =
            admitted_human_loop_provider(AttendedMode::Attended, Some(Arc::clone(&provider)))
                .ok_or_else(|| "attended run dropped its surface HITL provider".to_string())?;
        assert!(Arc::ptr_eq(&attended, &provider));
        assert!(
            admitted_human_loop_provider(AttendedMode::Unattended, Some(provider)).is_none(),
            "unattended run retained an interactive HITL provider"
        );
        Ok(())
    }

    #[tokio::test]
    async fn background_complex_run_keeps_auto_ingest_scope_after_driver_waiter_abort()
    -> Result<(), String> {
        use crate::tasks::task_runtime::{AttendedMode, DomainProfile};
        use echo_agent::testing::MockLlmClient;

        let workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
        let shadow = tempfile::tempdir().map_err(|error| error.to_string())?;
        let store = Arc::new(
            TaskRuntimeStore::new_in_memory_with_shadow_root(shadow.path())
                .map_err(|error| error.to_string())?,
        );
        let run_id = "background-auto-ingest";
        store
            .create_run(
                run_id,
                "global",
                "background-conversation",
                "background-root",
                DomainProfile::AcademicResearch,
                "persist research evidence",
                "agent_autonomous",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .configure_run_continuation(run_id, true, false, None, None)
            .map_err(|error| error.to_string())?;
        store
            .transition_run(run_id, TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        let llm = Arc::new(
            MockLlmClient::new()
                .with_model_name("background-auto-ingest")
                .then_tool_call("research-call", "semantic_scholar_search", "{}")
                .with_response("Research evidence persisted."),
        );
        let agent = crate::agent_handle::AgentHandle::new(
            echo_agent::agent::ReactAgentBuilder::new()
                .model("background-auto-ingest")
                .llm_client(llm)
                .build()
                .map_err(|error| error.to_string())?,
        );
        let identity = crate::workspace::WorkspaceIoIdentity::global(workspace.path());
        let receipt = crate::state::ScopedWorkspaceIoReceipt::global_for_test(workspace.path());
        let workspace_io = receipt.invocation();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        agent
            .write(move |agent| {
                crate::research_connectors::install_auto_ingest_barrier_fixture(
                    agent, identity, entered_tx, release_rx,
                );
            })
            .await;
        let mut driver = tokio::spawn(drive_run_async_inner(
            RunExecutionPayload {
                run_id: run_id.to_string(),
                store,
                cancel: CancellationToken::new(),
                memory_generation: None,
                trace_sink: None,
                prompt: "Search for durable Agent research runtimes.".to_string(),
                plan_policy: RunPlanPolicy::AllowDirect,
                workspace_io: Some(workspace_io),
            },
            agent,
        ));
        tokio::select! {
            entered = entered_rx => {
                if entered.is_err() {
                    let outcome = driver.await;
                    return Err(format!(
                        "background AutoIngest closure did not start; driver outcome: {outcome:?}"
                    ));
                }
            }
            outcome = &mut driver => {
                return Err(format!(
                    "background driver settled before AutoIngest entered: {outcome:?}"
                ));
            }
        }
        driver.abort();
        let _ = driver.await;
        release_tx
            .send(())
            .map_err(|_| "background AutoIngest closure dropped its release barrier".to_string())?;
        for _ in 0..100 {
            if crate::research::list_sources(workspace.path(), None, None)
                .is_ok_and(|sources| !sources.is_empty())
            {
                return Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        Err("background AutoIngest did not persist after driver waiter abort".to_string())
    }

    /// Run-level cancel token roundtrip: cancellation does not release driver
    /// ownership; the registration remains until the driver settles and drops.
    #[test]
    fn run_cancel_token_roundtrip() -> Result<(), String> {
        use crate::tasks::task_runtime::store::TaskRuntimeStore;
        use crate::tasks::task_runtime::{AttendedMode, DomainProfile, TaskRunStatus};
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        store
            .create_run(
                "r1",
                "default",
                "conversation",
                "message",
                DomainProfile::General,
                "cancel roundtrip",
                "test",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run("r1", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        let tok = CancellationToken::new();
        let registration = store
            .register_run_cancellation("r1", tok.clone())
            .map_err(|error| error.to_string())?;
        assert!(
            store
                .request_cancel("r1")
                .map_err(|error| error.to_string())?,
            "first request_cancel should find and trigger the token"
        );
        assert!(tok.is_cancelled(), "token should be cancelled");
        assert!(
            store
                .request_cancel("r1")
                .map_err(|error| error.to_string())?,
            "the active driver remains addressable until registration drop"
        );
        drop(registration);
        assert!(!store.is_run_active("r1"));
        Ok(())
    }

    #[test]
    fn nested_run_cancel_registration_keeps_outer_driver() -> Result<(), String> {
        use crate::tasks::task_runtime::store::TaskRuntimeStore;
        use crate::tasks::task_runtime::{AttendedMode, DomainProfile, TaskRunStatus};

        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        store
            .create_run(
                "r1",
                "default",
                "conversation",
                "message",
                DomainProfile::General,
                "nested cancel",
                "test",
                AttendedMode::Attended,
            )
            .map_err(|error| error.to_string())?;
        store
            .transition_run("r1", TaskRunStatus::Running)
            .map_err(|error| error.to_string())?;
        let outer = CancellationToken::new();
        let inner = CancellationToken::new();
        let _outer_registration = store
            .register_run_cancellation("r1", outer.clone())
            .map_err(|error| error.to_string())?;
        {
            let _inner_registration = store
                .register_run_cancellation("r1", inner)
                .map_err(|error| error.to_string())?;
        }

        assert!(
            store
                .request_cancel("r1")
                .map_err(|error| error.to_string())?
        );
        assert!(outer.is_cancelled());
        Ok(())
    }
}
