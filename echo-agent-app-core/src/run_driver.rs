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
//! - `AgentPool::acquire` returns an **owned** `AgentHandle` (not a guard);
//!   the pool's write-lock is held only during `create_agent` and released
//!   before `execute_run` runs — so `execute_run` holds no pool lock. No
//!   deadlock. (Current `agent_pool.rs:264` already does this; preserved.)
//! - Background runs MUST use an independent `CancellationToken` (spec §5.5),
//!   never the chat turn's token — the caller (`create_complex_task`) supplies
//!   it via `RunPayload.cancel`.
//!
//! Result closure (emit-after-write) is added in Phase B5 (spec §6.1); here we
//! only drive `execute_run`, which already handles 6-state transitions +
//! fire-and-forget `memory_bridge`.

use std::sync::Arc;

use echo_agent::agent::CancellationToken;
use echo_agent::evolution::MemoryLayerManager;
use echo_agent::llm::LlmClient;

use crate::agent_pool::AgentPool;
use crate::tasks::task_runtime::executor::{RunOutcome, WorkerTraceSink, execute_run};
use crate::tasks::task_runtime::store::TaskRuntimeStore;

/// Fully-owned payload for driving a Run in the background or foreground.
///
/// Every field is owned or `Arc`'d so the whole struct is `Send + 'static`
/// and can be `move`d into `tokio::spawn`. `brief` is intentionally absent —
/// `execute_run` reads the run's goal/plan from the store, so the driver needs
/// no copy of it.
pub struct RunPayload {
    pub run_id: String,
    pub pool: Arc<AgentPool>,
    pub store: Arc<TaskRuntimeStore>,
    /// Independent cancel token for this run (NOT the chat turn's token for
    /// background runs — see spec §5.5). Foreground runs may share the turn
    /// token, but using an independent one keeps the logic uniform.
    pub cancel: CancellationToken,
    pub reviewer_llm: Option<Arc<dyn LlmClient>>,
    pub layer_manager: Option<Arc<MemoryLayerManager>>,
    /// Worker-trace sink forwarded into `execute_run` so worker events reach
    /// the frontend. `Some` for foreground (inline streaming to the chat sink);
    /// `None` for background (events go via Tauri emit from the trace path).
    pub trace_sink: Option<WorkerTraceSink>,
}

/// Drive an already-created TaskRuntime run to completion on an isolated pool
/// agent. Acquires a pool agent for `run_id`, hands it to `execute_run` as the
/// `primary_agent`, and returns the run's terminal outcome.
///
/// `execute_run` internally performs the 6-state transitions
/// (Running → Completed/Failed/Paused/Cancelled) and fires
/// `memory_bridge::write_memory_candidate` at terminal states. This function
/// adds only the pool-isolation wiring on top.
pub async fn drive_run_async(payload: RunPayload) -> Result<RunOutcome, String> {
    // acquire returns an OWNED AgentHandle (not a guard); the pool write-lock
    // is held only during create_agent and released here, so execute_run below
    // runs without holding any pool lock (spec §5.6, no deadlock).
    let pool_agent = payload
        .pool
        .acquire(&payload.run_id)
        .await
        .map_err(|e| format!("pool acquire failed for run {}: {e}", payload.run_id))?;
    let result = execute_run(
        payload.store.clone(),
        Some(pool_agent),
        payload.reviewer_llm.clone(),
        payload.layer_manager.clone(),
        None, // run_store (RunStore) — not wired from the chat path
        payload.trace_sink.clone(),
        &payload.run_id,
        payload.cancel,
        // B5.1: autonomous runs block the memory write so a Completed run has
        // its taskrun:completed:{run_id} memory durable before any follow-up
        // question can fire (eliminates the recall race, spec §6.1). layer_manager
        // must be Some for the write to actually happen; if None the write is a
        // no-op (blocking + None → returns immediately, same as before B5.1).
        crate::tasks::task_runtime::memory_bridge::MemoryPolicy::Blocking,
    )
    .await;
    // Run is terminal (or failed to start) — drop its run-level cancel token so
    // the map doesn't accumulate stale entries. No-op if `cancel_run` already
    // removed it (spec §5.5 cleanup).
    payload.store.unregister_run_cancel_token(&payload.run_id);
    result.map_err(|e| format!("execute_run failed for run {}: {e}", payload.run_id))
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
    fn run_cancel_token_roundtrip() {
        use crate::tasks::task_runtime::store::TaskRuntimeStore;
        let store = TaskRuntimeStore::new_in_memory().expect("in-memory store");
        let tok = CancellationToken::new();
        store.register_run_cancel_token("r1", tok.clone());
        assert!(
            store.cancel_run("r1"),
            "first cancel_run should find and trigger the token"
        );
        assert!(tok.is_cancelled(), "token should be cancelled");
        assert!(
            !store.cancel_run("r1"),
            "second cancel_run is a no-op — token already removed"
        );
    }
}
