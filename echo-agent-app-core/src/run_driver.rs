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
use echo_agent::evolution::MemoryLayerManager;

use crate::agent_pool::AgentPool;
use crate::tasks::task_runtime::executor::{ExecSink, RunOutcome, RunPlanPolicy, drive_agent_run};
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
    pub layer_manager: Option<Arc<MemoryLayerManager>>,
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
    pub(crate) receipt_owner: crate::tasks::task_runtime::store::RunDriverReceiptOwner,
}

struct RunExecutionPayload {
    run_id: String,
    store: Arc<TaskRuntimeStore>,
    cancel: CancellationToken,
    layer_manager: Option<Arc<MemoryLayerManager>>,
    trace_sink: Option<ExecSink>,
    prompt: String,
    plan_policy: RunPlanPolicy,
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
        layer_manager,
        trace_sink,
        prompt,
        plan_policy,
        receipt_owner,
    } = payload;
    let settlement_store = store.clone();
    let settlement_run_id = run_id.clone();
    let settlement_cancel = cancel.clone();
    let pool_lease = match pool.acquire(&run_id).await {
        Ok(lease) => lease,
        Err(error) => {
            let message = format!("pool acquire failed for run {run_id}: {error}");
            settle_driver_error(&store, &run_id, &message, cancel.is_cancelled())?;
            return Err(message);
        }
    };
    let pool_agent = pool_lease.agent();
    receipt_owner.retain(pool.retain_for_supervised_run(run_id.clone(), pool_lease));
    let execute_plan =
        crate::tasks::task_runtime::ExecuteTaskTool::new(store.clone(), pool_agent.clone());
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
            layer_manager,
            trace_sink,
            prompt,
            plan_policy,
        },
        pool_agent,
    )
    .await;
    if let Err(error) = &result {
        settle_driver_error(
            &settlement_store,
            &settlement_run_id,
            error,
            settlement_cancel.is_cancelled(),
        )?;
    }
    // The TaskRuntime supervisor releases the pool receipt only after its own
    // durable terminal readback (or after settlement debt retry succeeds).
    result
}

async fn drive_run_async_inner(
    payload: RunExecutionPayload,
    pool_agent: crate::agent_handle::AgentHandle,
) -> Result<RunOutcome, String> {
    let _cancel_registration = payload
        .store
        .register_run_cancellation(&payload.run_id, payload.cancel.clone())
        .map_err(|error| format!("register run cancellation failed: {error}"))?;
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
    )
    .await;
    drive_result.map_err(|error| {
        format!(
            "agent-driven run failed for run {}: {error}",
            payload.run_id
        )
    })?;

    let run = payload
        .store
        .get_run(&payload.run_id)
        .map_err(|error| format!("read final run status failed: {error}"))?
        .ok_or_else(|| format!("run {} disappeared after execution", payload.run_id))?;
    let outcome = match run.status {
        TaskRunStatus::Completed => RunOutcome::Completed,
        TaskRunStatus::Cancelled => RunOutcome::Cancelled,
        TaskRunStatus::Failed => RunOutcome::Failed {
            failed_task_id: "<run>".to_string(),
            error: "agent-driven run failed".to_string(),
        },
        TaskRunStatus::Paused => RunOutcome::Paused {
            failed_task_id: "<run>".to_string(),
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
            MemoryPolicy::Blocking,
            payload.layer_manager.as_ref(),
            &payload.store,
            event,
        )
        .await;
    }
    Ok(outcome)
}

fn settle_driver_error(
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-time guarantee that `RunPayload` is `Send + 'static` — the
    /// prerequisite for `tokio::spawn(drive_run_async(payload))` on the
    /// background path (spec §5.6 mine 1). If any field loses `Send`, this
    /// fails to compile.
    #[test]
    fn run_payload_is_send_and_static() {
        fn assert_send_static<T: Send + 'static>() {}
        assert_send_static::<RunPayload>();
    }

    /// Run-level cancel token roundtrip (spec §5.5): register → cancel triggers
    /// the token → second cancel is a no-op (token removed). Mirrors the
    /// task-level `cancel_task` semantics.
    #[test]
    fn run_cancel_token_roundtrip() -> Result<(), String> {
        use crate::tasks::task_runtime::store::TaskRuntimeStore;
        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
        let tok = CancellationToken::new();
        let _registration = store
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
            !store
                .request_cancel("r1")
                .map_err(|error| error.to_string())?,
            "second request_cancel is a no-op — token already removed"
        );
        Ok(())
    }

    #[test]
    fn nested_run_cancel_registration_restores_outer_driver() -> Result<(), String> {
        use crate::tasks::task_runtime::store::TaskRuntimeStore;

        let store = Arc::new(TaskRuntimeStore::new_in_memory().map_err(|error| error.to_string())?);
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
